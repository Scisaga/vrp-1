//! vrp 命令行：import-v1 / eval / bench / solve / solve-isolated。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;
use std::{env, fs};

use prost::Message;
use vrp_core::constraint::ConstraintId;
use vrp_core::rng::Rng;
use vrp_core::{Change, Solution, State, oracle};
use vrp_import_v1::Imported;
use vrp_model::Compiled;
use vrp_proto::engine::v1 as pb;

const USAGE: &str = "用法：
  vrp import-v1 <v1 场景.json> <输出.pb>
  vrp export-scene <v1 请求.json> <原生场景.json> --id <稳定ID>
            [--source-crs GCJ-02|WGS84] [--source-name <相对路径>] [--sha256 <哈希>]
  vrp eval <v1 场景.json | 请求.pb> [--solution <v1 文件>] [--matrix <v1 文件>]
           [--weight <约束>=<权重>]... [--show <约束>]
            --solution 取另一份文件里的指派，--matrix 取另一份文件里的矩阵
            --weight 覆盖约束权重，可重复，例如 --weight balance_agent_loading=1soft
            --show 列出某条约束的前 20 条明细
  vrp bench [--corpus <目录>] [--moves <每个场景的随机改动数>]
            目录优先级：--corpus > VRP_CORPUS_DIR > ./corpus（从项目根目录执行）
            存在 bench.txt 时只运行其中的请求相对路径，否则递归读取 JSON
  vrp solve <请求.pb | v1 场景.json> <结果.pb>
            [--preset fast|standard|deep] [--time-limit-ms <毫秒>]
            [--unimproved-ms <毫秒>] [--threads <数量>] [--seed <整数>]
            [--cpu-multiplier <倍数>] [--weight <约束>=<权重>]...
            [--search nearest|baseline|alpha|alpha-kopt] [--k-opt-depth <2..12>]
            指定 --preset 会重置输入中的时限，再应用命令行时限覆盖
            CPU 倍数默认取 VRP_CPU_MULTIPLIER，未设置时为 1.0
  vrp solve-isolated <请求.pb | v1 场景.json> <结果.pb> [同 solve 选项]
            [--memory-mib <默认1024，0不限>] [--max-frame-mib <默认256>]
            [--setup-timeout-ms <默认30000>] [--cancel-grace-ms <默认2000>]";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("import-v1") => import_v1(&args[1..]),
        Some("export-scene") => export_scene(&args[1..]),
        Some("eval") => eval(&args[1..]),
        Some("bench") => bench(&args[1..]),
        Some("solve") => solve(&args[1..], false),
        Some("solve-isolated") => solve(&args[1..], true),
        Some("worker-child" | "solver-child") => {
            let limit = args
                .get(1)
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(256 * 1024 * 1024);
            vrp_worker::serve_child(limit).map_err(|e| e.to_string())
        }
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn solve(args: &[String], isolated: bool) -> Result<(), String> {
    let args = Args::parse(args)?;
    let [input, output] = args.positional[..] else {
        return Err(USAGE.into());
    };
    if input == output {
        return Err("请求和结果必须使用不同的文件路径".into());
    }
    if Path::new(output).exists() {
        return Err(format!("结果文件已存在，请使用新的路径：{output}"));
    }
    for &(name, _) in &args.options {
        if ![
            "preset",
            "time-limit-ms",
            "unimproved-ms",
            "threads",
            "seed",
            "cpu-multiplier",
            "weight",
            "search",
            "k-opt-depth",
        ]
        .contains(&name)
            && !(isolated
                && [
                    "memory-mib",
                    "max-frame-mib",
                    "cancel-grace-ms",
                    "setup-timeout-ms",
                ]
                .contains(&name))
        {
            return Err(format!("未知 solve 选项 --{name}"));
        }
    }
    let (mut request, imported) = load(Path::new(input))?;
    for warning in imported.iter().flat_map(|i| &i.warnings) {
        eprintln!("警告：{warning}");
    }
    let options = request.options.get_or_insert_with(Default::default);
    if let Some(preset) = args.option("preset") {
        options.preset = match preset {
            "fast" => pb::SolvePreset::Fast,
            "standard" => pb::SolvePreset::Standard,
            "deep" => pb::SolvePreset::Deep,
            _ => return Err("--preset 必须为 fast、standard 或 deep".into()),
        } as i32;
        options.time_limit_ms = 0;
        options.unimproved_stop_ms = 0;
    }
    for (name, slot) in [
        ("time-limit-ms", &mut options.time_limit_ms),
        ("unimproved-ms", &mut options.unimproved_stop_ms),
        ("seed", &mut options.seed),
    ] {
        if let Some(value) = args.option(name) {
            *slot = value
                .parse()
                .map_err(|_| format!("--{name} 必须是非负整数"))?;
        }
    }
    if let Some(value) = args.option("threads") {
        options.threads = value.parse().map_err(|_| "--threads 必须是 u32 整数")?;
    }
    if let Some(value) = args.option("search") {
        options.search_strategy = match value {
            "nearest" => pb::SearchStrategy::Nearest,
            "baseline" => pb::SearchStrategy::Baseline,
            "alpha" => pb::SearchStrategy::Alpha,
            "alpha-kopt" => pb::SearchStrategy::AlphaKOpt,
            _ => return Err("--search 必须为 nearest、baseline、alpha 或 alpha-kopt".into()),
        } as i32;
    }
    if let Some(value) = args.option("k-opt-depth") {
        options.k_opt_depth = value.parse().map_err(|_| "--k-opt-depth 必须为整数")?;
    }
    let resolved = vrp_model::resolve_options(Some(options)).map_err(|e| e.0)?;
    let multiplier = args
        .option("cpu-multiplier")
        .map(str::to_owned)
        .or_else(|| env::var("VRP_CPU_MULTIPLIER").ok())
        .unwrap_or_else(|| "1.0".into());
    let multiplier = multiplier
        .parse::<f64>()
        .map_err(|_| "CPU 倍数必须为正数")?;
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let budget = vrp_model::ThreadBudget::from_cpu_count(cpus, multiplier).map_err(|e| e.0)?;
    budget.validate_request(&resolved).map_err(|e| e.0)?;
    for text in args.all("weight") {
        let (constraint, weight) = parse_weight_override(text)?;
        let problem = request.problem.as_mut().ok_or("请求里没有 problem")?;
        *vrp_model::weight_slot_mut(
            problem.weights.get_or_insert_with(Default::default),
            constraint,
        ) = Some(weight);
    }
    // Freeze fully materialized input before either execution path; legacy CLI
    // fallback parameters retain their existing interpretation.
    vrp_model::materialize_matrices(&mut request).map_err(|e| e.0)?;
    vrp_model::validate_ready_matrices(&request).map_err(|e| e.0)?;
    let executor = vrp_model::Executor::new(budget, 0).map_err(|e| e.0)?;
    eprintln!(
        "求解参数：{:?}，总时限 {:?} ms，无改进时限 {:?} ms，{} 线程 / {} 额度",
        resolved.preset,
        resolved.time_limit_ms,
        resolved.unimproved_stop_ms,
        resolved.threads,
        budget.total_threads
    );
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let result = if isolated {
        let mut limits = vrp_worker::IsolationOptions::default();
        for (name, slot) in [
            ("cancel-grace-ms", &mut limits.cancel_grace),
            ("setup-timeout-ms", &mut limits.setup_timeout),
        ] {
            if let Some(value) = args.option(name) {
                *slot = std::time::Duration::from_millis(
                    value.parse().map_err(|_| format!("--{name} 必须为整数"))?,
                );
            }
        }
        if let Some(value) = args.option("memory-mib") {
            let mib: u64 = value.parse().map_err(|_| "--memory-mib 必须为非负整数")?;
            limits.memory_limit_bytes = if mib == 0 {
                None
            } else {
                Some(mib.checked_mul(1024 * 1024).ok_or("内存额度过大")?)
            };
        }
        if let Some(value) = args.option("max-frame-mib") {
            let mib: u32 = value.parse().map_err(|_| "--max-frame-mib 必须为正整数")?;
            limits.max_frame_bytes = mib.checked_mul(1024 * 1024).ok_or("IPC 帧上限过大")?;
        }
        let child_args = vec![
            "worker-child".into(),
            limits.max_frame_bytes.to_string().into(),
        ];
        let supervisor = vrp_worker::Supervisor::new(
            std::sync::Arc::new(executor),
            env::current_exe().map_err(|e| e.to_string())?,
            child_args,
            limits,
        )
        .map_err(|e| e.to_string())?;
        supervisor
            .solve(&request, &cancelled)
            .map_err(|e| e.to_string())?
    } else {
        executor
            .solve(&request, &cancelled)
            .map_err(|e| e.to_string())?
    };
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| format!("{output}: {e}"))?;
    file.write_all(&result.encode_to_vec())
        .map_err(|e| format!("{output}: {e}"))?;
    let score = result.score.unwrap();
    let stats = result.stats.as_ref().unwrap();
    println!(
        "{}hard/{}medium/{}soft；未派 {}；耗时 {} ms；评估 {} 次；停止原因 {}；结果 {}",
        score.hard,
        score.medium,
        score.soft,
        result.unassigned.len(),
        stats.elapsed_ms,
        stats.evaluated_moves,
        stats.termination_reason,
        output
    );
    Ok(())
}

