use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use vrp_model::{Executor, ThreadBudget};
use vrp_proto::{engine::v1 as pb, worker::v1 as worker};
use vrp_worker::{IsolationOptions, Supervisor, WorkerError};

fn request() -> pb::SolveRequest {
    pb::SolveRequest {
        problem: Some(pb::Problem {
            locations: (0..6)
                .map(|i| pb::Location {
                    id: format!("l{i}"),
                    ..Default::default()
                })
                .collect(),
            matrices: vec![pb::TravelMatrix {
                profile: "car".into(),
                durations: vec![0; 6 * 6 * 4],
                distances: vec![0; 6 * 6 * 4],
                ..Default::default()
            }],
            agents: vec![pb::Agent {
                id: "a".into(),
                ..Default::default()
            }],
            tickets: (0..5)
                .map(|t| pb::Ticket {
                    id: format!("t{t}"),
                    location: t + 1,
                    original_agent: Some(0),
                    pinned: true,
                    latest_start: 86400,
                    latest_end: 86400,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }),
        options: Some(pb::SolveOptions {
            threads: 2,
            time_limit_ms: 150,
            search_strategy: pb::SearchStrategy::AlphaKOpt as i32,
            ..Default::default()
        }),
        initial_solution: None,
    }
}

fn executor() -> Arc<Executor> {
    Arc::new(Executor::new(ThreadBudget { total_threads: 4 }, 4).unwrap())
}
fn limits() -> IsolationOptions {
    IsolationOptions {
        progress_interval: Duration::from_millis(20),
        heartbeat_timeout: Duration::from_secs(2),
        setup_timeout: Duration::from_secs(5),
        cancel_grace: Duration::from_millis(500),
        ..Default::default()
    }
}
fn supervisor(executor: Arc<Executor>, limits: IsolationOptions) -> Supervisor {
    Supervisor::new(
        executor,
        PathBuf::from(env!("CARGO_BIN_EXE_vrp")),
        vec![
            "worker-child".into(),
            limits.max_frame_bytes.to_string().into(),
        ],
        limits,
    )
    .unwrap()
}

#[test]
fn isolated_child_returns_verified_results_progress_and_releases_slots() {
    let executor = executor();
    let supervisor = supervisor(executor.clone(), limits());
    let request = request();
    let mut seen_progress = false;
    let result = supervisor
        .solve_observed(&request, &AtomicBool::new(false), |p| {
            seen_progress = true;
            assert_ne!(p.process_id, std::process::id());
            assert_eq!(executor.status().reserved_threads, 2);
        })
        .unwrap();
    assert!(seen_progress);
    assert_eq!(
        result.stats.as_ref().unwrap().termination_reason,
        "time_limit"
    );
    assert_eq!(result.stats.as_ref().unwrap().threads, 2);
    assert!(result.unassigned.is_empty());
    let compiled = vrp_model::compile(&pb::SolveRequest {
        initial_solution: result.solution,
        ..request
    })
    .unwrap();
    let expected =
        vrp_core::oracle::evaluate(&compiled.problem, compiled.initial.as_ref().unwrap()).score;
    let actual = result.score.unwrap();
    assert_eq!(
        (actual.hard, actual.medium, actual.soft),
        (expected.hard, expected.medium, expected.soft)
    );
    assert_eq!(executor.status().reserved_threads, 0);
}

#[test]
fn isolated_leaf_rejects_unprepared_matrices_and_accepts_explicit_materialization() {
    let executor = executor();
    let supervisor = supervisor(executor.clone(), limits());
    let mut input = request();
    input.problem.as_mut().unwrap().matrices.clear();
    let error = supervisor
        .solve(&input, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(error, WorkerError::Child { ref code, .. } if code == "invalid_request"));
    assert_eq!(executor.status().reserved_threads, 0);
    vrp_model::materialize_matrices(&mut input).unwrap();
    vrp_model::validate_ready_matrices(&input).unwrap();
    assert!(supervisor.solve(&input, &AtomicBool::new(false)).is_ok());
    let matrix = &mut input.problem.as_mut().unwrap().matrices[0];
    matrix.durations[4..8].copy_from_slice(&vrp_core::problem::UNKNOWN.to_le_bytes());
    matrix.distances[4..8].copy_from_slice(&vrp_core::problem::UNKNOWN.to_le_bytes());
    let error = supervisor
        .solve(&input, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(error, WorkerError::Child { ref code, .. } if code == "invalid_request"));
}

#[test]
fn reserved_solver_does_not_reserve_twice_and_rejects_foreign_permits() {
    let executor = executor();
    let supervisor = supervisor(executor.clone(), limits());
    let cancel = AtomicBool::new(false);
    let permit = executor.reserve_threads(2, &cancel).unwrap();
    let result = supervisor
        .solve_reserved_observed(&request(), &permit, &cancel, |_| {})
        .unwrap();
    assert_eq!(result.stats.unwrap().threads, 2);
    assert_eq!(executor.status().reserved_threads, 2);
    let other = Executor::new(ThreadBudget { total_threads: 2 }, 0).unwrap();
    let foreign = other.reserve_threads(2, &cancel).unwrap();
    assert!(matches!(
        supervisor.solve_reserved_observed(&request(), &foreign, &cancel, |_| {}),
        Err(WorkerError::Protocol(_))
    ));
    drop(permit);
    assert_eq!(executor.status().reserved_threads, 0);
}

#[test]
fn cancellation_returns_best_solution_and_deep_uses_unimproved_time() {
    let executor = executor();
    let supervisor = supervisor(executor.clone(), limits());
    let cancelled = AtomicBool::new(false);
    let mut request = request();
    request.options.as_mut().unwrap().time_limit_ms = 5000;
    let result = supervisor
        .solve_observed(&request, &cancelled, |p| {
            if p.phase == worker::ChildPhase::Searching as i32 {
                cancelled.store(true, Ordering::Release);
            }
        })
        .unwrap();
    assert_eq!(result.stats.unwrap().termination_reason, "cancelled");
    assert_eq!(executor.status().reserved_threads, 0);
    cancelled.store(false, Ordering::Release);
    request.options.as_mut().unwrap().preset = pb::SolvePreset::Deep as i32;
    request.options.as_mut().unwrap().time_limit_ms = 0;
    request.options.as_mut().unwrap().unimproved_stop_ms = 120;
    let result = supervisor.solve(&request, &cancelled).unwrap();
    assert_eq!(
        result.stats.as_ref().unwrap().termination_reason,
        "unimproved"
    );
    assert!(result.stats.as_ref().unwrap().elapsed_ms >= 120);
    assert_eq!(executor.status().reserved_threads, 0);
}

fn faulty_command(stall: bool) -> (PathBuf, Vec<OsString>) {
    #[cfg(windows)]
    {
        (
            PathBuf::from("powershell.exe"),
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                if stall {
                    "Start-Sleep -Seconds 30".into()
                } else {
                    "exit 17".into()
                },
            ],
        )
    }
    #[cfg(not(windows))]
    {
        (
            PathBuf::from("sh"),
            vec![
                "-c".into(),
                if stall {
                    "sleep 30".into()
                } else {
                    "exit 17".into()
                },
            ],
        )
    }
}

#[test]
fn crashed_and_stuck_children_are_reaped_and_do_not_poison_future_jobs() {
    let executor = executor();
    for stall in [false, true] {
        let (program, args) = faulty_command(stall);
        let mut config = limits();
        config.setup_timeout = Duration::from_millis(150);
        config.cancel_grace = Duration::from_millis(100);
        let faulty = Supervisor::new(executor.clone(), program, args, config).unwrap();
        let started = Instant::now();
        let mut request = request();
        // Fill the input pipe while the child refuses to read: the supervisor
        // must still enforce its deadline and unblock/reap the writer thread.
        if stall {
            request.problem.as_mut().unwrap().id = "x".repeat(256 * 1024);
        }
        let error = faulty.solve(&request, &AtomicBool::new(false)).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5));
        if stall {
            assert!(matches!(error, WorkerError::Killed { .. }), "{error}");
        }
        assert_eq!(executor.status().reserved_threads, 0);
        assert_eq!(executor.status().running_jobs, 0);
    }
    assert!(
        supervisor(executor.clone(), limits())
            .solve(&request(), &AtomicBool::new(false))
            .is_ok()
    );
}

