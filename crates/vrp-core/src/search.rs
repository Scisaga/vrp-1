//! 可取消、按时间预算运行的多起点搜索；所有线程共享同一个最佳方案及停止条件。
use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::rng::Rng;
use crate::{Change, Problem, Score, Solution, State};

mod alpha;
mod kopt;
mod monitor;
mod neighborhood;
pub use monitor::{SearchEvent, SearchMonitor, SearchProgress};
use neighborhood::{Candidates, MoveBuffers};

/// Baseline 用于同预算对照；Candidate 参考 LKH 的候选边与扰动思想，非完整 LKH-3。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchStrategy {
    Baseline,
    #[default]
    Candidate,
    Alpha,
    AlphaKOpt,
}

#[derive(Clone, Debug)]
pub struct SearchOptions {
    pub time_limit: Option<Duration>,
    pub unimproved_limit: Option<Duration>,
    pub threads: usize,
    pub seed: u64,
    pub strategy: SearchStrategy,
    pub k_opt_depth: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            time_limit: Some(Duration::from_secs(30)),
            unimproved_limit: None,
            threads: 1,
            seed: 0,
            strategy: SearchStrategy::Candidate,
            k_opt_depth: 5,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum StopReason {
    TimeLimit = 1,
    Unimproved = 2,
    Cancelled = 3,
    Completed = 4,
}

#[derive(Debug)]
pub struct SearchResult {
    pub solution: Solution,
    pub score: Score,
    pub elapsed: Duration,
    pub best_found: Duration,
    pub evaluated_moves: u64,
    pub threads: usize,
    pub seed: u64,
    pub stop_reason: StopReason,
    pub alpha_profiles: u32,
    pub ascent_iterations: u32,
    pub preprocessing: Duration,
    pub k_opt_improvements: u64,
    pub patching_attempts: u64,
    pub patched_improvements: u64,
}

#[derive(Debug)]
pub struct SearchError(pub String);
impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for SearchError {}

struct Best {
    solution: Solution,
    score: Score,
    found: Duration,
}

struct Shared<'a> {
    started: Instant,
    options: &'a SearchOptions,
    cancelled: &'a AtomicBool,
    best: Mutex<Best>,
    last_improved_ns: AtomicU64,
    reason: AtomicU8,
    k_opt_improvements: AtomicU64,
    patching_attempts: AtomicU64,
    patched_improvements: AtomicU64,
    monitor: Option<&'a SearchMonitor>,
}

fn nanos(d: Duration) -> u64 {
    d.as_nanos().min(u64::MAX as u128) as u64
}

impl Shared<'_> {
    fn stop(&self, reason: u8) {
        let _ = self
            .reason
            .compare_exchange(0, reason, Ordering::Relaxed, Ordering::Relaxed);
    }

    fn should_stop(&self) -> bool {
        if self.reason.load(Ordering::Relaxed) != 0 {
            return true;
        }
        if self.cancelled.load(Ordering::Relaxed) {
            self.stop(StopReason::Cancelled as u8);
            return true;
        }
        let elapsed = self.started.elapsed();
        if self
            .options
            .time_limit
            .is_some_and(|limit| elapsed >= limit)
        {
            self.stop(StopReason::TimeLimit as u8);
            return true;
        }
        if let Some(limit) = self.options.unimproved_limit {
            let last = Duration::from_nanos(self.last_improved_ns.load(Ordering::Acquire));
            if elapsed.saturating_sub(last) >= limit {
                // 在最佳方案锁内复核，避免另一个线程刚改善却被旧时间戳终止。
                let best = self.best.lock().expect("best solution mutex poisoned");
                if self.started.elapsed().saturating_sub(best.found) >= limit {
                    self.stop(StopReason::Unimproved as u8);
                    return true;
                }
            }
        }
        false
    }

    fn offer(&self, state: &State<'_>) {
        let mut best = self.best.lock().expect("best solution mutex poisoned");
        if state.score() > best.score {
            best.solution = state.solution();
            best.score = state.score();
            best.found = self.started.elapsed();
            self.last_improved_ns
                .store(nanos(best.found), Ordering::Release);
            if let Some(monitor) = self.monitor {
                monitor.improve(best.score, best.found, best.solution.assigned_count());
            }
        }
    }

    fn snapshot(&self) -> Solution {
        self.best
            .lock()
            .expect("best solution mutex poisoned")
            .solution
            .clone()
    }
}

