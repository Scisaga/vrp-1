use crate::framing::{read_frame, write_frame};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use vrp_core::SearchMonitor;
use vrp_proto::{engine::v1 as engine, worker::v1 as pb};

/// Run once in an isolated process. stdin accepts Start followed by Cancel;
/// stdout contains only framed protobuf events. EOF cancels an orphaned solve.
pub fn serve_child(max_frame_bytes: u32) -> io::Result<()> {
    let mut input = io::stdin();
    let start = match read_frame::<pb::ChildCommand>(&mut input, max_frame_bytes)? {
        Some(pb::ChildCommand {
            command: Some(pb::child_command::Command::Start(start)),
        }) => start,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected a Start frame",
            ));
        }
    };
    let request = start
        .request
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing request"))?;
    let interval = Duration::from_millis(u64::from(start.progress_interval_ms.clamp(10, 1000)));
    let cancelled = Arc::new(AtomicBool::new(false));
    let control = cancelled.clone();
    // This reader may block until stdin closes. Process exit ends the detached
    // control thread; it never retains a resource in the supervisor process.
    std::thread::Builder::new()
        .name("vrp-child-control".into())
        .spawn(move || {
            loop {
                match read_frame::<pb::ChildCommand>(&mut input, max_frame_bytes) {
                    Ok(Some(pb::ChildCommand {
                        command: Some(pb::child_command::Command::Cancel(true)),
                    })) => {
                        control.store(true, Ordering::Release);
                        return;
                    }
                    Ok(Some(pb::ChildCommand {
                        command: Some(pb::child_command::Command::Cancel(false)),
                    })) => continue,
                    _ => {
                        control.store(true, Ordering::Release);
                        return;
                    }
                }
            }
        })?;
    let monitor = Arc::new(SearchMonitor::default());
    let output = Arc::new(Mutex::new(io::stdout()));
    let done = Arc::new((Mutex::new(false), Condvar::new()));
    let progress_thread = {
        let (monitor, output, done, cancelled) = (
            monitor.clone(),
            output.clone(),
            done.clone(),
            cancelled.clone(),
        );
        std::thread::Builder::new()
            .name("vrp-child-progress".into())
            .spawn(move || {
                loop {
                    let snapshot = monitor.snapshot();
                    let phase = match snapshot {
                        None => pb::ChildPhase::Compiling,
                        Some(s) if s.finished => pb::ChildPhase::Explaining,
                        Some(_) => pb::ChildPhase::Searching,
                    };
                    let event = pb::ChildEvent {
                        event: Some(pb::child_event::Event::Progress(pb::ChildProgress {
                            phase: phase as i32,
                            process_id: std::process::id(),
                            search: snapshot.map(|s| engine::Progress {
                                elapsed_ms: millis(s.elapsed),
                                best_found_ms: millis(s.best_found),
                                evaluated_moves: s.evaluated_moves,
                                best_score: Some(engine::Score {
                                    hard: s.score.hard,
                                    medium: s.score.medium,
                                    soft: s.score.soft,
                                }),
                            }),
                        })),
                    };
                    if write_frame(
                        &mut *output.lock().expect("stdout poisoned"),
                        &event,
                        max_frame_bytes,
                    )
                    .is_err()
                    {
                        cancelled.store(true, Ordering::Release);
                        break;
                    }
                    let finished = done.0.lock().expect("completion poisoned");
                    if *finished {
                        break;
                    }
                    let (finished, _) = done
                        .1
                        .wait_timeout(finished, interval)
                        .expect("completion poisoned");
                    if *finished {
                        break;
                    }
                }
            })?
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        vrp_model::validate_ready_matrices(&request).map_err(|e| pb::ChildFailure {
            code: "invalid_request".into(),
            message: e.0,
        })?;
        let compiled = vrp_model::compile(&request).map_err(|e| pb::ChildFailure {
            code: "invalid_request".into(),
            message: e.0,
        })?;
        vrp_model::solve_observed(&compiled, &cancelled, Some(&monitor)).map_err(|e| {
            pb::ChildFailure {
                code: "internal".into(),
                message: e.0,
            }
        })
    }));
    *done.0.lock().expect("completion poisoned") = true;
    done.1.notify_all();
    progress_thread
        .join()
        .map_err(|_| io::Error::other("progress thread panicked"))?;
    let event = match result {
        Ok(Ok(result)) => pb::child_event::Event::Result(Box::new(result)),
        Ok(Err(e)) => pb::child_event::Event::Failure(e),
        Err(_) => pb::child_event::Event::Failure(pb::ChildFailure {
            code: "internal".into(),
            message: "solver process panicked".into(),
        }),
    };
    write_frame(
        &mut *output.lock().expect("stdout poisoned"),
        &pb::ChildEvent { event: Some(event) },
        max_frame_bytes,
    )
}

fn millis(d: Duration) -> u64 {
    d.as_millis().min(u64::MAX as u128) as u64
}
