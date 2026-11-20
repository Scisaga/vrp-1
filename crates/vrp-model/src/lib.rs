//! 契约（`vrp-proto`）与内核（`vrp-core`）之间的转换。
//!
//! - [`compile`]：校验请求、用兜底估算补齐未知边，编译成 [`vrp_core::Problem`]；
//! - [`build_result`]：把方案和全量评分转回 `SolveResult`。
//! - [`Executor`]：在进程内共享线程预算、有界排队，再编译与运行求解。

mod compile;
mod executor;
pub mod fallback;
mod options;
mod ready;
mod result;
mod solve;
mod weights;

use std::fmt;

pub use compile::{Compiled, DEFAULT_ESTIMATE_BUFFER, MatrixStats, compile, materialize_matrices};
pub use executor::{ExecutionError, ExecutionPermit, ExecutionStatus, Executor};
pub use options::{ResolvedSolveOptions, ThreadBudget, resolve_options};
pub use ready::{prepare_matrices, validate_ready_matrices};
pub use result::{build_result, solution_to_proto, unassigned_reasons};
pub use solve::{solve, solve_observed};
pub use weights::{weight_slot, weight_slot_mut, weights_from_proto};

/// 请求不合法。消息面向调用方，指出具体字段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelError(pub String);

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ModelError {}

impl From<vrp_core::ProblemError> for ModelError {
    fn from(e: vrp_core::ProblemError) -> Self {
        ModelError(e.0)
    }
}

impl From<vrp_core::SolutionError> for ModelError {
    fn from(e: vrp_core::SolutionError) -> Self {
        ModelError(format!("initial_solution: {}", e.0))
    }
}