/// `cancelled` 可由宿主随时置为 true。返回全局最佳方案，计时不含输入编译及结果解释。
pub fn solve(
    problem: &Problem,
    initial: Option<&Solution>,
    options: &SearchOptions,
    cancelled: &AtomicBool,
) -> Result<SearchResult, SearchError> {
    solve_observed(problem, initial, options, cancelled, None)
}

pub fn solve_observed(
    problem: &Problem,
    initial: Option<&Solution>,
    options: &SearchOptions,
    cancelled: &AtomicBool,
    monitor: Option<&SearchMonitor>,
) -> Result<SearchResult, SearchError> {
    if options.threads == 0 || options.threads > u32::MAX as usize {
        return Err(SearchError("threads must be a positive u32".into()));
    }
    if !(2..=12).contains(&options.k_opt_depth) {
        return Err(SearchError("k_opt_depth must be between 2 and 12".into()));
    }
    if options.time_limit.is_none() && options.unimproved_limit.is_none() {
        return Err(SearchError(
            "at least one termination limit is required".into(),
        ));
    }
    let started = Instant::now();
    let initial = initial
        .cloned()
        .unwrap_or_else(|| Solution::with_pinned(problem));
    let score = State::new(problem, &initial)
        .map_err(|e| SearchError(e.0))?
        .score();
    if let Some(monitor) = monitor {
        monitor.start(started, score, initial.assigned_count());
    }
    let shared = Shared {
        started,
        options,
        cancelled,
        best: Mutex::new(Best {
            solution: initial.clone(),
            score,
            found: Duration::ZERO,
        }),
        last_improved_ns: AtomicU64::new(0),
        reason: AtomicU8::new(0),
        k_opt_improvements: AtomicU64::new(0),
        patching_attempts: AtomicU64::new(0),
        patched_improvements: AtomicU64::new(0),
        monitor,
    };
    let mut evaluated_moves = 0;
    let mut actual_threads = 0;
    let mut alpha_profiles = 0;
    let mut ascent_iterations = 0;
    let mut preprocessing = Duration::ZERO;
    if !shared.should_stop() {
        if problem.tickets.is_empty() || problem.agents.is_empty() {
            shared.stop(StopReason::Completed as u8);
        } else {
            // 按矩阵构造一次，所有搜索线程共享；构造时间也在求解预算内。
            let pre_started = Instant::now();
            let candidates = match options.strategy {
                SearchStrategy::Baseline => None,
                SearchStrategy::Candidate => {
                    Some(Candidates::build(problem, || shared.should_stop()))
                }
                SearchStrategy::Alpha | SearchStrategy::AlphaKOpt => {
                    let budget = options.time_limit.map_or(Duration::from_millis(500), |t| {
                        (t / 10).min(Duration::from_millis(500))
                    });
                    Some(Candidates::build_alpha(problem, budget, || {
                        shared.should_stop()
                    }))
                }
            };
            preprocessing = pre_started.elapsed();
            if let Some(candidates) = &candidates {
                alpha_profiles = candidates.alpha_profiles;
                ascent_iterations = candidates.ascent_iterations;
            }
            let counts = std::thread::scope(|scope| -> Result<Vec<u64>, SearchError> {
                let mut handles = Vec::new();
                let mut spawn_error = None;
                for i in 0..options.threads {
                    if shared.should_stop() {
                        break;
                    }
                    let shared = &shared;
                    let initial = &initial;
                    let candidates = candidates.as_ref();
                    let seed = options
                        .seed
                        .wrapping_add((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
                    match std::thread::Builder::new()
                        .name(format!("vrp-search-{i}"))
                        .spawn_scoped(scope, move || {
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    run(problem, initial, shared, candidates, seed)
                                }));
                            if result.is_err() {
                                shared.stop(255);
                            }
                            result
                        }) {
                        Ok(handle) => handles.push(handle),
                        Err(e) => {
                            shared.stop(255);
                            spawn_error =
                                Some(SearchError(format!("could not start search thread: {e}")));
                            break;
                        }
                    }
                }
                let mut counts = Vec::new();
                for handle in handles {
                    match handle.join() {
                        Ok(Ok(count)) => counts.push(count),
                        _ => {
                            spawn_error = Some(SearchError("search thread panicked".into()));
                        }
                    }
                }
                if let Some(e) = spawn_error {
                    return Err(e);
                }
                Ok(counts)
            })?;
            actual_threads = counts.len();
            evaluated_moves = counts.into_iter().sum();
        }
    }
    let elapsed = started.elapsed();
    if let Some(monitor) = monitor {
        monitor.finish(elapsed);
    }
    let reason = match shared.reason.load(Ordering::Relaxed) {
        1 => StopReason::TimeLimit,
        2 => StopReason::Unimproved,
        3 => StopReason::Cancelled,
        _ => StopReason::Completed,
    };
    let k_opt_improvements = shared.k_opt_improvements.load(Ordering::Relaxed);
    let best = shared
        .best
        .into_inner()
        .map_err(|_| SearchError("best solution mutex poisoned".into()))?;
    Ok(SearchResult {
        solution: best.solution,
        score: best.score,
        elapsed,
        best_found: best.found,
        evaluated_moves,
        threads: actual_threads,
        seed: options.seed,
        stop_reason: reason,
        alpha_profiles,
        ascent_iterations,
        preprocessing,
        k_opt_improvements,
        patching_attempts: shared.patching_attempts.load(Ordering::Relaxed),
        patched_improvements: shared.patched_improvements.load(Ordering::Relaxed),
    })
}

