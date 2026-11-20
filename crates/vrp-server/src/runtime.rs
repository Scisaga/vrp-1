use crate::{AppError, Result, job_id, now_ms, page_size, scenario_id, store::Store, terminal};
use prost::Message;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::sync::watch;
use vrp_model::{Executor, ThreadBudget};
use vrp_proto::{app::v1 as pb, engine::v1 as engine};
use vrp_worker::{IsolationOptions, Supervisor, WorkerError};
mod geometry;
mod operations;
pub(crate) mod tasks;
mod workbench;

#[derive(Clone, Debug)]
pub struct Config {
    pub data_dir: PathBuf,
    pub budget: ThreadBudget,
    pub max_running: u32,
    pub max_queued: u32,
    pub max_records: u64,
    pub max_payload_bytes: u64,
    pub max_request_bytes: usize,
    pub memory_budget_bytes: u64,
    pub host_reserve_bytes: u64,
    pub isolation: IsolationOptions,
    pub map_config: pb::MapConfig,
    pub geo: tasks::GeoConfig,
    pub basemap: tasks::basemaps::BasemapConfig,
}
impl Default for Config {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
        Self {
            data_dir: PathBuf::from("var"),
            budget: ThreadBudget {
                total_threads: cpus,
            },
            max_running: 4,
            max_queued: 128,
            max_records: 10_000,
            max_payload_bytes: 4 * 1024 * 1024 * 1024,
            max_request_bytes: 32 * 1024 * 1024,
            memory_budget_bytes: 4 * 1024 * 1024 * 1024,
            host_reserve_bytes: 512 * 1024 * 1024,
            isolation: IsolationOptions {
                max_frame_bytes: 64 * 1024 * 1024,
                ..Default::default()
            },
            map_config: pb::MapConfig::default(),
            geo: tasks::GeoConfig::default(),
            basemap: tasks::basemaps::BasemapConfig::default(),
        }
    }
}
impl Config {
    fn running_limit(&self) -> Result<u32> {
        let child_memory = self
            .isolation
            .memory_limit_bytes
            .filter(|&n| n > 0)
            .ok_or_else(|| AppError::invalid("server requires a child memory limit"))?;
        let per_job = child_memory
            .checked_add(4 * u64::from(self.isolation.max_frame_bytes))
            .ok_or_else(|| AppError::invalid("memory budget overflow"))?;
        let memory_slots = self
            .memory_budget_bytes
            .saturating_sub(self.host_reserve_bytes)
            / per_job;
        let slots = memory_slots
            .min(u64::from(self.max_running))
            .min(u64::from(self.budget.total_threads));
        if slots == 0
            || self.max_queued == 0
            || self.max_records == 0
            || self.max_payload_bytes == 0
            || self.max_request_bytes == 0
            || self.max_request_bytes.saturating_add(64) > self.isolation.max_frame_bytes as usize
        {
            return Err(AppError::invalid(
                "invalid capacity limits, or memory budget cannot admit one job",
            ));
        }
        Ok(slots as u32)
    }
}

struct Active {
    threads: u32,
    cancelled: Arc<AtomicBool>,
}
struct State {
    store: Store,
    active: BTreeMap<i64, Active>,
    watchers: HashMap<i64, watch::Sender<pb::Job>>,
    task_active: BTreeMap<String, tasks::TaskActive>,
    geo_pending_poll_ms: i64,
}
impl State {
    fn reserved(&self) -> u32 {
        self.active.values().map(|a| a.threads).sum::<u32>()
            + self.task_active.values().map(|a| a.threads).sum::<u32>()
    }
    fn publish(&mut self, job: &pb::Job) {
        let id = job.id.parse::<i64>().expect("database job ID");
        if let Some(sender) = self.watchers.get(&id) {
            sender.send_replace(job.clone());
        }
        if terminal(job) {
            self.watchers.remove(&id);
        }
        self.watchers
            .retain(|_, sender| sender.receiver_count() > 0);
    }
}
struct Inner {
    config: Config,
    max_running: u32,
    state: Mutex<State>,
    changed: Condvar,
    stopping: AtomicBool,
    supervisor: Supervisor,
    program: PathBuf,
}
#[derive(Clone)]
pub struct Service {
    inner: Arc<Inner>,
}
pub struct Runtime {
    pub service: Service,
    dispatcher: Option<JoinHandle<()>>,
}

