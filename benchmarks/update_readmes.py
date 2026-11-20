"""Publish bilingual evidence summaries and figures after every record is terminal."""
import argparse
import json
from pathlib import Path

ROOT=Path(__file__).resolve().parents[1]
START='<!-- solver-evaluation:start -->'
END='<!-- solver-evaluation:end -->'
LABELS={
    'a01-basic-50':('Basic delivery','基础配送'),
    'a02-dense-200':('Dense / long routes','密集长路线'),
    'a03-regional-480':('Regional / four depots','跨区四网点'),
    'a04-growth-1000':('Scale / heterogeneous limits','规模与异构上限'),
    'a05-skills-levels-200':('Skills / qualifications / capacity','技能、资质与容量'),
    'a06-tight-wait-capacity-50':('Tight windows / waits / capacity','紧窗、等待与容量')}
ACCEPTANCE_LABELS={
    'b01-pickup-two-dimensions-200':('Pickup / two capacity dimensions','取送与双维容量'),
    'b02-precedence-50':('Precedence / windows / shifts','前置依赖、窗口与班次'),
    'b03-execution-insert-200':('Insert during execution','执行中插单'),
    'b04-execution-cancel-50':('Cancel loaded delivery','取消已装货配送'),
    'b05-fault-completed-lock-50':('Vehicle fault / completed work / locks','车辆故障、完成事实与锁定'),
    'b06-insufficient-unreachable-50':('Insufficient resources / conflicts / unreachable','资源不足、冲突与不可达')}


