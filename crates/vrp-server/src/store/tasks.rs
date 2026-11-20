use super::*;
use sha2::Digest;
use std::path::PathBuf;

#[derive(Debug, Default)]
pub(crate) struct DeletedTasks {
    pub(crate) artifact_paths: Vec<PathBuf>,
    pub(crate) task_ids: Vec<String>,
}

impl Store {
    /// Only explicit saves append to this outbox. Opening an existing database
    /// deliberately does not enqueue its historical scenarios.
    pub(crate) fn pending_geo(&self, limit: usize) -> Result<Vec<(String, u64)>> {
        let mut statement = self
            .conn
            .prepare("SELECT scenario_id,version FROM geo_pending ORDER BY rowid LIMIT ?1")?;
        Ok(statement
            .query_map([limit as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub(crate) fn finish_geo_pending(&mut self, id: &str, version: u64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM geo_pending WHERE scenario_id=?1 AND version=?2",
            params![id, version as i64],
        )?;
        Ok(())
    }

    pub(crate) fn consume_geo_pending(
        &mut self,
        id: &str,
        version: u64,
        task: pb::Task,
        input: &pb::TaskInput,
    ) -> Result<Option<pb::Task>> {
        self.atomic(|store| {
            let pending: bool = store.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM geo_pending WHERE scenario_id=?1 AND version=?2)",
                params![id, version as i64],
                |r| r.get(0),
            )?;
            if !pending {
                return Ok(None);
            }
            let latest: i64 = store.conn.query_row(
                "SELECT coalesce(max(version),0) FROM scenarios WHERE id=?1",
                [id],
                |r| r.get(0),
            )?;
            if latest as u64 != version || store.operation(id, 0)?.is_some() {
                store.finish_geo_pending(id, version)?;
                return Ok(None);
            }
            if !task
                .scenario
                .as_ref()
                .is_some_and(|r| r.scenario_id == id && r.version == version)
                || task.idempotency_key.is_empty()
            {
                return Err(AppError::internal(
                    "prefetch does not match its saved scenario",
                ));
            }
            // Coordinates, rather than business version, identify a prefetch.
            // A later save with unchanged coordinates reuses the earlier task.
            let old: Option<Vec<u8>> = store
                .conn
                .query_row(
                    "SELECT meta FROM generic_tasks WHERE idem=?1",
                    [&task.idempotency_key],
                    |r| r.get(0),
                )
                .optional()?;
            let task = match old {
                Some(bytes) => pb::Task::decode(bytes.as_slice())?,
                None => store.insert_task(task, input)?,
            };
            store.finish_geo_pending(id, version)?;
            Ok(Some(task))
        })
    }

    /// Follow retries only while they refer to the same committed scene.
    pub(crate) fn workflow_attempts(&self, id: &str, version: u64) -> Result<Vec<String>> {
        let mut attempts = Vec::new();
        let mut next = id.to_owned();
        while !next.is_empty() {
            if attempts.len() >= 64 || attempts.contains(&next) {
                return Err(AppError::conflict("invalid task retry lineage"));
            }
            let task = self.task(&next)?;
            if task.result_scenario_version != version {
                break;
            }
            next = task.source_task_id;
            attempts.push(task.id);
        }
        Ok(attempts)
    }

    pub(crate) fn job_for_key(&self, key: &str) -> Result<Option<pb::Job>> {
        let bytes: Option<Vec<u8>> = self
            .conn
            .query_row("SELECT meta FROM jobs WHERE idem=?1", [key], |r| r.get(0))
            .optional()?;
        bytes
            .map(|b| Ok(pb::Job::decode(b.as_slice())?))
            .transpose()
    }

    pub(crate) fn init_tasks(&mut self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS generic_tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT, meta BLOB NOT NULL,
                input BLOB NOT NULL, idem TEXT UNIQUE, fingerprint BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS task_artifacts (
                task_id TEXT NOT NULL, id TEXT NOT NULL, meta BLOB NOT NULL,
                path TEXT NOT NULL, PRIMARY KEY(task_id,id));
             CREATE TABLE IF NOT EXISTS road_packages (
                id TEXT PRIMARY KEY, region TEXT NOT NULL, meta BLOB NOT NULL,
                path TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS task_solver_links (
                job_id TEXT PRIMARY KEY, parent_id TEXT NOT NULL);",
        )?;
        // Base Store::open accounts the existing scenario/solver tables. Include
        // the new records before recovery so quota counters also survive restart.
        let (bytes, records): (i64, i64) = self.conn.query_row(
            "SELECT coalesce(sum(length(meta)+length(input)+length(fingerprint)),0),count(*) FROM generic_tasks",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        self.payload_bytes = self.payload_bytes.saturating_add(bytes.max(0) as u64);
        self.records = self.records.saturating_add(records.max(0) as u64);
        for table in ["task_artifacts", "road_packages"] {
            let sql = format!("SELECT meta FROM {table}");
            let mut statement = self.conn.prepare(&sql)?;
            for row in statement.query_map([], |r| r.get::<_, Vec<u8>>(0))? {
                let bytes = row?;
                let data = if table == "task_artifacts" {
                    pb::TaskArtifact::decode(bytes.as_slice())?.size_bytes
                } else {
                    pb::RoadPackage::decode(bytes.as_slice())?.size_bytes
                };
                self.payload_bytes = self.payload_bytes.saturating_add(data + bytes.len() as u64);
                self.records = self.records.saturating_add(1);
            }
        }
        for mut task in self.tasks()? {
            if !super::super::runtime::tasks::task_terminal(&task) {
                task.state = pb::TaskState::Interrupted as i32;
                task.finished_unix_ms = now_ms();
                task.error = Some(pb::Error {
                    code: "interrupted".into(),
                    message: "server restarted; retry from the retained task artifacts".into(),
                });
                self.update_task(&task)?;
            }
        }
        // Upgrade presentation metadata once at open; list/metadata polling
        // must not decode the large matrix request on every refresh.
        let scenes = {
            let mut statement = self
                .conn
                .prepare("SELECT id,version,meta,request FROM scenarios")?;
            statement
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Vec<u8>>(2)?,
                        r.get::<_, Vec<u8>>(3)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (id, version, bytes, input) in scenes {
            let mut scene = pb::Scenario::decode(bytes.as_slice())?;
            let request = vrp_proto::engine::v1::SolveRequest::decode(input.as_slice())?;
            let ready = vrp_model::validate_ready_matrices(&request).is_ok();
            if scene.matrix_ready != ready {
                scene.matrix_ready = ready;
                self.conn.execute(
                    "UPDATE scenarios SET meta=?3 WHERE id=?1 AND version=?2",
                    params![id, version, scene.encode_to_vec()],
                )?;
                self.payload_bytes = self
                    .payload_bytes
                    .saturating_sub(bytes.len() as u64)
                    .saturating_add(scene.encoded_len() as u64);
            }
        }
        Ok(())
    }

    pub(crate) fn insert_task(
        &mut self,
        mut task: pb::Task,
        input: &pb::TaskInput,
    ) -> Result<pb::Task> {
        let bytes = input.encode_to_vec();
        let fingerprint = sha2::Sha256::digest(&bytes).to_vec();
        if !task.idempotency_key.is_empty() {
            let old: Option<(Vec<u8>, Vec<u8>)> = self
                .conn
                .query_row(
                    "SELECT meta,fingerprint FROM generic_tasks WHERE idem=?1",
                    [&task.idempotency_key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((meta, hash)) = old {
                if hash != fingerprint {
                    return Err(AppError::conflict(
                        "idempotency key has different task input",
                    ));
                }
                return Ok(pb::Task::decode(meta.as_slice())?);
            }
        }
        self.capacity(
            bytes.len() + task.encoded_len() + fingerprint.len() + 64,
            true,
        )?;
        let idem = (!task.idempotency_key.is_empty()).then_some(task.idempotency_key.as_str());
        self.conn.execute(
            "INSERT INTO generic_tasks(meta,input,idem,fingerprint) VALUES(?1,?2,?3,?4)",
            params![task.encode_to_vec(), bytes, idem, fingerprint],
        )?;
        task.id = format!("task-{}", self.conn.last_insert_rowid());
        task.revision = 1;
        self.conn.execute(
            "UPDATE generic_tasks SET meta=?2 WHERE id=?1",
            params![task_number(&task.id)?, task.encode_to_vec()],
        )?;
        self.records += 1;
        self.payload_bytes += (bytes.len() + task.encoded_len() + fingerprint.len()) as u64;
        Ok(task)
    }

    pub(crate) fn update_task(&mut self, task: &pb::Task) -> Result<()> {
        let old = self.task(&task.id)?;
        let mut next = task.clone();
        next.revision = old
            .revision
            .checked_add(1)
            .ok_or_else(|| AppError::internal("task revision overflow"))?;
        let previous = old.encoded_len() as u64;
        let current = next.encoded_len() as u64;
        self.capacity(current.saturating_sub(previous) as usize, false)?;
        self.conn.execute(
            "UPDATE generic_tasks SET meta=?2 WHERE id=?1",
            params![task_number(&next.id)?, next.encode_to_vec()],
        )?;
        self.payload_bytes = self
            .payload_bytes
            .saturating_sub(previous)
            .saturating_add(current);
        Ok(())
    }

    pub(crate) fn task(&self, id: &str) -> Result<pb::Task> {
        let bytes: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT meta FROM generic_tasks WHERE id=?1",
                [task_number(id)?],
                |r| r.get(0),
            )
            .optional()?;
        Ok(pb::Task::decode(
            bytes
                .ok_or_else(|| AppError::new("not_found", "task not found"))?
                .as_slice(),
        )?)
    }

    pub(crate) fn task_input(&self, id: &str) -> Result<pb::TaskInput> {
        let bytes: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT input FROM generic_tasks WHERE id=?1",
                [task_number(id)?],
                |r| r.get(0),
            )
            .optional()?;
        Ok(pb::TaskInput::decode(
            bytes
                .ok_or_else(|| AppError::new("not_found", "task not found"))?
                .as_slice(),
        )?)
    }

    pub(crate) fn tasks(&self) -> Result<Vec<pb::Task>> {
        let mut statement = self
            .conn
            .prepare("SELECT meta FROM generic_tasks ORDER BY id DESC")?;
        statement
            .query_map([], |r| r.get::<_, Vec<u8>>(0))?
            .map(|r| Ok(pb::Task::decode(r?.as_slice())?))
            .collect()
    }

    pub(crate) fn link_solver(&mut self, id: &str, parent: &str) -> Result<()> {
        self.job(crate::job_id(id)?)?;
        self.task(parent)?;
        if self.solver_parent(id)?.is_some_and(|old| old != parent) {
            return Err(AppError::conflict(
                "solver already belongs to a different workflow",
            ));
        }
        self.conn.execute(
            "INSERT OR IGNORE INTO task_solver_links(job_id,parent_id) VALUES(?1,?2)",
            params![id, parent],
        )?;
        Ok(())
    }

    pub(crate) fn solver_parent(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT parent_id FROM task_solver_links WHERE job_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub(crate) fn put_artifact(&mut self, artifact: &pb::TaskArtifact, path: &Path) -> Result<()> {
        self.task(&artifact.task_id)?;
        let previous: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT meta FROM task_artifacts WHERE task_id=?1 AND id=?2",
                params![artifact.task_id, artifact.id],
                |r| r.get(0),
            )
            .optional()?;
        let previous_bytes = previous
            .as_ref()
            .map(|b| pb::TaskArtifact::decode(b.as_slice()).map(|a| a.size_bytes + b.len() as u64))
            .transpose()?
            .unwrap_or(0);
        let current_bytes = artifact
            .size_bytes
            .checked_add(artifact.encoded_len() as u64)
            .ok_or_else(|| AppError::invalid("artifact size overflow"))?;
        self.capacity(
            current_bytes.saturating_sub(previous_bytes) as usize,
            previous.is_none(),
        )?;
        self.conn.execute(
            "INSERT INTO task_artifacts(task_id,id,meta,path) VALUES(?1,?2,?3,?4)
             ON CONFLICT(task_id,id) DO UPDATE SET meta=excluded.meta,path=excluded.path",
            params![
                artifact.task_id,
                artifact.id,
                artifact.encode_to_vec(),
                path.to_string_lossy().as_ref()
            ],
        )?;
        self.payload_bytes = self
            .payload_bytes
            .saturating_sub(previous_bytes)
            .saturating_add(current_bytes);
        if previous.is_none() {
            self.records += 1;
        }
        Ok(())
    }

    pub(crate) fn artifacts(&self, id: &str) -> Result<Vec<pb::TaskArtifact>> {
        let mut statement = self
            .conn
            .prepare("SELECT meta FROM task_artifacts WHERE task_id=?1 ORDER BY id")?;
        statement
            .query_map([id], |r| r.get::<_, Vec<u8>>(0))?
            .map(|r| Ok(pb::TaskArtifact::decode(r?.as_slice())?))
            .collect()
    }

    pub(crate) fn artifact_path(&self, task: &str, id: &str) -> Result<PathBuf> {
        let path: Option<String> = self
            .conn
            .query_row(
                "SELECT path FROM task_artifacts WHERE task_id=?1 AND id=?2",
                params![task, id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(PathBuf::from(path.ok_or_else(|| {
            AppError::new("not_found", "artifact not found")
        })?))
    }

    pub(super) fn task_references_solver(&self, task: &pb::Task, job: &str) -> Result<bool> {
        Ok(task.result_job_id == job
            || task.dependency_ids.iter().any(|id| id == job)
            || matches!(self.task_input(&task.id)?.input,
                Some(pb::task_input::Input::Geometry(geometry)) if geometry.job_id == job))
    }

    pub(crate) fn delete_plan(&mut self, id: i64) -> Result<DeletedTasks> {
        let job = self.job(id)?;
        if let Some(parent) = self.solver_parent(&job.id)? {
            let owner = self.task(&parent)?;
            if owner.kind() != pb::TaskKind::GeoPlan {
                return Err(AppError::conflict(format!(
                    "plan is owned by retained task {parent}; delete that task first"
                )));
            }
            return self.delete_generic_task(&parent);
        }
        self.delete(id)?;
        Ok(DeletedTasks::default())
    }

    pub(crate) fn delete_generic_task(&mut self, id: &str) -> Result<DeletedTasks> {
        self.atomic(|store| {
            store.retain_committed_matrix(id)?;
            store.delete_generic_task_inner(id)
        })
    }

    /// A plan result is disposable; its committed scene matrix is a separate
    /// retained asset. Promote the exact producing snapshot before collection.
    fn retain_committed_matrix(&mut self, id: &str) -> Result<()> {
        let owner = self.task(id)?;
        if owner.kind() != pb::TaskKind::GeoPlan || owner.result_scenario_version == 0 {
            return Ok(());
        }
        let Some(reference) = owner.scenario else {
            return Ok(());
        };
        let pair: Option<(Vec<u8>, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT meta,request FROM scenarios WHERE id=?1 AND version=?2",
                params![reference.scenario_id, owner.result_scenario_version as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((bytes, request)) = pair else {
            return Ok(());
        };
        let mut scene = pb::Scenario::decode(bytes.as_slice())?;
        let expected = format!("{:x}", sha2::Sha256::digest(&request));
        for mut matrix in self.tasks()?.into_iter().filter(|t| {
            t.parent_id == id
                && t.kind() == pb::TaskKind::MatrixPrepare
                && t.state() == pb::TaskState::Succeeded
        }) {
            let artifacts = self.artifacts(&matrix.id)?;
            if !artifacts.iter().any(|a| a.id == "network") {
                continue;
            }
            if !artifacts
                .iter()
                .any(|a| a.id == "request" && a.sha256 == expected)
            {
                return Err(AppError::conflict(
                    "saved scene matrix provenance differs from its workflow artifact; retain it for inspection",
                ));
            }
            matrix.parent_id.clear();
            matrix.scenario = Some(pb::ScenarioRef {
                scenario_id: scene.id.clone(),
                version: scene.version,
            });
            self.update_task(&matrix)?;
            scene.geo = Some(pb::ScenarioGeoStatus {
                task_id: matrix.id,
                status: "Succeeded".into(),
                coordinate_hash: matrix.coordinate_hash,
                road_package_ids: matrix.road_package_ids,
                ..Default::default()
            });
        }
        let updated = scene.encode_to_vec();
        // This promotion belongs to a collection transaction which removes the
        // larger controller/solver payload. Do not require spare quota merely
        // to delete an existing plan from a full store.
        self.conn.execute(
            "UPDATE scenarios SET meta=?3 WHERE id=?1 AND version=?2",
            params![scene.id, scene.version as i64, updated],
        )?;
        self.payload_bytes = self
            .payload_bytes
            .saturating_sub(bytes.len() as u64)
            .saturating_add(updated.len() as u64);
        Ok(())
    }

    fn delete_generic_task_inner(&mut self, id: &str) -> Result<DeletedTasks> {
        self.task(id)?;
        let tasks = self.tasks()?;
        let mut ids = std::collections::BTreeSet::from([id.to_owned()]);
        // A completed workflow can be collected as a unit. Requiring each leaf
        // to be removed first would deadlock its parent/dependency references.
        loop {
            let previous = ids.len();
            for task in &tasks {
                if ids.contains(&task.parent_id) {
                    ids.insert(task.id.clone());
                }
            }
            if previous == ids.len() {
                break;
            }
        }
        if tasks.iter().any(|task| {
            ids.contains(&task.id) && !super::super::runtime::tasks::task_terminal(task)
        }) {
            return Err(AppError::conflict(
                "only terminal task trees can be deleted",
            ));
        }
        if let Some(other) = tasks.iter().find(|other| {
            !ids.contains(&other.id)
                && (ids.contains(&other.source_task_id)
                    || other.dependency_ids.iter().any(|v| ids.contains(v)))
        }) {
            return Err(AppError::conflict(format!(
                "workflow is referenced by retained task {}; delete that task first",
                other.id
            )));
        }
        let mut solvers = std::collections::BTreeSet::new();
        for task_id in &ids {
            let mut statement = self
                .conn
                .prepare("SELECT job_id FROM task_solver_links WHERE parent_id=?1")?;
            for job in statement.query_map([task_id], |r| r.get::<_, String>(0))? {
                solvers.insert(job?);
            }
        }
        for other in tasks.iter().filter(|other| !ids.contains(&other.id)) {
            for solver in &solvers {
                if self.task_references_solver(other, solver)? {
                    return Err(AppError::conflict(format!(
                        "plan is referenced by retained task {}; delete that task first",
                        other.id
                    )));
                }
            }
        }
        let mut statement = self.conn.prepare("SELECT meta FROM scenarios")?;
        for row in statement.query_map([], |r| r.get::<_, Vec<u8>>(0))? {
            if pb::Scenario::decode(row?.as_slice())?
                .geo
                .is_some_and(|geo| ids.contains(&geo.task_id))
            {
                return Err(AppError::conflict(
                    "task is referenced by a retained scenario version",
                ));
            }
        }
        drop(statement);
        let mut paths = vec![];
        let mut total = 0;
        let mut records = ids.len() as u64;
        for task in tasks.iter().filter(|t| ids.contains(&t.id)) {
            let artifacts = self.artifacts(&task.id)?;
            paths.extend(
                artifacts
                    .iter()
                    .map(|a| self.artifact_path(&task.id, &a.id))
                    .collect::<Result<Vec<_>>>()?,
            );
            total += task.encoded_len() as u64
                + self.task_input(&task.id)?.encoded_len() as u64
                + 32
                + artifacts
                    .iter()
                    .map(|a| a.encoded_len() as u64 + a.size_bytes)
                    .sum::<u64>();
            records += artifacts.len() as u64;
        }
        for other in tasks.iter().filter(|t| !ids.contains(&t.id)) {
            let input = self.task_input(&other.id)?;
            let network = match input.input {
                Some(pb::task_input::Input::Geometry(g)) => Some(g.network_artifact),
                Some(pb::task_input::Input::Matrix(m)) => Some(m.network_artifact),
                _ => None,
            };
            if network
                .as_ref()
                .is_some_and(|network| paths.iter().any(|p| p.to_string_lossy() == *network))
            {
                return Err(AppError::conflict(format!(
                    "road snapshot is used by retained task {}; delete that task first",
                    other.id
                )));
            }
            for artifact in self.artifacts(&other.id)? {
                if paths.contains(&self.artifact_path(&other.id, &artifact.id)?) {
                    return Err(AppError::conflict(format!(
                        "artifact is shared with retained task {}; delete that task first",
                        other.id
                    )));
                }
            }
        }
        self.atomic(|store| {
            let mut solver_ids = solvers
                .iter()
                .map(|v| crate::job_id(v))
                .collect::<Result<Vec<_>>>()?;
            solver_ids.sort_unstable_by(|a, b| b.cmp(a));
            for solver in solver_ids {
                // Preserve the original execution audit and derived-job checks.
                store.delete_job_record(solver)?;
            }
            for task_id in &ids {
                store.conn.execute(
                    "DELETE FROM task_solver_links WHERE parent_id=?1",
                    [task_id],
                )?;
                store
                    .conn
                    .execute("DELETE FROM task_artifacts WHERE task_id=?1", [task_id])?;
                store.conn.execute(
                    "DELETE FROM generic_tasks WHERE id=?1",
                    [task_number(task_id)?],
                )?;
            }
            store.payload_bytes = store.payload_bytes.saturating_sub(total);
            store.records = store.records.saturating_sub(records);
            Ok(())
        })?;
        Ok(DeletedTasks {
            artifact_paths: paths,
            task_ids: ids.into_iter().collect(),
        })
    }

    pub(crate) fn put_road_package(
        &mut self,
        package: &pb::RoadPackage,
        path: &Path,
    ) -> Result<()> {
        let old: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT meta FROM road_packages WHERE id=?1",
                [&package.id],
                |r| r.get(0),
            )
            .optional()?;
        let previous = old
            .as_ref()
            .map(|b| pb::RoadPackage::decode(b.as_slice()))
            .transpose()?;
        if previous
            .as_ref()
            .is_some_and(|p| p.sha256 != package.sha256 || p.size_bytes != package.size_bytes)
        {
            return Err(AppError::conflict("road package content is immutable"));
        }
        let previous_bytes = previous
            .as_ref()
            .map_or(0, |p| p.size_bytes + old.as_ref().unwrap().len() as u64);
        let current_bytes = package
            .size_bytes
            .checked_add(package.encoded_len() as u64)
            .ok_or_else(|| AppError::invalid("road package size overflow"))?;
        self.capacity(
            current_bytes.saturating_sub(previous_bytes) as usize,
            old.is_none(),
        )?;
        self.conn.execute(
            "INSERT INTO road_packages(id,region,meta,path) VALUES(?1,?2,?3,?4)
             ON CONFLICT(id) DO UPDATE SET meta=excluded.meta,path=excluded.path",
            params![
                package.id,
                package.region_id,
                package.encode_to_vec(),
                path.to_string_lossy().as_ref()
            ],
        )?;
        self.payload_bytes = self
            .payload_bytes
            .saturating_sub(previous_bytes)
            .saturating_add(current_bytes);
        if old.is_none() {
            self.records += 1;
        }
        Ok(())
    }

    pub(crate) fn road_packages(&self) -> Result<Vec<pb::RoadPackage>> {
        let mut statement = self
            .conn
            .prepare("SELECT meta FROM road_packages ORDER BY id")?;
        let packages: Vec<_> = statement
            .query_map([], |r| r.get::<_, Vec<u8>>(0))?
            .map(|r| Ok(pb::RoadPackage::decode(r?.as_slice())?))
            .collect::<Result<_>>()?;
        let tasks = self.tasks()?;
        Ok(packages
            .into_iter()
            .map(|mut p| {
                // A package's producing download/import task is an audit record,
                // not a consumer. Otherwise every source would be permanently
                // uncollectable immediately after its successful preparation.
                p.reference_count = tasks
                    .iter()
                    .filter(|t| {
                        t.kind != pb::TaskKind::RoadPackagePrepare as i32
                            && t.road_package_ids.contains(&p.id)
                    })
                    .count() as u64;
                p
            })
            .collect())
    }

    pub(crate) fn road_package(&self, id: &str) -> Result<pb::RoadPackage> {
        self.road_packages()?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| AppError::new("not_found", "road package not found"))
    }

    pub(crate) fn road_path(&self, id: &str) -> Result<PathBuf> {
        let path: Option<String> = self
            .conn
            .query_row("SELECT path FROM road_packages WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(PathBuf::from(path.ok_or_else(|| {
            AppError::new("not_found", "road package not found")
        })?))
    }

    pub(crate) fn delete_road_package(&mut self, id: &str) -> Result<PathBuf> {
        let package = self.road_package(id)?;
        if package.reference_count > 0 {
            return Err(AppError::conflict(
                "road package is referenced by retained tasks",
            ));
        }
        let path = self.road_path(id)?;
        self.conn
            .execute("DELETE FROM road_packages WHERE id=?1", [id])?;
        self.payload_bytes = self
            .payload_bytes
            .saturating_sub(package.size_bytes + package.encoded_len() as u64);
        self.records = self.records.saturating_sub(1);
        Ok(path)
    }
}

pub(super) fn task_number(id: &str) -> Result<i64> {
    id.strip_prefix("task-")
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0 && format!("task-{v}") == id)
        .ok_or_else(|| AppError::invalid("invalid task ID"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../corpus")
                .join(format!(
                    "task-store-{}-{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn open(&self) -> Store {
            let mut store = Store::open(&self.0, 1024 * 1024, 100).unwrap();
            store.init_tasks().unwrap();
            store
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../corpus")
                .canonicalize()
                .unwrap();
            let target = self.0.canonicalize().unwrap();
            assert!(target.starts_with(&root) && target != root);
            std::fs::remove_dir_all(target).unwrap();
        }
    }
    fn finished_task(store: &mut Store) -> pb::Task {
        store
            .insert_task(
                pb::Task {
                    kind: pb::TaskKind::MatrixPrepare as i32,
                    state: pb::TaskState::Succeeded as i32,
                    ..Default::default()
                },
                &pb::TaskInput::default(),
            )
            .unwrap()
    }
    fn save_request(expected_version: u64) -> pb::SaveScenarioRequest {
        pb::SaveScenarioRequest {
            scenario_id: "outbox-scene".into(),
            expected_version,
            request: Some(vrp_proto::engine::v1::SolveRequest {
                problem: Some(vrp_proto::engine::v1::Problem {
                    locations: vec![vrp_proto::engine::v1::Location {
                        lng: 121.0,
                        lat: 31.0,
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
    fn prefetch(scene: &pb::Scenario) -> (pb::Task, pb::TaskInput) {
        (
            pb::Task {
                kind: pb::TaskKind::RoadPackagePrepare as i32,
                scenario: Some(pb::ScenarioRef {
                    scenario_id: scene.id.clone(),
                    version: scene.version,
                }),
                idempotency_key: format!("prefetch:{}:same-coordinates", scene.id),
                ..Default::default()
            },
            pb::TaskInput::default(),
        )
    }
    #[test]
    fn scene_save_and_prefetch_consumption_survive_both_restart_windows() {
        let f = Fixture::new();
        let mut store = f.open();
        let request = save_request(0);
        let scene = store
            .save_scenario(&request, &request.request.as_ref().unwrap().encode_to_vec())
            .unwrap();
        drop(store); // Saved, but dispatcher never ran.
        let mut store = f.open();
        assert_eq!(store.pending_geo(10).unwrap(), vec![(scene.id.clone(), 1)]);
        assert!(store.tasks().unwrap().is_empty());
        let (task, data) = prefetch(&scene);
        let task = store
            .consume_geo_pending(&scene.id, 1, task, &data)
            .unwrap()
            .unwrap();
        drop(store); // Task committed, dispatcher had not started its worker.
        let mut store = f.open();
        assert!(store.pending_geo(10).unwrap().is_empty());
        assert_eq!(store.tasks().unwrap().len(), 1);
        assert_eq!(
            store.task(&task.id).unwrap().state(),
            pb::TaskState::Interrupted
        );
        let (again, data) = prefetch(&scene);
        assert!(
            store
                .consume_geo_pending(&scene.id, 1, again, &data)
                .unwrap()
                .is_none()
        );
        // A new business version with identical coordinates reuses that task.
        let request = save_request(1);
        let next = store
            .save_scenario(&request, &request.request.as_ref().unwrap().encode_to_vec())
            .unwrap();
        let (again, data) = prefetch(&next);
        assert_eq!(
            store
                .consume_geo_pending(&next.id, 2, again, &data)
                .unwrap()
                .unwrap()
                .id,
            task.id
        );
        assert_eq!(store.tasks().unwrap().len(), 1);
    }
    #[test]
    fn failed_outbox_write_or_ack_rolls_back_payload_and_quota() {
        let f = Fixture::new();
        let mut store = f.open();
        let request = save_request(0);
        let bytes = request.request.as_ref().unwrap().encode_to_vec();
        store.conn.execute_batch("CREATE TEMP TRIGGER fail_enqueue BEFORE INSERT ON geo_pending BEGIN SELECT RAISE(ABORT,'crash before enqueue'); END;").unwrap();
        assert!(store.save_scenario(&request, &bytes).is_err());
        assert!(store.scenario_metadata(&request.scenario_id, 0).is_err());
        assert_eq!((store.payload_bytes, store.records), (0, 0));
        store
            .conn
            .execute_batch("DROP TRIGGER fail_enqueue;")
            .unwrap();
        let scene = store.save_scenario(&request, &bytes).unwrap();
        let accounting = (store.payload_bytes, store.records);
        store.conn.execute_batch("CREATE TEMP TRIGGER fail_ack BEFORE DELETE ON geo_pending BEGIN SELECT RAISE(ABORT,'crash before ack'); END;").unwrap();
        let (task, data) = prefetch(&scene);
        assert!(
            store
                .consume_geo_pending(&scene.id, 1, task, &data)
                .is_err()
        );
        assert!(store.tasks().unwrap().is_empty());
        assert_eq!((store.payload_bytes, store.records), accounting);
        assert_eq!(store.pending_geo(10).unwrap().len(), 1);
        drop(store);
        let mut store = f.open();
        let (task, data) = prefetch(&scene);
        store
            .consume_geo_pending(&scene.id, 1, task, &data)
            .unwrap()
            .unwrap();
        assert!(store.pending_geo(10).unwrap().is_empty());
        assert_eq!(store.tasks().unwrap().len(), 1);
    }
    #[test]
    fn superseded_saves_are_skipped_and_historical_scenes_are_not_backfilled() {
        let f = Fixture::new();
        let mut store = f.open();
        let first = save_request(0);
        let scene = store
            .save_scenario(&first, &first.request.as_ref().unwrap().encode_to_vec())
            .unwrap();
        let next = save_request(1);
        store
            .save_scenario(&next, &next.request.as_ref().unwrap().encode_to_vec())
            .unwrap();
        let (task, data) = prefetch(&scene);
        assert!(
            store
                .consume_geo_pending(&scene.id, 1, task, &data)
                .unwrap()
                .is_none()
        );
        assert!(store.tasks().unwrap().is_empty());
        store.finish_geo_pending(&scene.id, 2).unwrap();
        drop(store);
        let store = f.open();
        assert!(store.pending_geo(10).unwrap().is_empty());
        assert_eq!(store.scenario_metadata(&scene.id, 0).unwrap().version, 2);
    }
    #[test]
    fn workflow_retry_origin_survives_multiple_missing_solver_checkpoints() {
        let f = Fixture::new();
        let mut store = f.open();
        let mut oldest = finished_task(&mut store);
        oldest.result_scenario_version = 2;
        store.update_task(&oldest).unwrap();
        let mut middle = finished_task(&mut store);
        middle.result_scenario_version = 2;
        middle.source_task_id = oldest.id.clone();
        store.update_task(&middle).unwrap();
        let mut latest = finished_task(&mut store);
        latest.result_scenario_version = 2;
        latest.source_task_id = middle.id.clone();
        store.update_task(&latest).unwrap();
        assert_eq!(
            store.workflow_attempts(&latest.id, 2).unwrap(),
            vec![latest.id.clone(), middle.id.clone(), oldest.id.clone()]
        );
        let key = format!("geo-plan:{}:solve", oldest.id);
        let job = store
            .insert_job(pb::Job::default(), b"frozen", &key, b"hash")
            .unwrap();
        assert_eq!(store.job_for_key(&key).unwrap().unwrap().id, job.id);
        middle.result_scenario_version = 1;
        store.update_task(&middle).unwrap();
        assert_eq!(
            store.workflow_attempts(&latest.id, 2).unwrap(),
            vec![latest.id]
        );
    }
    #[test]
    fn replacing_artifacts_and_restarting_preserves_quota_accounting() {
        let f = Fixture::new();
        let mut store = f.open();
        let task = finished_task(&mut store);
        let artifact = pb::TaskArtifact {
            task_id: task.id.clone(),
            id: "request".into(),
            size_bytes: 4,
            ..Default::default()
        };
        let path = f.0.join("artifact.pb");
        std::fs::write(&path, b"data").unwrap();
        store.put_artifact(&artifact, &path).unwrap();
        let bytes = store.payload_bytes;
        let records = store.records;
        store.put_artifact(&artifact, &path).unwrap();
        assert_eq!(store.payload_bytes, bytes);
        assert_eq!(store.records, records);
        drop(store);
        let store = f.open();
        assert_eq!(store.payload_bytes, bytes);
        assert_eq!(store.records, records);
    }
    #[test]
    fn packages_are_accounted_once_and_references_protect_content() {
        let f = Fixture::new();
        let mut store = f.open();
        let package = pb::RoadPackage {
            id: "road-a".into(),
            sha256: "a".into(),
            size_bytes: 4,
            ..Default::default()
        };
        let path = f.0.join("a.osm.pbf");
        std::fs::write(&path, b"data").unwrap();
        store.put_road_package(&package, &path).unwrap();
        let bytes = store.payload_bytes;
        store.put_road_package(&package, &path).unwrap();
        assert_eq!(store.payload_bytes, bytes);
        let mut producer = finished_task(&mut store);
        producer.kind = pb::TaskKind::RoadPackagePrepare as i32;
        producer.road_package_ids.push(package.id.clone());
        store.update_task(&producer).unwrap();
        assert_eq!(store.road_package(&package.id).unwrap().reference_count, 0);
        let mut changed = package.clone();
        changed.sha256 = "b".into();
        assert!(store.put_road_package(&changed, &path).is_err());
        let mut task = finished_task(&mut store);
        task.road_package_ids.push(package.id.clone());
        store.update_task(&task).unwrap();
        assert!(store.delete_road_package(&package.id).is_err());
        drop(store);
        let store = f.open();
        assert_eq!(store.road_package(&package.id).unwrap().reference_count, 1);
    }
    #[test]
    fn revisions_are_monotonic_even_for_a_stale_snapshot() {
        let f = Fixture::new();
        let mut store = f.open();
        let task = finished_task(&mut store);
        store.update_task(&task).unwrap();
        store.update_task(&task).unwrap();
        assert_eq!(store.task(&task.id).unwrap().revision, 3);
        assert!(task_number("task-01").is_err());
    }
    #[test]
    fn deleting_a_terminal_tree_releases_quota_but_retains_external_dependencies() {
        let f = Fixture::new();
        let mut store = f.open();
        let root = finished_task(&mut store);
        let mut child = finished_task(&mut store);
        child.parent_id = root.id.clone();
        store.update_task(&child).unwrap();
        let mut parent = root.clone();
        parent.dependency_ids.push(child.id.clone());
        store.update_task(&parent).unwrap();
        let mut external = finished_task(&mut store);
        external.dependency_ids.push(child.id.clone());
        store.update_task(&external).unwrap();
        assert!(store.delete_generic_task(&root.id).is_err());
        store.delete_generic_task(&external.id).unwrap();
        store.delete_generic_task(&root.id).unwrap();
        assert_eq!(store.payload_bytes, 0);
        assert_eq!(store.records, 0);
        drop(store);
        assert!(f.open().tasks().unwrap().is_empty());
    }
    #[test]
    fn workflow_collection_checks_solver_audit_and_rolls_back_atomically() {
        let f = Fixture::new();
        let mut store = f.open();
        let root = finished_task(&mut store);
        let solver = store
            .insert_job(
                pb::Job {
                    state: pb::JobState::Succeeded as i32,
                    execution_basis: Some(Default::default()),
                    ..Default::default()
                },
                b"frozen",
                "",
                b"hash",
            )
            .unwrap();
        store.link_solver(&solver.id, &root.id).unwrap();
        let bytes = store.payload_bytes;
        let records = store.records;
        assert!(store.delete_generic_task(&root.id).is_err());
        assert_eq!(store.payload_bytes, bytes);
        assert_eq!(store.records, records);
        assert!(store.task(&root.id).is_ok());
        let mut solver = solver;
        solver.execution_basis = None;
        store
            .conn
            .execute(
                "UPDATE jobs SET meta=?2 WHERE id=?1",
                params![crate::job_id(&solver.id).unwrap(), solver.encode_to_vec()],
            )
            .unwrap();
        store.delete_generic_task(&root.id).unwrap();
        assert_eq!(store.payload_bytes, 0);
        assert_eq!(store.records, 0);
        assert!(store.job(crate::job_id(&solver.id).unwrap()).is_err());
    }

    struct StoredWorkflow {
        parent: pb::Task,
        matrix: pb::Task,
        geometry: pb::Task,
        job: pb::Job,
        frozen: Vec<u8>,
    }
    fn committed_workflow(store: &mut Store, fixture: &Fixture) -> StoredWorkflow {
        let request = save_request(0);
        let frozen = request.request.as_ref().unwrap().encode_to_vec();
        store.save_scenario(&request, &frozen).unwrap();
        store.save_scenario(&save_request(1), &frozen).unwrap();
        let package = pb::RoadPackage {
            id: "retained-road".into(),
            sha256: "pbf-hash".into(),
            size_bytes: 4,
            ..Default::default()
        };
        let package_path = fixture.0.join("retained.pbf");
        std::fs::write(&package_path, b"data").unwrap();
        store.put_road_package(&package, &package_path).unwrap();
        let mut producer = finished_task(store);
        producer.kind = pb::TaskKind::RoadPackagePrepare as i32;
        producer.road_package_ids.push(package.id.clone());
        store.update_task(&producer).unwrap();
        let mut parent = finished_task(store);
        parent.kind = pb::TaskKind::GeoPlan as i32;
        parent.scenario = Some(pb::ScenarioRef {
            scenario_id: request.scenario_id.clone(),
            version: 1,
        });
        parent.result_scenario_version = 2;
        store.update_task(&parent).unwrap();
        let mut matrix = finished_task(store);
        matrix.parent_id = parent.id.clone();
        matrix.scenario = parent.scenario.clone();
        matrix.road_package_ids = vec![package.id];
        store.update_task(&matrix).unwrap();
        for (id, bytes) in [
            ("request", frozen.clone()),
            ("network", b"immutable-graph".to_vec()),
        ] {
            let path = fixture.0.join(format!("{}-{id}", matrix.id));
            std::fs::write(&path, &bytes).unwrap();
            store
                .put_artifact(
                    &pb::TaskArtifact {
                        task_id: matrix.id.clone(),
                        id: id.into(),
                        size_bytes: bytes.len() as u64,
                        sha256: format!("{:x}", sha2::Sha256::digest(&bytes)),
                        ..Default::default()
                    },
                    &path,
                )
                .unwrap();
        }
        let job = store
            .insert_job(
                pb::Job {
                    state: pb::JobState::Succeeded as i32,
                    scenario: Some(pb::ScenarioRef {
                        scenario_id: request.scenario_id,
                        version: 2,
                    }),
                    has_result: true,
                    ..Default::default()
                },
                &frozen,
                "",
                b"hash",
            )
            .unwrap();
        store.link_solver(&job.id, &parent.id).unwrap();
        parent.result_job_id = job.id.clone();
        parent.dependency_ids.push(matrix.id.clone());
        store.update_task(&parent).unwrap();
        let mut geometry = finished_task(store);
        geometry.kind = pb::TaskKind::GeometryGenerate as i32;
        geometry.parent_id = parent.id.clone();
        geometry.scenario = parent.scenario.clone();
        geometry.dependency_ids = vec![matrix.id.clone(), job.id.clone()];
        store.update_task(&geometry).unwrap();
        StoredWorkflow {
            parent,
            matrix,
            geometry,
            job,
            frozen,
        }
    }

    #[test]
    fn deleting_a_plan_retains_its_committed_matrix_graph_and_package_reference() {
        let f = Fixture::new();
        let mut store = f.open();
        let workflow = committed_workflow(&mut store, &f);
        let job_id = crate::job_id(&workflow.job.id).unwrap();
        store.max_bytes = store.payload_bytes;
        assert_eq!(
            store.delete(job_id).unwrap_err().code,
            "conflict",
            "low-level job deletion must not bypass workflow ownership"
        );
        let deleted = store.delete_plan(job_id).unwrap();
        assert!(deleted.task_ids.contains(&workflow.parent.id));
        assert!(deleted.task_ids.contains(&workflow.geometry.id));
        assert!(!deleted.task_ids.contains(&workflow.matrix.id));
        assert!(store.job(job_id).is_err());
        let matrix = store.task(&workflow.matrix.id).unwrap();
        assert!(matrix.parent_id.is_empty());
        assert_eq!(matrix.scenario.unwrap().version, 2);
        assert_eq!(
            store.scenario("outbox-scene", 2).unwrap().1,
            workflow.frozen
        );
        assert_eq!(
            store
                .scenario_metadata("outbox-scene", 2)
                .unwrap()
                .geo
                .unwrap()
                .task_id,
            workflow.matrix.id
        );
        assert_eq!(
            std::fs::read(store.artifact_path(&workflow.matrix.id, "network").unwrap()).unwrap(),
            b"immutable-graph"
        );
        assert_eq!(
            store.road_package("retained-road").unwrap().reference_count,
            1
        );
        assert!(
            store.delete_generic_task(&workflow.matrix.id).is_err(),
            "saved scene protects its source matrix"
        );
        let accounting = (store.payload_bytes, store.records);
        drop(store);
        let mut store = f.open();
        assert_eq!((store.payload_bytes, store.records), accounting);
        let deleted = store
            .delete_scenario(&pb::DeleteScenarioRequest {
                scenario_id: "outbox-scene".into(),
                expected_version: 2,
            })
            .unwrap();
        assert!(deleted.task_ids.contains(&workflow.matrix.id));
        assert!(store.scenario_metadata("outbox-scene", 0).is_err());
        assert_eq!(
            store.road_package("retained-road").unwrap().reference_count,
            0
        );
    }

    #[test]
    fn plan_delete_rolls_back_matrix_promotion_when_an_external_typed_task_uses_the_solver() {
        let f = Fixture::new();
        let mut store = f.open();
        let workflow = committed_workflow(&mut store, &f);
        let external = store
            .insert_task(
                pb::Task {
                    kind: pb::TaskKind::GeometryGenerate as i32,
                    state: pb::TaskState::Succeeded as i32,
                    ..Default::default()
                },
                &pb::TaskInput {
                    input: Some(pb::task_input::Input::Geometry(pb::GeometryGenerateInput {
                        job_id: workflow.job.id.clone(),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
            )
            .unwrap();
        let accounting = (store.payload_bytes, store.records);
        let error = store
            .delete_plan(crate::job_id(&workflow.job.id).unwrap())
            .unwrap_err();
        assert_eq!(error.code, "conflict");
        assert!(error.message.contains(&external.id));
        assert_eq!((store.payload_bytes, store.records), accounting);
        assert_eq!(
            store.task(&workflow.matrix.id).unwrap().parent_id,
            workflow.parent.id
        );
        assert!(
            store
                .scenario_metadata("outbox-scene", 2)
                .unwrap()
                .geo
                .is_none()
        );
        assert!(store.job(crate::job_id(&workflow.job.id).unwrap()).is_ok());
        // Scene collection also rolls back rather than leaving a dangling task.
        assert!(
            store
                .delete_scenario(&pb::DeleteScenarioRequest {
                    scenario_id: "outbox-scene".into(),
                    expected_version: 2
                })
                .is_err()
        );
        assert!(store.scenario_metadata("outbox-scene", 2).is_ok());
        assert_eq!((store.payload_bytes, store.records), accounting);
        store.delete_generic_task(&external.id).unwrap();
        store
            .delete_plan(crate::job_id(&workflow.job.id).unwrap())
            .unwrap();
    }
}
