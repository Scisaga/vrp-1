//! Offline evaluation adapter. Inputs/results remain generated engine ProtoJSON.
use serde_json::json;
use std::fs;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use vrp_proto::engine::v1 as pb;

fn emit(value: serde_json::Value) {
    let mut out = io::stdout().lock();
    writeln!(out, "{value}").expect("write progress");
    out.flush().expect("flush progress");
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    emit(
        json!({"event":"process_started", "unix_ms":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs_f64()*1000.0}),
    );
    let args: Vec<_> = std::env::args().collect();
    if args.len() < 4 {
        return Err("usage: vrp-bench-adapter freeze|eval|solve input.json output.json [budget_ms seed threads strategy cancel_file]".into());
    }
    let io_start = Instant::now();
    let mut request: pb::SolveRequest = if args[1] == "freeze-v1" {
        let value = serde_json::from_slice(&fs::read(&args[2])?)?;
        let mut imported = vrp_import_v1::import(&value)?;
        let mut request = std::mem::take(&mut imported.request);
        let mut coordinate_conflicts = 0;
        let overlay_edges = if let Some(path) = args.get(4) {
            let matrix_value = serde_json::from_slice(&fs::read(path)?)?;
            let matrix_import = vrp_import_v1::import(&matrix_value)?;
            if let (Some(problem), Some(other)) = (&request.problem, &matrix_import.request.problem)
            {
                let coords: std::collections::HashMap<_, _> = other
                    .locations
                    .iter()
                    .map(|p| (p.id.as_str(), (p.lng, p.lat)))
                    .collect();
                coordinate_conflicts = problem
                    .locations
                    .iter()
                    .filter(|p| {
                        coords
                            .get(p.id.as_str())
                            .is_some_and(|v| *v != (p.lng, p.lat))
                    })
                    .count();
            }
            if coordinate_conflicts == 0 {
                vrp_import_v1::fill_matrix_from(&matrix_value, &imported, &mut request)?
            } else {
                0
            }
        } else {
            0
        };
        let conversion = json!({"warnings":imported.warnings,"matrix_overlay_edges":overlay_edges,
            "paired_coordinate_conflicts":coordinate_conflicts,
            "matrix_policy":if coordinate_conflicts==0 {"paired matrix overlay, then frozen local fallback"} else {"paired coordinates differ: use original request and frozen local fallback only"},
            "problem_source":"original request; optional paired result supplies matrix only"});
        fs::write(
            std::path::Path::new(&args[3]).with_extension("conversion.json"),
            serde_json::to_vec_pretty(&conversion)?,
        )?;
        request
    } else {
        serde_json::from_slice(&fs::read(&args[2])?)?
    };
    let input_ms = io_start.elapsed().as_secs_f64() * 1000.0;
    if args[1] == "freeze" || args[1] == "freeze-v1" {
        vrp_model::materialize_matrices(&mut request)?;
        vrp_model::validate_ready_matrices(&request)?;
        let compiled = vrp_model::compile(&request)?;
        let weights = request
            .problem
            .as_mut()
            .ok_or("missing problem")?
            .weights
            .get_or_insert_with(Default::default);
        for constraint in vrp_core::constraint::ConstraintId::ALL {
            let weight = compiled.problem.weights.get(constraint);
            *vrp_model::weight_slot_mut(weights, constraint) = Some(pb::Weight {
                hard: weight.hard,
                medium: weight.medium,
                soft: weight.soft,
            });
        }
        fs::write(&args[3], serde_json::to_vec(&request)?)?;
        return Ok(());
    }
    if args[1] == "solve" {
        if args.len() != 9 {
            return Err("solve needs budget_ms seed threads strategy cancel_file".into());
        }
        let options = request.options.get_or_insert_with(Default::default);
        options.time_limit_ms = args[4].parse()?;
        options.seed = args[5].parse()?;
        options.threads = args[6].parse()?;
        options.search_strategy = args[7].parse()?;
        options.unimproved_stop_ms = 0;
        options.preset = 0;
    }
    vrp_model::validate_ready_matrices(&request)?;
    let begin = Instant::now();
    let compiled = vrp_model::compile(&request)?;
    let compile_ms = begin.elapsed().as_secs_f64() * 1000.0;
    emit(json!({"event":"ready", "input_ms":input_ms, "compile_ms":compile_ms}));
    if args[1] == "eval" {
        let solution = compiled
            .initial
            .clone()
            .unwrap_or_else(|| vrp_core::Solution::with_pinned(&compiled.problem));
        let e = vrp_core::oracle::evaluate(&compiled.problem, &solution);
        let result = vrp_model::build_result(&compiled, &solution, &e, pb::SolveStats::default());
        fs::write(&args[3], serde_json::to_vec(&result)?)?;
        return Ok(());
    }
    let monitor = vrp_core::search::SearchMonitor::with_history();
    let cancelled = AtomicBool::new(false);
    let done = AtomicBool::new(false);
    let solve_start = Instant::now();
    let mut model_total_ms = 0.0;
    let result = std::thread::scope(|scope| {
        let poller = scope.spawn(|| {
            while !done.load(Ordering::Acquire) {
                if std::path::Path::new(&args[8]).exists() {
                    cancelled.store(true, Ordering::Release);
                }
                for e in monitor.take_history() {
                    emit(
                        json!({"event":"incumbent", "elapsed_ms":e.elapsed.as_secs_f64()*1000.0,
                        "score":[e.score.hard,e.score.medium,e.score.soft], "assigned":e.assigned}),
                    );
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let result = vrp_model::solve_observed(&compiled, &cancelled, Some(&monitor));
        model_total_ms = solve_start.elapsed().as_secs_f64() * 1000.0;
        done.store(true, Ordering::Release);
        poller.join().expect("progress thread");
        result
    })?;
    for e in monitor.take_history() {
        emit(
            json!({"event":"incumbent", "elapsed_ms":e.elapsed.as_secs_f64()*1000.0,
            "score":[e.score.hard,e.score.medium,e.score.soft], "assigned":e.assigned}),
        );
    }
    let solve_and_explain_ms = solve_start.elapsed().as_secs_f64() * 1000.0;
    let out_start = Instant::now();
    fs::write(&args[3], serde_json::to_vec(&result)?)?;
    emit(
        json!({"event":"finished", "solve_and_explain_ms":solve_and_explain_ms,
        "search_elapsed_ms":monitor.snapshot().map(|s| s.elapsed.as_secs_f64()*1000.0),
        "explain_and_result_ms":model_total_ms-result.stats.as_ref().map_or(0.0, |s| s.elapsed_ms as f64),
        "output_ms":out_start.elapsed().as_secs_f64()*1000.0, "stats":result.stats}),
    );
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