/// 位置参数与 `--名称 值` 形式的选项。
struct Args<'a> {
    positional: Vec<&'a str>,
    options: Vec<(&'a str, &'a str)>,
}

impl<'a> Args<'a> {
    fn parse(args: &'a [String]) -> Result<Args<'a>, String> {
        let mut positional = Vec::new();
        let mut options = Vec::new();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if let Some(name) = a.strip_prefix("--") {
                let value = it
                    .next()
                    .ok_or_else(|| format!("选项 --{name} 缺少取值\n{USAGE}"))?;
                options.push((name, value.as_str()));
            } else {
                positional.push(a.as_str());
            }
        }
        Ok(Args {
            positional,
            options,
        })
    }

    fn option(&self, name: &str) -> Option<&'a str> {
        self.all(name).next()
    }

    /// 可重复的选项，按出现顺序。
    fn all(&self, name: &str) -> impl Iterator<Item = &'a str> {
        self.options
            .iter()
            .filter(move |(n, _)| *n == name)
            .map(|(_, v)| *v)
    }
}

/// 解析 `约束=权重`。权重写法同 v1 的 "0hard/1medium/0soft"，可以只写非零的级别（如 "1soft"），"0" 表示关闭。
fn parse_weight_override(text: &str) -> Result<(ConstraintId, pb::Weight), String> {
    let bad = || format!("--weight {text}：应写成 约束=权重，例如 balance_agent_loading=1soft");
    let (name, value) = text.split_once('=').ok_or_else(bad)?;
    let constraint = ConstraintId::from_name(name.trim()).ok_or(format!("未知约束 {name}"))?;
    let mut weight = pb::Weight::default();
    if value.trim() != "0" {
        for part in value.split('/') {
            let part = part.trim();
            let (number, slot) = if let Some(n) = part.strip_suffix("hard") {
                (n, &mut weight.hard)
            } else if let Some(n) = part.strip_suffix("medium") {
                (n, &mut weight.medium)
            } else if let Some(n) = part.strip_suffix("soft") {
                (n, &mut weight.soft)
            } else {
                return Err(bad());
            };
            *slot = number.trim().parse().map_err(|_| bad())?;
        }
    }
    Ok((constraint, weight))
}

