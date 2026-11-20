use super::*;

fn client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(180))
        .user_agent("vrp-1/road-package-preparation")
        .build()
        .map_err(geo_error)
}

fn source_length(response: &reqwest::blocking::Response) -> Option<u64> {
    response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}
fn source_header(
    response: &reqwest::blocking::Response,
    name: reqwest::header::HeaderName,
) -> String {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}
fn source_unchanged(package: &pb::RoadPackage, input: &pb::RoadPackagePrepareInput) -> bool {
    !input.force_download
        && package.source_url == input.url
        && !input.url.is_empty()
        && package.size_bytes == input.estimated_bytes
        && ((!package.source_etag.is_empty() && package.source_etag == input.source_etag)
            || (package.source_etag.is_empty()
                && input.source_etag.is_empty()
                && !package.source_last_modified.is_empty()
                && package.source_last_modified == input.source_last_modified))
}
fn pb_bounds(b: Bounds) -> pb::GeoBounds {
    pb::GeoBounds {
        south: b.south,
        west: b.west,
        north: b.north,
        east: b.east,
    }
}
fn intersects(a: &pb::GeoBounds, b: Bounds) -> bool {
    let a = Bounds {
        south: a.south,
        west: a.west,
        north: a.north,
        east: a.east,
    };
    a.split().iter().any(|a| {
        b.split().iter().any(|b| {
            a.west <= b.east && a.east >= b.west && a.south <= b.north && a.north >= b.south
        })
    })
}
fn iso_timestamp(seconds: i64) -> String {
    let days = seconds.div_euclid(86400);
    let time = seconds.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    y += i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}
