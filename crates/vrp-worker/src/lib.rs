//! Local process isolation.
mod child;
mod framing;
mod platform;
mod process;
pub use child::serve_child;
pub use process::{ProcessRunner, serve_process_child};

use framing::{read_frame, write_frame};
use prost::Message;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use vrp_model::{ExecutionError, ExecutionPermit, Executor, ResolvedSolveOptions};
use vrp_proto::{engine::v1 as engine, worker::v1 as pb};

#[derive(Clone, Debug)]
pub struct IsolationOptions {
    pub max_frame_bytes: u32,
    /// Windows: committed memory of the job; Linux: process address space.
    pub memory_limit_bytes: Option<u64>,
    pub setup_timeout: Duration,
    pub heartbeat_timeout: Duration,
    pub progress_interval: Duration,
    pub execution_grace: Duration,
    pub cancel_grace: Duration,
}
impl Default for IsolationOptions {
    fn default() -> Self {
        Self {
            max_frame_bytes: 256 * 1024 * 1024,
            memory_limit_bytes: Some(1024 * 1024 * 1024),
            setup_timeout: Duration::from_secs(30),
            heartbeat_timeout: Duration::from_secs(5),
            progress_interval: Duration::from_millis(250),
            execution_grace: Duration::from_secs(5),
            cancel_grace: Duration::from_secs(2),
        }
    }
}

#[derive(Debug)]
pub enum WorkerError {
    Admission(ExecutionError),
    Io(io::Error),
    Protocol(String),
    Child { code: String, message: String },
    Killed { process_id: u32, reason: String },
}
impl fmt::Display for WorkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Admission(e) => e.fmt(f),
            Self::Io(e) => e.fmt(f),
            Self::Protocol(s) => write!(f, "child protocol: {s}"),
            Self::Child { code, message } => write!(f, "child {code}: {message}"),
            Self::Killed { process_id, reason } => {
                write!(f, "child {process_id} was terminated: {reason}")
            }
        }
    }
}
impl std::error::Error for WorkerError {}
impl From<io::Error> for WorkerError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<ExecutionError> for WorkerError {
    fn from(e: ExecutionError) -> Self {
        Self::Admission(e)
    }
}

pub struct Supervisor {
    executor: Arc<Executor>,
    program: PathBuf,
    args: Vec<OsString>,
    options: IsolationOptions,
}

pub(crate) fn validate_isolation_options(options: &IsolationOptions) -> Result<(), WorkerError> {
    if options.max_frame_bytes == 0
        || options.memory_limit_bytes == Some(0)
        || options.setup_timeout.is_zero()
        || options.heartbeat_timeout.is_zero()
        || options.cancel_grace.is_zero()
        || options.progress_interval < Duration::from_millis(10)
        || options.progress_interval > Duration::from_secs(1)
        || options.heartbeat_timeout <= options.progress_interval
    {
        return Err(WorkerError::Protocol("invalid isolation limits".into()));
    }
    Ok(())
}

impl Supervisor {
    /// The executable is a trusted solver child, not an untrusted-code sandbox.
    pub fn new(
        executor: Arc<Executor>,
        program: PathBuf,
        args: Vec<OsString>,
        options: IsolationOptions,
    ) -> Result<Self, WorkerError> {
        validate_isolation_options(&options)?;
        Ok(Self {
            executor,
            program,
            args,
            options,
        })
    }

    pub fn solve(
        &self,
        request: &engine::SolveRequest,
        cancelled: &AtomicBool,
    ) -> Result<engine::SolveResult, WorkerError> {
        self.solve_observed(request, cancelled, |_| {})
    }

    /// The progress callback must return promptly. It is invoked only in the caller.
    pub fn solve_observed(
        &self,
        request: &engine::SolveRequest,
        cancelled: &AtomicBool,
        progress: impl FnMut(&pb::ChildProgress),
    ) -> Result<engine::SolveResult, WorkerError> {
        let options =
            vrp_model::resolve_options(request.options.as_ref()).map_err(ExecutionError::from)?;
        if request.encoded_len().saturating_add(32) > self.options.max_frame_bytes as usize {
            return Err(WorkerError::Protocol(
                "request exceeds IPC frame limit".into(),
            ));
        }
        let permit = self.executor.reserve(&options, cancelled)?;
        self.solve_reserved_observed(request, &permit, cancelled, progress)
    }