fn read_json(path: &Path) -> Result<serde_json::Value, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: JSON 解析失败：{e}", path.display()))
}

/// 读入请求：`.pb` 直接解码，其余按 v1 JSON 导入。
fn load(path: &Path) -> Result<(pb::SolveRequest, Option<Imported>), String> {
    if path.extension().is_some_and(|e| e == "pb") {
        let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let request = pb::SolveRequest::decode(bytes.as_slice())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        return Ok((request, None));
    }
    let mut imported =
        vrp_import_v1::import(&read_json(path)?).map_err(|e| format!("{}: {e}", path.display()))?;
    let request = std::mem::take(&mut imported.request);
    Ok((request, Some(imported)))
}

fn export_scene(args: &[String]) -> Result<(), String> {
    let args = Args::parse(args)?;
    let [input, output] = args.positional[..] else {
        return Err(USAGE.into());
    };
    let id = args.option("id").ok_or("--id is required")?;
    let value = read_json(Path::new(input))?;
    let scene = vrp_import_v1::scene::export(
        &value,
        id,
        args.option("source-name").unwrap_or(input),
        args.option("sha256").unwrap_or_default(),
        args.option("source-crs"),
    )
    .map_err(|e| e.0)?;
    let bytes = serde_json::to_vec_pretty(&scene).map_err(|e| e.to_string())?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes).map_err(|e| e.to_string())?;
    println!("Native scenario exported");
    Ok(())
}

