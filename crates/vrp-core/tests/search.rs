mod common;
use common::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use vrp_core::{
    ConstraintId as C, Problem, SearchOptions, SearchStrategy, Solution, StopReason, Weight,
    Weights, oracle, solve,
};

fn problem() -> Problem {
    let mut s = spec(
        17,
        Weights::none()
            .with(C::TicketUnassigned, Weight::medium(1000))
            .with(C::MinimizeTravelTime, Weight::soft(1)),
    );
    s.agents.extend([agent("a", 0, 0), agent("b", 16, 0)]);
    for i in 0..16 {
        let mut t = ticket(&format!("t{i}"), i, 30);
        if i == 0 {
            t.pinned = true;
            t.original_agent = Some(0);
        }
        s.tickets.push(t);
    }
    Problem::new(s).unwrap()
}

#[test]
fn optional_history_records_actual_monotone_incumbents_and_final_score() {
    let p = problem();
    let monitor = vrp_core::SearchMonitor::with_history();
    let result = vrp_core::solve_observed(
        &p,
        None,
        &SearchOptions {
            time_limit: Some(Duration::from_millis(80)),
            threads: 2,
            ..SearchOptions::default()
        },
        &AtomicBool::new(false),
        Some(&monitor),
    )
    .unwrap();
    let events = monitor.take_history();
    assert!(!events.is_empty());
    for pair in events.windows(2) {
        assert!(pair[0].elapsed <= pair[1].elapsed);
        assert!(pair[0].score < pair[1].score);
    }
    assert_eq!(events.last().unwrap().score, result.score);
    assert_eq!(
        events.last().unwrap().assigned,
        result.solution.assigned_count()
    );
    assert!(events.last().unwrap().elapsed <= result.elapsed);
    assert!(monitor.take_history().is_empty());
    assert!(vrp_core::SearchMonitor::default().take_history().is_empty());
}

#[test]
fn both_strategies_support_parallel_search_and_keep_the_best_warm_start() {
    let p = problem();
    let initial = Solution {
        routes: vec![(0..8).collect(), (8..16).rev().collect()],
    };
    let initial_score = oracle::evaluate(&p, &initial).score;
    for strategy in [
        SearchStrategy::Baseline,
        SearchStrategy::Candidate,
        SearchStrategy::Alpha,
        SearchStrategy::AlphaKOpt,
    ] {
        let result = solve(
            &p,
            Some(&initial),
            &SearchOptions {
                threads: 2,
                time_limit: Some(Duration::from_millis(100)),
                strategy,
                seed: 19,
                ..SearchOptions::default()
            },
            &AtomicBool::new(false),
        )
        .unwrap();
        result.solution.validate(&p).unwrap();
        assert_eq!(result.score, oracle::evaluate(&p, &result.solution).score);
        assert!(result.score >= initial_score);
        assert_eq!(result.solution.assigned_count(), 16);
        assert_eq!(result.threads, 2);
        assert!(result.evaluated_moves > 0);
        assert!(result.best_found <= result.elapsed);
        assert_eq!(result.stop_reason, StopReason::TimeLimit);
        assert!(result.elapsed < Duration::from_secs(5));
    }
}

#[test]
fn cold_start_inserts_work_and_respects_pins() {
    let p = problem();
    let result = solve(
        &p,
        None,
        &SearchOptions {
            time_limit: Some(Duration::from_millis(150)),
            threads: 2,
            ..SearchOptions::default()
        },
        &AtomicBool::new(false),
    )
    .unwrap();
    result.solution.validate(&p).unwrap();
    assert_eq!(result.solution.assigned_count(), 16);
    assert_eq!(result.score, oracle::evaluate(&p, &result.solution).score);
}

