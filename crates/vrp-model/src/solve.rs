use std::sync::atomic::AtomicBool;
use std::time::Duration;

use vrp_core::{SearchOptions, StopReason, oracle};
use vrp_proto::engine::v1 as pb;

use crate::{Compiled, ModelError, build_result};

/// 在调用方已经授予线程额度后运行求解；不负责队列、网络和文件 I/O。
pub fn solve(compiled: &Compiled, cancelled: &AtomicBool) -> Result<pb::SolveResult, ModelError> {
    solve_observed(compiled, cancelled, None)
}

pub fn solve_observed(
    compiled: &Compiled,
    cancelled: &AtomicBool,
    monitor: Option<&vrp_core::SearchMonitor>,
) -> Result<pb::SolveResult, ModelError> {
    let o = compiled.options;
    let result = vrp_core::solve_observed(
        &compiled.problem,
        compiled.initial.as_ref(),
        &SearchOptions {
            time_limit: o.time_limit_ms.map(Duration::from_millis),
            unimproved_limit: o.unimproved_stop_ms.map(Duration::from_millis),
            threads: o.threads as usize,
            seed: o.seed,
            strategy: match o.search_strategy {
                pb::SearchStrategy::Nearest => vrp_core::SearchStrategy::Candidate,
                pb::SearchStrategy::Baseline => vrp_core::SearchStrategy::Baseline,
                pb::SearchStrategy::Alpha => vrp_core::SearchStrategy::Alpha,
                pb::SearchStrategy::AlphaKOpt => vrp_core::SearchStrategy::AlphaKOpt,
            },
            k_opt_depth: o.k_opt_depth as usize,
        },
        cancelled,
        monitor,
    )
    .map_err(|e| ModelError(e.0))?;
    let evaluation = oracle::evaluate(&compiled.problem, &result.solution);
    if evaluation.score != result.score {
        return Err(ModelError(
            "final incremental score differs from independent oracle".into(),
        ));
    }
    let millis = |d: Duration| d.as_millis().min(u64::MAX as u128) as u64;
    Ok(build_result(
        compiled,
        &result.solution,
        &evaluation,
        pb::SolveStats {
            elapsed_ms: millis(result.elapsed),
            best_found_ms: millis(result.best_found),
            evaluated_moves: result.evaluated_moves,
            threads: result.threads as u32,
            seed: result.seed,
            engine_version: env!("CARGO_PKG_VERSION").into(),
            termination_reason: match result.stop_reason {
                StopReason::TimeLimit => "time_limit",
                StopReason::Unimproved => "unimproved",
                StopReason::Cancelled => "cancelled",
                StopReason::Completed => "completed",
            }
            .into(),
            alpha_profiles: result.alpha_profiles,
            ascent_iterations: result.ascent_iterations,
            preprocessing_ms: millis(result.preprocessing),
            k_opt_improvements: result.k_opt_improvements,
            patching_attempts: result.patching_attempts,
            patched_improvements: result.patched_improvements,
        },
    ))
}