    /// Run under a permit already reserved by this same executor. This permits
    /// the host to prepare geographic inputs and solve under one admission.
    pub fn solve_reserved_observed(
        &self,
        request: &engine::SolveRequest,
        permit: &ExecutionPermit<'_>,
        cancelled: &AtomicBool,
        mut progress: impl FnMut(&pb::ChildProgress),
    ) -> Result<engine::SolveResult, WorkerError> {
        let options =
            vrp_model::resolve_options(request.options.as_ref()).map_err(ExecutionError::from)?;
        if !permit.covers(&self.executor, options.threads) {
            return Err(WorkerError::Protocol(
                "execution permit does not cover solver".into(),
            ));
        }
        // An allowance for the envelope avoids cloning an already oversized request.
        if request.encoded_len().saturating_add(32) > self.options.max_frame_bytes as usize {
            return Err(WorkerError::Protocol(
                "request exceeds IPC frame limit".into(),
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
                Ok(c) => c,
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e.into());
                }
            };
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        // Declared before receiver: on unwind receiver drops before RunningChild
        // joins its reader, so a blocked bounded-channel send cannot deadlock.
        let mut running = RunningChild {
            child,
            containment,
            commands: None,
            reader: None,
            writer: None,
            tree_terminated: false,
        };
        let (events, incoming) = mpsc::sync_channel(4);
        let (commands, outgoing) = mpsc::channel();
        running.commands = Some(commands);
        let limit = self.options.max_frame_bytes;
        running.reader = Some(
            std::thread::Builder::new()
                .name("vrp-child-output".into())
                .spawn(move || {
                    let mut stdout = stdout;
                    loop {
                        let event = read_frame::<pb::ChildEvent>(&mut stdout, limit);
                        let terminal = !matches!(
                            &event,
                            Ok(Some(pb::ChildEvent {
                                event: Some(pb::child_event::Event::Progress(_))
                            }))
                        );
                        if events.send(event).is_err() || terminal {
                            break;
                        }
                    }
                })?,
        );
        let start = pb::ChildCommand {
            command: Some(pb::child_command::Command::Start(Box::new(
                pb::ChildStart {
                    request: Some(request.clone()),
                    progress_interval_ms: self.options.progress_interval.as_millis() as u32,
                },
            ))),
        };
        running.writer = Some(
            std::thread::Builder::new()
                .name("vrp-child-input".into())
                .spawn(move || {
                    let mut stdin = stdin;
                    write_frame(&mut stdin, &start, limit)?;
                    for command in outgoing {
                        write_frame(&mut stdin, &command, limit)?;
                    }
                    Ok(())
                })?,
        );
        let result = self.watch(&mut running, &incoming, &options, cancelled, &mut progress);
        drop(incoming);
        // Reap child and I/O threads before returning the permit, also on failures.
        drop(running);
        result
    }