impl Runtime {
    pub fn open(mut config: Config, program: PathBuf, args: Vec<OsString>) -> Result<Self> {
        config.map_config = crate::maps::normalize(config.map_config)?;
        let max_running = config.running_limit()?;
        let mut store = Store::open(
            &config.data_dir,
            config.max_payload_bytes,
            config.max_records,
        )?;
        store.init_tasks()?;
        store.init_basemaps()?;
        let executor = Executor::new(config.budget, 0).map_err(|e| AppError::invalid(e.0))?;
        let supervisor = Supervisor::new(
            Arc::new(executor),
            program.clone(),
            args,
            config.isolation.clone(),
        )
        .map_err(|e| AppError::invalid(e.to_string()))?;
        let service = Service {
            inner: Arc::new(Inner {
                config,
                max_running,
                state: Mutex::new(State {
                    store,
                    active: BTreeMap::new(),
                    watchers: HashMap::new(),
                    task_active: BTreeMap::new(),
                    geo_pending_poll_ms: 0,
                }),
                changed: Condvar::new(),
                stopping: AtomicBool::new(false),
                supervisor,
                program,
            }),
        };
        let runner = service.clone();
        service.recover_basemap_files()?;
        service.register_local_roads()?;
        service.recover_operations()?;
        let dispatcher = std::thread::Builder::new()
            .name("vrp-dispatch".into())
            .spawn(move || runner.dispatch_loop())?;
        Ok(Self {
            service,
            dispatcher: Some(dispatcher),
        })
    }
    pub fn shutdown(mut self) {
        self.stop_and_join();
    }
    fn stop_and_join(&mut self) {
        self.service.stop();
        if let Some(handle) = self.dispatcher.take() {
            let _ = handle.join();
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

impl Service {
    fn state(&self) -> Result<MutexGuard<'_, State>> {
        self.inner
            .state
            .lock()
            .map_err(|_| AppError::internal("task state unavailable"))
    }
    pub fn request_limit(&self) -> usize {
        self.inner.config.max_request_bytes
    }
    pub fn is_stopping(&self) -> bool {
        self.inner.stopping.load(Ordering::Acquire)
    }
    pub fn stop(&self) {
        self.inner.stopping.store(true, Ordering::Release);
        if let Ok(state) = self.state() {
            for active in state.active.values() {
                active.cancelled.store(true, Ordering::Release);
            }
            for active in state.task_active.values() {
                active.cancelled.store(true, Ordering::Release);
            }
        }
        self.inner.changed.notify_all();
    }
    fn accepting(&self) -> Result<()> {
        if self.is_stopping() {
            Err(AppError::new("unavailable", "server is stopping"))
        } else {
            Ok(())
        }
    }
    fn validate(&self, request: &engine::SolveRequest) -> Result<u32> {
        if request.encoded_len() > self.request_limit() {
            return Err(AppError::new(
                "too_large",
                "request exceeds configured size limit",
            ));
        }
        let p = request
            .problem
            .as_ref()
            .ok_or_else(|| AppError::invalid("problem is required"))?;
        let auxiliary: std::collections::HashSet<u32> = p
            .execution
            .iter()
            .flat_map(|e| &e.tickets)
            .filter(|t| t.auxiliary)
            .map(|t| t.ticket)
            .collect();
        let business_tickets = p
            .tickets
            .iter()
            .enumerate()
            .filter(|(i, _)| !auxiliary.contains(&(*i as u32)))
            .count();
        if business_tickets > 1000 || p.tickets.len() > 2200 || p.agents.len() > 200 {
            return Err(AppError::invalid(
                "service supports at most 1000 tickets and 200 agents per job",
            ));
        }
        let options = vrp_model::resolve_options(request.options.as_ref())
            .map_err(|e| AppError::invalid(e.0))?;
        self.inner
            .config
            .budget
            .validate_request(&options)
            .map_err(|e| AppError::invalid(e.0))?;
        Ok(options.threads)
    }

    pub fn save_scenario(&self, request: pb::SaveScenarioRequest) -> Result<pb::Scenario> {
        scenario_id(&request.scenario_id)?;
        if request.name.len() > 512 {
            return Err(AppError::invalid("scenario name is too long"));
        }
        if request
            .origin
            .as_ref()
            .is_some_and(|v| v.encoded_len() > 1_048_576)
        {
            return Err(AppError::invalid("scenario origin metadata exceeds 1 MiB"));
        }
        if request
            .display
            .as_ref()
            .is_some_and(|d| d.encoded_len() > 1_048_576)
        {
            return Err(AppError::invalid("scenario display metadata exceeds 1 MiB"));
        }
        let input = request
            .request
            .as_ref()
            .ok_or_else(|| AppError::invalid("request is required"))?;
        self.validate(input)?;
        crate::maps::quality(input)?;
        let bytes = input.encode_to_vec();
        let mut state = self.state()?;
        self.accepting()?;
        let saved = self.save_with_execution(&mut state, &request, &bytes)?;
        let input = request.request.as_ref().expect("validated request").clone();
        drop(state);
        if let Err(error) = self.prefetch_roads(&saved, &input) {
            eprintln!("road prefetch: {}", error.code);
        }
        Ok(saved)
    }
    pub fn get_scenario(&self, id: &str, version: u64) -> Result<pb::ScenarioVersion> {
        scenario_id(id)?;
        if version > i64::MAX as u64 {
            return Err(AppError::invalid("scenario version out of range"));
        }
        let (meta, bytes) = self.state()?.store.scenario(id, version)?;
        Ok(pb::ScenarioVersion {
            scenario: Some(self.scenario_geo(meta)?),
            request: Some(engine::SolveRequest::decode(bytes.as_slice())?),
        })
    }
    pub fn list_scenarios(
        &self,
        request: pb::ListScenariosRequest,
    ) -> Result<pb::ListScenariosResponse> {
        let limit = page_size(request.page_size)?;
        if !request.after_id.is_empty() {
            scenario_id(&request.after_id)?;
        }
        let mut scenarios = self
            .state()?
            .store
            .scenarios(&request.after_id, limit + 1)?;
        let more = scenarios.len() > limit;
        scenarios.truncate(limit);
        scenarios = scenarios
            .into_iter()
            .map(|s| self.scenario_geo(s))
            .collect::<Result<Vec<_>>>()?;
        let next_after_id = if more {
            scenarios.last().unwrap().id.clone()
        } else {
            String::new()
        };
        Ok(pb::ListScenariosResponse {
            scenarios,
            next_after_id,
        })
    }

    pub fn get_scenario_metadata(&self, id: &str, version: u64) -> Result<pb::Scenario> {
        scenario_id(id)?;
        if version > i64::MAX as u64 {
            return Err(AppError::invalid("scenario version out of range"));
        }
        let meta = self.state()?.store.scenario_metadata(id, version)?;
        self.scenario_geo(meta)
    }

    pub fn delete_scenario(&self, request: pb::DeleteScenarioRequest) -> Result<pb::Empty> {
        scenario_id(&request.scenario_id)?;
        if request.expected_version == 0 || request.expected_version > i64::MAX as u64 {
            return Err(AppError::invalid(
                "a positive current scenario version is required",
            ));
        }
        let mut state = self.state()?;
        self.accepting()?;
        let deleted = state.store.delete_scenario(&request)?;
        drop(state);
        self.cleanup_deleted_tasks(deleted)?;
        Ok(pb::Empty {})
    }

    pub fn submit(&self, request: pb::SubmitJobRequest) -> Result<pb::Job> {
        if !matches!(
            pb::JobKind::try_from(request.kind),
            Ok(pb::JobKind::Planning | pb::JobKind::Replanning)
        ) {
            return Err(AppError::invalid(
                "use the dedicated optimization/replan/adjust endpoint",
            ));
        }
        if request.idempotency_key.len() > 128
            || !request
                .idempotency_key
                .bytes()
                .all(|b| b.is_ascii_graphic())
        {
            return Err(AppError::invalid(
                "idempotency key must contain at most 128 visible ASCII characters",
            ));
        }
        let (mut input, scenario) = match request
            .source
            .ok_or_else(|| AppError::invalid("request or scenario is required"))?
        {
            pb::submit_job_request::Source::Request(input) => (*input, None),
            pb::submit_job_request::Source::Scenario(reference) => {
                if reference.version == 0 {
                    return Err(AppError::invalid(
                        "an explicit scenario version is required",
                    ));
                }
                let version = self.get_scenario(&reference.scenario_id, reference.version)?;
                (version.request.unwrap(), Some(reference))
            }
        };
        if request.options.is_some() {
            input.options = request.options;
        }
        vrp_model::validate_ready_matrices(&input)
            .map_err(|e| AppError::new("matrix_not_ready", e.0))?;
        let threads = self.validate(&input)?;
        let bytes = input.encode_to_vec();
        let mut hash = Sha256::new();
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
        if request.kind != 0 {
            hash.update(request.kind.to_le_bytes());
        }
        if let Some(s) = &scenario {
            hash.update(s.encode_to_vec());
        }
        let fingerprint = hash.finalize();
        let mut state = self.state()?;
        if let Some(job) = state
            .store
            .idempotent(&request.idempotency_key, &fingerprint)?
        {
            return Ok(job);
        }
        self.accepting()?;
        if state.store.queued()? >= self.inner.config.max_queued {
            return Err(AppError::new("capacity", "task queue is full"));
        }
        let job = state.store.insert_job(
            pb::Job {
                scenario,
                kind: request.kind,
                state: pb::JobState::Queued as i32,
                threads,
                revision: 1,
                created_unix_ms: now_ms(),
                ..Default::default()
            },
            &bytes,
            &request.idempotency_key,
            &fingerprint,
        )?;
        self.inner.changed.notify_all();
        Ok(job)
    }
    pub fn get_job(&self, id: &str) -> Result<pb::Job> {
        self.state()?.store.job(job_id(id)?)
    }
    pub fn list_jobs(&self, request: pb::ListJobsRequest) -> Result<pb::ListJobsResponse> {
        let limit = page_size(request.page_size)?;
        let before = if request.before_id.is_empty() {
            0
        } else {
            job_id(&request.before_id)?
        };
        if !request.scenario_id.is_empty() {
            scenario_id(&request.scenario_id)?;
        }
        let mut jobs = self
            .state()?
            .store
            .jobs(before, &request.scenario_id, limit + 1)?;
        let more = jobs.len() > limit;
        jobs.truncate(limit);
        let next_before_id = if more {
            jobs.last().unwrap().id.clone()
        } else {
            String::new()
        };
        Ok(pb::ListJobsResponse {
            jobs,
            next_before_id,
        })
    }
    pub fn cancel(&self, id: &str) -> Result<pb::Job> {
        let id = job_id(id)?;
        let mut state = self.state()?;
        let mut job = state.store.job(id)?;
        if terminal(&job) || job.state() == pb::JobState::Cancelling {
            return Ok(job);
        }
        if let Some(active) = state.active.get(&id) {
            active.cancelled.store(true, Ordering::Release);
            job.state = pb::JobState::Cancelling as i32;
        } else {
            job.state = pb::JobState::Cancelled as i32;
            job.finished_unix_ms = now_ms();
        }
        job.revision += 1;
        state.store.update(&job, None)?;
        state.publish(&job);
        self.inner.changed.notify_all();
        Ok(job)
    }
    pub fn delete(&self, id: &str) -> Result<pb::Empty> {
        let deleted = self.state()?.store.delete_plan(job_id(id)?)?;
        self.cleanup_deleted_tasks(deleted)?;
        Ok(pb::Empty {})
    }
    pub fn payload(&self, id: &str, result: bool) -> Result<Vec<u8>> {
        self.state()?.store.payload(job_id(id)?, result)
    }
    pub fn get_job_trend(&self, id: &str) -> Result<pb::JobTrend> {
        self.state()?.store.job_trend(job_id(id)?)
    }
    pub fn subscribe(&self, id: &str) -> Result<watch::Receiver<pb::Job>> {
        let id = job_id(id)?;
        let mut state = self.state()?;
        let job = state.store.job(id)?;
        if terminal(&job) {
            return Ok(watch::channel(job).1);
        }
        Ok(state
            .watchers
            .entry(id)
            .or_insert_with(|| watch::channel(job).0)
            .subscribe())
    }
    pub fn health(&self) -> Result<pb::HealthStatus> {
        let state = self.state()?;
        Ok(pb::HealthStatus {
            ready: !self.is_stopping(),
            total_threads: self.inner.config.budget.total_threads,
            reserved_threads: state.reserved(),
            running_jobs: state.active.len() as u32,
            queued_jobs: state.store.queued()?,
            max_running_jobs: self.inner.max_running,
            payload_bytes: state.store.payload_bytes,
            engine_version: env!("CARGO_PKG_VERSION").into(),
        })
    }

    fn admit(&self) -> Result<Option<(pb::Job, Arc<AtomicBool>)>> {
        let mut state = self.state()?;
        if self.is_stopping() || state.active.len() >= self.inner.max_running as usize {
            return Ok(None);
        }
        let Some(mut job) = state.store.first_queued()? else {
            return Ok(None);
        };
        if job.threads > self.inner.config.budget.total_threads {
            job.state = pb::JobState::Failed as i32;
            job.finished_unix_ms = now_ms();
            job.revision += 1;
            job.error =
                Some(AppError::invalid("saved job exceeds current thread budget").envelope());
            state.store.update(&job, None)?;
            state.publish(&job);
            return Ok(None);
        }
        if job.threads > self.inner.config.budget.total_threads - state.reserved() {
            return Ok(None);
        }
        let per_job = self.inner.config.isolation.memory_limit_bytes.unwrap_or(0)
            + 4 * u64::from(self.inner.config.isolation.max_frame_bytes);
        if self.reserved_memory(&state).saturating_add(per_job)
            > self
                .inner
                .config
                .memory_budget_bytes
                .saturating_sub(self.inner.config.host_reserve_bytes)
        {
            return Ok(None);
        }
        job.state = pb::JobState::Running as i32;
        job.resource_limits = Some(pb::TaskResources {
            cpu_threads: job.threads,
            memory_bytes: self.inner.config.isolation.memory_limit_bytes.unwrap_or(0),
            io_slots: 0,
            class: "solve".into(),
        });
        job.started_unix_ms = now_ms();
        job.revision += 1;
        state.store.update(&job, None)?;
        state.publish(&job);
        let cancelled = Arc::new(AtomicBool::new(false));
        state.active.insert(
            job_id(&job.id)?,
            Active {
                threads: job.threads,
                cancelled: cancelled.clone(),
            },
        );
        Ok(Some((job, cancelled)))
    }

    fn dispatch_loop(&self) {
        let mut workers: Vec<JoinHandle<()>> = Vec::new();
        loop {
            let mut i = 0;
            while i < workers.len() {
                if workers[i].is_finished() {
                    if workers.swap_remove(i).join().is_err() {
                        self.stop();
                    }
                } else {
                    i += 1;
                }
            }
            if self.is_stopping()
                && workers.is_empty()
                && self.state().is_ok_and(|s| s.task_active.is_empty())
            {
                break;
            }
            if !self.is_stopping() {
                self.dispatch_replan();
                self.dispatch_tasks();
            }
            if self.is_stopping() && self.state().is_ok_and(|s| !s.task_active.is_empty()) {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            match self.admit() {
                Ok(Some((job, flag))) => {
                    let service = self.clone();
                    let id = job.id;
                    let task_id = id.clone();
                    match std::thread::Builder::new()
                        .name(format!("vrp-job-{id}"))
                        .spawn(move || {
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    service.execute(&task_id, &flag)
                                }))
                                .unwrap_or_else(|_| {
                                    Err(AppError::internal("execution thread panicked"))
                                });
                            if let Err(e) = service.finish(&task_id, result) {
                                eprintln!("job finalization: {e}");
                                service.stop();
                            }
                        }) {
                        Ok(handle) => workers.push(handle),
                        Err(e) => {
                            if self.finish(&id, Err(e.into())).is_err() {
                                self.stop();
                            }
                        }
                    }
                    continue;
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("dispatch: {e}");
                    self.stop();
                }
            }
            if let Ok(state) = self.inner.state.lock() {
                let _guard = self
                    .inner
                    .changed
                    .wait_timeout(state, Duration::from_millis(25));
            }
        }
    }

