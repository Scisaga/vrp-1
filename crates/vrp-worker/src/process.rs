//! A task runner sharing the solver's framing and operating-system containment.

use crate::framing::{read_frame, write_frame};
use crate::{IsolationOptions, WorkerError, platform, validate_isolation_options};
use prost::Message;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};
use vrp_model::ExecutionError;
use vrp_proto::worker::v1 as pb;

/// Runs a trusted native task executable with generic protobuf input/output.
/// Admission belongs to the caller, which must hold its shared execution permit
/// until `run_reserved` returns (including child and I/O-thread cleanup).
pub struct ProcessRunner {
    program: PathBuf,
    args: Vec<OsString>,
    options: IsolationOptions,
}

impl ProcessRunner {
    pub fn new(
        program: PathBuf,
        args: Vec<OsString>,
        options: IsolationOptions,
    ) -> Result<Self, WorkerError> {
        validate_isolation_options(&options)?;
        Ok(Self {
            program,
            args,
            options,
        })
    }

    /// `setup_timeout` is the total task deadline for a generic process. Progress
    /// callbacks run on the caller and must return promptly.
    pub fn run_reserved<Req: Message, Res: Message + Default>(
        &self,
        request: &Req,
        cancelled: &AtomicBool,
        mut progress: impl FnMut(&pb::ProcessProgress),
    ) -> Result<Res, WorkerError> {
        if cancelled.load(Ordering::Acquire) {
            return Err(ExecutionError::Cancelled.into());
        }
        if request.encoded_len().saturating_add(32) > self.options.max_frame_bytes as usize {
            return Err(WorkerError::Protocol(
                "task request exceeds IPC frame limit".into(),
            ));
        }
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        platform::configure(&mut command, self.options.memory_limit_bytes)?;
        let mut child = command.spawn()?;
        let containment =
            match platform::Containment::attach(&child, self.options.memory_limit_bytes) {
                Ok(value) => value,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error.into());
                }
            };
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut running = RunningProcess {
            child,
            containment,
            commands: None,
            reader: None,
            writer: None,
            tree_terminated: false,
        };
        let (incoming_tx, incoming) = mpsc::sync_channel(4);
        let limit = self.options.max_frame_bytes;
        running.reader = Some(
            std::thread::Builder::new()
                .name("vrp-task-output".into())
                .spawn(move || {
                    let mut stdout = stdout;
                    loop {
                        let result = read_frame::<pb::ProcessEvent>(&mut stdout, limit);
                        let ended = !matches!(result, Ok(Some(_)));
                        if incoming_tx.send(result).is_err() || ended {
                            break;
                        }
                    }
                })?,
        );
        let (outgoing_tx, outgoing) = mpsc::channel();
        running.commands = Some(outgoing_tx);
        let start = pb::ProcessCommand {
            command: Some(pb::process_command::Command::Start(pb::ProcessStart {
                payload: request.encode_to_vec(),
                progress_interval_ms: self.options.progress_interval.as_millis() as u32,
            })),
        };
        running.writer = Some(
            std::thread::Builder::new()
                .name("vrp-task-input".into())
                .spawn(move || {
                    let mut stdin = stdin;
                    write_frame(&mut stdin, &start, limit)?;
                    for command in outgoing {
                        write_frame(&mut stdin, &command, limit)?;
                    }
                    Ok(())
                })?,
        );
        let outcome = self.watch(&mut running, &incoming, cancelled, &mut progress);
        drop(incoming);
        drop(running);
        outcome
    }

    fn watch<Res: Message + Default>(
        &self,
        running: &mut RunningProcess,
        incoming: &mpsc::Receiver<io::Result<Option<pb::ProcessEvent>>>,
        cancelled: &AtomicBool,
        progress: &mut impl FnMut(&pb::ProcessProgress),
    ) -> Result<Res, WorkerError> {
        let started = Instant::now();
        let mut last_contact = started;
        let mut stop = None;
        loop {
            match incoming.recv_timeout(Duration::from_millis(20)) {
                Ok(Ok(Some(event))) => {
                    last_contact = Instant::now();
                    match event.event {
                        Some(pb::process_event::Event::Progress(value)) => {
                            if value.process_id != running.child.id()
                                || value.completed > value.total && value.total > 0
                            {
                                return Err(WorkerError::Protocol("invalid task progress".into()));
                            }
                            progress(&value);
                        }
                        Some(pb::process_event::Event::Result(bytes)) => {
                            if cancelled.load(Ordering::Acquire) {
                                return Err(ExecutionError::Cancelled.into());
                            }
                            if stop.is_some() {
                                return Err(WorkerError::Protocol(
                                    "task returned result after deadline".into(),
                                ));
                            }
                            let result = Res::decode(bytes.as_slice()).map_err(|e| {
                                WorkerError::Protocol(format!("invalid task result: {e}"))
                            })?;
                            running.commands.take();
                            let exit_started = Instant::now();
                            loop {
                                if platform::exited(&mut running.child)? {
                                    running.terminate_tree();
                                    let status = running.child.wait()?;
                                    if status.success() {
                                        return Ok(result);
                                    }
                                    return Err(WorkerError::Child {
                                        code: "exit".into(),
                                        message: status.to_string(),
                                    });
                                }
                                if exit_started.elapsed() >= self.options.cancel_grace {
                                    return Err(WorkerError::Killed {
                                        process_id: running.child.id(),
                                        reason: "task did not exit after result".into(),
                                    });
                                }
                                std::thread::sleep(Duration::from_millis(5));
                            }
                        }
                        Some(pb::process_event::Event::Failure(value)) => {
                            return Err(WorkerError::Child {
                                code: value.code,
                                message: value.message,
                            });
                        }
                        None => return Err(WorkerError::Protocol("empty task event".into())),
                    }
                }
                Ok(Ok(None)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(WorkerError::Child {
                        code: "exit".into(),
                        message: "task exited without a result (crash or resource limit)".into(),
                    });
                }
                Ok(Err(error)) => return Err(error.into()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let reason = if cancelled.load(Ordering::Acquire) {
                Some("task cancel grace expired")
            } else if started.elapsed() > self.options.setup_timeout {
                Some("task deadline exceeded")
            } else if last_contact.elapsed() > self.options.heartbeat_timeout {
                Some("task heartbeat lost")
            } else {
                None
            };
            if let Some(reason) = reason
                && stop.is_none()
            {
                if let Some(commands) = &running.commands {
                    let _ = commands.send(pb::ProcessCommand {
                        command: Some(pb::process_command::Command::Cancel(true)),
                    });
                }
                stop = Some((Instant::now(), reason));
            }
            if let Some((when, reason)) = stop
                && when.elapsed() >= self.options.cancel_grace
            {
                return Err(WorkerError::Killed {
                    process_id: running.child.id(),
                    reason: reason.into(),
                });
            }
        }
    }
}

struct RunningProcess {
    child: Child,
    containment: platform::Containment,
    commands: Option<mpsc::Sender<pb::ProcessCommand>>,
    reader: Option<std::thread::JoinHandle<()>>,
    writer: Option<std::thread::JoinHandle<io::Result<()>>>,
    tree_terminated: bool,
}
impl RunningProcess {
    fn terminate_tree(&mut self) {
        if !self.tree_terminated {
            self.containment.terminate();
            self.tree_terminated = true;
        }
    }
}
impl Drop for RunningProcess {
    fn drop(&mut self) {
        self.terminate_tree();
        let _ = self.child.kill();
        self.commands.take();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

/// Serve one trusted generic task using length-prefixed protobuf frames.
/// A background heartbeat continues during long native library calls; EOF or a
/// Cancel frame sets the shared flag. The callback must poll it cooperatively.
pub fn serve_process_child<Req, Res>(
    max_frame_bytes: u32,
    execute: impl FnOnce(
        Req,
        &AtomicBool,
        &mut dyn FnMut(&str, u64, u64),
    ) -> Result<Res, pb::ChildFailure>,
) -> io::Result<()>
where
    Req: Message + Default,
    Res: Message,
{
    let mut input = io::stdin();
    let start = match read_frame::<pb::ProcessCommand>(&mut input, max_frame_bytes)? {
        Some(pb::ProcessCommand {
            command: Some(pb::process_command::Command::Start(value)),
        }) => value,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected task Start frame",
            ));
        }
    };
    let request = Req::decode(start.payload.as_slice())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let interval = Duration::from_millis(u64::from(start.progress_interval_ms.clamp(10, 1000)));
    let cancelled = Arc::new(AtomicBool::new(false));
    let control = cancelled.clone();
    std::thread::Builder::new()
        .name("vrp-task-control".into())
        .spawn(move || {
            loop {
                match read_frame::<pb::ProcessCommand>(&mut input, max_frame_bytes) {
                    Ok(Some(pb::ProcessCommand {
                        command: Some(pb::process_command::Command::Cancel(false)),
                    })) => continue,
                    _ => {
                        control.store(true, Ordering::Release);
                        break;
                    }
                }
            }
        })?;
    let output = Arc::new(Mutex::new(io::stdout()));
    let status = Arc::new(Mutex::new(pb::ProcessProgress {
        phase: "starting".into(),
        process_id: std::process::id(),
        ..Default::default()
    }));
    let done = Arc::new((Mutex::new(false), Condvar::new()));
    let heartbeat = {
        let (output, status, done, cancelled) = (
            output.clone(),
            status.clone(),
            done.clone(),
            cancelled.clone(),
        );
        std::thread::Builder::new()
            .name("vrp-task-progress".into())
            .spawn(move || {
                loop {
                    let progress = status.lock().expect("task progress poisoned").clone();
                    let event = pb::ProcessEvent {
                        event: Some(pb::process_event::Event::Progress(progress)),
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
                    let finished = done.0.lock().expect("task completion poisoned");
                    if *finished {
                        break;
                    }
                    let (finished, _) = done
                        .1
                        .wait_timeout(finished, interval)
                        .expect("task completion poisoned");
                    if *finished {
                        break;
                    }
                }
            })?
    };
    let mut update = |phase: &str, completed: u64, total: u64| {
        let mut value = status.lock().expect("task progress poisoned");
        value.phase = phase.into();
        value.completed = completed;
        value.total = total;
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        execute(request, &cancelled, &mut update)
    }));
    *done.0.lock().expect("task completion poisoned") = true;
    done.1.notify_all();
    heartbeat
        .join()
        .map_err(|_| io::Error::other("task heartbeat panicked"))?;
    let event = if cancelled.load(Ordering::Acquire) {
        pb::process_event::Event::Failure(pb::ChildFailure {
            code: "cancelled".into(),
            message: "task was cancelled".into(),
        })
    } else {
        match outcome {
            Ok(Ok(value)) => pb::process_event::Event::Result(value.encode_to_vec()),
            Ok(Err(value)) => pb::process_event::Event::Failure(value),
            Err(_) => pb::process_event::Event::Failure(pb::ChildFailure {
                code: "internal".into(),
                message: "task process panicked".into(),
            }),
        }
    };
    write_frame(
        &mut *output.lock().expect("stdout poisoned"),
        &pb::ProcessEvent { event: Some(event) },
        max_frame_bytes,
    )
}