    fn watch(
        &self,
        running: &mut RunningChild,
        incoming: &mpsc::Receiver<io::Result<Option<pb::ChildEvent>>>,
        options: &ResolvedSolveOptions,
        cancelled: &AtomicBool,
        progress: &mut impl FnMut(&pb::ChildProgress),
    ) -> Result<engine::SolveResult, WorkerError> {
        let started = Instant::now();
        let mut last_contact = started;
        let mut phase_started = started;
        let mut phase = pb::ChildPhase::Compiling;
        let mut search: Option<engine::Progress> = None;
        let mut stop: Option<(Instant, String)> = None;
        loop {
            match incoming.recv_timeout(Duration::from_millis(20)) {
                Ok(Ok(Some(event))) => {
                    last_contact = Instant::now();
                    match event.event {
                        Some(pb::child_event::Event::Progress(p)) => {
                            let next = pb::ChildPhase::try_from(p.phase)
                                .map_err(|_| WorkerError::Protocol("unknown child phase".into()))?;
                            if p.process_id != running.child.id() {
                                return Err(WorkerError::Protocol("child PID mismatch".into()));
                            }
                            if next != phase {
                                phase = next;
                                phase_started = last_contact;
                            }
                            if let Some(value) = &p.search {
                                if value.best_found_ms > value.elapsed_ms {
                                    return Err(WorkerError::Protocol(
                                        "invalid progress times".into(),
                                    ));
                                }
                                search = Some(*value);
                            }
                            progress(&p);
                        }
                        Some(pb::child_event::Event::Result(result)) => {
                            if result.solution.is_none()
                                || result.score.is_none()
                                || result.stats.is_none()
                            {
                                return Err(WorkerError::Protocol("incomplete result".into()));
                            }
                            // Close stdin, then allow a bounded interval for clean exit.
                            running.commands.take();
                            let exit_started = Instant::now();
                            loop {
                                if platform::exited(&mut running.child)? {
                                    running.terminate_tree();
                                    let status = running.child.wait()?;
                                    if status.success() {
                                        return Ok(*result);
                                    }
                                    return Err(WorkerError::Child {
                                        code: "exit".into(),
                                        message: status.to_string(),
                                    });
                                }
                                if exit_started.elapsed() >= self.options.cancel_grace {
                                    return Err(WorkerError::Killed {
                                        process_id: running.child.id(),
                                        reason: "did not exit after result".into(),
                                    });
                                }
                                std::thread::sleep(Duration::from_millis(5));
                            }
                        }
                        Some(pb::child_event::Event::Failure(f)) => {
                            return Err(WorkerError::Child {
                                code: f.code,
                                message: f.message,
                            });
                        }
                        None => return Err(WorkerError::Protocol("empty event".into())),
                    }
                }
                Ok(Ok(None)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(WorkerError::Child {
                        code: "exit".into(),
                        message: "child exited without a result (crash or resource limit)".into(),
                    });
                }
                Ok(Err(e)) => return Err(e.into()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let reason = if cancelled.load(Ordering::Acquire) {
                Some("cancel grace expired")
            } else if phase != pb::ChildPhase::Searching
                && phase_started.elapsed() > self.options.setup_timeout
            {
                Some("compile/explain deadline exceeded")
            } else if last_contact.elapsed() > self.options.heartbeat_timeout {
                Some("child heartbeat lost")
            } else if phase == pb::ChildPhase::Searching
                && search.is_some_and(|p| {
                    let grace = self.options.execution_grace.as_millis();
                    options
                        .time_limit_ms
                        .is_some_and(|limit| u128::from(p.elapsed_ms) > u128::from(limit) + grace)
                        || options.unimproved_stop_ms.is_some_and(|limit| {
                            u128::from(p.elapsed_ms - p.best_found_ms) > u128::from(limit) + grace
                        })
                })
            {
                Some("search termination deadline exceeded")
            } else {
                None
            };
            if let Some(reason) = reason
                && stop.is_none()
            {
                let command = pb::ChildCommand {
                    command: Some(pb::child_command::Command::Cancel(true)),
                };
                if let Some(sender) = &running.commands {
                    let _ = sender.send(command);
                }
                stop = Some((Instant::now(), reason.into()));
            }
            if let Some((at, reason)) = &stop
                && at.elapsed() >= self.options.cancel_grace
            {
                return Err(WorkerError::Killed {
                    process_id: running.child.id(),
                    reason: reason.clone(),
                });
            }
        }
    }
}

struct RunningChild {
    child: Child,
    containment: platform::Containment,
    commands: Option<mpsc::Sender<pb::ChildCommand>>,
    reader: Option<std::thread::JoinHandle<()>>,
    writer: Option<std::thread::JoinHandle<io::Result<()>>>,
    tree_terminated: bool,
}
impl RunningChild {
    fn terminate_tree(&mut self) {
        if !self.tree_terminated {
            self.containment.terminate();
            self.tree_terminated = true;
        }
    }
}
impl Drop for RunningChild {
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
