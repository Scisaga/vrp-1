//! Application task orchestration and managed road data. No geography enters core.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;
use vrp_geo::{Bounds, GeoPoint};
pub(crate) mod basemaps;
mod roads;
pub(crate) mod workflow;

#[derive(Clone, Debug)]
pub struct GeoConfig {
    pub auto_prefetch: bool,
    pub catalog_url: String,
    pub download_budget_bytes: u64,
    pub local_import_dir: Option<PathBuf>,
    pub road_wait: Duration,
    pub worker_memory_bytes: u64,
}
impl Default for GeoConfig {
    fn default() -> Self {
        Self {
            auto_prefetch: true,
            catalog_url: "https://download.geofabrik.de/index-v1.json".into(),
            download_budget_bytes: 500 * 1024 * 1024,
            local_import_dir: None,
            road_wait: Duration::from_secs(180),
            worker_memory_bytes: 1536 * 1024 * 1024,
        }
    }
}
pub(crate) struct TaskActive {
    pub threads: u32,
    pub memory: u64,
    pub cancelled: Arc<AtomicBool>,
}
struct GeoLease<'a> {
    service: &'a Service,
    id: &'a str,
}
impl Drop for GeoLease<'_> {
    fn drop(&mut self) {
        self.service.release_geo(self.id);
    }
}
pub fn task_terminal(task: &pb::Task) -> bool {
    matches!(
        task.state(),
        pb::TaskState::Succeeded
            | pb::TaskState::Partial
            | pb::TaskState::Failed
            | pb::TaskState::Cancelled
            | pb::TaskState::Interrupted
    )
}
fn digest(b: &[u8]) -> String {
    format!("{:x}", Sha256::digest(b))
}
fn download_task(t: &pb::Task) -> bool {
    t.kind() == pb::TaskKind::BasemapPrepare
        || t.kind() == pb::TaskKind::RoadPackagePrepare && t.scenario.is_none()
}
fn geo_error(e: impl std::fmt::Display) -> AppError {
    AppError::new("geo_error", e.to_string())
}
fn points(input: &engine::SolveRequest) -> Result<Vec<GeoPoint>> {
    Ok(input
        .problem
        .as_ref()
        .ok_or_else(|| AppError::invalid("problem required"))?
        .locations
        .iter()
        .map(|p| GeoPoint {
            lng: p.lng,
            lat: p.lat,
        })
        .collect())
}
fn bounds(input: &engine::SolveRequest, buffer: f64) -> Result<Bounds> {
    Bounds::from_points(&points(input)?)
        .and_then(|b| b.expanded_km(buffer))
        .map_err(geo_error)
}
fn coordinate_hash(input: &engine::SolveRequest) -> String {
    let Some(p) = &input.problem else {
        return String::new();
    };
    let mut h = Sha256::new();
    for l in &p.locations {
        h.update((l.id.len() as u64).to_le_bytes());
        h.update(l.id.as_bytes());
        h.update(l.lng.to_bits().to_le_bytes());
        h.update(l.lat.to_bits().to_le_bytes());
    }
    format!("{:x}", h.finalize())
}
fn new_task(
    kind: pb::TaskKind,
    scenario: Option<pb::ScenarioRef>,
    parent: &str,
    key: &str,
) -> pb::Task {
    pb::Task {
        kind: kind as i32,
        state: pb::TaskState::Queued as i32,
        scenario,
        parent_id: parent.into(),
        created_unix_ms: now_ms(),
        idempotency_key: key.into(),
        ..Default::default()
    }
}
impl Service {
    pub(crate) fn queue_plan_geometry(&self, job: &pb::Job) -> Result<Option<pb::Task>> {
        let Some(reference) = job.scenario.as_ref() else {
            return Ok(None);
        };
        let key = format!("mcp-geometry-{}", job.id);
        let mut state = self.state()?;
        if let Some(task) = state.store.task_by_key(&key)? {
            return Ok(Some(task));
        }
        let (_, bytes) = state
            .store
            .scenario(&reference.scenario_id, reference.version)?;
        let request = engine::SolveRequest::decode(bytes.as_slice())?;
        let problem = request.problem.as_ref().unwrap();
        let coordinates = coordinate_hash(&request);
        let tasks = state.store.tasks()?;
        // Match the original business snapshot. Its locations and matrices survive
        // projection; the candidate may have different tickets and live positions.
        let source = tasks
            .iter()
            .filter(|task| {
                task.kind() == pb::TaskKind::MatrixPrepare
                    && task.state() == pb::TaskState::Succeeded
                    && (task.coordinate_hash.is_empty() || task.coordinate_hash == coordinates)
            })
            .find_map(|task| {
                let request_path = state.store.artifact_path(&task.id, "request").ok()?;
                let bytes = fs::read(request_path).ok()?;
                let snapshot = engine::SolveRequest::decode(bytes.as_slice())
                    .ok()?
                    .problem?;
                if snapshot.locations != problem.locations || snapshot.matrices != problem.matrices
                {
                    return None;
                }
                let path = state.store.artifact_path(&task.id, "network").ok()?;
                Some((task, path))
            });
        let Some((source, path)) = source else {
            return Ok(None);
        };
        self.accepting()?;
        let geometry = pb::GeometryGenerateInput {
            job_id: job.id.clone(),
            network_artifact: path.to_string_lossy().into(),
            road_package_ids: source.road_package_ids.clone(),
        };
        let input = pb::TaskInput {
            input: Some(pb::task_input::Input::Geometry(geometry)),
            ..Default::default()
        };
        let mut task = new_task(
            pb::TaskKind::GeometryGenerate,
            job.scenario.clone(),
            "",
            &key,
        );
        task.result_job_id.clone_from(&job.id);
        task.road_package_ids.clone_from(&source.road_package_ids);
        task.dependency_ids = vec![source.id.clone(), job.id.clone()];
        let task = state.store.insert_task(task, &input)?;
        self.inner.changed.notify_all();
        Ok(Some(task))
    }

