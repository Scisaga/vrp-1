use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use vrp_model::{Executor, ThreadBudget};
use vrp_proto::worker::v1::{GeoRunRequest, GeoRunResponse};
use vrp_worker::{IsolationOptions, ProcessRunner, WorkerError};

fn runner(mut options: IsolationOptions) -> ProcessRunner {
    let target = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let fixture: PathBuf = target.join("examples").join(if cfg!(windows) {
        "process_fixture.exe"
    } else {
        "process_fixture"
    });
    assert!(
        fixture.exists(),
        "Cargo must build the process_fixture example: {}",
        fixture.display()
    );
    options.progress_interval = Duration::from_millis(20);
    options.heartbeat_timeout = Duration::from_millis(300);
    options.cancel_grace = Duration::from_millis(100);
    ProcessRunner::new(fixture, vec![], options).unwrap()
}

#[test]
fn typed_task_has_progress_and_uses_the_callers_existing_reservation() {
    let executor = Executor::new(ThreadBudget { total_threads: 1 }, 0).unwrap();
    let cancel = AtomicBool::new(false);
    let permit = executor.reserve_threads(1, &cancel).unwrap();
    let runner = runner(IsolationOptions::default());
    let request = GeoRunRequest {
        output_path: "typed-response".into(),
        threads: 100,
        ..Default::default()
    };
    let mut events = 0;
    let result: GeoRunResponse = runner
        .run_reserved(&request, &cancel, |progress| {
            events += 1;
            assert_ne!(progress.process_id, std::process::id());
            assert_eq!(executor.status().reserved_threads, 1);
        })
        .unwrap();
    assert_eq!(result.output_path, "typed-response");
    assert_eq!(result.model_edges, 7);
    assert!(events > 0);
    assert_eq!(executor.status().reserved_threads, 1);
    drop(permit);
    assert_eq!(executor.status().reserved_threads, 0);
}

#[test]
fn cancellation_timeout_and_panic_do_not_affect_the_next_task() {
    let cancel = AtomicBool::new(false);
    let task = runner(IsolationOptions::default());
    let error = task
        .run_reserved::<_, GeoRunResponse>(
            &GeoRunRequest {
                threads: 2000,
                ..Default::default()
            },
            &cancel,
            |p| {
                if p.phase == "working" {
                    cancel.store(true, Ordering::Release);
                }
            },
        )
        .unwrap_err();
    assert!(
        matches!(error, WorkerError::Child { .. } | WorkerError::Admission(_)),
        "{error}"
    );
    cancel.store(false, Ordering::Release);
    let error = task
        .run_reserved::<_, GeoRunResponse>(
            &GeoRunRequest {
                output_path: "panic".into(),
                ..Default::default()
            },
            &cancel,
            |_| {},
        )
        .unwrap_err();
    assert!(matches!(error, WorkerError::Child { .. }), "{error}");
    let short = runner(IsolationOptions {
        setup_timeout: Duration::from_millis(80),
        ..Default::default()
    });
    let started = Instant::now();
    let error = short
        .run_reserved::<_, GeoRunResponse>(
            &GeoRunRequest {
                output_path: "ignore-cancel".into(),
                threads: 3000,
                ..Default::default()
            },
            &cancel,
            |_| {},
        )
        .unwrap_err();
    assert!(matches!(error, WorkerError::Killed { .. }), "{error}");
    assert!(started.elapsed() < Duration::from_secs(2));
    let result: GeoRunResponse = task
        .run_reserved(&GeoRunRequest::default(), &cancel, |_| {})
        .unwrap();
    assert_eq!(result.model_edges, 7);
}

#[test]
fn callback_unwind_reaps_a_busy_child_and_frame_limits_are_enforced() {
    let task = runner(IsolationOptions::default());
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = task.run_reserved::<_, GeoRunResponse>(
            &GeoRunRequest {
                threads: 3000,
                ..Default::default()
            },
            &AtomicBool::new(false),
            |_| panic!("host progress callback failed"),
        );
    }));
    assert!(failure.is_err());
    let tiny = runner(IsolationOptions {
        max_frame_bytes: 32,
        ..Default::default()
    });
    let result = tiny.run_reserved::<_, GeoRunResponse>(
        &GeoRunRequest {
            output_path: "x".repeat(1024),
            ..Default::default()
        },
        &AtomicBool::new(false),
        |_| {},
    );
    assert!(matches!(result, Err(WorkerError::Protocol(_))));
    let result: GeoRunResponse = task
        .run_reserved(&GeoRunRequest::default(), &AtomicBool::new(false), |_| {})
        .unwrap();
    assert_eq!(result.model_edges, 7);
}

#[test]
fn generic_tasks_honor_memory_limits_and_pre_start_cancellation() {
    let limited = runner(IsolationOptions {
        memory_limit_bytes: Some(1024 * 1024),
        ..Default::default()
    });
    assert!(
        limited
            .run_reserved::<_, GeoRunResponse>(
                &GeoRunRequest::default(),
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
    );
    let unavailable = ProcessRunner::new(
        PathBuf::from("not-an-existing-vrp-task-executable"),
        vec![],
        IsolationOptions::default(),
    )
    .unwrap();
    let result = unavailable.run_reserved::<_, GeoRunResponse>(
        &GeoRunRequest::default(),
        &AtomicBool::new(true),
        |_| {},
    );
    assert!(matches!(
        result,
        Err(WorkerError::Admission(vrp_model::ExecutionError::Cancelled))
    ));
}