fn insert_unassigned(
    state: &mut State<'_>,
    shared: &Shared<'_>,
    rng: &mut Rng,
    evaluated: &mut u64,
    report: &mut monitor::MoveReport<'_>,
) {
    let p = state.problem();
    let mut order: Vec<u32> = (0..p.tickets.len() as u32).collect();
    for i in (1..order.len()).rev() {
        order.swap(i, rng.below(i + 1));
    }
    let mut route = Vec::with_capacity(p.tickets.len());
    for t in order {
        if state.agent_of(t).is_some() {
            continue;
        }
        let mut best_score = state.score();
        let mut best = None;
        for &a in &p.candidates[t as usize] {
            route.clear();
            route.extend_from_slice(state.route(a));
            for at in 0..=route.len() {
                if shared.should_stop() {
                    return;
                }
                route.insert(at, t);
                let score = state.evaluate(Change {
                    routes: &[(a, &route)],
                });
                *evaluated += 1;
                route.remove(at);
                if score > best_score {
                    best_score = score;
                    best = Some((a, at));
                }
            }
        }
        if let Some((a, at)) = best {
            route.clear();
            route.extend_from_slice(state.route(a));
            route.insert(at, t);
            state.apply(Change {
                routes: &[(a, &route)],
            });
            shared.offer(state);
        }
        report.publish(*evaluated);
    }
}