    pub(crate) fn plan_geometry_task(&self, job_id: &str) -> Result<Option<pb::Task>> {
        self.state()?
            .store
            .task_by_key(&format!("mcp-geometry-{job_id}"))
    }

    fn task_resources(&self, task: &mut pb::Task, state: &State) {
        if task.resource_request.is_none() {
            task.resource_request = Some(if let Some(job) = &task.solver {
                job.resource_limits.clone().unwrap_or(pb::TaskResources {
                    cpu_threads: job.threads,
                    class: "solve".into(),
                    ..Default::default()
                })
            } else {
                pb::TaskResources {
                    cpu_threads: if matches!(
                        task.kind(),
                        pb::TaskKind::MatrixPrepare
                            | pb::TaskKind::GeometryGenerate
                            | pb::TaskKind::BasemapPrepare
                    ) {
                        1
                    } else {
                        0
                    },
                    memory_bytes: if task.kind() == pb::TaskKind::BasemapPrepare {
                        self.inner.config.basemap.worker_memory_bytes
                    } else if matches!(
                        task.kind(),
                        pb::TaskKind::MatrixPrepare | pb::TaskKind::GeometryGenerate
                    ) {
                        self.inner.config.geo.worker_memory_bytes
                    } else {
                        0
                    },
                    io_slots: u32::from(download_task(task)),
                    class: format!("{:?}", task.kind()),
                }
            });
        }
        task.resources_in_use = Some(if let Some(active) = state.task_active.get(&task.id) {
            pb::TaskResources {
                cpu_threads: active.threads,
                memory_bytes: active.memory,
                ..Default::default()
            }
        } else if let Ok(id) = task.id.parse::<i64>() {
            state
                .active
                .get(&id)
                .map(|active| pb::TaskResources {
                    cpu_threads: active.threads,
                    memory_bytes: task.resource_request.as_ref().map_or(0, |r| r.memory_bytes),
                    ..Default::default()
                })
                .unwrap_or_default()
        } else {
            pb::TaskResources::default()
        });
    }
    pub fn submit_task(&self, request: pb::SubmitTaskRequest) -> Result<pb::Task> {
        let mut input = request
            .input
            .ok_or_else(|| AppError::invalid("typed task input is required"))?;
        let original_hash = digest(&input.encode_to_vec());
        if !request.idempotency_key.is_empty()
            && matches!(input.input, Some(pb::task_input::Input::RoadPackage(_)))
        {
            let state = self.state()?;
            for old in state.store.tasks()? {
                if old.idempotency_key == request.idempotency_key {
                    if state.store.task_input(&old.id)?.input_sha256 == original_hash {
                        return Ok(old);
                    }
                    return Err(AppError::conflict(
                        "idempotency key has different task input",
                    ));
                }
            }
        }
        match input.input.as_mut() {
            Some(pb::task_input::Input::Basemap(v)) => {
                if !v.import_path.is_empty() || v.source.is_some() || !v.reuse_path.is_empty() {
                    return Err(AppError::invalid(
                        "use the managed basemap import/prepare endpoint",
                    ));
                }
                self.prepare_basemap(
                    v.request
                        .clone()
                        .ok_or_else(|| AppError::invalid("basemap request required"))?,
                )
            }
            Some(pb::task_input::Input::Solve(v)) => {
                v.idempotency_key = request.idempotency_key;
                let job = self.submit(v.clone())?;
                self.get_task(&job.id)
            }
            Some(pb::task_input::Input::GeoPlan(v)) => {
                v.idempotency_key = request.idempotency_key;
                self.plan_with_roads(v.clone())
            }
            Some(pb::task_input::Input::RoadPackage(v)) => {
                let mut prepared = if !v.import_path.is_empty() {
                    self.import_package_input(pb::ImportRoadPackageRequest {
                        path: v.import_path.clone(),
                        name: v.name.clone(),
                        region_id: v.region_id.clone(),
                        coverage_geojson: v.coverage_geojson.clone(),
                    })?
                } else {
                    self.prepare_package_input(&v.region_id)?
                };
                prepared.force_download = v.force_download;
                self.package_task_key(prepared, "", &request.idempotency_key, &original_hash)
            }
            Some(pb::task_input::Input::Matrix(v)) => {
                if !v.network_artifact.is_empty() {
                    return Err(AppError::invalid(
                        "matrix preparation cannot receive a filesystem path",
                    ));
                }
                let solve_input = v.request.as_ref().ok_or_else(|| {
                    AppError::invalid("matrix input requires native SolveRequest")
                })?;
                self.validate(solve_input)?;
                for id in &v.road_package_ids {
                    self.get_road_package(id)?;
                }
                let mut s = self.state()?;
                let t = s.store.insert_task(
                    new_task(
                        pb::TaskKind::MatrixPrepare,
                        None,
                        "",
                        &request.idempotency_key,
                    ),
                    &input,
                )?;
                drop(s);
                self.inner.changed.notify_all();
                Ok(t)
            }
            Some(pb::task_input::Input::Geometry(v)) => {
                let job = self.get_job(&v.job_id)?;
                if !terminal(&job) || !job.has_result {
                    return Err(AppError::conflict(
                        "geometry requires terminal solver result",
                    ));
                }
                let owner = v.network_artifact.clone();
                if !owner.starts_with("task-") {
                    return Err(AppError::invalid(
                        "network_artifact must identify a task with a network artifact",
                    ));
                }
                let prepared_path = self.state()?.store.artifact_path(&owner, "request")?;
                let prepared = engine::SolveRequest::decode(fs::read(prepared_path)?.as_slice())?;
                let frozen =
                    engine::SolveRequest::decode(self.payload(&job.id, false)?.as_slice())?;
                if prepared.problem != frozen.problem {
                    return Err(AppError::conflict(
                        "network snapshot belongs to a different frozen solver input",
                    ));
                }
                v.network_artifact = self
                    .state()?
                    .store
                    .artifact_path(&owner, "network")?
                    .to_string_lossy()
                    .into();
                let mut t = new_task(
                    pb::TaskKind::GeometryGenerate,
                    job.scenario,
                    "",
                    &request.idempotency_key,
                );
                t.dependency_ids = vec![owner, v.job_id.clone()];
                let t = self.state()?.store.insert_task(t, &input)?;
                self.inner.changed.notify_all();
                Ok(t)
            }
            None => Err(AppError::invalid("typed task input required")),
        }
    }
    pub fn delete_task(&self, id: &str) -> Result<pb::Empty> {
        if !id.starts_with("task-") {
            return self.delete(id);
        }
        let deleted = self.state()?.store.delete_generic_task(id)?;
        self.cleanup_deleted_tasks(deleted)?;
        Ok(pb::Empty {})
    }
    pub(super) fn cleanup_deleted_tasks(&self, deleted: crate::store::DeletedTasks) -> Result<()> {
        let root = self
            .inner
            .config
            .data_dir
            .join("task-artifacts")
            .canonicalize()
            .ok();
        for path in deleted.artifact_paths {
            if let (Ok(path), Some(root)) = (path.canonicalize(), root.as_ref()) {
                if !path.starts_with(root) {
                    return Err(AppError::internal(
                        "artifact escaped managed data directory",
                    ));
                }
                if path.is_file() {
                    fs::remove_file(path)?;
                }
            }
        }
        if let Some(root) = root {
            for id in deleted.task_ids {
                if let Ok(dir) = self.task_dir(&id).canonicalize()
                    && dir != root
                    && dir.starts_with(&root)
                {
                    fs::remove_dir_all(dir)?;
                }
            }
        }
        Ok(())
    }
    pub(super) fn scenario_geo(&self, mut scene: pb::Scenario) -> Result<pb::Scenario> {
        let state = self.state()?;
        let tasks = state.store.tasks()?;
        let matches_scene = |t: &&pb::Task| {
            t.scenario.as_ref().is_some_and(|r| {
                r.scenario_id == scene.id
                    && (r.version == scene.version || t.result_scenario_version == scene.version)
            })
        };
        let latest = tasks
            .iter()
            .filter(matches_scene)
            .find(|t| t.kind() == pb::TaskKind::GeoPlan)
            .or_else(|| {
                tasks
                    .iter()
                    .filter(matches_scene)
                    .find(|t| t.kind() == pb::TaskKind::MatrixPrepare && t.parent_id.is_empty())
            })
            .or_else(|| {
                tasks
                    .iter()
                    .filter(matches_scene)
                    .find(|t| t.kind() == pb::TaskKind::RoadPackagePrepare)
            })
            .cloned();
        if let Some(task) = latest {
            let quality = state
                .store
                .tasks()?
                .into_iter()
                .filter(|t| t.id == task.id || t.parent_id == task.id)
                .find_map(|t| state.store.artifact_path(&t.id, "quality").ok())
                .and_then(|path| fs::read(path).ok())
                .and_then(|bytes| {
                    serde_json::from_slice::<pb::MatrixPreparationReport>(&bytes).ok()
                });
            let status = format!("{:?}", task.state());
            scene.geo = Some(pb::ScenarioGeoStatus {
                task_id: task.id,
                status,
                coordinate_hash: quality.as_ref().map_or_else(
                    || task.coordinate_hash.clone(),
                    |q| q.coordinate_hash.clone(),
                ),
                road_package_ids: task.road_package_ids,
                model_edges: quality.as_ref().map_or(0, |q| q.model_edges),
                fallback_edges: quality.as_ref().map_or(0, |q| q.fallback_edges),
                message: task.progress.map_or_else(String::new, |p| p.message),
            });
        }
        Ok(scene)
    }
    fn task_dir(&self, id: &str) -> PathBuf {
        self.inner.config.data_dir.join("task-artifacts").join(id)
    }
    fn road_dir(&self) -> PathBuf {
        self.inner.config.data_dir.join("road-packages")
    }
    pub fn get_task(&self, id: &str) -> Result<pb::Task> {
        let s = self.state()?;
        if id.starts_with("task-") {
            let mut t = s.store.task(id)?;
            t.artifacts = s.store.artifacts(id)?;
            t.child_ids = s
                .store
                .tasks()?
                .iter()
                .filter(|v| v.parent_id == id)
                .map(|v| v.id.clone())
                .collect();
            for child in &t.child_ids {
                t.artifacts.extend(s.store.artifacts(child)?);
            }
            if !t.result_job_id.is_empty() {
                t.child_ids.push(t.result_job_id.clone());
                t.solver = s.store.job(job_id(&t.result_job_id)?).ok();
            }
            self.task_resources(&mut t, &s);
            return Ok(t);
        }
        let job = s.store.job(job_id(id)?)?;
        let mut t = pb::Task {
            id: job.id.clone(),
            kind: pb::TaskKind::Solve as i32,
            state: job.state,
            revision: job.revision,
            created_unix_ms: job.created_unix_ms,
            started_unix_ms: job.started_unix_ms,
            finished_unix_ms: job.finished_unix_ms,
            scenario: job.scenario.clone(),
            parent_id: s.store.solver_parent(id)?.unwrap_or_default(),
            error: job.error.clone(),
            result_job_id: job.id.clone(),
            solver: Some(job.clone()),
            ..Default::default()
        };
        t.progress = Some(pb::TaskProgress {
            phase: job
                .progress
                .as_ref()
                .map(|p| format!("{:?}", p.phase()))
                .unwrap_or_else(|| format!("{:?}", job.state())),
            ..Default::default()
        });
        if job.has_result {
            t.artifacts.push(pb::TaskArtifact {
                id: "result".into(),
                task_id: id.into(),
                kind: "solve_result".into(),
                name: "SolveResult.pb".into(),
                media_type: "application/protobuf".into(),
                href: format!("/v1/jobs/{id}/result"),
                ..Default::default()
            });
        }
        self.task_resources(&mut t, &s);
        Ok(t)
    }
    pub fn list_tasks(&self, q: pb::ListTasksRequest) -> Result<pb::ListTasksResponse> {
        let limit = page_size(q.page_size)?;
        let s = self.state()?;
        let mut all = s.store.tasks()?;
        let mut before = 0;
        loop {
            let jobs = s.store.jobs(before, &q.scenario_id, 100)?;
            if jobs.is_empty() {
                break;
            }
            before = jobs.last().unwrap().id.parse().unwrap();
            let n = jobs.len();
            drop(jobs.iter().map(|j| j.id.clone()).collect::<Vec<_>>());
            for job in jobs {
                all.push(pb::Task {
                    id: job.id.clone(),
                    kind: pb::TaskKind::Solve as i32,
                    state: job.state,
                    revision: job.revision,
                    created_unix_ms: job.created_unix_ms,
                    started_unix_ms: job.started_unix_ms,
                    finished_unix_ms: job.finished_unix_ms,
                    parent_id: s.store.solver_parent(&job.id)?.unwrap_or_default(),
                    scenario: job.scenario.clone(),
                    error: job.error.clone(),
                    result_job_id: job.id.clone(),
                    solver: Some(job),
                    ..Default::default()
                });
            }
            if n < 100 {
                break;
            }
        }
        for task in &mut all {
            self.task_resources(task, &s);
        }
        drop(s);
        all.retain(|t| {
            (q.scenario_id.is_empty()
                || t.scenario
                    .as_ref()
                    .is_some_and(|s| s.scenario_id == q.scenario_id))
                && q.kind.is_none_or(|k| t.kind == k)
                && q.state.is_none_or(|v| t.state == v)
        });
        all.sort_by(|a, b| {
            b.created_unix_ms
                .cmp(&a.created_unix_ms)
                .then_with(|| b.id.cmp(&a.id))
        });
        let skip = if q.before_id.is_empty() {
            0
        } else {
            all.iter()
                .position(|t| t.id == q.before_id)
                .map_or(all.len(), |i| i + 1)
        };
        let remaining = all.len().saturating_sub(skip);
        let tasks: Vec<_> = all.into_iter().skip(skip).take(limit).collect();
        let next = if remaining > limit {
            tasks.last().unwrap().id.clone()
        } else {
            String::new()
        };
        Ok(pb::ListTasksResponse {
            tasks,
            next_before_id: next,
        })
    }
    pub fn cancel_task(&self, id: &str) -> Result<pb::Task> {
        if !id.starts_with("task-") {
            self.cancel(id)?;
            return self.get_task(id);
        }
        let mut s = self.state()?;
        let mut task = s.store.task(id)?;
        if task_terminal(&task) {
            return Ok(task);
        }
        if let Some(a) = s.task_active.get(id) {
            a.cancelled.store(true, Ordering::Release);
            task.state = pb::TaskState::Cancelling as i32;
        } else {
            task.state = pb::TaskState::Cancelled as i32;
            task.finished_unix_ms = now_ms();
        }
        let children = s
            .store
            .tasks()?
            .into_iter()
            .filter(|t| t.parent_id == id && !task_terminal(t))
            .map(|t| t.id)
            .collect::<Vec<_>>();
        let solver = task.result_job_id.clone();
        s.store.update_task(&task)?;
        drop(s);
        for child in children {
            let _ = self.cancel_task(&child);
        }
        if !solver.is_empty() {
            let _ = self.cancel(&solver);
        }
        self.inner.changed.notify_all();
        self.get_task(id)
    }
    pub fn retry_task(&self, id: &str) -> Result<pb::Task> {
        if !id.starts_with("task-") {
            let job = self.get_job(id)?;
            if !terminal(&job) {
                return Err(AppError::conflict(
                    "cancel or finish the task before retrying",
                ));
            }
            let mut input = engine::SolveRequest::decode(self.payload(id, false)?.as_slice())?;
            if job.kind() == pb::JobKind::ManualAdjustment {
                let retried = self.adjust_plan(pb::AdjustPlanRequest {
                    job_id: job.source_job_id,
                    solution: input.initial_solution,
                    actor: job.actor,
                    reason: job.reason,
                })?;
                return self.get_task(&retried.id);
            }
            let execution_scene = job
                .execution_basis
                .as_ref()
                .map(|b| b.scenario_id.as_str())
                .or_else(|| job.scenario.as_ref().map(|s| s.scenario_id.as_str()));
            let operation = match execution_scene {
                Some(scene) => self.state()?.store.operation(scene, 0)?,
                None => None,
            };
            if let Some(op) = operation {
                let retried = self.replan_from(
                    pb::ReplanRequest {
                        scenario_id: op
                            .scenario
                            .as_ref()
                            .expect("persisted scenario")
                            .scenario_id
                            .clone(),
                        expected_revision: op.revision,
                        options: input.options,
                    },
                    &job.id,
                )?;
                return self.get_task(&retried.id);
            }
            if job.execution_basis.is_some() || job.kind() == pb::JobKind::RealtimeReplan {
                return Err(AppError::conflict("execution no longer exists"));
            }
            vrp_model::materialize_matrices(&mut input).map_err(geo_error)?;
            vrp_model::validate_ready_matrices(&input)
                .map_err(|e| AppError::new("matrix_not_ready", e.0))?;
            let threads = self.validate(&input)?;
            let bytes = input.encode_to_vec();
            let mut state = self.state()?;
            self.accepting()?;
            state.store.job(job_id(id)?)?;
            if let Some(reference) = &job.scenario
                && state.store.operation(&reference.scenario_id, 0)?.is_some()
            {
                return Err(AppError::conflict(
                    "execution started; retry using a current replan",
                ));
            }
            if state.store.queued()? >= self.inner.config.max_queued {
                return Err(AppError::new("capacity", "task queue is full"));
            }
            let retried = state.store.insert_job(
                pb::Job {
                    scenario: job.scenario,
                    kind: job.kind,
                    source_job_id: job.id,
                    threads,
                    revision: 1,
                    created_unix_ms: now_ms(),
                    ..Default::default()
                },
                &bytes,
                "",
                &Sha256::digest(&bytes),
            )?;
            drop(state);
            self.inner.changed.notify_all();
            return self.get_task(&retried.id);
        }
        let mut s = self.state()?;
        let old = s.store.task(id)?;
        if !task_terminal(&old) {
            return Err(AppError::conflict(
                "cancel or finish the task before retrying",
            ));
        }
        let input = s.store.task_input(id)?;
        let mut t = new_task(old.kind(), old.scenario.clone(), "", "");
        t.source_task_id = id.into();
        t.result_scenario_version = old.result_scenario_version;
        t.result_job_id = old.result_job_id;
        t.road_package_ids = old.road_package_ids;
        let t = s.store.insert_task(t, &input)?;
        drop(s);
        self.inner.changed.notify_all();
        Ok(t)
    }
    pub fn task_artifacts(&self, id: &str) -> Result<pb::ListTaskArtifactsResponse> {
        Ok(pb::ListTaskArtifactsResponse {
            artifacts: self.get_task(id)?.artifacts,
        })
    }
    pub fn task_artifact(&self, id: &str, artifact: &str) -> Result<(pb::TaskArtifact, PathBuf)> {
        let s = self.state()?;
        let a = s
            .store
            .artifacts(id)?
            .into_iter()
            .find(|a| a.id == artifact)
            .ok_or_else(|| AppError::new("not_found", "artifact not found"))?;
        let path = s.store.artifact_path(id, artifact)?;
        Ok((a, path))
    }
    pub fn plan_with_roads(&self, r: pb::GeoPlanRequest) -> Result<pb::Task> {
        if r.expected_version == 0 {
            return Err(AppError::invalid("expected_version must be positive"));
        }
        if !(r.connector_speed_kmh == 0.0 || (1.0..=100.0).contains(&r.connector_speed_kmh))
            || !r.buffer_km.is_finite()
            || !(0.0..=100.0).contains(&r.buffer_km)
        {
            return Err(AppError::invalid("invalid connector speed or buffer"));
        }
        let mut s = self.state()?;
        if !r.idempotency_key.is_empty() {
            for old in s.store.tasks()? {
                if old.idempotency_key == r.idempotency_key {
                    let previous = s.store.task_input(&old.id)?;
                    if let Some(pb::task_input::Input::GeoPlan(previous)) = previous.input
                        && previous == r
                    {
                        return Ok(old);
                    }
                    return Err(AppError::conflict(
                        "idempotency key has different task input",
                    ));
                }
            }
        }
        if s.store.operation(&r.scenario_id, 0)?.is_some() {
            return Err(AppError::conflict(
                "road matrix generation is only available before execution",
            ));
        }
        let (scene, bytes) = s.store.scenario(&r.scenario_id, 0)?;
        if scene.version != r.expected_version {
            return Err(AppError::conflict(
                "scenario changed; reload before planning",
            ));
        }
        let mut request = engine::SolveRequest::decode(bytes.as_slice())?;
        if r.options.is_some() {
            request.options = r.options;
        }
        self.validate(&request)?;
        let input = pb::TaskInput {
            input: Some(pb::task_input::Input::GeoPlan(r.clone())),
            frozen_request: Some(request.clone()),
            input_sha256: digest(&request.encode_to_vec()),
        };
        let mut task = new_task(
            pb::TaskKind::GeoPlan,
            Some(pb::ScenarioRef {
                scenario_id: r.scenario_id,
                version: r.expected_version,
            }),
            "",
            &r.idempotency_key,
        );
        task.coordinate_hash = coordinate_hash(&request);
        let t = s.store.insert_task(task, &input)?;
        drop(s);
        self.inner.changed.notify_all();
        Ok(t)
    }
    pub(super) fn prefetch_roads(
        &self,
        scene: &pb::Scenario,
        input: &engine::SolveRequest,
    ) -> Result<()> {
        if !self.inner.config.geo.auto_prefetch
            || input
                .problem
                .as_ref()
                .is_none_or(|p| p.locations.is_empty())
        {
            self.state()?
                .store
                .finish_geo_pending(&scene.id, scene.version)?;
            return Ok(());
        }
        let hash = coordinate_hash(input);
        let key = format!("prefetch:{}:{hash}", scene.id);
        let data = pb::TaskInput {
            input: Some(pb::task_input::Input::GeoPlan(pb::GeoPlanRequest {
                scenario_id: scene.id.clone(),
                expected_version: scene.version,
                idempotency_key: key.clone(),
                ..Default::default()
            })),
            frozen_request: Some(input.clone()),
            input_sha256: hash.clone(),
        };
        let mut t = new_task(
            pb::TaskKind::RoadPackagePrepare,
            Some(pb::ScenarioRef {
                scenario_id: scene.id.clone(),
                version: scene.version,
            }),
            "",
            &key,
        );
        t.progress = Some(pb::TaskProgress {
            phase: "coverage_check".into(),
            ..Default::default()
        });
        t.coordinate_hash = hash;
        let mut s = self.state()?;
        s.store
            .consume_geo_pending(&scene.id, scene.version, t, &data)?;
        drop(s);
        self.inner.changed.notify_all();
        Ok(())
    }
    fn drain_geo_pending(&self) -> Result<()> {
        let pending = {
            let mut state = self.state()?;
            let now = now_ms();
            if now < state.geo_pending_poll_ms {
                return Ok(());
            }
            state.geo_pending_poll_ms = now.saturating_add(1_000);
            state.store.pending_geo(8)?
        };
        for (id, version) in pending {
            let snapshot = {
                let mut state = self.state()?;
                if !self.inner.config.geo.auto_prefetch {
                    state.store.finish_geo_pending(&id, version)?;
                    continue;
                }
                match state.store.scenario(&id, version) {
                    Ok(snapshot) => snapshot,
                    Err(e) if e.code == "not_found" => {
                        state.store.finish_geo_pending(&id, version)?;
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            };
            let input = engine::SolveRequest::decode(snapshot.1.as_slice())?;
            self.prefetch_roads(&snapshot.0, &input)?;
        }
        Ok(())
    }
    pub(crate) fn dispatch_tasks(&self) {
        if let Err(error) = self.drain_geo_pending() {
            eprintln!("road prefetch outbox: {}", error.code);
        }
        let action = || -> Result<()> {
            let mut s = self.state()?;
            let tasks = s.store.tasks()?;
            let package_running = tasks
                .iter()
                .filter(|t| s.task_active.contains_key(&t.id) && download_task(t))
                .count();
            let basemap_running = tasks.iter().any(|t| {
                s.task_active.contains_key(&t.id) && t.kind() == pb::TaskKind::BasemapPrepare
            });
            let eligible = |t: &&pb::Task| {
                if t.state() != pb::TaskState::Queued || s.task_active.contains_key(&t.id) {
                    return false;
                }
                t.kind() != pb::TaskKind::GeometryGenerate
                    || t.result_job_id.is_empty()
                    || job_id(&t.result_job_id)
                        .and_then(|id| s.store.job(id))
                        .ok()
                        .is_none_or(|job| terminal(&job))
            };
            let package = if package_running < 2 {
                tasks.iter().rev().filter(eligible).find(|t| {
                    download_task(t)
                        && !(basemap_running && t.kind() == pb::TaskKind::BasemapPrepare)
                })
            } else {
                None
            };
            let selected = package.or_else(|| {
                if s.task_active.len() < 16 {
                    tasks
                        .iter()
                        .rev()
                        .filter(eligible)
                        .find(|t| !download_task(t))
                } else {
                    None
                }
            });
            let Some(mut task) = selected.cloned() else {
                return Ok(());
            };
            task.state = pb::TaskState::Running as i32;
            task.started_unix_ms = now_ms();
            s.store.update_task(&task)?;
            let cancelled = Arc::new(AtomicBool::new(false));
            s.task_active.insert(
                task.id.clone(),
                TaskActive {
                    threads: 0,
                    memory: 0,
                    cancelled: cancelled.clone(),
                },
            );
            drop(s);
            let service = self.clone();
            let id = task.id.clone();
            if let Err(e) = std::thread::Builder::new()
                .name(format!("vrp-{id}"))
                .spawn(move || {
                    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        service.run_task(&task, &cancelled)
                    }))
                    .unwrap_or_else(|_| Err(AppError::internal("task worker panicked")));
                    let _ = service.finish_task(&task.id, out);
                })
            {
                self.finish_task(&id, Err(e.into()))?;
            }
            Ok(())
        };
        if let Err(e) = action() {
            eprintln!("task dispatcher: {}", e.code);
        }
    }
    fn finish_task(&self, id: &str, result: Result<()>) -> Result<()> {
        let mut s = self.state()?;
        let active = s.task_active.remove(id);
        let mut t = s.store.task(id)?;
        if task_terminal(&t) {
            // Publication and task completion may commit together. A later
            // cancellation must not relabel an already published package.
        } else if active.is_some_and(|a| a.cancelled.load(Ordering::Acquire)) {
            t.state = pb::TaskState::Cancelled as i32;
        } else if let Err(e) = result {
            t.state = pb::TaskState::Failed as i32;
            t.error = Some(e.envelope());
        } else if !task_terminal(&t) {
            t.state = pb::TaskState::Succeeded as i32;
        }
        t.finished_unix_ms = now_ms();
        s.store.update_task(&t)?;
        self.inner.changed.notify_all();
        Ok(())
    }
    fn progress(
        &self,
        id: &str,
        phase: &str,
        completed: u64,
        total: u64,
        message: &str,
    ) -> Result<()> {
        let mut s = self.state()?;
        let mut t = s.store.task(id)?;
        t.progress = Some(pb::TaskProgress {
            phase: phase.into(),
            completed,
            total,
            message: message.into(),
            unit: if matches!(phase, "download" | "import" | "hash") {
                "bytes"
            } else {
                "items"
            }
            .into(),
        });
        s.store.update_task(&t)
    }
    fn run_task(&self, task: &pb::Task, cancel: &AtomicBool) -> Result<()> {
        let input = self.state()?.store.task_input(&task.id)?;
        match input.input.as_ref() {
            Some(pb::task_input::Input::Basemap(b)) => self.run_basemap(task, b, cancel),
            Some(pb::task_input::Input::RoadPackage(p)) => self.run_package(task, p, cancel),
            Some(pb::task_input::Input::GeoPlan(r))
                if task.kind() == pb::TaskKind::RoadPackagePrepare =>
            {
                let request = input
                    .frozen_request
                    .as_ref()
                    .ok_or_else(|| AppError::invalid("missing frozen input"))?;
                self.ensure_packages(task, request, cancel).map(|_| ())
            }
            Some(pb::task_input::Input::GeoPlan(r)) => self.run_plan(task, r, &input, cancel),
            Some(pb::task_input::Input::Matrix(m)) => self.run_matrix(task, m, cancel).map(|_| ()),
            Some(pb::task_input::Input::Geometry(g)) => self.run_geometry(task, g, cancel),
            _ => Err(AppError::invalid("unsupported task input")),
        }
    }
    fn acquire_geo(&self, id: &str, cancel: &AtomicBool) -> Result<()> {
        let memory = self.inner.config.geo.worker_memory_bytes;
        self.acquire_task_memory(id, cancel, memory)
    }
    fn acquire_task_memory(&self, id: &str, cancel: &AtomicBool, memory: u64) -> Result<()> {
        if memory == 0
            || memory
                > self
                    .inner
                    .config
                    .memory_budget_bytes
                    .saturating_sub(self.inner.config.host_reserve_bytes)
        {
            return Err(AppError::new(
                "capacity",
                "configured geo worker memory does not fit server memory budget",
            ));
        }
        loop {
            if cancel.load(Ordering::Acquire) || self.is_stopping() {
                return Err(AppError::new("cancelled", "task cancelled"));
            }
            let mut s = self.state()?;
            let used = self.reserved_memory(&s);
            let basemap = s.store.task(id)?.kind() == pb::TaskKind::BasemapPrepare;
            let geo_running = s.task_active.iter().any(|(k, v)| {
                k != id
                    && v.threads > 0
                    && s.store
                        .task(k)
                        .is_ok_and(|t| (t.kind() == pb::TaskKind::BasemapPrepare) == basemap)
            });
            if !geo_running
                && s.reserved() < self.inner.config.budget.total_threads
                && used.saturating_add(memory)
                    <= self
                        .inner
                        .config
                        .memory_budget_bytes
                        .saturating_sub(self.inner.config.host_reserve_bytes)
            {
                if let Some(a) = s.task_active.get_mut(id) {
                    a.threads = 1;
                    a.memory = memory;
                }
                let mut task = s.store.task(id)?;
                task.resource_request = Some(pb::TaskResources {
                    cpu_threads: 1,
                    memory_bytes: memory,
                    io_slots: u32::from(basemap),
                    class: if basemap { "basemap" } else { "geo" }.into(),
                });
                s.store.update_task(&task)?;
                return Ok(());
            }
            drop(s);
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    fn geo_lease<'a>(&'a self, id: &'a str, cancel: &AtomicBool) -> Result<GeoLease<'a>> {
        self.acquire_geo(id, cancel)?;
        Ok(GeoLease { service: self, id })
    }
    fn release_geo(&self, id: &str) {
        if let Ok(mut s) = self.state()
            && let Some(a) = s.task_active.get_mut(id)
        {
            a.threads = 0;
            a.memory = 0;
        }
        self.inner.changed.notify_all();
    }
    pub(super) fn reserved_memory(&self, s: &State) -> u64 {
        let per = self.inner.config.isolation.memory_limit_bytes.unwrap_or(0)
            + 4 * u64::from(self.inner.config.isolation.max_frame_bytes);
        per * s.active.len() as u64 + s.task_active.values().map(|v| v.memory).sum::<u64>()
    }
    fn artifact(&self, task: &str, id: &str, kind: &str, path: &Path) -> Result<pb::TaskArtifact> {
        let mut f = File::open(path)?;
        let mut h = Sha256::new();
        let mut b = [0; 65536];
        let mut size = 0;
        loop {
            let n = f.read(&mut b)?;
            if n == 0 {
                break;
            }
            h.update(&b[..n]);
            size += n as u64;
        }
        let a = pb::TaskArtifact {
            id: id.into(),
            task_id: task.into(),
            kind: kind.into(),
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
            media_type: if path.extension().is_some_and(|e| e == "json") {
                "application/json"
            } else {
                "application/protobuf"
            }
            .into(),
            size_bytes: size,
            sha256: format!("{:x}", h.finalize()),
            href: format!("/v1/tasks/{task}/artifacts/{id}"),
        };
        self.state()?.store.put_artifact(&a, path)?;
        Ok(a)
    }
}