def update(summary_path,preview_dir=None):
    s=json.loads(Path(summary_path).read_text(encoding='utf-8'))
    if s['missing_records'] or s['observed_records']!=s['expected_records']:
        raise ValueError('README publication requires a complete campaign, including failures')
    if not s['versions']['vrp0_commit']: raise ValueError('no legacy comparison: publish the native-only report without cross-version README claims')
    config=s['versions']['config']
    if any(x.get('private') or x.get('request_file') for x in config['scenarios']): raise ValueError('private individual results cannot be published')
    from readme_figures import render
    render(summary_path,preview_dir)
    comparisons={(x['scenario'],x['mode']):x for x in s['comparisons']}
    rows={(x['scenario'],x['mode'],x['engine']):x for x in s['instances']}
    wins=sum(c['wins'] for c in s['comparisons']);ties=sum(c['ties'] for c in s['comparisons']);losses=sum(c['losses'] for c in s['comparisons'])
    excluded=sum(c['noncomparable'] for c in s['comparisons'])
    accepted=sum(x['acceptance_passed'] for x in s['instances'] if x['suite']=='acceptance' and x['engine']=='vrp1')
    total_acceptance=sum(x['observed'] for x in s['instances'] if x['suite']=='acceptance' and x['engine']=='vrp1')
    shared=sum(x['observed'] for x in s['instances'] if x['suite']=='common')
    full=sum(x['full_feasible'] for x in s['instances'] if x['suite']=='common')
    timed=s['observed_records']-s['statuses'].get('unsupported',0)
    overrun=s.get('evidence_verification',{}).get('max_observed_search_overrun_ms')
    gate=s['comparability_gate']
    def percent(scenario,mode):
        c=comparisons[(scenario,mode)]; d=c['cost_improvement_percent']; n=c['wins']+c['ties']+c['losses']
        return ('—' if d['median'] is None else f"{d['median']:+.2f}%")+f" ({n}/{len(config['seeds'])})"
    def resource(engine,key,scale=1):
        values=[x[key]['median']/scale for x in s['instances'] if x['engine']==engine and x['suite']=='common' and x[key]['median'] is not None]
        return '—' if not values else f'{min(values):.1f}–{max(values):.1f}'
    def first(scenario,engine):
        value=rows[(scenario,'cold',engine)]['first_full_feasible_ms']['median']
        return '—' if value is None else f'{value:.1f}'
    for lang,file,anchor in [('en','README.md','## Development and verification'),('zh','README.zh-CN.md','## 开发与验证')]:
        en=lang=='en'
        locale='en' if en else 'zh-CN'
        def figure(name,alt): return f'![{alt}](docs/benchmarks/round-1/readme-{name}.{locale}.svg)'
        title='Reproducible solver evaluation' if en else '可复现求解评测'
        lines=[START,'## '+title,'']
        lines += [('The comparison project, [vrp-0](https://github.com/Scisaga/vrp-0), is another open-source planning engine by the author of vrp-1, implemented in Java with OptaPlanner. The results below apply to the source revision, dependencies and configuration pinned in the report.' if en else
                   '对照项目 [vrp-0](https://github.com/Scisaga/vrp-0) 是本项目作者开源的另一个规划引擎，使用 Java / OptaPlanner 实现。以下结果仅对应报告中固定的源码提交、依赖与配置。'),'']
        if en:
            lines += [f"The first round measured **{len(config['scenarios'])} synthetic scenarios, {len(config['seeds'])} seeds, {config['budget_ms']/1000:g} seconds and {config['threads']} solver thread per run**, serially on Windows / Ryzen AI 7 PRO 350 with one CPU affinity and a 2 GiB process-tree limit. vrp-0 `{s['versions']['vrp0_commit'][:7]}` uses OptaPlanner `999-20240618-SNAPSHOT` / JDK 21; vrp-1 uses base `{s['versions']['vrp1_base_commit'][:7]}` plus the [recorded evaluation build](docs/benchmarks/round-1/report.md). Java receives three 20-second warmups per group; its last warmup still shows JIT activity, so fully compiled steady-state convergence has not been established.",'',
                f"**{timed} timed runs: {shared} shared-objective solves and {total_acceptance} native capability checks.** All {full}/{shared} shared outputs were fully assigned, hard=0 and independently validated. Paired vrp-1 **win/tie/loss: {wins}/{ties}/{losses}**, with {excluded} noncomparable pairs. The score gate checked {gate['checks']} samples ({gate['distinct_input_mappings']} distinct mappings) and {gate['full_problem_initial_plans']} full-problem initial plans before timing.",'',
                '**Final quality under the same budget.** Percentages are medians of paired reductions in total travel seconds; positive values favor this vrp-1 configuration. Whiskers show the minimum–maximum seed range. Cold construction and continuation from identical initial routes are separate experiments.','',
                figure('quality-summary','Paired final travel-cost reduction by scenario, with seed ranges'),'',
                '| Shared scenario (orders / vehicles) | Cold travel reduction | Continuation reduction | First full feasible: vrp-0 (ms) | vrp-1 (ms) |',
                '|---|---:|---:|---:|---:|']
        else:
            lines += [f"首轮实测 **{len(config['scenarios'])} 个合成场景，每例 {len(config['seeds'])} 个种子、每次 {config['budget_ms']/1000:g} 秒、{config['threads']} 个求解线程**，在 Windows / Ryzen AI 7 PRO 350 上串行执行，固定单 CPU 亲和性和 2 GiB 进程树内存上限。旧版为 vrp-0 `{s['versions']['vrp0_commit'][:7]}`、OptaPlanner `999-20240618-SNAPSHOT` / JDK 21；新版为基础提交 `{s['versions']['vrp1_base_commit'][:7]}` 加[记录的评测构建](docs/benchmarks/round-1/report.md)。每组 Java 先预热三次、每次 20 秒；末轮仍有 JIT 活动，尚未证明完全编译后的稳态收敛。",'',
                f"**实际计时 {timed} 次：共同目标求解 {shared} 次，新版能力验收 {total_acceptance} 次。** 共同集 {full}/{shared} 个最终方案全派、hard=0 且统一校验通过；新版配对**胜/平/负为 {wins}/{ties}/{losses}**，另有 {excluded} 对不可比较。计时前完成 {gate['checks']} 次小案例核对（{gate['distinct_input_mappings']} 种不同映射）及 {gate['full_problem_initial_plans']} 份完整初解核对。",'',
                '**同预算最终质量。** 百分比是配对总行驶秒数下降的中位数，正数表示此配置的 vrp-1 成本较低；误差线为五种子的最小–最大范围。冷建解与相同初始路线继续优化分别比较。','',
                figure('quality-summary','各场景最终行驶成本改善及种子范围'),'',
                '| 共同场景（单 / 车） | 冷建解成本下降 | 继续优化成本下降 | 首次全派可行：vrp-0（毫秒） | vrp-1（毫秒） |',
                '|---|---:|---:|---:|---:|']
        for spec in config['scenarios']:
            if spec['suite']!='common': continue
            label=LABELS[spec['id']][0 if en else 1]
            lines.append(f"| {label} ({spec['tickets']} / {spec['agents']}) | {percent(spec['id'],'cold')} | {percent(spec['id'],'continue')} | {first(spec['id'],'vrp0')} | {first(spec['id'],'vrp1')} |")
        lines += ['',
            ('Parentheses give eligible pairs, requiring both outputs fully feasible. First-full times are cold-construction medians: full assignment and zero hard violations, which is a different threshold from equal travel cost.' if en else
             '括号为有效配对数，要求双方全派可行。首次全派可行时间为冷建解中位数，标准是全部工单已派且无硬违约；它与达到同等行驶成本是不同门槛。'),'',
            '**Actual solve curves: cold construction.**' if en else '**真实求解曲线：从零建解。**','',
            figure('quality-cold','Cold construction: measured travel-cost curves for all six common scenarios' if en else '冷建解：六个共同场景的实测行驶成本曲线'),'',
            '**Actual solve curves: continuation from identical routes.**' if en else '**真实求解曲线：从相同初始路线继续优化。**','',
            figure('quality-continue','Continuation: measured travel-cost curves for all six common scenarios' if en else '继续优化：六个共同场景的实测行驶成本曲线'),'',
            ('Lines are medians of actual best-solution events; shading is the five-seed range. Cost is shown only after **all five seeds** are fully assigned with zero hard violations. Gaps remain missing, with no interpolation before the first feasible incumbent. Each panel has its own y scale; lower total travel hours is better. Search time includes candidate preprocessing and initial construction. [Full curves including unassigned counts](docs/benchmarks/round-1/report.md#quality-versus-time).' if en else
             '实线/虚线来自真实最佳方案事件的中位数，阴影为五种子范围。只有**五个种子全部全派且无硬违约后**才显示成本；首个可行方案前保留空白，不插值。各面板纵轴独立，总行驶小时越低越好；横轴预算包含候选预处理和初始建解。[包含未派数量的完整曲线](docs/benchmarks/round-1/report.md#quality-versus-time)。'),'',
            '**Time to equal business quality.**' if en else '**达到同等业务质量的耗时。**','',
            figure('equal-quality','First observed time to paired equal-cost targets, with seed ranges' if en else '达到配对同等成本目标的首次实测时间及种子范围'),'',
            ('For each scenario, start mode and seed, the target is the **worse of the two final fully feasible travel costs**. The figure shows the first actual time each solver reaches that target, on a logarithmic time axis. This is a post hoc paired target, not a preregistered industry quality threshold or a universal speedup factor. Missing hits remain censored; the [full report](docs/benchmarks/round-1/report.md#equal-quality-and-limitations) also retains the stricter, better-final-cost target.' if en else
             '每个场景、启动方式和种子，以**双方最终全派可行成本中较差者**为目标，查找各自首次达到的真实时刻；时间轴为对数刻度。这是事后配对目标，不是预先定义的行业质量标准，也不据此计算通用提速倍率。未命中保留删失标记；[完整报告](docs/benchmarks/round-1/report.md#equal-quality-and-limitations)另保留以较好最终成本为目标的更严格结果。'),'']
        if en:
            lines += ['**CPU, memory and stability.**','',
                '| Runtime | CPU seconds / command | Peak committed MiB | Memory lifetime |','|---|---:|---:|---|']
        else:
            lines += ['**CPU、内存与稳定性。**','',
                '| 运行时 | 每条指令 CPU 秒 | 峰值提交内存 MiB | 内存生命周期 |','|---|---:|---:|---|']
        lines += [f"| vrp-0 / prewarmed JVM | {resource('vrp0','cpu_seconds')} | {resource('vrp0','peak_tree_commit_bytes',1024**2)} | {'Group, including warmup and earlier seeds' if en else '整组 JVM，含预热和前序种子'} |",
                  f"| vrp-1 | {resource('vrp1','cpu_seconds')} | {resource('vrp1','peak_tree_commit_bytes',1024**2)} | {'One adapter process' if en else '单次适配器进程'} |",'']
        lines += [
            'Ranges span per-instance medians across both start modes; the figure below shows cold construction with seed ranges. CPU includes each measured command; Rust includes native result explanation, while Java startup/warmup is separate. Memory lifetimes differ; the logarithmic memory axis keeps both visible and does not establish a deployed-service memory ratio. Sampled RSS, exact wall times, cold startup and JIT activity remain in the full report.' if en else
            '表中范围来自两种启动方式下的各实例中位数；下图展示冷建解的中位数和种子范围。CPU 为测量指令的实际用量；Rust 含原生结果解释，Java 启动和预热单列。内存生命周期不同，内存图使用对数轴展示双方，不能当作完整部署服务的内存倍率。采样 RSS、实际墙钟、冷启动及 JIT 活动保留在完整报告。','',
            figure('resources','Measured CPU and peak process-tree committed memory, with seed ranges' if en else 'CPU 和进程树峰值提交内存实测及种子范围'),'']
        if overrun is not None:
            lines += [(f"All **{s['statuses'].get('succeeded',0)}/{timed} timed runs** in this public round succeeded; maximum observed search-budget overrun was **{overrun:.1f} ms**. Unsupported legacy capability records ({s['statuses'].get('unsupported',0)}) remain explicit and are excluded from algorithm rankings." if en else
                       f"本公开轮次 **{s['statuses'].get('succeeded',0)}/{timed} 次计时运行成功**；实际搜索预算最大超额 **{overrun:.1f} 毫秒**。旧版能力不支持记录（{s['statuses'].get('unsupported',0)} 条）明确保留，不进入算法排名。"),'']
        lines += [(f'**Native capability acceptance: {accepted}/{total_acceptance}.** These synthetic frozen execution projections use v2 semantics and are evaluated separately from shared algorithm quality.' if en else
                   f'**新版能力验收：{accepted}/{total_acceptance}。** 以下是采用 v2 语义的合成冻结执行投影，与共同算法质量分开统计。'),'',
                  '| Capability | Passed / runs | Median unassigned |' if en else '| 能力场景 | 通过 / 运行 | 未派数量中位数 |',
                  '|---|---:|---:|']
        for spec in config['scenarios']:
            if spec['suite']!='acceptance': continue
            row=next(x for x in s['instances'] if x['scenario']==spec['id'] and x['engine']=='vrp1')
            label=ACCEPTANCE_LABELS[spec['id']][0 if en else 1]
            lines.append(f"| {label} | {row['acceptance_passed']}/{row['observed']} | {row['unassigned']['median']} |")
        lines += ['',
            ('Fault acceptance explicitly keeps blocked cargo unassigned; the infeasible case tests truthful partial service, so neither row claims full assignment. These are frozen projections, not real field-event replays. Private historical migration results and diagnostics remain in `corpus/` and are excluded from all public totals.' if en else
             '故障验收明确要求故障货物未派；不可行场景验收的是如实给出部分安排，两者都不声称全派。这些是冻结投影，不是真实现场事件回放。私有历史迁移逐例结果及诊断保留在 `corpus/`，不混入此处公开统计。'),'',
            ('See the [method and reproduction commands](docs/evaluation.md), [complete report](docs/benchmarks/round-1/report.md), [published incumbent events](docs/benchmarks/round-1/traces.json), and [figure source fingerprints](docs/benchmarks/round-1/readme-figures.json). Conclusions apply to these versions, objectives, scenarios and budgets; they do not establish a general result about the OptaPlanner framework or production workloads.' if en else
             '详见[方法与复现命令](docs/evaluation.md)、[完整报告](docs/benchmarks/round-1/report.md)、[公开的真实改善事件](docs/benchmarks/round-1/traces.json)和[图表数据指纹](docs/benchmarks/round-1/readme-figures.json)。结论只适用于指定版本、目标、场景与预算，不代表对整个 OptaPlanner 框架或生产分布的判断。'),END,'']
        path=ROOT/file; text=path.read_text(encoding='utf-8'); section='\n'.join(lines)+'\n'
        if START in text:
            before,rest=text.split(START,1);_,after=rest.split(END,1); text=before+section+after.lstrip('\n')
        else: text=text.replace(anchor,section+anchor,1)
        link='<a href="#reproducible-solver-evaluation">Evaluation</a> ·' if en else '<a href="#可复现求解评测">评测</a> ·'
        if link not in text:
            devlink='<a href="#development-and-verification">Development</a> ·' if en else '<a href="#开发与验证">开发与验证</a> ·'
            text=text.replace(devlink,link+'\n  '+devlink,1)
        path.write_text(text,encoding='utf-8')


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--summary',default='docs/benchmarks/round-1/summary.json')
    p.add_argument('--preview-dir',help='optional local PNG directory for visual review')
    a=p.parse_args();update(a.summary,a.preview_dir)