    fn execute(&self, id: &str, flag: &AtomicBool) -> Result<engine::SolveResult> {
        let bytes = self.payload(id, false)?;
        let request = engine::SolveRequest::decode(bytes.as_slice())?;
        drop(bytes);
        self.validate(&request)?;
        self.inner
            .supervisor
            .solve_observed(&request, flag, |progress| {
                let update = || -> Result<()> {
                    let mut state = self.state()?;
                    let mut job = state.store.job(job_id(id)?)?;
                    job.progress = Some(*progress);
                    job.revision += 1;
                    state.store.update(&job, None)?;
                    state.publish(&job);
                    Ok(())
                };
                if update().is_err() {
                    flag.store(true, Ordering::Release);
                    self.stop();
                }
            })
            .map_err(|error| match error {
                WorkerError::Child { code, message } => AppError::new(
                    if code == "invalid_request" {
                        "invalid_request"
                    } else {
                        "worker_failed"
                    },
                    message.chars().take(2048).collect::<String>(),
                ),
                WorkerError::Killed { reason, .. } => AppError::new("worker_killed", reason),
                WorkerError::Admission(vrp_model::ExecutionError::Cancelled) => {
                    AppError::new("cancelled_before_start", "cancelled before worker started")
                }
                e => AppError::new("worker_failed", e.to_string()),
            })
    }