fn run(
    p: &Problem,
    initial: &Solution,
    shared: &Shared<'_>,
    candidates: Option<&Candidates>,
    seed: u64,
) -> u64 {
    let mut state = State::new(p, initial).expect("validated initial solution");
    let mut rng = Rng::new(seed);
    let mut evaluated = 0;
    let mut report = monitor::MoveReport::new(shared.monitor);
    insert_unassigned(&mut state, shared, &mut rng, &mut evaluated, &mut report);
    let mut buffers = MoveBuffers::new(p.tickets.len());
    let mut kopt = (shared.options.strategy == SearchStrategy::AlphaKOpt)
        .then(|| kopt::KOpt::new(p.tickets.len()));
    let mut history = [state.score(); 128];
    let mut local_best = state.score();
    let mut iteration = 0usize;
    while !shared.should_stop() {
        iteration = iteration.wrapping_add(1);
        if iteration.is_multiple_of(256) {
            report.publish(evaluated);
        }
        if iteration.is_multiple_of(128)
            && let (Some(kopt), Some(graph)) = (&mut kopt, candidates)
            && kopt.improve(
                &mut state,
                graph,
                &mut rng,
                shared.options.k_opt_depth,
                || shared.should_stop(),
                &mut evaluated,
            )
        {
            shared.k_opt_improvements.fetch_add(1, Ordering::Relaxed);
            if state.score() > local_best {
                local_best = state.score();
                shared.offer(&state);
            }
        }
        if iteration.is_multiple_of(2048) {
            state = State::new(p, &shared.snapshot()).expect("validated best solution");
            local_best = state.score();
            // 交替使用双桥扰动和移除/修复；双桥不改变工单归属，固定工单也可参与。
            if candidates.is_some()
                && iteration.is_multiple_of(4096)
                && buffers.kick(&state, &mut rng)
            {
                state.apply(Change {
                    routes: &[(buffers.a, &buffers.first)],
                });
                evaluated += 1;
                shared.offer(&state);
                history.fill(state.score());
                continue;
            }
            // 简单移除/修复；工作方案可暂时退步，返回值始终取共享的最佳方案。
            for _ in 0..(p.tickets.len() / 10).clamp(1, 16) {
                if shared.should_stop() {
                    break;
                }
                let t = rng.below(p.tickets.len()) as u32;
                if p.tickets[t as usize].pinned {
                    continue;
                }
                if let Some(a) = state.agent_of(t) {
                    buffers.first.clear();
                    buffers
                        .first
                        .extend(state.route(a).iter().copied().filter(|&x| x != t));
                    state.apply(Change {
                        routes: &[(a, &buffers.first)],
                    });
                    evaluated += 1;
                    shared.offer(&state);
                }
            }
            insert_unassigned(&mut state, shared, &mut rng, &mut evaluated, &mut report);
            history.fill(state.score());
            continue;
        }
        if !buffers.generate(&state, candidates, &mut rng) {
            continue;
        }
        let routes = [
            (buffers.a, buffers.first.as_slice()),
            (buffers.b.unwrap_or(buffers.a), buffers.second.as_slice()),
        ];
        let change = Change {
            routes: &routes[..if buffers.b.is_some() { 2 } else { 1 }],
        };
        let score = state.evaluate(change);
        evaluated += 1;
        let slot = iteration % history.len();
        if score >= state.score() || score >= history[slot] {
            state.apply(change);
            if state.score() > local_best {
                local_best = state.score();
                shared.offer(&state);
            }
        }
        history[slot] = state.score();
    }
    report.publish(evaluated);
    if let Some(kopt) = kopt {
        shared
            .patching_attempts
            .fetch_add(kopt.patching_attempts, Ordering::Relaxed);
        shared
            .patched_improvements
            .fetch_add(kopt.patched_improvements, Ordering::Relaxed);
    }
    evaluated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentSpec, ConstraintId as C, Matrix, ProblemSpec, TicketSpec, Weight, Weights};
    #[test]
    fn only_strict_improvement_resets_the_global_timer() {
        let p = Problem::new(ProblemSpec {
            matrices: vec![Matrix {
                n: 1,
                duration: vec![0],
                distance: vec![0],
                buffer_permille: vec![],
            }],
            agents: vec![AgentSpec::default()],
            tickets: vec![TicketSpec::default()],
            weights: Weights::none().with(C::TicketUnassigned, Weight::hard(1)),
            ..ProblemSpec::default()
        })
        .unwrap();
        let initial = Solution::empty(1);
        let worse = State::new(&p, &initial).unwrap();
        let better = State::new(
            &p,
            &Solution {
                routes: vec![vec![0]],
            },
        )
        .unwrap();
        let options = SearchOptions {
            time_limit: None,
            unimproved_limit: Some(Duration::from_secs(10)),
            ..Default::default()
        };
        let cancelled = AtomicBool::new(false);
        let shared = Shared {
            started: Instant::now() - Duration::from_secs(20),
            options: &options,
            cancelled: &cancelled,
            best: Mutex::new(Best {
                solution: initial,
                score: worse.score(),
                found: Duration::ZERO,
            }),
            last_improved_ns: AtomicU64::new(0),
            reason: AtomicU8::new(0),
            k_opt_improvements: AtomicU64::new(0),
            patching_attempts: AtomicU64::new(0),
            patched_improvements: AtomicU64::new(0),
            monitor: None,
        };
        shared.offer(&better);
        let improved_at = shared.best.lock().unwrap().found;
        assert!(improved_at >= Duration::from_secs(20));
        shared.offer(&better);
        shared.offer(&worse);
        assert_eq!(shared.best.lock().unwrap().found, improved_at);
        assert!(!shared.should_stop());
        // 即使无锁快路径读到旧时间戳，锁内复核也不能误停。
        shared.last_improved_ns.store(0, Ordering::Release);
        assert!(!shared.should_stop());
        shared.best.lock().unwrap().found = Duration::ZERO;
        assert!(shared.should_stop());
        assert_eq!(
            shared.reason.load(Ordering::Relaxed),
            StopReason::Unimproved as u8
        );
    }
}
