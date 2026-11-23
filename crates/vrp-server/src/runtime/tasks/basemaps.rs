use super::*;
use vrp_proto::{maps::v1 as maps, worker::v1 as worker};

pub(crate) fn asset_digest(path: &str) -> String {
    digest(path.as_bytes())
}

#[derive(Clone, Debug)]
pub struct BasemapConfig {
    pub automatic: bool,
    pub catalog_url: String,
    pub archive_base_url: String,
    pub download_budget_bytes: u64,
    pub disk_budget_bytes: u64,
    pub worker_memory_bytes: u64,
}
impl Default for BasemapConfig {
    fn default() -> Self {
        Self {
            automatic: true,
            catalog_url: "https://build-metadata.protomaps.dev/builds.json".into(),
            archive_base_url: "https://build.protomaps.com".into(),
            download_budget_bytes: 500 * 1024 * 1024,
            disk_budget_bytes: 10 * 1024 * 1024 * 1024,
            worker_memory_bytes: 512 * 1024 * 1024,
        }
    }
}
impl Service {
    pub(crate) fn recover_basemap_files(&self) -> Result<()> {
        let root = self.basemap_dir();
        let registered: std::collections::HashSet<_> = self
            .state()?
            .store
            .basemaps()?
            .into_iter()
            .map(|p| format!("{}.pmtiles", p.id))
            .collect();
        if root.is_dir() {
            let canonical = root.canonicalize()?;
            for entry in fs::read_dir(&root)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.ends_with(".pmtiles")
                    && !registered.contains(&name)
                    && entry.file_type()?.is_file()
                {
                    let path = entry.path().canonicalize()?;
                    if path.starts_with(&canonical) {
                        fs::remove_file(path)?;
                    }
                }
            }
        }
        // Task directories are server-created; only remove the known temporary
        // basemap filename, never road or business artifacts.
        for task in self.state()?.store.tasks()? {
            if task.kind() == pb::TaskKind::BasemapPrepare && task_terminal(&task) {
                let temp = self.task_dir(&task.id).join("basemap.partial");
                if temp.is_file() {
                    fs::remove_file(temp)?;
                }
            }
        }
        Ok(())
    }
    fn basemap_dir(&self) -> PathBuf {
        self.inner.config.data_dir.join("basemap-packages")
    }
    fn basemap_context(
        &self,
        mut context: maps::BasemapContext,
    ) -> Result<(maps::BasemapContext, String, Vec<maps::Bounds>)> {
        if [&context.scenario_id, &context.job_id, &context.operation_id]
            .iter()
            .filter(|v| !v.is_empty())
            .count()
            != 1
        {
            return Err(AppError::invalid(
                "exactly one saved basemap context is required",
            ));
        }
        let state = self.state()?;
        let (key, mut locations) = if !context.job_id.is_empty() {
            let request = engine::SolveRequest::decode(
                state
                    .store
                    .payload(job_id(&context.job_id)?, false)?
                    .as_slice(),
            )?;
            (
                format!("job:{}", context.job_id),
                request.problem.map_or_else(Vec::new, |p| p.locations),
            )
        } else if !context.scenario_id.is_empty() {
            let (scene, bytes) = state
                .store
                .scenario(&context.scenario_id, context.version)?;
            context.version = scene.version;
            let input = engine::SolveRequest::decode(bytes.as_slice())?;
            (
                format!("scene:{}:{}", context.scenario_id, context.version),
                input.problem.map_or_else(Vec::new, |p| p.locations),
            )
        } else {
            let op = state
                .store
                .operation(&context.operation_id, context.version)?
                .ok_or_else(|| AppError::new("not_found", "operation not found"))?;
            context.version = op.revision;
            let scene = op
                .scenario
                .as_ref()
                .ok_or_else(|| AppError::internal("operation has no scene"))?;
            let (_, bytes) = state.store.scenario(&scene.scenario_id, scene.version)?;
            let input = engine::SolveRequest::decode(bytes.as_slice())?;
            let mut locations = input.problem.map_or_else(Vec::new, |p| p.locations);
            locations.extend(op.vehicles.into_iter().filter_map(|v| v.position));
            (
                format!("operation:{}:{}", context.operation_id, context.version),
                locations,
            )
        };
        drop(state);
        locations.retain(|p| p.lat.is_finite() && p.lng.is_finite());
        let pts: Vec<_> = locations
            .iter()
            .map(|p| GeoPoint {
                lng: p.lng,
                lat: p.lat,
            })
            .collect();
        let area = Bounds::from_points(&pts)
            .and_then(|b| b.expanded_km(10.0))
            .map_err(geo_error)?;
        let bounds: Vec<_> = area
            .split()
            .into_iter()
            .map(|b| maps::Bounds {
                west: b.west,
                east: b.east,
                south: b.south,
                north: b.north,
            })
            .collect();
        vrp_basemap::validate_bounds(&bounds).map_err(geo_error)?;
        Ok((context, key, bounds))
    }
    pub fn list_basemaps(
        &self,
        q: maps::ListBasemapsRequest,
    ) -> Result<maps::ListBasemapsResponse> {
        let limit = page_size(q.page_size)?;
        let state = self.state()?;
        let all = state.store.basemaps()?;
        let used = all.iter().map(|p| p.size_bytes).sum();
        let mut packages = Vec::new();
        for mut p in all
            .into_iter()
            .filter(|p| q.after_id.is_empty() || p.id > q.after_id)
            .take(limit + 1)
        {
            p.references = state.store.basemap_references(&p.id)?;
            p.reference_count = p.references.len() as u64;
            packages.push(p);
        }
        let next = if packages.len() > limit {
            packages.truncate(limit);
            packages.last().unwrap().id.clone()
        } else {
            String::new()
        };
        Ok(maps::ListBasemapsResponse {
            packages,
            next_after_id: next,
            used_bytes: used,
            limit_bytes: self.inner.config.basemap.disk_budget_bytes,
        })
    }
    pub fn get_basemap(&self, id: &str) -> Result<maps::BasemapPackage> {
        let state = self.state()?;
        let (mut p, _) = state.store.basemap(id)?;
        p.references = state.store.basemap_references(id)?;
        p.reference_count = p.references.len() as u64;
        Ok(p)
    }
    pub fn basemap_resource(&self, id: &str) -> Result<(maps::BasemapPackage, PathBuf)> {
        let (p, path) = self.state()?.store.basemap(id)?;
        let root = self.basemap_dir().canonicalize()?;
        let real = path.canonicalize()?;
        if !real.starts_with(&root) || real == root {
            return Err(AppError::internal("basemap escaped managed root"));
        }
        Ok((p, real))
    }
    pub fn select_basemap(&self, context: maps::BasemapContext) -> Result<maps::BasemapSelection> {
        let config = &self.inner.config.map_config;
        if config.custom || config.disabled {
            return Ok(maps::BasemapSelection {
                context: Some(context),
                custom: config.custom,
                disabled: config.disabled,
                display_max_zoom: config.max_zoom,
                ..Default::default()
            });
        }
        let (context, key, bounds) = self.basemap_context(context)?;
        let state = self.state()?;
        let selected = state.store.basemap_binding(&key)?;
        let scenario = if !context.job_id.is_empty() {
            state.store.job(job_id(&context.job_id)?)?.scenario
        } else if !context.operation_id.is_empty() {
            state
                .store
                .operation(&context.operation_id, context.version)?
                .and_then(|op| op.scenario)
        } else {
            Some(pb::ScenarioRef {
                scenario_id: context.scenario_id.clone(),
                version: context.version,
            })
        };
        // A user-selected package can also serve a later scenario revision, a
        // result, or an execution snapshot when it still covers those bounds.
        let inherited = scenario
            .map(|reference| {
                state
                    .store
                    .scene_basemap_bindings(&reference.scenario_id, reference.version)
            })
            .transpose()?
            .unwrap_or_default();
        let all = state.store.basemaps()?;
        let usable = |p: &&maps::BasemapPackage| {
            p.enabled
                && p.assets_version == vrp_basemap::ASSETS_VERSION
                && vrp_basemap::covers(&p.bounds, &bounds)
        };
        let package = all
            .iter()
            .filter(usable)
            .find(|p| p.id == selected)
            .or_else(|| {
                inherited
                    .iter()
                    .find_map(|id| all.iter().filter(usable).find(|p| &p.id == id))
            })
            .or_else(|| {
                all.iter()
                    .filter(usable)
                    .filter(|p| p.native_max_zoom >= 14 || p.format != "mvt")
                    .min_by_key(|p| p.size_bytes)
            })
            .cloned();
        let task=state.store.tasks()?.into_iter().find(|t|t.kind()==pb::TaskKind::BasemapPrepare&&state.store.task_input(&t.id).ok().is_some_and(|i|matches!(i.input,Some(pb::task_input::Input::Basemap(b)) if !b.estimate_only&&(b.selection_key==key||b.waiting_selections.iter().any(|r|r.key==key))&&b.bounds==bounds)));
        let status = if package.is_some() {
            "ready"
        } else if task.as_ref().is_some_and(|t| !task_terminal(t)) {
            "preparing"
        } else if task
            .as_ref()
            .is_some_and(|t| t.state() == pb::TaskState::Failed)
        {
            "failed"
        } else {
            "missing"
        };
        let mut result = maps::BasemapSelection {
            context: Some(context),
            bounds,
            selected: package.as_ref().is_some_and(|p| p.id == selected),
            bound_package_id: selected,
            package,
            task_id: task.map_or_else(String::new, |t| t.id),
            status: status.into(),
            automatic: self.inner.config.basemap.automatic,
            display_max_zoom: config.max_zoom,
            ..Default::default()
        };
        if let Some(p) = &result.package {
            result.archive_url = format!("/v1/basemaps/{}/archive.pmtiles", p.id);
            result.style_url = format!("/v1/basemaps/{}/style-light.json", p.id);
            result.standard_style_url = format!("/v1/basemaps/{}/style-standard.json", p.id);
        }
        Ok(result)
    }
    pub fn set_basemap_selection(
        &self,
        r: maps::SetBasemapSelectionRequest,
    ) -> Result<maps::BasemapSelection> {
        let (context, key, bounds) = self.basemap_context(
            r.context
                .ok_or_else(|| AppError::invalid("context required"))?,
        )?;
        let mut state = self.state()?;
        let (p, _) = state.store.basemap(&r.package_id)?;
        if !p.enabled
            || p.assets_version != vrp_basemap::ASSETS_VERSION
            || !vrp_basemap::covers(&p.bounds, &bounds)
        {
            return Err(AppError::conflict(
                "basemap does not cover this display snapshot",
            ));
        }
        let current = state.store.basemap_binding(&key)?;
        if current != r.expected_package_id && current != p.id {
            return Err(AppError::conflict("display selection changed"));
        }
        state.store.bind_basemap(&key, &context, &p.id)?;
        drop(state);
        self.select_basemap(context)
    }
    pub fn prepare_basemap(&self, mut request: maps::PrepareBasemapRequest) -> Result<pb::Task> {
        request.estimate_task_id = request.estimate_task_id.trim().into();
        self.submit_basemap(request, false)
    }
    pub fn estimate_basemap(&self, request: maps::PrepareBasemapRequest) -> Result<pb::Task> {
        self.submit_basemap(request, true)
    }
    fn submit_basemap(
        &self,
        mut request: maps::PrepareBasemapRequest,
        estimate_only: bool,
    ) -> Result<pb::Task> {
        self.accepting()?;
        if self.inner.config.map_config.disabled || self.inner.config.map_config.custom {
            return Err(AppError::conflict(
                "managed basemaps are overridden by startup map configuration",
            ));
        }
        if request.native_max_zoom == 0 {
            request.native_max_zoom = 14;
        }
        if request.native_max_zoom > 15 || request.automatic && request.native_max_zoom > 14 {
            return Err(AppError::invalid(
                "automatic native precision is at most 14; detail 15 is explicit",
            ));
        }
        let (context, key, bounds) = if let Some(c) = request.context.clone() {
            let (c, k, b) = self.basemap_context(c)?;
            (Some(c), k, b)
        } else {
            vrp_basemap::validate_bounds(&request.bounds).map_err(geo_error)?;
            (None, String::new(), request.bounds.clone())
        };
        request.context = context.clone();
        request.bounds = bounds.clone();
        let mut state = self.state()?;
        let existing = state
            .store
            .basemaps()?
            .into_iter()
            .filter(|p| {
                p.enabled
                    && p.assets_version == vrp_basemap::ASSETS_VERSION
                    && p.native_max_zoom >= request.native_max_zoom
                    && vrp_basemap::covers(&p.bounds, &bounds)
            })
            .min_by_key(|p| p.size_bytes);
        if let Some(p) = existing.filter(|_| !request.force_update) {
            if estimate_only {
                return Ok(pb::Task {
                    id: p.task_id,
                    state: pb::TaskState::Succeeded as i32,
                    kind: pb::TaskKind::BasemapPrepare as i32,
                    basemap_package_id: p.id,
                    basemap_estimate: Some(maps::BasemapEstimate {
                        bounds,
                        native_max_zoom: request.native_max_zoom,
                        source: p.source,
                        ..Default::default()
                    }),
                    ..Default::default()
                });
            }
            if let Some(c) = &context {
                state.store.bind_basemap(&key, c, &p.id)?;
            }
            return Ok(pb::Task {
                id: p.task_id,
                kind: pb::TaskKind::BasemapPrepare as i32,
                state: pb::TaskState::Succeeded as i32,
                basemap_package_id: p.id,
                ..Default::default()
            });
        }
        let mut identity = request.clone();
        identity.idempotency_key.clear();
        identity.automatic = false;
        identity.context = None;
        identity.name.clear();
        let mut bytes = identity.encode_to_vec();
        bytes.push(u8::from(estimate_only));
        // A deleted/disabled completed package must allow a fresh preparation;
        // failed attempts remain idempotent until explicitly retried.
        if !estimate_only && !request.force_update
            && let Some(previous)=state.store.tasks()?.into_iter().find(|t|t.kind()==pb::TaskKind::BasemapPrepare && t.state()==pb::TaskState::Succeeded && state.store.task_input(&t.id).is_ok_and(|i|matches!(i.input,Some(pb::task_input::Input::Basemap(b)) if !b.estimate_only && b.bounds==bounds && b.request.as_ref().map(|r|r.native_max_zoom)==Some(request.native_max_zoom)))) {
            bytes.extend_from_slice(previous.id.as_bytes());
        }
        if request.force_update {
            bytes.extend_from_slice(&now_ms().to_le_bytes());
        }
        let idem = if request.idempotency_key.is_empty() {
            format!("basemap:{}", digest(&bytes))
        } else {
            request.idempotency_key.clone()
        };
        for task in state.store.tasks()? {
            if task.idempotency_key == idem {
                let mut old = state.store.task_input(&task.id)?;
                let Some(pb::task_input::Input::Basemap(b)) = old.input.as_mut() else {
                    return Err(AppError::conflict(
                        "idempotency key belongs to another task",
                    ));
                };
                if b.bounds != bounds
                    || b.request.as_ref().map(|v| v.native_max_zoom)
                        != Some(request.native_max_zoom)
                {
                    return Err(AppError::conflict(
                        "idempotency key has different basemap input",
                    ));
                }
                if let Some(c) = &context
                    && !estimate_only
                    && !b.waiting_selections.iter().any(|r| r.key == key)
                {
                    b.waiting_selections.push(maps::BasemapReference {
                        key: key.clone(),
                        context: Some(c.clone()),
                    });
                    state.store.freeze_basemap_input(&task.id, &old)?;
                }
                return Ok(task);
            }
        }
        let mut input = maps::BasemapTaskInput {
            request: Some(request.clone()),
            bounds,
            selection_key: key.clone(),
            waiting_selections: context
                .clone()
                .map(|c| maps::BasemapReference {
                    key,
                    context: Some(c),
                })
                .into_iter()
                .collect(),
            transfer_limit: self.inner.config.basemap.download_budget_bytes,
            estimate_only,
            ..Default::default()
        };
        if !request.estimate_task_id.is_empty() && !estimate_only {
            let estimated = state.store.task(&request.estimate_task_id)?;
            let old = state.store.task_input(&request.estimate_task_id)?;
            let Some(pb::task_input::Input::Basemap(previous)) = old.input else {
                return Err(AppError::invalid("not a basemap estimate"));
            };
            if estimated.state() != pb::TaskState::Succeeded
                || estimated.basemap_estimate.is_none()
                || !previous.estimate_only
                || previous.bounds != input.bounds
                || previous.request.as_ref().map(|r| r.native_max_zoom)
                    != Some(request.native_max_zoom)
            {
                return Err(AppError::conflict(
                    "estimate does not match this preparation",
                ));
            }
            input.source = previous.source;
            input.reuse_path = previous.reuse_path;
        }
        let task = new_task(
            pb::TaskKind::BasemapPrepare,
            context
                .as_ref()
                .filter(|c| !c.scenario_id.is_empty())
                .map(|c| pb::ScenarioRef {
                    scenario_id: c.scenario_id.clone(),
                    version: c.version,
                }),
            "",
            &idem,
        );
        let task = state.store.insert_task(
            task,
            &pb::TaskInput {
                input: Some(pb::task_input::Input::Basemap(Box::new(input))),
                ..Default::default()
            },
        )?;
        drop(state);
        self.inner.changed.notify_all();
        Ok(task)
    }
    pub fn import_basemap(&self, r: maps::ImportBasemapRequest) -> Result<pb::Task> {
        self.accepting()?;
        let mut path = PathBuf::from(r.path).canonicalize()?;
        if path.is_dir() {
            path = path.join("archive.pmtiles").canonicalize()?;
        }
        if !path.is_file() || !path.extension().is_some_and(|v| v == "pmtiles") {
            return Err(AppError::invalid(
                "import requires a server-visible PMTiles file or directory",
            ));
        }
        if path.metadata()?.len() > self.inner.config.basemap.disk_budget_bytes {
            return Err(AppError::new("too_large", "basemap exceeds disk budget"));
        }
        let input = maps::BasemapTaskInput {
            import_path: path.to_string_lossy().into(),
            request: Some(maps::PrepareBasemapRequest {
                name: r.name.clone(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let key = format!(
            "basemap-import:{}:{}:{:?}:{}",
            input.import_path,
            path.metadata()?.len(),
            path.metadata()?.modified()?,
            digest(r.name.as_bytes())
        );
        let task = self.state()?.store.insert_task(
            new_task(pb::TaskKind::BasemapPrepare, None, "", &key),
            &pb::TaskInput {
                input: Some(pb::task_input::Input::Basemap(Box::new(input))),
                ..Default::default()
            },
        )?;
        self.inner.changed.notify_all();
        Ok(task)
    }
    pub fn enable_basemap(&self, r: maps::EnableBasemapRequest) -> Result<maps::BasemapPackage> {
        let mut state = self.state()?;
        let (mut p, path) = state.store.basemap(&r.package_id)?;
        if p.version != r.expected_version {
            return Err(AppError::conflict("basemap version changed"));
        }
        p.enabled = r.enabled;
        p.state = if p.enabled {
            maps::PackageState::Ready
        } else {
            maps::PackageState::Disabled
        } as i32;
        p.version += 1;
        state.store.put_basemap(&p, &path)?;
        Ok(p)
    }
    pub fn update_basemap(&self, id: &str) -> Result<pb::Task> {
        let p = self.get_basemap(id)?;
        self.prepare_basemap(maps::PrepareBasemapRequest {
            bounds: p.bounds,
            native_max_zoom: p.native_max_zoom,
            force_update: true,
            name: p.name,
            ..Default::default()
        })
    }
    pub fn delete_basemap(&self, id: &str) -> Result<pb::Empty> {
        let (_, path) = self.basemap_resource(id)?;
        let deleted = self.state()?.store.delete_basemap(id)?;
        if deleted.canonicalize()? != path {
            return Err(AppError::internal("basemap path changed"));
        }
        fs::remove_file(path)?;
        Ok(pb::Empty {})
    }
    pub(super) fn run_basemap(
        &self,
        task: &pb::Task,
        input: &maps::BasemapTaskInput,
        cancel: &AtomicBool,
    ) -> Result<()> {
        self.acquire_task_memory(
            &task.id,
            cancel,
            self.inner.config.basemap.worker_memory_bytes,
        )?;
        let _lease = GeoLease {
            service: self,
            id: &task.id,
        };
        fs::create_dir_all(self.basemap_dir())?;
        self.recover_basemap_files()?;
        fs::create_dir_all(self.task_dir(&task.id))?;
        let mut input = input.clone();
        if !input.import_path.is_empty() {
            let source = Path::new(&input.import_path);
            let length = source.metadata()?.len();
            let possible = self
                .state()?
                .store
                .basemaps()?
                .iter()
                .any(|p| p.size_bytes == length);
            if possible {
                self.progress(&task.id, "hash", 0, length, "检查已验证内容是否可以复用")?;
                let (_, hash) = vrp_basemap::file_hash(source).map_err(geo_error)?;
                let mut state = self.state()?;
                if cancel.load(Ordering::Acquire) {
                    return Err(AppError::new("cancelled", "basemap cancelled"));
                }
                if let Ok((package, path)) = state.store.basemap(&hash)
                    && path.is_file()
                {
                    let mut completed = state.store.task(&task.id)?;
                    completed.basemap_package_id = package.id;
                    completed.state = pb::TaskState::Succeeded as i32;
                    completed.progress = Some(pb::TaskProgress {
                        phase: "ready".into(),
                        completed: length,
                        total: length,
                        message: "复用已验证的相同底图包，无需新增磁盘空间".into(),
                        unit: "bytes".into(),
                    });
                    state.store.update_task(&completed)?;
                    return Ok(());
                }
            }
        }
        let used: u64 = self
            .state()?
            .store
            .basemaps()?
            .iter()
            .map(|p| p.size_bytes)
            .sum();
        let available = self
            .inner
            .config
            .basemap
            .disk_budget_bytes
            .saturating_sub(used);
        input.output_limit = available;
        if available
            < if input.import_path.is_empty() {
                32768
            } else {
                127
            }
        {
            return Err(AppError::new("capacity", "basemap disk budget exhausted"));
        }
        if input.import_path.is_empty() && input.source.is_none() {
            self.progress(&task.id, "source", 0, 0, "正在确定离线底图源版本")?;
            let max = input.request.as_ref().map_or(14, |r| r.native_max_zoom);
            let reuse = self
                .state()?
                .store
                .basemaps()?
                .into_iter()
                .filter(|p| {
                    !input.request.as_ref().is_some_and(|r| r.force_update)
                        && p.native_max_zoom < max
                        && p.enabled
                        && vrp_basemap::covers(&p.bounds, &input.bounds)
                })
                .max_by_key(|p| p.native_max_zoom);
            if let Some(p) = reuse
                && let Some(old) = p.source.clone()
                && let Ok((resolved, _)) = vrp_basemap::resolve_source(
                    &old.url,
                    &self.inner.config.basemap.archive_base_url,
                )
                && resolved.etag == old.etag
                && resolved.size_bytes == old.size_bytes
            {
                input.source = Some(old);
                input.reuse_path = self
                    .state()?
                    .store
                    .basemap(&p.id)?
                    .1
                    .to_string_lossy()
                    .into();
                let mut state = self.state()?;
                let mut active = state.store.task(&task.id)?;
                active.basemap_package_id = p.id;
                state.store.update_task(&active)?;
            }
            if input.source.is_none() {
                let (source, used) = vrp_basemap::resolve_source(
                    &self.inner.config.basemap.catalog_url,
                    &self.inner.config.basemap.archive_base_url,
                )
                .map_err(geo_error)?;
                input.source = Some(source);
                input.transfer_limit = input.transfer_limit.saturating_sub(used);
            }
        }
        let stored = pb::TaskInput {
            input: Some(pb::task_input::Input::Basemap(Box::new(input.clone()))),
            ..Default::default()
        };
        self.state()?
            .store
            .freeze_basemap_input(&task.id, &stored)?;
        let output = self.task_dir(&task.id).join("basemap.partial");
        let mut options = self.inner.config.isolation.clone();
        options.memory_limit_bytes = Some(self.inner.config.basemap.worker_memory_bytes);
        options.setup_timeout = Duration::from_secs(1800);
        let args = vec![
            "basemap-child".into(),
            options.max_frame_bytes.to_string().into(),
        ];
        let runner = vrp_worker::ProcessRunner::new(self.inner.program.clone(), args, options)
            .map_err(geo_error)?;
        let result: Result<worker::BasemapRunResponse> = runner
            .run_reserved(
                &worker::BasemapRunRequest {
                    input: Some(input.clone()),
                    output_path: output.to_string_lossy().into(),
                },
                cancel,
                |p| {
                    let _ = self.progress(&task.id, &p.phase, p.completed, p.total, "");
                },
            )
            .map_err(geo_error);
        let response = match result {
            Ok(r) => r,
            Err(e) => {
                let _ = fs::remove_file(&output);
                return Err(e);
            }
        };
        if input.estimate_only {
            let estimate = response
                .estimate
                .ok_or_else(|| AppError::internal("missing basemap estimate"))?;
            let mut state = self.state()?;
            let mut task = state.store.task(&task.id)?;
            task.basemap_estimate = Some(estimate);
            task.progress = Some(pb::TaskProgress {
                phase: "estimated".into(),
                completed: 1,
                total: 1,
                message: "范围与新增下载量预估完成，等待手动提交准备".into(),
                ..Default::default()
            });
            state.store.update_task(&task)?;
            return Ok(());
        }
        if cancel.load(Ordering::Acquire) {
            let _ = fs::remove_file(&output);
            return Err(AppError::new("cancelled", "basemap cancelled"));
        }
        if Path::new(&response.output_path) != output {
            return Err(AppError::internal("unexpected basemap output path"));
        }
        let mut package = response
            .package
            .ok_or_else(|| AppError::internal("missing basemap manifest"))?;
        self.progress(&task.id, "publish", 0, 1, "正在校验内容指纹并发布离线底图")?;
        let (size, hash) = vrp_basemap::file_hash(&output).map_err(geo_error)?;
        if size != package.size_bytes || hash != package.sha256 || size > available {
            return Err(AppError::internal("basemap output size/hash mismatch"));
        }
        let destination = self.basemap_dir().join(format!("{hash}.pmtiles"));
        let mut state = self.state()?;
        if cancel.load(Ordering::Acquire) {
            let _ = fs::remove_file(&output);
            return Err(AppError::new("cancelled", "basemap cancelled"));
        }
        if let Ok((existing, _)) = state.store.basemap(&hash) {
            package = existing;
            fs::remove_file(&output)?;
        } else {
            if destination.exists() {
                let (_, existing_hash) = vrp_basemap::file_hash(&destination).map_err(geo_error)?;
                if existing_hash != hash {
                    return Err(AppError::internal("existing basemap content differs"));
                }
                fs::remove_file(&output)?;
            } else {
                fs::rename(&output, &destination)?;
            }
            package.task_id = task.id.clone();
            package.created_unix_ms = now_ms();
            package.name = input
                .request
                .as_ref()
                .map(|r| r.name.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("离线底图 · {} 级", package.native_max_zoom));
        }
        state.store.atomic(|store| {
            store.put_basemap(&package, &destination)?;
            if let Some(context) = input.request.as_ref().and_then(|r| r.context.as_ref())
                && !input.selection_key.is_empty()
            {
                store.bind_basemap(&input.selection_key, context, &package.id)?;
            }
            if let Some(pb::task_input::Input::Basemap(latest)) = store.task_input(&task.id)?.input
            {
                for reference in latest.waiting_selections {
                    if let Some(context) = reference.context {
                        store.bind_basemap(&reference.key, &context, &package.id)?;
                    }
                }
            }
            let mut completed = store.task(&task.id)?;
            completed.basemap_package_id = package.id;
            completed.state = pb::TaskState::Succeeded as i32;
            completed.progress = Some(pb::TaskProgress {
                phase: "ready".into(),
                completed: 1,
                total: 1,
                message: "离线底图已就绪".into(),
                ..Default::default()
            });
            store.update_task(&completed)
        })
    }
}
pub(crate) fn execute(
    request: worker::BasemapRunRequest,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, u64, u64),
) -> std::result::Result<worker::BasemapRunResponse, worker::ChildFailure> {
    let run = || -> std::io::Result<worker::BasemapRunResponse> {
        let input = request
            .input
            .as_ref()
            .ok_or_else(|| std::io::Error::other("basemap input required"))?;
        let prepared =
            vrp_basemap::prepare(input, Path::new(&request.output_path), cancel, progress)?;
        let (package, estimate) = match prepared {
            vrp_basemap::Prepared::Package(p) => (Some(*p), None),
            vrp_basemap::Prepared::Estimate(e) => (None, Some(e)),
        };
        Ok(worker::BasemapRunResponse {
            package,
            estimate,
            output_path: request.output_path.clone(),
        })
    };
    run().map_err(|e| worker::ChildFailure {
        code: "basemap_error".into(),
        message: e.to_string(),
    })
}