    fn finish(&self, id: &str, result: Result<engine::SolveResult>) -> Result<()> {
        let result_bytes = result.as_ref().ok().map(Message::encode_to_vec);
        let mut state = self.state()?;
        let id = job_id(id)?;
        let mut job = state.store.job(id)?;
        job.finished_unix_ms = now_ms();
        job.revision += 1;
        match result {
            Ok(result) => {
                let cancelled = result
                    .stats
                    .as_ref()
                    .is_some_and(|s| s.termination_reason == "cancelled");
                job.state = if cancelled {
                    pb::JobState::Cancelled
                } else {
                    pb::JobState::Succeeded
                } as i32;
                job.has_result = true;
                job.score = result.score;
                job.stats = result.stats;
            }
            Err(e) => {
                job.state = if e.code == "cancelled_before_start" {
                    pb::JobState::Cancelled
                } else {
                    pb::JobState::Failed
                } as i32;
                job.error = Some(e.envelope());
            }
        }
        if let Err(error) = state.store.update(&job, result_bytes.as_deref()) {
            job.state = pb::JobState::Failed as i32;
            job.has_result = false;
            job.score = None;
            job.stats = None;
            job.error = Some(AppError::new("result_storage_failed", error.message).envelope());
            state.store.update(&job, None)?;
        }
        state.publish(&job);
        drop(state);
        self.complete_replan(&job);
        self.state()?.active.remove(&id);
        self.inner.changed.notify_all();
        Ok(())
    }
}