struct PendingFile {
    path: PathBuf,
    remove: bool,
}
impl Drop for PendingFile {
    fn drop(&mut self) {
        if self.remove {
            let _ = fs::remove_file(&self.path);
        }
    }
}
impl Service {
    fn read_catalog(&self) -> Result<vrp_geo::catalog::Catalog> {
        if self.inner.config.geo.catalog_url.is_empty() {
            return Err(AppError::new("unavailable", "road catalog is disabled"));
        }
        fs::create_dir_all(self.road_dir())?;
        let cache = self.road_dir().join("catalog.json");
        if cache
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|v| v.elapsed().ok())
            .is_some_and(|age| age < Duration::from_secs(86400))
        {
            return vrp_geo::catalog::Catalog::parse(&fs::read_to_string(cache)?)
                .map_err(geo_error);
        }
        let response = client()?
            .get(&self.inner.config.geo.catalog_url)
            .timeout(Duration::from_secs(10))
            .send()
            .and_then(|r| r.error_for_status())
            .map_err(geo_error)?;
        if response
            .content_length()
            .is_some_and(|n| n > 16 * 1024 * 1024)
        {
            return Err(AppError::new("too_large", "road catalog exceeds limit"));
        }
        let mut text = String::new();
        response
            .take(16 * 1024 * 1024 + 1)
            .read_to_string(&mut text)?;
        if text.len() > 16 * 1024 * 1024 {
            return Err(AppError::new("too_large", "road catalog exceeds limit"));
        }
        let catalog = vrp_geo::catalog::Catalog::parse(&text).map_err(geo_error)?;
        fs::write(cache, text)?;
        Ok(catalog)
    }
    pub fn road_catalog(&self) -> Result<pb::RoadCatalog> {
        match self.read_catalog() {
            Ok(c) => {
                let roads = self.state()?.store.road_packages()?;
                Ok(pb::RoadCatalog {
                    available: true,
                    message:
                        "Geofabrik raw OSM regions; selection minimizes additional download bytes"
                            .into(),
                    entries: c
                        .entries
                        .into_iter()
                        .map(|v| pb::RoadCatalogEntry {
                            id: v.id.clone(),
                            parent: v.parent.unwrap_or_default(),
                            name: v.name,
                            url: v.pbf_url,
                            coverage_geojson: v.coverage.to_string(),
                            size_bytes: 0,
                            cached: roads.iter().any(|p| p.region_id == v.id && p.enabled),
                        })
                        .collect(),
                })
            }
            Err(e) => Ok(pb::RoadCatalog {
                available: false,
                message: e.message,
                entries: vec![],
            }),
        }
    }
    pub fn list_road_packages(
        &self,
        q: pb::ListRoadPackagesRequest,
    ) -> Result<pb::ListRoadPackagesResponse> {
        let limit = page_size(q.page_size)?;
        let rows = self.state()?.store.road_packages()?;
        let mut packages: Vec<_> = rows
            .into_iter()
            .filter(|p| p.id > q.after_id)
            .take(limit + 1)
            .collect();
        let next = if packages.len() > limit {
            packages.truncate(limit);
            packages.last().unwrap().id.clone()
        } else {
            String::new()
        };
        Ok(pb::ListRoadPackagesResponse {
            packages,
            next_after_id: next,
        })
    }
    pub fn get_road_package(&self, id: &str) -> Result<pb::RoadPackage> {
        let mut package = self.state()?.store.road_package(id)?;
        let state = self.state()?;
        let tasks = state.store.tasks()?;
        let mut scenarios = BTreeSet::new();
        let mut graphs = BTreeMap::new();
        for task in tasks.iter().filter(|t| {
            t.kind() != pb::TaskKind::RoadPackagePrepare
                && t.road_package_ids.iter().any(|p| p == id)
        }) {
            package.referencing_task_ids.push(task.id.clone());
            if let Some(scene) = &task.scenario {
                scenarios.insert(scene.scenario_id.clone());
            }
            for artifact in state
                .store
                .artifacts(&task.id)?
                .into_iter()
                .filter(|a| a.kind == "road_snapshot")
            {
                graphs.entry(artifact.sha256.clone()).or_insert(artifact);
            }
        }
        package.referencing_scenario_ids = scenarios.into_iter().collect();
        package.graph_artifacts = graphs.into_values().collect();
        Ok(package)
    }
    pub fn road_package_versions(&self, id: &str) -> Result<pb::ListRoadPackagesResponse> {
        let s = self.state()?;
        let region = s.store.road_package(id)?.region_id;
        Ok(pb::ListRoadPackagesResponse {
            packages: s
                .store
                .road_packages()?
                .into_iter()
                .filter(|v| v.region_id == region)
                .collect(),
            next_after_id: String::new(),
        })
    }
    pub fn enable_road_package(&self, r: pb::EnableRoadPackageRequest) -> Result<pb::RoadPackage> {
        let mut s = self.state()?;
        let mut p = s.store.road_package(&r.package_id)?;
        if r.expected_version != 0 && r.expected_version != p.version {
            return Err(AppError::conflict("road package version changed"));
        }
        p.enabled = r.enabled;
        p.state = if r.enabled {
            pb::RoadPackageState::Ready
        } else {
            pb::RoadPackageState::Disabled
        } as i32;
        let path = s.store.road_path(&p.id)?;
        let others = s.store.road_packages()?;
        s.store.atomic(|store| {
            if p.enabled && !p.region_id.is_empty() {
                for mut old in others
                    .iter()
                    .filter(|v| v.region_id == p.region_id && v.id != p.id && v.enabled)
                    .cloned()
                {
                    old.enabled = false;
                    old.state = pb::RoadPackageState::Disabled as i32;
                    let path = store.road_path(&old.id)?;
                    store.put_road_package(&old, &path)?;
                }
            }
            store.put_road_package(&p, &path)
        })?;
        Ok(p)
    }
    pub fn delete_road_package(&self, id: &str) -> Result<pb::Empty> {
        let path = self.state()?.store.delete_road_package(id)?;
        let root = self.road_dir().canonicalize()?;
        if let Ok(actual) = path.canonicalize() {
            if !actual.starts_with(&root) {
                return Err(AppError::internal(
                    "managed road path escaped data directory",
                ));
            }
            fs::remove_file(actual)?;
        }
        Ok(pb::Empty {})
    }
    pub fn import_road_package(&self, r: pb::ImportRoadPackageRequest) -> Result<pb::Task> {
        let input = self.import_package_input(r)?;
        self.package_task(input, "")
    }
    pub(super) fn import_package_input(
        &self,
        r: pb::ImportRoadPackageRequest,
    ) -> Result<pb::RoadPackagePrepareInput> {
        let path = PathBuf::from(&r.path).canonicalize()?;
        if !path.is_file()
            || !path
                .file_name()
                .is_some_and(|v| v.to_string_lossy().ends_with(".pbf"))
        {
            return Err(AppError::invalid(
                "import requires a server-visible OSM PBF file",
            ));
        }
        let size = path.metadata()?.len();
        if size > self.inner.config.max_payload_bytes {
            return Err(AppError::new(
                "too_large",
                "road package exceeds storage limit",
            ));
        }
        let input = pb::RoadPackagePrepareInput {
            region_id: if r.region_id.is_empty() {
                path.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .replace(".osm", "")
            } else {
                r.region_id
            },
            name: r.name,
            import_path: path.to_string_lossy().into(),
            coverage_geojson: r.coverage_geojson,
            estimated_bytes: size,
            ..Default::default()
        };
        Ok(input)
    }
    pub fn prepare_road_package(&self, region: &str) -> Result<pb::Task> {
        self.prepare_road_package_force(region, false)
    }
    pub fn prepare_road_package_force(&self, region: &str, force: bool) -> Result<pb::Task> {
        let mut input = self.prepare_package_input(region)?;
        input.force_download = force;
        self.package_task(input, "")
    }
    pub(super) fn prepare_package_input(
        &self,
        region: &str,
    ) -> Result<pb::RoadPackagePrepareInput> {
        let c = self.read_catalog()?;
        let e = c
            .entries
            .into_iter()
            .find(|e| e.id == region)
            .ok_or_else(|| AppError::new("not_found", "catalog region not found"))?;
        let response = client()?
            .head(&e.pbf_url)
            .timeout(Duration::from_secs(10))
            .send()
            .and_then(|r| r.error_for_status())
            .map_err(geo_error)?;
        let size = source_length(&response)
            .ok_or_else(|| AppError::invalid("source does not declare download size"))?;
        Ok(pb::RoadPackagePrepareInput {
            region_id: e.id,
            name: e.name,
            url: e.pbf_url,
            coverage_geojson: e.coverage.to_string(),
            estimated_bytes: size,
            source_etag: source_header(&response, reqwest::header::ETAG),
            source_last_modified: source_header(&response, reqwest::header::LAST_MODIFIED),
            ..Default::default()
        })
    }
    pub fn update_road_package(&self, id: &str) -> Result<pb::Task> {
        self.update_road_package_force(id, false)
    }
    pub fn update_road_package_force(&self, id: &str, force: bool) -> Result<pb::Task> {
        let p = self.get_road_package(id)?;
        if p.source_url.is_empty() {
            let path = self.state()?.store.road_path(id)?;
            return self.package_task(
                pb::RoadPackagePrepareInput {
                    region_id: p.region_id,
                    name: p.name,
                    import_path: path.to_string_lossy().into(),
                    coverage_geojson: p.coverage_geojson,
                    estimated_bytes: p.size_bytes,
                    ..Default::default()
                },
                "",
            );
        }
        self.prepare_road_package_force(&p.region_id, force)
    }
    pub fn validate_road_package(&self, id: &str) -> Result<pb::Task> {
        let p = self.get_road_package(id)?;
        let path = self.state()?.store.road_path(id)?;
        self.package_task(
            pb::RoadPackagePrepareInput {
                region_id: p.region_id,
                name: p.name,
                import_path: path.to_string_lossy().into(),
                coverage_geojson: p.coverage_geojson,
                estimated_bytes: p.size_bytes,
                ..Default::default()
            },
            "",
        )
    }
    fn source_cached_package(
        &self,
        input: &pb::RoadPackagePrepareInput,
    ) -> Result<Option<pb::RoadPackage>> {
        let s = self.state()?;
        let mut packages = s.store.road_packages()?;
        packages.sort_by_key(|p| std::cmp::Reverse(p.version));
        Ok(packages.into_iter().find(|p| {
            !input.url.is_empty()
                && p.source_url == input.url
                && s.store.road_path(&p.id).is_ok_and(|path| {
                    path.metadata()
                        .is_ok_and(|meta| meta.is_file() && meta.len() == p.size_bytes)
                })
        }))
    }
    fn finish_cached_package(&self, task: &pb::Task, mut package: pb::RoadPackage) -> Result<()> {
        package.checked_unix_ms = now_ms();
        fs::create_dir_all(self.task_dir(&task.id))?;
        let manifest = self.task_dir(&task.id).join("package.json");
        {
            let mut s = self.state()?;
            let path = s.store.road_path(&package.id)?;
            s.store.put_road_package(&package, &path)?;
            let mut current = s.store.task(&task.id)?;
            current.road_package_ids = vec![package.id.clone()];
            s.store.update_task(&current)?;
        }
        fs::write(
            &manifest,
            serde_json::to_vec_pretty(&package).map_err(geo_error)?,
        )?;
        self.artifact(&task.id, "package", "road_package_manifest", &manifest)?;
        self.progress(
            &task.id,
            "source_cache_hit",
            package.size_bytes,
            package.size_bytes,
            "Source unchanged; reused verified local package without downloading",
        )
    }
    fn package_task(&self, input: pb::RoadPackagePrepareInput, parent: &str) -> Result<pb::Task> {
        self.package_task_key(input, parent, "", "")
    }
    pub(super) fn package_task_key(
        &self,
        input: pb::RoadPackagePrepareInput,
        parent: &str,
        key: &str,
        input_hash: &str,
    ) -> Result<pb::Task> {
        if !input.url.is_empty()
            && input.estimated_bytes > self.inner.config.geo.download_budget_bytes
            && !self
                .source_cached_package(&input)?
                .is_some_and(|p| source_unchanged(&p, &input))
        {
            return Err(AppError::new(
                "too_large",
                "road region exceeds configured additional-download budget",
            ));
        }
        let mut s = self.state()?;
        let mut shared = None;
        for t in s.store.tasks()? {
            if !key.is_empty() && t.idempotency_key == key {
                if s.store.task_input(&t.id)?.input_sha256 == input_hash {
                    return Ok(t);
                }
                return Err(AppError::conflict(
                    "idempotency key has different task input",
                ));
            }
            if t.kind() == pb::TaskKind::RoadPackagePrepare && !task_terminal(&t) {
                let i = s.store.task_input(&t.id)?;
                if let Some(pb::task_input::Input::RoadPackage(p)) = i.input
                    && p.region_id == input.region_id
                    && p.url == input.url
                    && p.import_path == input.import_path
                {
                    if key.is_empty() {
                        return Ok(t);
                    }
                    shared = Some(t.id.clone());
                }
            }
        }
        let mut task = new_task(pb::TaskKind::RoadPackagePrepare, None, parent, key);
        if let Some(shared) = shared {
            task.dependency_ids.push(shared);
        }
        let task = s.store.insert_task(
            task,
            &pb::TaskInput {
                input: Some(pb::task_input::Input::RoadPackage(input)),
                input_sha256: input_hash.into(),
                ..Default::default()
            },
        )?;
        drop(s);
        self.inner.changed.notify_all();
        Ok(task)
    }
    pub(crate) fn register_local_roads(&self) -> Result<()> {
        let Some(dir) = &self.inner.config.geo.local_import_dir else {
            return Ok(());
        };
        let dir = dir.canonicalize()?;
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path
                .file_name()
                .is_some_and(|p| p.to_string_lossy().ends_with(".pbf"))
            {
                let region = path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .replace(".osm", "");
                let existing = self
                    .state()?
                    .store
                    .road_packages()?
                    .into_iter()
                    .any(|p| p.region_id == region);
                if !existing {
                    self.import_road_package(pb::ImportRoadPackageRequest {
                        path: path.to_string_lossy().into(),
                        region_id: region,
                        ..Default::default()
                    })?;
                }
            }
        }
        Ok(())
    }
    pub(super) fn run_package(
        &self,
        task: &pb::Task,
        p: &pb::RoadPackagePrepareInput,
        cancel: &AtomicBool,
    ) -> Result<()> {
        if let Some(shared) = task.dependency_ids.first() {
            loop {
                if cancel.load(Ordering::Acquire) || self.is_stopping() {
                    return Err(AppError::new("cancelled", "shared download wait cancelled"));
                }
                let source = self.get_task(shared)?;
                if task_terminal(&source) {
                    if source.state() != pb::TaskState::Succeeded {
                        return Err(AppError::new(
                            "download_failed",
                            "shared download did not complete",
                        ));
                    }
                    let mut s = self.state()?;
                    let mut current = s.store.task(&task.id)?;
                    current.road_package_ids = source.road_package_ids;
                    s.store.update_task(&current)?;
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        fs::create_dir_all(self.road_dir())?;
        fs::create_dir_all(self.task_dir(&task.id))?;
        let cached = self.source_cached_package(p)?;
        if let Some(package) = cached
            .as_ref()
            .filter(|package| source_unchanged(package, p))
        {
            return self.finish_cached_package(task, package.clone());
        }
        let temp = self.task_dir(&task.id).join("package.partial");
        let _partial = PendingFile {
            path: temp.clone(),
            remove: true,
        };
        let mut out = File::create(&temp)?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut buf = [0u8; 65536];
        let mut observed_etag = String::new();
        let mut observed_modified = String::new();
        let mut source: Box<dyn Read> = if !p.import_path.is_empty() {
            Box::new(File::open(&p.import_path)?)
        } else {
            let mut request = client()?.get(&p.url);
            if !p.force_download
                && let Some(package) = &cached
            {
                if !package.source_etag.is_empty() {
                    request = request.header(reqwest::header::IF_NONE_MATCH, &package.source_etag);
                } else if !package.source_last_modified.is_empty() {
                    request = request.header(
                        reqwest::header::IF_MODIFIED_SINCE,
                        &package.source_last_modified,
                    );
                }
            }
            let response = request.send().map_err(geo_error)?;
            if response.status() == reqwest::StatusCode::NOT_MODIFIED {
                let package = cached.ok_or_else(|| {
                    AppError::new(
                        "invalid_source",
                        "source returned 304 without a verified local package",
                    )
                })?;
                drop(out);
                return self.finish_cached_package(task, package);
            }
            let response = response.error_for_status().map_err(geo_error)?;
            observed_etag = source_header(&response, reqwest::header::ETAG);
            observed_modified = source_header(&response, reqwest::header::LAST_MODIFIED);
            Box::new(response)
        };
        let limit = if p.url.is_empty() {
            self.inner.config.max_payload_bytes
        } else {
            self.inner.config.geo.download_budget_bytes
        };
        let mut last = Instant::now();
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(AppError::new(
                    "cancelled",
                    "road package preparation cancelled",
                ));
            }
            let n = source.read(&mut buf)?;
            if n == 0 {
                break;
            }
            size += n as u64;
            if size > limit {
                return Err(AppError::new(
                    "too_large",
                    "road download exceeded configured byte budget",
                ));
            }
            out.write_all(&buf[..n])?;
            hash.update(&buf[..n]);
            if last.elapsed() > Duration::from_millis(500) {
                self.progress(
                    &task.id,
                    "download",
                    size,
                    p.estimated_bytes,
                    "Downloading selected region",
                )?;
                last = Instant::now();
            }
        }
        out.sync_all()?;
        drop(out);
        let sha = format!("{:x}", hash.finalize());
        {
            let _lease = self.geo_lease(&task.id, cancel)?;
            let validation = self.task_dir(&task.id).join("validation.pb");
            let validation_request = vrp_proto::worker::v1::GeoRunRequest {
                kind: vrp_proto::worker::v1::GeoTaskKind::ValidatePackage as i32,
                pbf_paths: vec![temp.to_string_lossy().into()],
                output_path: validation.to_string_lossy().into(),
                threads: 1,
                ..Default::default()
            };
            let validated = self.geo_runner(&task.id, &validation_request, cancel)?;
            self.validate_output(&validated, &validation)?;
        }
        let info = vrp_geo::inspect_pbf(&temp).map_err(geo_error)?;
        let id = format!("road-{}", &sha[..24]);
        let path = self.road_dir().join(format!("{sha}.osm.pbf"));
        let mut published = PendingFile {
            path: path.clone(),
            remove: !path.exists(),
        };
        if !path.exists() {
            fs::rename(&temp, &path)?;
        } else {
            fs::remove_file(&temp)?;
        }
        if cancel.load(Ordering::Acquire) {
            return Err(AppError::new(
                "cancelled",
                "road package preparation cancelled",
            ));
        }
        let mut s = self.state()?;
        let existing = s.store.road_packages()?;
        let version = existing
            .iter()
            .filter(|v| v.region_id == p.region_id)
            .map(|v| v.version)
            .max()
            .unwrap_or(0)
            + 1;
        let mut package = existing
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .unwrap_or_else(|| pb::RoadPackage {
                id: id.clone(),
                region_id: p.region_id.clone(),
                name: if p.name.is_empty() {
                    p.region_id.clone()
                } else {
                    p.name.clone()
                },
                provider: if p.url.is_empty() {
                    "local-osm"
                } else {
                    "geofabrik"
                }
                .into(),
                source_url: p.url.clone(),
                source_format: "osm.pbf".into(),
                coverage_geojson: p.coverage_geojson.clone(),
                bounds: info.bounds.map(pb_bounds),
                sha256: sha,
                size_bytes: size,
                downloaded_unix_ms: now_ms(),
                source_timestamp: info.timestamp_unix.map(iso_timestamp).unwrap_or_default(),
                version,
                enabled: true,
                state: pb::RoadPackageState::Ready as i32,
                task_id: task.id.clone(),
                reference_count: 0,
                attribution: "© OpenStreetMap contributors, ODbL 1.0".into(),
                ..Default::default()
            });
        if !p.url.is_empty() && (package.source_url.is_empty() || package.source_url == p.url) {
            package.source_url = p.url.clone();
            package.source_etag = observed_etag;
            package.source_last_modified = observed_modified;
        }
        package.checked_unix_ms = now_ms();
        s.store.atomic(|store| {
            if !existing.iter().any(|p| p.id == package.id)
                && package.enabled
                && !package.region_id.is_empty()
            {
                for mut old in existing
                    .iter()
                    .filter(|p| p.region_id == package.region_id && p.enabled)
                    .cloned()
                {
                    old.enabled = false;
                    old.state = pb::RoadPackageState::Disabled as i32;
                    let old_path = store.road_path(&old.id)?;
                    store.put_road_package(&old, &old_path)?;
                }
            }
            store.put_road_package(&package, &path)
        })?;
        published.remove = false;
        let mut t = s.store.task(&task.id)?;
        t.road_package_ids = vec![id];
        s.store.update_task(&t)?;
        drop(s);
        let manifest = self.task_dir(&task.id).join("package.json");
        fs::write(
            &manifest,
            serde_json::to_vec_pretty(&package).map_err(geo_error)?,
        )?;
        self.artifact(&task.id, "package", "road_package_manifest", &manifest)?;
        Ok(())
    }
    fn source_report(
        &self,
        task: &pb::Task,
        report: &pb::SourceSelectionReport,
        packages: &[String],
    ) -> Result<()> {
        fs::create_dir_all(self.task_dir(&task.id))?;
        let path = self.task_dir(&task.id).join("source-selection.json");
        fs::write(&path, serde_json::to_vec_pretty(report).map_err(geo_error)?)?;
        self.artifact(&task.id, "sources", "source_selection", &path)?;
        let mut state = self.state()?;
        let mut current = state.store.task(&task.id)?;
        current.road_package_ids = packages.to_vec();
        state.store.update_task(&current)
    }
    pub(super) fn ensure_packages(
        &self,
        task: &pb::Task,
        input: &engine::SolveRequest,
        cancel: &AtomicBool,
    ) -> Result<Vec<String>> {
        let started = Instant::now();
        let stored = self.state()?.store.task_input(&task.id)?;
        let requested = match stored.input {
            Some(pb::task_input::Input::GeoPlan(v)) => Some(v),
            _ => None,
        };
        let wait = requested
            .as_ref()
            .filter(|r| r.road_wait_ms > 0)
            .map(|r| Duration::from_millis(r.road_wait_ms.min(180000)))
            .unwrap_or(self.inner.config.geo.road_wait);
        let buffer = requested
            .as_ref()
            .filter(|r| r.buffer_km > 0.0)
            .map_or(10.0, |r| r.buffer_km);
        let area = bounds(input, buffer)?;
        let scene_bounds = Bounds::from_points(&points(input)?).ok().map(pb_bounds);
        let mut existing = self.state()?.store.road_packages()?;
        existing.retain(|p| p.enabled);
        existing.sort_by(|a, b| {
            b.version
                .cmp(&a.version)
                .then_with(|| b.downloaded_unix_ms.cmp(&a.downloaded_unix_ms))
        });
        let mut regions = BTreeSet::new();
        existing.retain(|p| {
            regions.insert(if p.region_id.is_empty() {
                p.id.clone()
            } else {
                p.region_id.clone()
            })
        });
        // Prove existing coverage without first depending on an online catalog.
        // Imported packages may have a reliable polygon but no PBF header bbox.
        let mut local_catalog = vrp_geo::catalog::Catalog { entries: vec![] };
        let mut cached = BTreeSet::new();
        let mut cached_ids = BTreeMap::new();
        for package in &existing {
            if let Ok(coverage) = serde_json::from_str(&package.coverage_geojson) {
                let id = format!("cache/{}", package.id);
                let entry = vrp_geo::catalog::CatalogEntry {
                    id: id.clone(),
                    parent: None,
                    name: package.name.clone(),
                    pbf_url: String::new(),
                    coverage,
                };
                if entry.covers_point(GeoPoint { lng: 0.0, lat: 0.0 }).is_ok() {
                    cached.insert(id.clone());
                    cached_ids.insert(id, package.id.clone());
                    local_catalog.entries.push(entry);
                }
            }
        }
        let local_plan = local_catalog
            .plan_with_budget(area, &BTreeMap::new(), &cached, 0)
            .map_err(geo_error)?;
        let mut selected: Vec<_> = local_plan
            .packages
            .iter()
            .filter_map(|p| cached_ids.get(&p.region_id).cloned())
            .collect();
        let cached_report = |selected: &[String], reason: String| pb::SourceSelectionReport {
            scene_bounds,
            routing_bounds: Some(pb_bounds(area)),
            sources: existing
                .iter()
                .filter(|p| selected.contains(&p.id))
                .map(|p| pb::SelectedRoadSource {
                    region_id: p.region_id.clone(),
                    url: p.source_url.clone(),
                    size_bytes: p.size_bytes,
                    reused: true,
                    coverage_geojson: p.coverage_geojson.clone(),
                    package_id: p.id.clone(),
                })
                .collect(),
            coverage_complete: local_plan.coverage_complete,
            uncovered_fraction: local_plan.uncovered_fraction,
            reason,
            ..Default::default()
        };
        if local_plan.coverage_complete {
            self.source_report(task, &cached_report(&selected,
                "Verified cached polygon union covers the routing area; no catalog or regional PBF request is needed".into()), &selected)?;
            return Ok(selected);
        }
        let mut catalog = match self.read_catalog() {
            Ok(c) => c,
            Err(e) => {
                // Header rectangles can make an undeclared local source useful,
                // but do not turn its contents into proven polygon coverage.
                for package in existing.iter().filter(|p| {
                    p.coverage_geojson.is_empty()
                        && p.bounds.as_ref().is_some_and(|b| intersects(b, area))
                }) {
                    if !selected.contains(&package.id) {
                        selected.push(package.id.clone());
                    }
                }
                self.progress(&task.id, "geometric_fallback", 0, 0, &e.message)?;
                self.source_report(task, &cached_report(&selected, format!(
                    "Catalog unavailable; retained verified cached polygon coverage and candidate local header bounds. Sources without a coverage declaration cannot prove completeness; missing roads use explicit geometric fallback: {}", e.message)), &selected)?;
                return Ok(selected);
            }
        };
        catalog.entries.extend(local_catalog.entries);
        for entry in &catalog.entries {
            if let Some(p) = existing.iter().find(|p| {
                p.region_id == entry.id || entry.id.ends_with(&format!("/{}", p.region_id))
            }) {
                cached.insert(entry.id.clone());
                cached_ids.insert(entry.id.clone(), p.id.clone());
                if p.coverage_geojson.is_empty() {
                    let mut enriched = p.clone();
                    enriched.coverage_geojson = entry.coverage.to_string();
                    let mut state = self.state()?;
                    let path = state.store.road_path(&p.id)?;
                    state.store.put_road_package(&enriched, &path)?;
                }
            }
        }
        // True polygon union is checked before HEAD/download, including holes and
        // anti-meridian coverage. Fully covered scenarios cause no PBF requests.
        let cache_plan = catalog
            .plan_with_budget(area, &BTreeMap::new(), &cached, 0)
            .map_err(geo_error)?;
        selected = cache_plan
            .packages
            .iter()
            .filter_map(|p| cached_ids.get(&p.region_id).cloned())
            .collect();
        if cache_plan.coverage_complete {
            let report=pb::SourceSelectionReport{scene_bounds,routing_bounds:Some(pb_bounds(area)),sources:cache_plan.packages.iter().map(|p|pb::SelectedRoadSource{region_id:p.region_id.clone(),url:p.url.clone(),size_bytes:cached_ids.get(&p.region_id).and_then(|id|existing.iter().find(|v|&v.id==id)).map_or(0,|p|p.size_bytes),reused:true,coverage_geojson:catalog.entries.iter().find(|e|e.id==p.region_id).map_or_else(String::new,|e|e.coverage.to_string()),package_id:cached_ids.get(&p.region_id).cloned().unwrap_or_default()}).collect(),coverage_complete:true,reason:"Verified cached polygon union covers the routing area; no regional PBF download is needed".into(),..Default::default()};
            self.source_report(task, &report, &selected)?;
            return Ok(selected);
        }
        let size_cache = self.road_dir().join("catalog-sizes.json");
        let mut sizes: BTreeMap<String, u64> = if size_cache
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|t| t < Duration::from_secs(86400))
        {
            fs::read(&size_cache)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default()
        } else {
            BTreeMap::new()
        };
        sizes.retain(|_, size| *size > 0);
        let http = client()?;
        for entry in catalog.candidates(area) {
            if cancel.load(Ordering::Acquire) {
                return Err(AppError::new("cancelled", "coverage check cancelled"));
            }
            if started.elapsed() >= wait {
                break;
            }
            if cached.contains(&entry.id) || sizes.contains_key(&entry.id) {
                continue;
            }
            if let Ok(response) = http
                .head(&entry.pbf_url)
                .timeout(Duration::from_secs(5))
                .send()
                .and_then(|r| r.error_for_status())
                && let Some(size) = source_length(&response)
            {
                sizes.insert(entry.id.clone(), size);
            }
        }
        fs::write(size_cache, serde_json::to_vec(&sizes).map_err(geo_error)?)?;
        let plan = catalog
            .plan_with_budget(
                area,
                &sizes,
                &cached,
                self.inner.config.geo.download_budget_bytes,
            )
            .map_err(geo_error)?;
        let mut report=pb::SourceSelectionReport{scene_bounds,routing_bounds:Some(pb_bounds(area)),sources:plan.packages.iter().map(|p|pb::SelectedRoadSource{region_id:p.region_id.clone(),url:p.url.clone(),size_bytes:if p.reused{cached_ids.get(&p.region_id).and_then(|id|existing.iter().find(|v|&v.id==id)).map_or(0,|v|v.size_bytes)}else{p.download_bytes},reused:p.reused,coverage_geojson:catalog.entries.iter().find(|e|e.id==p.region_id).map_or_else(String::new,|e|e.coverage.to_string()),package_id:cached_ids.get(&p.region_id).cloned().unwrap_or_default()}).collect(),additional_download_bytes:plan.download_bytes,coverage_complete:plan.coverage_complete,uncovered_fraction:plan.uncovered_fraction,reason:"Select cached coverage first, then regional files by additional bytes within the configured budget; build only the scene routing area".into(),..Default::default()};
        let mut dependencies = vec![];
        for package in plan.packages {
            if package.reused {
                if let Some(id) = cached_ids.get(&package.region_id)
                    && !selected.contains(id)
                {
                    selected.push(id.clone());
                }
                continue;
            }
            if started.elapsed() >= wait {
                continue;
            }
            if let Some(entry) = catalog.entries.iter().find(|e| e.id == package.region_id) {
                let t = self.package_task(
                    pb::RoadPackagePrepareInput {
                        region_id: entry.id.clone(),
                        name: entry.name.clone(),
                        url: entry.pbf_url.clone(),
                        coverage_geojson: entry.coverage.to_string(),
                        estimated_bytes: package.download_bytes,
                        ..Default::default()
                    },
                    "",
                )?;
                dependencies.push(t.id);
            }
        }
        {
            let mut s = self.state()?;
            let mut t = s.store.task(&task.id)?;
            t.dependency_ids = dependencies.clone();
            t.road_package_ids = selected.clone();
            if !plan.coverage_complete {
                t.warnings.push("Regional data coverage is partial; missing paths will use explicit geometric estimates".into());
            }
            s.store.update_task(&t)?;
        }
        report.dependency_task_ids = dependencies.clone();
        self.source_report(task, &report, &selected)?;
        while !dependencies.is_empty() && started.elapsed() < wait {
            if cancel.load(Ordering::Acquire) {
                return Err(AppError::new("cancelled", "road dependency wait cancelled"));
            }
            if dependencies
                .iter()
                .all(|id| self.get_task(id).is_ok_and(|t| task_terminal(&t)))
            {
                break;
            }
            self.progress(
                &task.id,
                "waiting_roads",
                started.elapsed().as_secs(),
                wait.as_secs(),
                "Waiting for shared road packages; geometric fallback remains available",
            )?;
            std::thread::sleep(Duration::from_millis(250));
        }
        for id in dependencies {
            if let Ok(t) = self.get_task(&id) {
                if t.progress.as_ref().is_none_or(|p| p.phase != "cache_hit") {
                    let remote = matches!(self.state()?.store.task_input(&id)?.input,Some(pb::task_input::Input::RoadPackage(p)) if !p.url.is_empty());
                    if remote {
                        report.actual_download_bytes += t
                            .road_package_ids
                            .iter()
                            .filter_map(|id| self.get_road_package(id).ok())
                            .map(|p| p.size_bytes)
                            .sum::<u64>();
                    }
                }
                for id in t.road_package_ids {
                    if !selected.contains(&id) {
                        selected.push(id);
                    }
                }
            }
        }
        for source in &mut report.sources {
            if source.package_id.is_empty()
                && let Some(package) = selected
                    .iter()
                    .filter_map(|id| self.get_road_package(id).ok())
                    .find(|p| p.region_id == source.region_id)
            {
                source.package_id = package.id;
                source.size_bytes = package.size_bytes;
            }
        }
        let ready: BTreeSet<_> = report
            .sources
            .iter()
            .filter(|s| !s.package_id.is_empty())
            .map(|s| s.region_id.clone())
            .collect();
        let availability = catalog
            .plan_with_budget(area, &BTreeMap::new(), &ready, 0)
            .map_err(geo_error)?;
        report.coverage_complete = availability.coverage_complete;
        report.uncovered_fraction = availability.uncovered_fraction;
        self.source_report(task, &report, &selected)?;
        let mut s = self.state()?;
        let mut t = s.store.task(&task.id)?;
        t.road_package_ids = selected.clone();
        s.store.update_task(&t)?;
        Ok(selected)
    }
}

