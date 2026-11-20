//! vrp-1 求解内核：内部表示、评分（约束集 v2，见 docs/constraints-v2.md）、搜索。
//!
//! 只依赖标准库，不做 I/O，不持有全局状态。

pub mod constraint;
pub mod eval;
pub mod execution;
mod numeric;
pub mod oracle;
pub mod problem;
pub mod rng;
pub mod score;
pub mod search;
pub mod solution;
pub mod state;
mod timing;

pub use constraint::{CONSTRAINT_COUNT, ConstraintId, Weight, Weights};
pub use problem::{
    AgentSpec, Matrix, Problem, ProblemError, ProblemSpec, TicketKind, TicketSpec, UNREACHABLE,
};
pub use score::Score;
pub use search::{
    SearchError, SearchMonitor, SearchOptions, SearchProgress, SearchResult, SearchStrategy,
    StopReason, solve, solve_observed,
};
pub use solution::{NONE, Solution, SolutionError};
pub use state::{Change, State};