#[test]
fn a_panicking_host_progress_callback_still_reaps_the_child_and_returns_slots() {
    let executor = executor();
    let supervisor = supervisor(executor.clone(), limits());
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = supervisor.solve_observed(&request(), &AtomicBool::new(false), |_| {
            panic!("host callback failed")
        });
    }));
    assert!(failure.is_err());
    assert_eq!(executor.status().reserved_threads, 0);
    assert_eq!(executor.status().running_jobs, 0);
    assert!(
        supervisor
            .solve(&request(), &AtomicBool::new(false))
            .is_ok()
    );
}

#[test]
fn memory_limits_and_frame_limits_fail_only_the_current_job() {
    let executor = executor();
    let mut config = limits();
    config.memory_limit_bytes = Some(1024 * 1024); // below a Rust solver process's working requirement
    let limited = supervisor(executor.clone(), config);
    assert!(limited.solve(&request(), &AtomicBool::new(false)).is_err());
    assert_eq!(executor.status().reserved_threads, 0);
    let mut config = limits();
    config.max_frame_bytes = 32;
    assert!(matches!(
        supervisor(executor.clone(), config).solve(&request(), &AtomicBool::new(false)),
        Err(WorkerError::Protocol(_))
    ));
    assert_eq!(executor.status().reserved_threads, 0);
    assert!(
        supervisor(executor.clone(), limits())
            .solve(&request(), &AtomicBool::new(false))
            .is_ok()
    );
}

#[test]
fn several_isolated_jobs_share_the_same_machine_budget() {
    let executor = executor();
    let supervisor = supervisor(executor.clone(), limits());
    let request = request();
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..4 {
            handles.push(scope.spawn(|| {
                supervisor.solve_observed(&request, &AtomicBool::new(false), |_| {
                    assert!(executor.status().reserved_threads <= 4);
                    assert!(executor.status().running_jobs <= 2);
                })
            }));
        }
        for handle in handles {
            assert!(handle.join().unwrap().is_ok());
        }
    });
    assert_eq!(executor.status().reserved_threads, 0);
    assert_eq!(executor.status().queued_jobs, 0);
}

#[test]
fn invalid_inputs_return_a_structured_error_and_release_the_permit() {
    let executor = executor();
    let supervisor = supervisor(executor.clone(), limits());
    let error = supervisor
        .solve(&pb::SolveRequest::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(error, WorkerError::Child { ref code, .. } if code == "invalid_request"));
    assert_eq!(executor.status().reserved_threads, 0);
}