fn import_v1(args: &[String]) -> Result<(), String> {
    let args = Args::parse(args)?;
    let [input, output] = args.positional[..] else {
        return Err(USAGE.into());
    };
    let (request, imported) = load(Path::new(input))?;
    for w in imported.iter().flat_map(|i| &i.warnings) {
        eprintln!("警告：{w}");
    }
    fs::write(output, request.encode_to_vec()).map_err(|e| format!("{output}: {e}"))?;
    println!("已写入 {output}（{} 字节）", request.encoded_len());
    Ok(())
}

fn eval(args: &[String]) -> Result<(), String> {
    let args = Args::parse(args)?;
    let [input] = args.positional[..] else {
        return Err(USAGE.into());
    };
    let (mut request, imported) = load(Path::new(input))?;
    let mut warnings = imported
        .as_ref()
        .map(|i| i.warnings.clone())
        .unwrap_or_default();
    if let Some(path) = args.option("solution") {
        let imported = imported
            .as_ref()
            .ok_or("--solution 只能配合 v1 JSON 场景使用")?;
        let (solution, more) =
            vrp_import_v1::assignment_from(&read_json(Path::new(path))?, imported)
                .map_err(|e| format!("{path}: {e}"))?;
        request.initial_solution = Some(solution);
        warnings.extend(more);
    }
    if let Some(path) = args.option("matrix") {
        let imported = imported
            .as_ref()
            .ok_or("--matrix 只能配合 v1 JSON 场景使用")?;
        let filled =
            vrp_import_v1::fill_matrix_from(&read_json(Path::new(path))?, imported, &mut request)
                .map_err(|e| format!("{path}: {e}"))?;
        println!("矩阵：从 {path} 覆盖 {filled} 条边");
    }
    let mut overrides = Vec::new();
    for text in args.all("weight") {
        let (constraint, w) = parse_weight_override(text)?;
        let problem = request.problem.as_mut().ok_or("请求里没有 problem")?;
        let weights = problem.weights.get_or_insert_with(Default::default);
        *vrp_model::weight_slot_mut(weights, constraint) = Some(w);
        overrides.push(format!(
            "{} = {}hard/{}medium/{}soft",
            constraint.name(),
            w.hard,
            w.medium,
            w.soft
        ));
    }

    let started = Instant::now();
    let compiled = vrp_model::compile(&request).map_err(|e| e.0)?;
    let compile_ms = started.elapsed().as_secs_f64() * 1e3;
    let p = &compiled.problem;
    let solution = compiled
        .initial
        .clone()
        .unwrap_or_else(|| Solution::with_pinned(p));
    let evaluation = oracle::evaluate(p, &solution);
    let s = &compiled.stats;

    println!("场景：{input}");
    println!(
        "规模：工单 {}，agent {}，地点 {}，矩阵 {} 张",
        p.tickets.len(),
        p.agents.len(),
        p.matrices[0].n,
        p.matrices.len()
    );
    println!(
        "矩阵：实测 {}，缓存 {}，模型 {}，兜底 {}，不可达 {}（共 {} 条边）；编译 {compile_ms:.1} ms",
        s.exact, s.cached, s.model, s.fallback, s.unreachable, s.entries
    );
    if s.model + s.fallback > 0 {
        let b = &compiled.estimate_buffer;
        println!(
            "估算边排班余量：模型 +{}‰，兜底 +{}‰",
            b.model_permille, b.fallback_permille
        );
    }
    for o in &overrides {
        println!("权重覆盖：{o}");
    }
    if !warnings.is_empty() {
        println!("导入警告（{}）：", warnings.len());
        for w in warnings.iter().take(20) {
            println!("  - {w}");
        }
    }
    if let Some(score) = imported.as_ref().and_then(|i| i.recorded_score.as_ref()) {
        println!("v1 记录的分数：{score}");
    }
    println!(
        "v2 分数：{}（已派 {} / {}）",
        evaluation.score,
        solution.assigned_count(),
        p.tickets.len()
    );
    println!("约束明细：");
    for c in ConstraintId::ALL {
        let t = evaluation.total(c);
        if !p.weights.get(c).is_zero() {
            println!(
                "  {:<46} 匹配 {:>6}  量 {:>10}  {}",
                c.name(),
                t.matches,
                t.amount,
                t.score
            );
        }
    }
    if let Some(name) = args.option("show") {
        let constraint = ConstraintId::from_name(name).ok_or(format!("未知约束 {name}"))?;
        let start_of: std::collections::HashMap<u32, i64> = evaluation
            .routes
            .iter()
            .flat_map(|r| r.stops.iter().map(|s| (s.ticket, s.start)))
            .collect();
        println!("{name} 的明细（最多 20 条）：");
        for m in evaluation
            .matches
            .iter()
            .filter(|m| m.constraint == constraint)
            .take(20)
        {
            let agents: Vec<&str> = m
                .agents
                .iter()
                .map(|&a| p.agent_ids[a as usize].as_str())
                .collect();
            let tickets: Vec<String> = m
                .tickets
                .iter()
                .map(|&t| {
                    let tk = &p.tickets[t as usize];
                    let start = start_of.get(&t).map_or("-".into(), |s| clock(*s));
                    format!(
                        "{}（计划日 {}，时间窗 {}~{}，开始 {start}）",
                        p.ticket_ids[t as usize],
                        tk.day,
                        clock(tk.earliest_start),
                        clock(tk.latest_end)
                    )
                })
                .collect();
            println!(
                "  量 {:>6}  agent {:?}  工单 {}",
                m.amount,
                agents,
                tickets.join("，")
            );
        }
    }
    Ok(())
}

