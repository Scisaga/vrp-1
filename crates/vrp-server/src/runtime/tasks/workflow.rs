use super::*;
use vrp_proto::worker::v1 as worker;

impl Service {
    fn begin_child(
        &self,
        parent: &pb::Task,
        kind: pb::TaskKind,
        input: pb::TaskInput,
        packages: &[String],
    ) -> Result<(pb::Task, Arc<AtomicBool>)> {
        let mut s = self.state()?;
        let mut t = new_task(kind, parent.scenario.clone(), &parent.id, "");
        t.state = pb::TaskState::Running as i32;
        t.started_unix_ms = now_ms();
        t.road_package_ids = packages.to_vec();
        let t = s.store.insert_task(t, &input)?;
        let flag = Arc::new(AtomicBool::new(false));
        s.task_active.insert(
            t.id.clone(),
            TaskActive {
                threads: 0,
                memory: 0,
                cancelled: flag.clone(),
            },
        );
        Ok((t, flag))
    }
    pub(super) fn run_plan(
        &self,
        task: &pb::Task,
        r: &pb::GeoPlanRequest,
        input: &pb::TaskInput,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let source = input
            .frozen_request
            .as_ref()
            .ok_or_else(|| AppError::invalid("missing frozen scene"))?;
        let (saved_version, network, packages) = if task.result_scenario_version > 0 {
            // The scene-CAS and this checkpoint were committed atomically. A retry
            // resumes the saved version, never writes another business version.
            let mut owner = task.source_task_id.clone();
            let mut network = None;
            for _ in 0..64 {
                if owner.is_empty() {
                    break;
                }
                let s = self.state()?;
                for child in s
                    .store
                    .tasks()?
                    .iter()
                    .filter(|t| t.parent_id == owner && t.kind() == pb::TaskKind::MatrixPrepare)
                {
                    if let Ok(path) = s.store.artifact_path(&child.id, "network") {
                        network = Some(path);
                        break;
                    }
                }
                if network.is_some() {
                    break;
                }
                owner = s.store.task(&owner)?.source_task_id;
            }
            (
                task.result_scenario_version,
                network.ok_or_else(|| AppError::conflict("frozen road snapshot is unavailable"))?,
                task.road_package_ids.clone(),
            )
        } else {
            self.progress(
                &task.id,
                "road_coverage",
                0,
                0,
                "Checking cached regional road data",
            )?;
            let packages = self.ensure_packages(task, source, cancel)?;
            if cancel.load(Ordering::Acquire) {
                return Err(AppError::new("cancelled", "planning cancelled"));
            }
            let matrix = pb::MatrixPrepareInput {
                request: Some(source.clone()),
                road_package_ids: packages.clone(),
                connector_speed_kmh: r.connector_speed_kmh,
                buffer_km: r.buffer_km,
                network_artifact: String::new(),
            };
            let (child, flag) = self.begin_child(
                task,
                pb::TaskKind::MatrixPrepare,
                pb::TaskInput {
                    input: Some(pb::task_input::Input::Matrix(Box::new(matrix.clone()))),
                    ..Default::default()
                },
                &packages,
            )?;
            let result = self.run_matrix(&child, &matrix, &flag);
            self.finish_task(&child.id, result.as_ref().map(|_| ()).map_err(Clone::clone))?;
            let (prepared, network) = result?;
            if cancel.load(Ordering::Acquire) {
                return Err(AppError::new(
                    "cancelled",
                    "planning cancelled before scene commit",
                ));
            }
            self.progress(
                &task.id,
                "saving_matrix",
                0,
                0,
                "Saving immutable matrix scenario version",
            )?;
            let saved = {
                let mut s = self.state()?;
                let (meta, _) = s.store.scenario(&r.scenario_id, 0)?;
                if meta.version != r.expected_version {
                    return Err(AppError::conflict(
                        "scenario changed during road preparation; matrix was retained as an artifact",
                    ));
                }
                if s.store.operation(&r.scenario_id, 0)?.is_some() {
                    return Err(AppError::conflict("execution started during preparation"));
                }
                let warnings = s.store.task(&child.id)?.warnings;
                let value = pb::SaveScenarioRequest {
                    scenario_id: r.scenario_id.clone(),
                    name: meta.name,
                    expected_version: r.expected_version,
                    request: Some(prepared.clone()),
                    display: meta.display,
                    origin: meta.origin,
                };
                s.store.atomic(|store| {
                    let saved = store.save_scenario(&value, &prepared.encode_to_vec())?;
                    // This internal CAS already prepared roads; only business
                    // saves should start automatic prefetch work.
                    store.finish_geo_pending(&saved.id, saved.version)?;
                    let mut t = store.task(&task.id)?;
                    t.result_scenario_version = saved.version;
                    t.road_package_ids = packages.clone();
                    t.warnings.extend(warnings);
                    t.dependency_ids.push(child.id.clone());
                    store.update_task(&t)?;
                    Ok(saved)
                })?
            };
            (saved.version, network, packages)
        };
        let attempts = self
            .state()?
            .store
            .workflow_attempts(&task.id, saved_version)?;
        let mut origin = attempts.last().cloned().unwrap_or_else(|| task.id.clone());
        let old_job = if task.result_job_id.is_empty() {
            None
        } else {
            self.get_job(&task.result_job_id).ok()
        };
        if old_job
            .as_ref()
            .is_some_and(|j| terminal(j) && !j.has_result)
        {
            origin = task.id.clone();
        }
        // A process may stop after submit and before the parent checkpoint.
        // Search the entire retry lineage, including fresh retry jobs created
        // after an earlier solver failed, before issuing another solve.
        let recovered = {
            let s = self.state()?;
            let mut recovered = None;
            for attempt in &attempts {
                if let Some(job) = s.store.job_for_key(&format!("geo-plan:{attempt}:solve"))?
                    && job.scenario.as_ref().is_some_and(|v| {
                        v.scenario_id == r.scenario_id && v.version == saved_version
                    })
                {
                    if !terminal(&job) || job.has_result {
                        recovered = Some(job);
                        break;
                    }
                    // A solver can fail before its parent records its ID.
                    // A subsequent retry needs a fresh key in that case too.
                    origin = task.id.clone();
                }
            }
            recovered
        };
        let job = if let Some(job) =
            recovered.or_else(|| old_job.filter(|j| !terminal(j) || j.has_result))
        {
            job
        } else {
            self.submit(pb::SubmitJobRequest {
                idempotency_key: format!("geo-plan:{origin}:solve"),
                source: Some(pb::submit_job_request::Source::Scenario(pb::ScenarioRef {
                    scenario_id: r.scenario_id.clone(),
                    version: saved_version,
                })),
                options: r.options,
                kind: pb::JobKind::Planning as i32,
            })?
        };
        {
            let mut s = self.state()?;
            if s.store.solver_parent(&job.id)?.is_none() {
                s.store.link_solver(&job.id, &task.id)?;
            }
            let mut t = s.store.task(&task.id)?;
            t.result_job_id = job.id.clone();
            if !t.dependency_ids.contains(&job.id) {
                t.dependency_ids.push(job.id.clone());
            }
            s.store.update_task(&t)?;
        }
        self.progress(
            &task.id,
            "solving",
            0,
            0,
            "Matrices ready; waiting for solver result",
        )?;
        loop {
            if cancel.load(Ordering::Acquire) || self.is_stopping() {
                let _ = self.cancel(&job.id);
                return Err(AppError::new("cancelled", "planning cancelled"));
            }
            let job = self.get_job(&job.id)?;
            if terminal(&job) {
                if !job.has_result {
                    return Err(AppError::new(
                        "solve_failed",
                        job.error.map_or_else(
                            || "solver did not produce a result".into(),
                            |e| e.message,
                        ),
                    ));
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let geometry = pb::GeometryGenerateInput {
            job_id: job.id.clone(),
            network_artifact: network.to_string_lossy().into(),
            road_package_ids: packages.clone(),
        };
        let (child, flag) = self.begin_child(
            task,
            pb::TaskKind::GeometryGenerate,
            pb::TaskInput {
                input: Some(pb::task_input::Input::Geometry(geometry.clone())),
                ..Default::default()
            },
            &packages,
        )?;
        self.progress(
            &task.id,
            "geometry",
            0,
            0,
            "Generating paths from the frozen road snapshot",
        )?;
        let result = self.run_geometry(&child, &geometry, &flag);
        self.finish_task(&child.id, result.clone())?;
        let mut s = self.state()?;
        let mut t = s.store.task(&task.id)?;
        t.dependency_ids.push(child.id.clone());
        if let Err(e) = result {
            t.state = pb::TaskState::Partial as i32;
            t.warnings.push(format!(
                "Plan is ready; road geometry could not be generated: {}",
                e.message
            ));
        }
        t.progress = Some(pb::TaskProgress {
            phase: "completed".into(),
            message: "Plan is ready".into(),
            ..Default::default()
        });
        s.store.update_task(&t)?;
        Ok(())
    }
    pub(super) fn run_matrix(
        &self,
        task: &pb::Task,
        m: &pb::MatrixPrepareInput,
        cancel: &AtomicBool,
    ) -> Result<(engine::SolveRequest, PathBuf)> {
        fs::create_dir_all(self.task_dir(&task.id))?;
        let _lease = self.geo_lease(&task.id, cancel)?;
        let output = self.task_dir(&task.id).join("request.pb");
        let network = self.task_dir(&task.id).join("network.json");
        let paths = {
            let s = self.state()?;
            m.road_package_ids
                .iter()
                .map(|id| {
                    s.store
                        .road_path(id)
                        .map(|p| p.to_string_lossy().into_owned())
                })
                .collect::<Result<Vec<_>>>()?
        };
        let cached_network = {
            let state = self.state()?;
            state
                .store
                .tasks()?
                .into_iter()
                .filter(|t| {
                    t.id != task.id
                        && t.kind() == pb::TaskKind::MatrixPrepare
                        && t.state() == pb::TaskState::Succeeded
                })
                .find_map(|t| {
                    let old = state.store.task_input(&t.id).ok()?;
                    let Some(pb::task_input::Input::Matrix(old)) = old.input else {
                        return None;
                    };
                    let mut old_packages = old.road_package_ids.clone();
                    old_packages.sort();
                    let mut packages = m.road_package_ids.clone();
                    packages.sort();
                    if old_packages != packages
                        || old.connector_speed_kmh != m.connector_speed_kmh
                        || old.buffer_km != m.buffer_km
                        || coordinate_hash(old.request.as_ref()?)
                            != coordinate_hash(m.request.as_ref()?)
                    {
                        return None;
                    }
                    state
                        .store
                        .artifact_path(&t.id, "network")
                        .ok()
                        .filter(|p| p.exists())
                })
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let location_covered = {
            let state = self.state()?;
            let packages = m
                .road_package_ids
                .iter()
                .map(|id| state.store.road_package(id))
                .collect::<Result<Vec<_>>>()?;
            points(
                m.request
                    .as_ref()
                    .ok_or_else(|| AppError::invalid("matrix request required"))?,
            )?
            .into_iter()
            .map(|point| {
                packages.iter().any(|p| {
                    if !p.coverage_geojson.is_empty() {
                        serde_json::from_str(&p.coverage_geojson)
                            .ok()
                            .and_then(|geometry| {
                                vrp_geo::catalog::coverage_contains_point(&geometry, point).ok()
                            })
                            .unwrap_or(false)
                    } else {
                        p.bounds.as_ref().is_some_and(|b| {
                            Bounds {
                                south: b.south,
                                west: b.west,
                                north: b.north,
                                east: b.east,
                            }
                            .contains(point)
                        })
                    }
                })
            })
            .collect()
        };
        let mut request = worker::GeoRunRequest {
            kind: worker::GeoTaskKind::BuildMatrix as i32,
            input: m.request.clone(),
            pbf_paths: paths,
            network_path: network.to_string_lossy().into(),
            output_path: output.to_string_lossy().into(),
            connector_speed_kph: m.connector_speed_kmh,
            buffer_km: m.buffer_km,
            threads: 1,
            cached_network_path: cached_network,
            location_covered,
            ..Default::default()
        };
        let mut result = self.geo_runner(&task.id, &request, cancel);
        let mut road_failure = None;
        if result.is_err() && !request.pbf_paths.is_empty() && !cancel.load(Ordering::Acquire) {
            road_failure = result.as_ref().err().map(|error| {
                format!(
                    "Road computation failed ({}): {}; prepared explicit geometric estimates",
                    error.code, error.message
                )
            });
            self.progress(
                &task.id,
                "geometric_fallback",
                0,
                0,
                "Road worker failed; preparing explicit geometric estimates",
            )?;
            request.pbf_paths.clear();
            request.cached_network_path.clear();
            request.network_path = self
                .task_dir(&task.id)
                .join("fallback-network.json")
                .to_string_lossy()
                .into();
            result = self.geo_runner(&task.id, &request, cancel);
        }
        let mut response = result?;
        if let Some(warning) = road_failure {
            response.warnings.insert(0, warning);
        }
        self.validate_output(&response, &output)?;
        let network = PathBuf::from(&response.network_path);
        let prepared = engine::SolveRequest::decode(fs::read(&output)?.as_slice())?;
        vrp_model::validate_ready_matrices(&prepared).map_err(geo_error)?;
        self.artifact(&task.id, "request", "solve_input", &output)?;
        let graph_artifact = if network.exists() {
            Some(self.artifact(&task.id, "network", "road_snapshot", &network)?)
        } else {
            None
        };
        let report = self.task_dir(&task.id).join("matrix-quality.json");
        let prepared_report = pb::MatrixPreparationReport {
            coordinate_hash: coordinate_hash(&prepared),
            model_edges: response.model_edges,
            fallback_edges: response.fallback_edges,
            location_count: prepared
                .problem
                .as_ref()
                .map_or(0, |p| p.locations.len() as u64),
            ready: true,
            scene_bounds: Bounds::from_points(&points(&prepared)?)
                .ok()
                .map(|b| pb::GeoBounds {
                    south: b.south,
                    west: b.west,
                    north: b.north,
                    east: b.east,
                }),
            routing_bounds: bounds(
                &prepared,
                if response.effective_buffer_km > 0.0 {
                    response.effective_buffer_km
                } else if m.buffer_km > 0.0 {
                    m.buffer_km
                } else {
                    10.0
                },
            )
            .ok()
            .map(|b| pb::GeoBounds {
                south: b.south,
                west: b.west,
                north: b.north,
                east: b.east,
            }),
            road_package_ids: m.road_package_ids.clone(),
            connector_speed_kmh: if m.connector_speed_kmh > 0.0 {
                m.connector_speed_kmh
            } else {
                15.0
            },
            graph_sha256: graph_artifact.map_or_else(String::new, |a| a.sha256),
            warnings: response.warnings.clone(),
        };
        fs::write(
            &report,
            serde_json::to_vec_pretty(&prepared_report).map_err(geo_error)?,
        )?;
        self.artifact(&task.id, "quality", "matrix_quality", &report)?;
        let mut s = self.state()?;
        let mut t = s.store.task(&task.id)?;
        t.warnings = response.warnings;
        s.store.update_task(&t)?;
        Ok((prepared, network))
    }
    pub(super) fn run_geometry(
        &self,
        task: &pb::Task,
        g: &pb::GeometryGenerateInput,
        cancel: &AtomicBool,
    ) -> Result<()> {
        fs::create_dir_all(self.task_dir(&task.id))?;
        let input = engine::SolveRequest::decode(self.payload(&g.job_id, false)?.as_slice())?;
        let result = engine::SolveResult::decode(self.payload(&g.job_id, true)?.as_slice())?;
        let _lease = self.geo_lease(&task.id, cancel)?;
        let output = self.task_dir(&task.id).join("geometry.pb");
        let request = worker::GeoRunRequest {
            kind: worker::GeoTaskKind::BuildGeometry as i32,
            input: Some(input),
            result: Some(result),
            network_path: g.network_artifact.clone(),
            output_path: output.to_string_lossy().into(),
            threads: 1,
            job_id: g.job_id.clone(),
            ..Default::default()
        };
        let result = self.geo_runner(&task.id, &request, cancel);
        let response = result?;
        self.validate_output(&response, &output)?;
        let geometry = pb::JobGeometry::decode(fs::read(&output)?.as_slice())?;
        self.save_geometry(geometry)?;
        self.artifact(&task.id, "geometry", "job_geometry", &output)?;
        Ok(())
    }
    pub(super) fn geo_runner(
        &self,
        id: &str,
        request: &worker::GeoRunRequest,
        cancel: &AtomicBool,
    ) -> Result<worker::GeoRunResponse> {
        let mut options = self.inner.config.isolation.clone();
        options.memory_limit_bytes = Some(self.inner.config.geo.worker_memory_bytes);
        options.setup_timeout = Duration::from_secs(900);
        let args = vec![
            "geo-child".into(),
            options.max_frame_bytes.to_string().into(),
        ];
        let runner = vrp_worker::ProcessRunner::new(self.inner.program.clone(), args, options)
            .map_err(geo_error)?;
        runner
            .run_reserved(request, cancel, |p| {
                let _ = self.progress(id, &p.phase, p.completed, p.total, "");
            })
            .map_err(geo_error)
    }
    pub(super) fn validate_output(
        &self,
        response: &worker::GeoRunResponse,
        path: &Path,
    ) -> Result<()> {
        if Path::new(&response.output_path) != path {
            return Err(AppError::internal(
                "worker returned an unexpected artifact path",
            ));
        }
        let meta = path.metadata()?;
        if meta.len() != response.output_size || meta.len() > self.request_limit() as u64 {
            return Err(AppError::new(
                "too_large",
                "worker artifact size exceeds limit or disagrees with manifest",
            ));
        }
        if Sha256::digest(fs::read(path)?).as_slice() != response.output_sha256 {
            return Err(AppError::internal("worker artifact checksum mismatch"));
        }
        Ok(())
    }
}

struct ChildObserver<'a> {
    cancel: &'a AtomicBool,
    progress: &'a mut dyn FnMut(&str, u64, u64),
}
impl vrp_geo::Observer for ChildObserver<'_> {
    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
    fn progress(&mut self, p: vrp_geo::Progress) {
        (self.progress)(&p.phase, p.completed, p.total.unwrap_or(0));
    }
}

pub(crate) fn execute_geo(
    request: worker::GeoRunRequest,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, u64, u64),
) -> std::result::Result<worker::GeoRunResponse, worker::ChildFailure> {
    let run = || -> Result<worker::GeoRunResponse> {
        if request.kind == worker::GeoTaskKind::ValidatePackage as i32 {
            let mut observer = ChildObserver { cancel, progress };
            let path = request
                .pbf_paths
                .first()
                .ok_or_else(|| AppError::invalid("PBF path required"))?;
            vrp_geo::validate_pbf(path, &mut observer).map_err(geo_error)?;
            let info = vrp_geo::inspect_pbf(path).map_err(geo_error)?;
            let metadata = pb::RoadPackage {
                bounds: info.bounds.map(|b| pb::GeoBounds {
                    south: b.south,
                    west: b.west,
                    north: b.north,
                    east: b.east,
                }),
                ..Default::default()
            };
            let bytes = metadata.encode_to_vec();
            fs::write(&request.output_path, &bytes)?;
            return Ok(worker::GeoRunResponse {
                output_path: request.output_path,
                output_size: bytes.len() as u64,
                output_sha256: Sha256::digest(bytes).to_vec(),
                ..Default::default()
            });
        }
        let mut input = request
            .input
            .ok_or_else(|| AppError::invalid("geo input required"))?;
        let pts = points(&input)?;
        let mut observer = ChildObserver { cancel, progress };
        let output = PathBuf::from(&request.output_path);
        if let Some(p) = output.parent() {
            fs::create_dir_all(p)?;
        }
        let mut warnings = vec![];
        let mut effective_buffer = if request.buffer_km > 0.0 {
            request.buffer_km
        } else {
            10.0
        };
        if !request.location_covered.is_empty() && request.location_covered.len() != pts.len() {
            return Err(AppError::invalid(
                "location coverage mask has the wrong length",
            ));
        }
        match worker::GeoTaskKind::try_from(request.kind).map_err(geo_error)? {
            worker::GeoTaskKind::BuildMatrix => {
                let speed = if request.connector_speed_kph == 0.0 {
                    15.0
                } else {
                    request.connector_speed_kph
                };
                let config = vrp_geo::Config {
                    connector_speed_kph: speed,
                    ..Default::default()
                };
                let area = Bounds::from_points(&pts)
                    .and_then(|b| {
                        b.expanded_km(if request.buffer_km == 0.0 {
                            10.0
                        } else {
                            request.buffer_km
                        })
                    })
                    .map_err(geo_error)?;
                let cached = if !request.cached_network_path.is_empty() {
                    match vrp_geo::RoadNetwork::load_observed(
                        &request.cached_network_path,
                        &mut observer,
                    ) {
                        Ok(network)
                            if network.config == config
                                && network
                                    .sources
                                    .iter()
                                    .map(|s| s.sha256.as_str())
                                    .collect::<BTreeSet<_>>()
                                    == request
                                        .pbf_paths
                                        .iter()
                                        .filter_map(|p| {
                                            Path::new(p)
                                                .file_name()
                                                .and_then(|v| v.to_str())
                                                .and_then(|v| v.strip_suffix(".osm.pbf"))
                                        })
                                        .collect::<BTreeSet<_>>() =>
                        {
                            (observer.progress)("graph_cache_hit", 1, 1);
                            Some(network)
                        }
                        Ok(_) => {
                            warnings.push("Cached graph parameters or package hashes differ; rebuilt from source".into());
                            None
                        }
                        Err(e) => {
                            warnings.push(format!("Cached graph was rejected and rebuilt: {e}"));
                            None
                        }
                    }
                } else {
                    None
                };
                let mut network = if let Some(network) = cached {
                    network
                } else if request.pbf_paths.is_empty() {
                    vrp_geo::RoadNetwork::from_ways(vec![], config.clone()).map_err(geo_error)?
                } else {
                    match vrp_geo::RoadNetwork::from_pbf(
                        &request
                            .pbf_paths
                            .iter()
                            .map(PathBuf::from)
                            .collect::<Vec<_>>(),
                        area,
                        config.clone(),
                        &mut observer,
                    ) {
                        Ok(n) => n,
                        Err(e) => {
                            if cancel.load(Ordering::Acquire) {
                                return Err(geo_error(e));
                            }
                            warnings.push(format!(
                                "Road graph unavailable; missing edges use geometric fallback: {e}"
                            ));
                            vrp_geo::RoadNetwork::from_ways(vec![], config.clone())
                                .map_err(geo_error)?
                        }
                    }
                };
                let mut matrix = network.matrix(&pts, &mut observer).map_err(geo_error)?;
                if let Some(area) = network.bounds {
                    let base = Bounds::from_points(&pts).map_err(geo_error)?;
                    effective_buffer = ((base.south - area.south).max(area.north - base.north))
                        .to_radians()
                        * 6371.0088;
                }
                if !request.pbf_paths.is_empty()
                    && network.boundary_limited()
                    && effective_buffer < 29.999
                    && matrix.entries.iter().any(Option::is_none)
                {
                    (observer.progress)("boundary_retry", 0, 1);
                    let wider = Bounds::from_points(&pts)
                        .and_then(|b| b.expanded_km(30.0))
                        .map_err(geo_error)?;
                    match vrp_geo::RoadNetwork::from_pbf(
                        &request
                            .pbf_paths
                            .iter()
                            .map(PathBuf::from)
                            .collect::<Vec<_>>(),
                        wider,
                        config,
                        &mut observer,
                    ) {
                        Ok(candidate) => {
                            matrix = candidate.matrix(&pts, &mut observer).map_err(geo_error)?;
                            network = candidate;
                            effective_buffer = 30.0;
                        }
                        Err(e) => {
                            if cancel.load(Ordering::Acquire) {
                                return Err(geo_error(e));
                            }
                            warnings.push(format!(
                                "30 km boundary retry failed; retained the original graph: {e}"
                            ));
                        }
                    }
                }
                if let Some(area) = network.bounds {
                    let base = Bounds::from_points(&pts).map_err(geo_error)?;
                    effective_buffer = ((base.south - area.south).max(area.north - base.north))
                        .to_radians()
                        * 6371.0088;
                }
                network
                    .save_observed(&request.network_path, &mut observer)
                    .map_err(geo_error)?;
                let p = input.problem.as_mut().unwrap();
                let n = pts.len();
                let profiles = if p.matrices.is_empty() {
                    vec!["car".into()]
                } else {
                    p.matrices.iter().map(|m| m.profile.clone()).collect()
                };
                let old = p.matrices.clone();
                p.matrices = profiles
                    .into_iter()
                    .enumerate()
                    .map(|(mi, profile)| {
                        let mut durations = Vec::with_capacity(n * n * 4);
                        let mut distances = Vec::with_capacity(n * n * 4);
                        let mut sources = vec![engine::EdgeSource::Model as u8; n * n];
                        for (k, entry) in matrix.entries.iter().enumerate() {
                            let was_unreachable = old
                                .get(mi)
                                .and_then(|m| m.durations.get(k * 4..k * 4 + 4))
                                .is_some_and(|v| v == u32::MAX.to_le_bytes());
                            let (t, d) = if k / n == k % n {
                                (0, 0)
                            } else if was_unreachable {
                                (u32::MAX, u32::MAX)
                            } else if let Some(v) = entry.as_ref().filter(|_| {
                                request.location_covered.is_empty()
                                    || (request.location_covered[k / n]
                                        && request.location_covered[k % n])
                            }) {
                                (v.duration_s, v.distance_m)
                            } else {
                                sources[k] = engine::EdgeSource::Fallback as u8;
                                (u32::MAX - 1, u32::MAX - 1)
                            };
                            durations.extend_from_slice(&t.to_le_bytes());
                            distances.extend_from_slice(&d.to_le_bytes());
                        }
                        engine::TravelMatrix {
                            profile,
                            durations,
                            distances,
                            sources,
                        }
                    })
                    .collect();
                let stats = vrp_model::prepare_matrices(&mut input).map_err(geo_error)?;
                if stats.fallback > 0 {
                    let policy = if input.problem.as_ref().is_some_and(|p| p.fallback.is_some()) {
                        "configured-geometric-v1; parameters retained in the frozen request"
                    } else {
                        "auto-geometric-v1; local Manhattan <=30km, Haversine x1.3 above 30km, speed-bands-v1"
                    };
                    warnings.push(format!(
                        "{} matrix entries use explicit geometric estimates ({policy})",
                        stats.fallback
                    ));
                }
                fs::write(&output, input.encode_to_vec())?;
            }
            worker::GeoTaskKind::BuildGeometry => {
                let network =
                    vrp_geo::RoadNetwork::load_observed(&request.network_path, &mut observer)
                        .map_err(geo_error)?;
                let graph = network.prepare(&pts, &mut observer).map_err(geo_error)?;
                let p = input.problem.as_ref().unwrap();
                let result = request
                    .result
                    .ok_or_else(|| AppError::invalid("result required"))?;
                let mut legs = vec![];
                let mut count = 0;
                for route in result
                    .solution
                    .as_ref()
                    .ok_or_else(|| AppError::invalid("solution required"))?
                    .routes
                    .iter()
                {
                    let agent = &p.agents[route.agent as usize];
                    let mut locations = vec![agent.start_location];
                    locations.extend(
                        route
                            .tickets
                            .iter()
                            .map(|i| p.tickets[*i as usize].location),
                    );
                    locations.push(agent.end_location);
                    for (index, pair) in locations.windows(2).enumerate() {
                        let cell = pair[0] as usize * p.locations.len() + pair[1] as usize;
                        if p.matrices
                            .get(agent.matrix as usize)
                            .and_then(|m| m.sources.get(cell))
                            .copied()
                            != Some(engine::EdgeSource::Model as u8)
                        {
                            continue;
                        }
                        let matrix = &p.matrices[agent.matrix as usize];
                        let expected_time = u32::from_le_bytes(
                            matrix.durations[cell * 4..cell * 4 + 4]
                                .try_into()
                                .map_err(geo_error)?,
                        );
                        let expected_distance = u32::from_le_bytes(
                            matrix.distances[cell * 4..cell * 4 + 4]
                                .try_into()
                                .map_err(geo_error)?,
                        );
                        if expected_time == u32::MAX {
                            continue;
                        }
                        let Some(route) = graph
                            .route(pair[0] as usize, pair[1] as usize, &mut observer)
                            .map_err(geo_error)?
                        else {
                            continue;
                        };
                        if route.duration_s != expected_time
                            || route.distance_m != expected_distance
                        {
                            return Err(AppError::conflict(
                                "road snapshot route does not match the frozen MODEL matrix",
                            ));
                        }
                        if route.points.len() < 2 || count + route.points.len() > 250_000 {
                            warnings.push(
                                "Some geometry was omitted to respect the display point limit"
                                    .into(),
                            );
                            continue;
                        }
                        count += route.points.len();
                        legs.push(pb::RouteGeometry {
                            agent_id: agent.id.clone(),
                            leg_index: index as u32,
                            from_location_id: p.locations[pair[0] as usize].id.clone(),
                            to_location_id: p.locations[pair[1] as usize].id.clone(),
                            points: route
                                .points
                                .into_iter()
                                .map(|p| pb::GeoPoint {
                                    lng: p.lng,
                                    lat: p.lat,
                                })
                                .collect(),
                            road_start_point_index: Some(route.road_start as u32),
                            road_end_point_index: Some(route.road_end as u32),
                        });
                    }
                }
                fs::write(
                    &output,
                    pb::JobGeometry {
                        job_id: request.job_id,
                        coordinate_system: "WGS84".into(),
                        provider: format!("osm-nearest-road-v1:{}", network.fingerprint),
                        legs,
                    }
                    .encode_to_vec(),
                )?;
            }
            worker::GeoTaskKind::ValidatePackage => {
                unreachable!("package validation handled before solve input")
            }
        }
        let stats = vrp_model::compile(&input).map_err(geo_error)?.stats;
        let bytes = fs::read(&output)?;
        Ok(worker::GeoRunResponse {
            output_path: request.output_path,
            network_path: request.network_path,
            output_size: bytes.len() as u64,
            output_sha256: Sha256::digest(bytes).to_vec(),
            model_edges: stats.model as u64,
            fallback_edges: stats.fallback as u64,
            warnings,
            effective_buffer_km: effective_buffer,
        })
    };
    run().map_err(|e| worker::ChildFailure {
        code: e.code.into(),
        message: e.message,
    })
}