#[cfg(test)]
mod offline_cache_tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../corpus")
                .join(format!(
                    "offline-road-cache-{}-{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn runtime(&self, url: &str) -> Runtime {
            let mut config = Config {
                data_dir: self.0.clone(),
                budget: ThreadBudget { total_threads: 1 },
                ..Default::default()
            };
            config.geo.auto_prefetch = false;
            config.geo.catalog_url = url.into();
            Runtime::open(config, std::env::current_exe().unwrap(), vec![]).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../corpus")
                .canonicalize()
                .unwrap();
            let actual = self.0.canonicalize().unwrap();
            assert!(actual.starts_with(&root) && actual != root);
            fs::remove_dir_all(actual).unwrap();
        }
    }
    struct CatalogProbe {
        url: String,
        hits: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl CatalogProbe {
        fn new() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}/catalog", listener.local_addr().unwrap());
            let hits = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let counter = hits.clone();
            let stopping = stop.clone();
            let thread = std::thread::spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            counter.fetch_add(1, Ordering::AcqRel);
                            let _ = stream.write_all(b"HTTP/1.1 502 Upstream Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2))
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                url,
                hits,
                stop,
                thread: Some(thread),
            }
        }
    }
    impl Drop for CatalogProbe {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            self.thread.take().unwrap().join().unwrap();
        }
    }
    fn polygon(west: f64, east: f64) -> String {
        serde_json::json!({"type":"Polygon","coordinates":[[[west,9.0],[east,9.0],[east,11.0],[west,11.0],[west,9.0]]]}).to_string()
    }
    fn input() -> engine::SolveRequest {
        engine::SolveRequest {
            problem: Some(engine::Problem {
                locations: vec![engine::Location {
                    lng: 10.0,
                    lat: 10.0,
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        }
    }
    fn setup(service: &Service, packages: Vec<pb::RoadPackage>) -> pb::Task {
        let mut state = service.state().unwrap();
        for package in packages {
            let path = service
                .inner
                .config
                .data_dir
                .join(format!("{}.pbf", package.id));
            // Selection uses the already-validated package record; decoding is
            // separately covered by the PBF/worker acceptance tests.
            fs::write(&path, b"retained").unwrap();
            state.store.put_road_package(&package, &path).unwrap();
        }
        state
            .store
            .insert_task(
                pb::Task {
                    kind: pb::TaskKind::GeoPlan as i32,
                    state: pb::TaskState::Succeeded as i32,
                    ..Default::default()
                },
                &pb::TaskInput::default(),
            )
            .unwrap()
    }
    fn package(id: &str, coverage: String, bounds: Option<pb::GeoBounds>) -> pb::RoadPackage {
        pb::RoadPackage {
            id: id.into(),
            region_id: id.into(),
            coverage_geojson: coverage,
            bounds,
            enabled: true,
            state: pb::RoadPackageState::Ready as i32,
            size_bytes: 8,
            ..Default::default()
        }
    }
    fn report(service: &Service, task: &pb::Task) -> pb::SourceSelectionReport {
        let path = service
            .state()
            .unwrap()
            .store
            .artifact_path(&task.id, "sources")
            .unwrap();
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn cached_polygon_union_without_header_bounds_skips_catalog_http() {
        let fixture = Fixture::new();
        let probe = CatalogProbe::new();
        let runtime = fixture.runtime(&probe.url);
        let service = &runtime.service;
        let task = setup(
            service,
            vec![
                package("west", polygon(9.0, 10.0), None),
                package("east", polygon(10.0, 11.0), None),
            ],
        );
        let selected = service
            .ensure_packages(&task, &input(), &AtomicBool::new(false))
            .unwrap();
        assert_eq!(selected.len(), 2);
        assert_eq!(
            probe.hits.load(Ordering::Acquire),
            0,
            "local coverage must not depend on catalog availability"
        );
        let report = report(service, &task);
        assert!(report.coverage_complete);
        assert_eq!(report.uncovered_fraction, 0.0);
        assert_eq!(report.additional_download_bytes, 0);
    }

    #[test]
    fn unavailable_catalog_retains_partial_polygons_and_only_header_candidates() {
        let fixture = Fixture::new();
        let runtime = fixture.runtime("");
        let service = &runtime.service;
        let task = setup(
            service,
            vec![
                package("west", polygon(9.0, 10.0), None),
                package(
                    "candidate",
                    String::new(),
                    Some(pb::GeoBounds {
                        west: 9.0,
                        east: 11.0,
                        south: 9.0,
                        north: 11.0,
                    }),
                ),
                package("unknown", String::new(), None),
                package(
                    "outside",
                    polygon(30.0, 31.0),
                    Some(pb::GeoBounds {
                        west: 9.0,
                        east: 40.0,
                        south: 9.0,
                        north: 11.0,
                    }),
                ),
            ],
        );
        let selected = service
            .ensure_packages(&task, &input(), &AtomicBool::new(false))
            .unwrap();
        assert!(selected.iter().any(|id| id == "west"));
        assert!(selected.iter().any(|id| id == "candidate"));
        assert!(!selected.iter().any(|id| id == "unknown" || id == "outside"));
        let report = report(service, &task);
        assert!(!report.coverage_complete);
        assert!(
            (0.4..0.6).contains(&report.uncovered_fraction),
            "{report:?}"
        );
        assert!(report.reason.contains("cannot prove completeness"));
    }
}