/// 相对计划起点的秒数 → "d+HH:MM:SS"。
fn clock(t: i64) -> String {
    let (d, s) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    format!("{d}+{:02}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
}

/// 语料目录下的全部 JSON 文件，按路径排序。
fn json_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            json_files(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
    Ok(())
}

/// 整理后的语料用显式请求清单，避免把历史结果、重复项和损坏原件当作问题。
fn benchmark_files(root: &Path) -> Result<(Vec<PathBuf>, bool), String> {
    let selection = root.join("bench.txt");
    if !selection.exists() {
        let mut files = Vec::new();
        json_files(root, &mut files).map_err(|e| format!("{}: {e}", root.display()))?;
        return Ok((files, false));
    }
    let canonical_root = root.canonicalize().map_err(|e| e.to_string())?;
    let contents = fs::read_to_string(&selection).map_err(|e| e.to_string())?;
    let mut files = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (line, value) in contents.lines().enumerate() {
        let value = value.trim();
        if value.is_empty() || value.starts_with('#') {
            continue;
        }
        let path = Path::new(value);
        if path.is_absolute() || path.extension().is_none_or(|e| e != "json") {
            return Err(format!("bench.txt 第 {} 行必须是 JSON 相对路径", line + 1));
        }
        let resolved = root
            .join(path)
            .canonicalize()
            .map_err(|e| format!("bench.txt 第 {} 行：{e}", line + 1))?;
        if !resolved.starts_with(&canonical_root) || !resolved.is_file() {
            return Err(format!("bench.txt 第 {} 行不在语料目录内", line + 1));
        }
        if seen.insert(resolved) {
            files.push(root.join(path));
        }
    }
    if files.is_empty() {
        return Err("bench.txt 没有选择任何请求；请检查 corpus/index.json 的排除原因".into());
    }
    Ok((files, true))
}

/// 随机把一张工单移到它的某个候选 agent 的随机位置，返回改动涉及的路线。
fn random_relocate(
    rng: &mut Rng,
    state: &State<'_>,
    from_buf: &mut Vec<u32>,
    to_buf: &mut Vec<u32>,
) -> Option<(Option<u32>, u32)> {
    let p = state.problem();
    let t = rng.below(p.tickets.len()) as u32;
    let candidates = &p.candidates[t as usize];
    if candidates.is_empty() {
        return None;
    }
    let to = candidates[rng.below(candidates.len())];
    let from = state.agent_of(t);
    to_buf.clear();
    to_buf.extend(state.route(to).iter().copied().filter(|&x| x != t));
    let at = rng.below(to_buf.len() + 1);
    to_buf.insert(at, t);
    match from {
        Some(f) if f != to => {
            from_buf.clear();
            from_buf.extend(state.route(f).iter().copied().filter(|&x| x != t));
            Some((Some(f), to))
        }
        _ => Some((None, to)),
    }
}

struct BenchRow {
    ns_per_move: f64,
    tickets: usize,
}

fn bench_one(path: &Path, root: &Path, moves: usize) -> Result<Option<BenchRow>, String> {
    let name = path
        .strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string();
    let json = read_json(path)?;
    if vrp_import_v1::find_plan(&json).is_none() {
        return Ok(None);
    }
    let mut imported =
        vrp_import_v1::import(&json).map_err(|e| format!("{name}: 导入失败：{e}"))?;
    let request = std::mem::take(&mut imported.request);
    let started = Instant::now();
    let compiled: Compiled =
        vrp_model::compile(&request).map_err(|e| format!("{name}: 编译失败：{e}"))?;
    let compile_ms = started.elapsed().as_secs_f64() * 1e3;
    let p = &compiled.problem;
    let solution = compiled
        .initial
        .clone()
        .unwrap_or_else(|| Solution::with_pinned(p));

    let started = Instant::now();
    let evaluation = oracle::evaluate(p, &solution);
    let oracle_us = started.elapsed().as_secs_f64() * 1e6;

    let mut state = State::new(p, &solution).map_err(|e| format!("{name}: {e}"))?;
    let mut rng = Rng::new(1);
    let (mut from_buf, mut to_buf) = (Vec::new(), Vec::new());
    let mut evaluated = 0usize;
    let started = Instant::now();
    if !p.tickets.is_empty() {
        for _ in 0..moves {
            if let Some((from, to)) = random_relocate(&mut rng, &state, &mut from_buf, &mut to_buf)
            {
                let routes: &[(u32, &[u32])] = match from {
                    Some(f) => &[(f, &from_buf), (to, &to_buf)],
                    None => &[(to, &to_buf)],
                };
                std::hint::black_box(state.evaluate(Change { routes }));
                evaluated += 1;
            }
        }
    }
    let ns_per_move = if evaluated > 0 {
        started.elapsed().as_secs_f64() * 1e9 / evaluated as f64
    } else {
        0.0
    };

    // 在真实数据上抽查：应用 200 个随机改动，每一步都与 oracle 对比。
    let mut mismatches = 0;
    for _ in 0..200.min(moves) {
        if p.tickets.is_empty() {
            break;
        }
        if let Some((from, to)) = random_relocate(&mut rng, &state, &mut from_buf, &mut to_buf) {
            let routes: &[(u32, &[u32])] = match from {
                Some(f) => &[(f, &from_buf), (to, &to_buf)],
                None => &[(to, &to_buf)],
            };
            let score = state.apply(Change { routes });
            mismatches += (score != oracle::evaluate(p, &state.solution()).score) as usize;
        }
    }

    let s = &compiled.stats;
    let fallback_pct = if s.entries > 0 {
        100.0 * s.fallback as f64 / s.entries as f64
    } else {
        0.0
    };
    println!(
        "{name}\n    工单 {:>4}  agent {:>4}  地点 {:>4}  兜底边 {:>5.1}%  警告 {:>2}  已派 {:>4}\n    v1 {:<34} v2 {}\n    编译 {compile_ms:>7.1} ms  全量评分 {oracle_us:>8.1} µs  增量评分 {ns_per_move:>7.0} ns/次  抽查{}",
        p.tickets.len(),
        p.agents.len(),
        p.matrices[0].n,
        fallback_pct,
        imported.warnings.len(),
        solution.assigned_count(),
        imported.recorded_score.as_deref().unwrap_or("-"),
        evaluation.score,
        if mismatches == 0 {
            "一致".to_string()
        } else {
            format!("不一致 {mismatches} 次")
        },
    );
    if mismatches > 0 {
        return Err(format!("{name}: 增量评分与 oracle 不一致"));
    }
    Ok(Some(BenchRow {
        ns_per_move,
        tickets: p.tickets.len(),
    }))
}

fn bench(args: &[String]) -> Result<(), String> {
    let args = Args::parse(args)?;
    let root = match args.option("corpus") {
        Some(dir) => PathBuf::from(dir),
        None => env::var_os("VRP_CORPUS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("corpus")),
    };
    let moves = args
        .option("moves")
        .map_or(Ok(20_000), |v| v.parse::<usize>())
        .map_err(|e| format!("--moves: {e}"))?;
    let (files, explicit_selection) = benchmark_files(&root)?;
    if explicit_selection {
        println!("使用 bench.txt 请求清单；未选择的原件及回放记录见 corpus/index.json");
    }
    if files.is_empty() {
        return Err(format!(
            "{} 中没有 JSON 测试数据；请将测试场景放入该目录，或用 --corpus 指定目录",
            root.display()
        ));
    }

    let (mut rows, mut skipped, mut failures) = (Vec::new(), 0, Vec::new());
    for path in &files {
        match bench_one(path, &root, moves) {
            Ok(Some(row)) => rows.push(row),
            Ok(None) if explicit_selection => {
                failures.push(format!("{}: 清单条目不是场景请求", path.display()));
            }
            Ok(None) => skipped += 1,
            Err(e) => failures.push(e),
        }
    }
    println!(
        "\n共 {} 个 JSON：评测 {}，跳过 {}（不是场景文件），失败 {}",
        files.len(),
        rows.len(),
        skipped,
        failures.len()
    );
    for f in &failures {
        println!("  失败：{f}");
    }
    let measured: Vec<&BenchRow> = rows.iter().filter(|r| r.tickets > 0).collect();
    if !measured.is_empty() {
        let mut ns: Vec<f64> = measured.iter().map(|r| r.ns_per_move).collect();
        ns.sort_by(f64::total_cmp);
        println!(
            "增量评分：中位数 {:.0} ns/次，最慢 {:.0} ns/次",
            ns[ns.len() / 2],
            ns[ns.len() - 1]
        );
    }
    if !failures.is_empty() {
        return Err(format!("基准失败：{} 个请求未通过", failures.len()));
    }
    if rows.is_empty() {
        return Err("没有可评测的场景请求".into());
    }
    Ok(())
}