#[test]
fn equal_scores_do_not_extend_the_unimproved_limit_across_threads() {
    let mut s = spec(1, Weights::none());
    s.agents.push(agent("a", 0, 0));
    for i in 0..8 {
        s.tickets.push(ticket(&format!("t{i}"), 0, 0));
    }
    let p = Problem::new(s).unwrap();
    let initial = Solution {
        routes: vec![(0..8).collect()],
    };
    let result = solve(
        &p,
        Some(&initial),
        &SearchOptions {
            time_limit: Some(Duration::from_secs(5)),
            unimproved_limit: Some(Duration::from_millis(50)),
            threads: 2,
            ..SearchOptions::default()
        },
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(result.stop_reason, StopReason::Unimproved);
    assert_eq!(result.best_found, Duration::ZERO);
    assert_eq!(result.solution, initial);
    assert!(result.evaluated_moves > 0);
    assert!(result.elapsed < Duration::from_secs(5));
}

#[test]
fn cancellation_before_start_and_during_search_returns_a_valid_best_solution() {
    let p = problem();
    let initial = Solution::with_pinned(&p);
    let cancelled = AtomicBool::new(true);
    let result = solve(&p, None, &SearchOptions::default(), &cancelled).unwrap();
    assert_eq!(result.stop_reason, StopReason::Cancelled);
    assert_eq!(result.threads, 0);
    assert_eq!(result.solution, initial);
    cancelled.store(false, Ordering::Release);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(50));
            cancelled.store(true, Ordering::Release);
        });
        let result = solve(
            &p,
            None,
            &SearchOptions {
                threads: 2,
                ..SearchOptions::default()
            },
            &cancelled,
        )
        .unwrap();
        assert_eq!(result.stop_reason, StopReason::Cancelled);
        assert!(result.elapsed < Duration::from_secs(5));
        result.solution.validate(&p).unwrap();
        assert_eq!(result.score, oracle::evaluate(&p, &result.solution).score);
        assert!(result.score >= oracle::evaluate(&p, &initial).score);
    });
}

#[test]
fn empty_or_unassignable_problems_complete_without_starting_threads() {
    for tickets in [0, 1] {
        let mut s = spec(1, Weights::default());
        for i in 0..tickets {
            s.tickets.push(ticket(&format!("t{i}"), 0, 0));
        }
        let p = Problem::new(s).unwrap();
        let result = solve(&p, None, &SearchOptions::default(), &AtomicBool::new(false)).unwrap();
        assert_eq!(result.stop_reason, StopReason::Completed);
        assert_eq!(result.threads, 0);
        assert_eq!(result.score, oracle::evaluate(&p, &result.solution).score);
    }
}

#[test]
fn invalid_search_options_and_warm_starts_are_rejected() {
    let p = problem();
    let cancelled = AtomicBool::new(false);
    assert!(
        solve(
            &p,
            Some(&Solution::empty(2)),
            &SearchOptions::default(),
            &cancelled
        )
        .is_err()
    );
    assert!(
        solve(
            &p,
            None,
            &SearchOptions {
                threads: 0,
                ..SearchOptions::default()
            },
            &cancelled
        )
        .is_err()
    );
    assert!(
        solve(
            &p,
            None,
            &SearchOptions {
                time_limit: None,
                ..SearchOptions::default()
            },
            &cancelled
        )
        .is_err()
    );
}

#[test]
fn progress_monitor_matches_final_global_best_and_total_move_count() {
    let p = problem();
    let monitor = vrp_core::SearchMonitor::default();
    assert!(monitor.snapshot().is_none());
    let result = vrp_core::solve_observed(
        &p,
        None,
        &SearchOptions {
            threads: 2,
            time_limit: Some(Duration::from_millis(60)),
            ..Default::default()
        },
        &AtomicBool::new(false),
        Some(&monitor),
    )
    .unwrap();
    let progress = monitor.snapshot().unwrap();
    assert_eq!(progress.score, result.score);
    assert_eq!(progress.evaluated_moves, result.evaluated_moves);
    assert_eq!(progress.best_found, result.best_found);
    assert_eq!(progress.elapsed, result.elapsed);
    assert!(progress.finished);
}
