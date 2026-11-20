use crate::{AppError, Result, now_ms};
use prost::Message;
use rusqlite::{Connection, OptionalExtension, params};
use std::fs::{File, OpenOptions};
use std::path::Path;
use vrp_proto::app::v1 as pb;
mod basemaps;
mod tasks;
pub(crate) use tasks::DeletedTasks;

/// Explicitly release the shared open-file-description lock before closing its
/// final owner. On Unix a concurrent fork may temporarily retain a CLOEXEC fd;
/// closing only our fd would otherwise keep the directory locked until exec.
struct DirectoryLock(File);
impl Drop for DirectoryLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(all(test, unix))]
mod directory_lock_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn a_closed_store_releases_its_lock_even_if_a_fork_style_fd_duplicate_remains() {
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus")
            .join(format!(
                "directory-lock-test-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        let original = Store::open(&directory, 1024 * 1024, 100).unwrap();
        // dup and fork both retain the same open file description. Holding this
        // duplicate deterministically models a child paused before CLOEXEC.
        let inherited = original._lock.0.try_clone().unwrap();
        drop(original);
        let reopened = Store::open(&directory, 1024 * 1024, 100)
            .expect("shutdown must release the directory before an inherited fd closes");
        drop(reopened);
        drop(inherited);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

pub(crate) struct Store {
    // Declaration order matters: close SQLite before releasing the directory.
    conn: Connection,
    _lock: DirectoryLock,
    pub payload_bytes: u64,
    records: u64,
    max_bytes: u64,
    max_records: u64,
}

fn summary_counts(input: &vrp_proto::engine::v1::SolveRequest) -> pb::ScenarioSummary {
    let Some(problem) = &input.problem else {
        return pb::ScenarioSummary::default();
    };
    let auxiliary: std::collections::HashSet<u32> = problem
        .execution
        .iter()
        .flat_map(|execution| &execution.tickets)
        .filter(|ticket| ticket.auxiliary)
        .map(|ticket| ticket.ticket)
        .collect();
    pb::ScenarioSummary {
        ticket_count: problem
            .tickets
            .iter()
            .enumerate()
            .filter(|(index, _)| !auxiliary.contains(&(*index as u32)))
            .count() as u32,
        agent_count: problem.agents.len() as u32,
        ..Default::default()
    }
}

fn backfill_summaries(conn: &Connection) -> Result<()> {
    let missing = {
        let mut statement = conn.prepare("SELECT meta FROM scenarios")?;
        let mut missing = Vec::new();
        for bytes in statement.query_map([], |row| row.get::<_, Vec<u8>>(0))? {
            let meta = pb::Scenario::decode(bytes?.as_slice())?;
            if meta.summary.is_none() {
                missing.push(meta);
            }
        }
        missing
    };
    if missing.is_empty() {
        return Ok(());
    }
    let transaction = conn.unchecked_transaction()?;
    for mut meta in missing {
        let bytes: Vec<u8> = transaction.query_row(
            "SELECT request FROM scenarios WHERE id=?1 AND version=?2",
            params![meta.id, meta.version as i64],
            |row| row.get(0),
        )?;
        meta.summary = Some(summary_counts(
            &vrp_proto::engine::v1::SolveRequest::decode(bytes.as_slice())?,
        ));
        transaction.execute(
            "UPDATE scenarios SET meta=?3 WHERE id=?1 AND version=?2",
            params![meta.id, meta.version as i64, meta.encode_to_vec()],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

impl Store {
    pub fn open(dir: &Path, max_bytes: u64, max_records: u64) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("server.lock"))?;
        lock.try_lock()
            .map_err(|_| AppError::conflict("data directory is already in use"))?;
        let lock = DirectoryLock(lock);
        let conn = Connection::open(dir.join("tasks.sqlite"))?;
        let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 6 {
            return Err(AppError::conflict(
                "database schema is newer than this server",
            ));
        }
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS scenarios (id TEXT NOT NULL, version INTEGER NOT NULL, meta BLOB NOT NULL, request BLOB NOT NULL, PRIMARY KEY(id,version));
            CREATE TABLE IF NOT EXISTS geo_pending (scenario_id TEXT NOT NULL, version INTEGER NOT NULL, PRIMARY KEY(scenario_id,version));
            CREATE TABLE IF NOT EXISTS jobs (id INTEGER PRIMARY KEY AUTOINCREMENT, state INTEGER NOT NULL, scenario_id TEXT NOT NULL, meta BLOB NOT NULL, request BLOB NOT NULL, result BLOB, idem TEXT UNIQUE, fingerprint BLOB NOT NULL);
            CREATE INDEX IF NOT EXISTS jobs_state ON jobs(state,id);
            CREATE INDEX IF NOT EXISTS jobs_scenario ON jobs(scenario_id,id);
            CREATE TABLE IF NOT EXISTS operations (id TEXT PRIMARY KEY, revision INTEGER NOT NULL, state BLOB NOT NULL, dirty INTEGER NOT NULL, closed INTEGER NOT NULL, due INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS operation_history (id TEXT NOT NULL, revision INTEGER NOT NULL, state BLOB NOT NULL, command BLOB NOT NULL, kind TEXT NOT NULL, event_key TEXT, fingerprint BLOB NOT NULL, PRIMARY KEY(id,revision), UNIQUE(id,event_key));
            CREATE TABLE IF NOT EXISTS resource_claims (resource_id TEXT PRIMARY KEY, scenario_id TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS job_geometry (job_id INTEGER PRIMARY KEY, payload BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS job_trends (job_id INTEGER PRIMARY KEY, payload BLOB NOT NULL, partial INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS basemap_packages (id TEXT PRIMARY KEY, meta BLOB NOT NULL, path TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS basemap_refs (owner TEXT PRIMARY KEY, context BLOB NOT NULL, package_id TEXT NOT NULL);
            PRAGMA user_version=6;")?;
        backfill_summaries(&conn)?;
        let payload_bytes = conn.query_row("SELECT coalesce((SELECT sum(length(request)+coalesce(length(result),0)) FROM jobs),0)+coalesce((SELECT sum(length(request)+length(meta)) FROM scenarios),0)+coalesce((SELECT sum(length(state)) FROM operations),0)+coalesce((SELECT sum(length(state)+length(command)) FROM operation_history),0)+coalesce((SELECT sum(length(payload)) FROM job_geometry),0)+coalesce((SELECT sum(length(payload)) FROM job_trends),0)", [], |r| r.get::<_, i64>(0))? as u64;
        let records = conn.query_row(
            "SELECT (SELECT count(*) FROM jobs)+(SELECT count(*) FROM scenarios)+(SELECT count(*) FROM operations)+(SELECT count(*) FROM operation_history)+(SELECT count(*) FROM job_geometry)",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64;
        let mut store = Self {
            conn,
            _lock: lock,
            payload_bytes,
            records,
            max_bytes,
            max_records,
        };
        let interrupted = {
            let mut stmt = store
                .conn
                .prepare("SELECT meta FROM jobs WHERE state IN (1,2)")?;
            stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for bytes in interrupted {
            let mut job = pb::Job::decode(bytes.as_slice())?;
            job.state = pb::JobState::Interrupted as i32;
            job.revision += 1;
            job.finished_unix_ms = now_ms();
            job.error = Some(pb::Error {
                code: "server_restarted".into(),
                message: "Previous execution was interrupted; submit a new job to retry.".into(),
            });
            store.update(&job, None)?;
        }
        Ok(store)
    }

    fn capacity(&self, extra: usize, new_record: bool) -> Result<()> {
        if self.payload_bytes.saturating_add(extra as u64) > self.max_bytes
            || (new_record && self.records >= self.max_records)
        {
            return Err(AppError::new(
                "capacity",
                "stored payload or record limit reached; remove finished jobs or increase limits",
            ));
        }
        Ok(())
    }

    pub fn save_scenario(
        &mut self,
        request: &pb::SaveScenarioRequest,
        bytes: &[u8],
    ) -> Result<pb::Scenario> {
        self.atomic(|store| {
            let saved = store.save_scenario_inner(request, bytes)?;
            store.conn.execute(
                "INSERT INTO geo_pending(scenario_id,version) VALUES(?1,?2)",
                params![saved.id, saved.version as i64],
            )?;
            Ok(saved)
        })
    }

    fn save_scenario_inner(
        &mut self,
        request: &pb::SaveScenarioRequest,
        bytes: &[u8],
    ) -> Result<pb::Scenario> {
        let current = self.conn.query_row(
            "SELECT coalesce(max(version),0) FROM scenarios WHERE id=?1",
            [&request.scenario_id],
            |r| r.get::<_, i64>(0),
        )? as u64;
        if current != request.expected_version {
            return Err(AppError::conflict(format!(
                "latest scenario version is {current}"
            )));
        }
        let version = current
            .checked_add(1)
            .filter(|&n| n <= i64::MAX as u64)
            .ok_or_else(|| AppError::conflict("scenario version exhausted"))?;
        let previous = if current > 0 {
            Some(self.scenario(&request.scenario_id, current)?.0)
        } else {
            None
        };
        let meta = pb::Scenario {
            id: request.scenario_id.clone(),
            name: request.name.clone(),
            version,
            saved_unix_ms: now_ms(),
            display: request
                .display
                .clone()
                .or_else(|| previous.as_ref().and_then(|p| p.display.clone())),
            origin: request
                .origin
                .clone()
                .or_else(|| previous.and_then(|p| p.origin)),
            matrix_quality: Some(crate::maps::quality(
                request.request.as_ref().expect("validated request"),
            )?),
            summary: Some(summary_counts(
                request.request.as_ref().expect("validated request"),
            )),
            matrix_ready: vrp_model::validate_ready_matrices(
                request.request.as_ref().expect("validated request"),
            )
            .is_ok(),
            geo: None,
        };
        self.capacity(bytes.len() + meta.encoded_len(), true)?;
        self.conn.execute(
            "INSERT INTO scenarios(id,version,meta,request) VALUES (?1,?2,?3,?4)",
            params![meta.id, version as i64, meta.encode_to_vec(), bytes],
        )?;
        self.payload_bytes += (bytes.len() + meta.encoded_len()) as u64;
        self.records += 1;
        self.enrich_scenario(meta)
    }

    pub fn scenario(&self, id: &str, version: u64) -> Result<(pb::Scenario, Vec<u8>)> {
        let pair: Option<(Vec<u8>, Vec<u8>)> = self.conn.query_row("SELECT meta,request FROM scenarios WHERE id=?1 AND (?2=0 OR version=?2) ORDER BY version DESC LIMIT 1", params![id, version as i64], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        let (meta, request) =
            pair.ok_or_else(|| AppError::new("not_found", "scenario version not found"))?;
        Ok((
            self.enrich_scenario(pb::Scenario::decode(meta.as_slice())?)?,
            request,
        ))
    }

    pub fn scenario_metadata(&self, id: &str, version: u64) -> Result<pb::Scenario> {
        let bytes: Option<Vec<u8>> = self.conn.query_row(
            "SELECT meta FROM scenarios WHERE id=?1 AND (?2=0 OR version=?2) ORDER BY version DESC LIMIT 1",
            params![id, version as i64], |r| r.get(0),
        ).optional()?;
        let bytes = bytes.ok_or_else(|| AppError::new("not_found", "scenario not found"))?;
        self.enrich_scenario(pb::Scenario::decode(bytes.as_slice())?)
    }

    pub fn scenarios(&self, after: &str, count: usize) -> Result<Vec<pb::Scenario>> {
        let mut stmt = self.conn.prepare("SELECT meta FROM scenarios s WHERE id>?1 AND version=(SELECT max(version) FROM scenarios WHERE id=s.id) ORDER BY id LIMIT ?2")?;
        stmt.query_map(params![after, count as i64], |r| r.get::<_, Vec<u8>>(0))?
            .map(|r| self.enrich_scenario(pb::Scenario::decode(r?.as_slice())?))
            .collect()
    }

    fn enrich_scenario(&self, mut meta: pb::Scenario) -> Result<pb::Scenario> {
        let summary = meta.summary.get_or_insert_default();
        summary.version_count = self.conn.query_row(
            "SELECT count(*) FROM scenarios WHERE id=?1",
            [&meta.id],
            |row| row.get(0),
        )?;
        summary.job_count = 0;
        let mut queued = false;
        let mut solving = false;
        let mut has_plan = false;
        let mut has_previous_plan = false;
        let mut statement = self
            .conn
            .prepare("SELECT meta FROM jobs WHERE scenario_id=?1")?;
        for bytes in statement.query_map([&meta.id], |row| row.get::<_, Vec<u8>>(0))? {
            let job = pb::Job::decode(bytes?.as_slice())?;
            summary.job_count += 1;
            queued |= job.state() == pb::JobState::Queued;
            solving |= matches!(
                job.state(),
                pb::JobState::Running | pb::JobState::Cancelling
            );
            if job.has_result {
                has_previous_plan = true;
                has_plan |= job
                    .scenario
                    .as_ref()
                    .is_some_and(|reference| reference.version == meta.version);
            }
        }
        let closed: Option<bool> = self
            .conn
            .query_row(
                "SELECT closed FROM operations WHERE id=?1",
                [&meta.id],
                |row| row.get(0),
            )
            .optional()?;
        summary.state = match closed {
            Some(true) => pb::ScenarioState::Closed,
            Some(false) => pb::ScenarioState::Executing,
            None if solving => pb::ScenarioState::Solving,
            None if queued => pb::ScenarioState::Queued,
            None if has_plan => pb::ScenarioState::HasPlan,
            None if has_previous_plan => pb::ScenarioState::StalePlan,
            None => pb::ScenarioState::NeedsPlan,
        } as i32;
        Ok(meta)
    }

    pub fn delete_scenario(&mut self, request: &pb::DeleteScenarioRequest) -> Result<DeletedTasks> {
        let meta = self.scenario_metadata(&request.scenario_id, 0)?;
        if meta.version != request.expected_version {
            return Err(AppError::conflict(
                "scenario changed; refresh before deleting",
            ));
        }
        let retained: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM operations WHERE id=?1)
                OR EXISTS(SELECT 1 FROM operation_history WHERE id=?1)
                OR EXISTS(SELECT 1 FROM resource_claims WHERE scenario_id=?1)",
            [&request.scenario_id],
            |row| row.get(0),
        )?;
        if retained {
            return Err(AppError::conflict(
                "scenarios with execution history are retained for audit",
            ));
        }
        let jobs = {
            let mut statement = self
                .conn
                .prepare("SELECT meta FROM jobs WHERE scenario_id=?1 ORDER BY id DESC")?;
            statement
                .query_map([&request.scenario_id], |row| row.get::<_, Vec<u8>>(0))?
                .map(|bytes| Ok(pb::Job::decode(bytes?.as_slice())?))
                .collect::<Result<Vec<_>>>()?
        };
        if jobs.iter().any(|job| !crate::terminal(job)) {
            return Err(AppError::conflict(
                "stop queued or running jobs before deleting the scenario",
            ));
        }
        let (records, bytes): (i64, i64) = self.conn.query_row(
            "SELECT count(*),sum(length(meta)+length(request)) FROM scenarios WHERE id=?1",
            [&request.scenario_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let tasks = self.tasks()?;
        let mut owned = tasks
            .iter()
            .filter(|task| {
                task.scenario
                    .as_ref()
                    .is_some_and(|r| r.scenario_id == request.scenario_id)
            })
            .map(|task| task.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        loop {
            let previous = owned.len();
            for task in &tasks {
                if owned.contains(&task.parent_id) {
                    owned.insert(task.id.clone());
                }
            }
            if previous == owned.len() {
                break;
            }
        }
        if tasks.iter().any(|task| {
            owned.contains(&task.id)
                && task
                    .scenario
                    .as_ref()
                    .is_some_and(|r| r.scenario_id != request.scenario_id)
        }) {
            return Err(AppError::conflict(
                "workflow contains another scenario's tasks; retain it for inspection",
            ));
        }
        let roots = tasks
            .iter()
            .filter(|task| owned.contains(&task.id) && !owned.contains(&task.parent_id))
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        self.atomic(|store| {
            // Release scene snapshot references only within this transaction.
            // Any external task, source-job or execution protection rolls back
            // the scene and every collected record together.
            store
                .conn
                .execute("DELETE FROM scenarios WHERE id=?1", [&request.scenario_id])?;
            store.conn.execute(
                "DELETE FROM geo_pending WHERE scenario_id=?1",
                [&request.scenario_id],
            )?;
            store.payload_bytes -= bytes as u64;
            store.records -= records as u64;
            let mut deleted = DeletedTasks::default();
            // Tasks are listed newest first: retries and consumers disappear
            // before their earlier source workflows and graph snapshots.
            for root in &roots {
                let collection = store.delete_generic_task(root)?;
                deleted.artifact_paths.extend(collection.artifact_paths);
                deleted.task_ids.extend(collection.task_ids);
            }
            for job in &jobs {
                let id = crate::job_id(&job.id)?;
                if store.job(id).is_ok() {
                    store.delete(id)?;
                }
            }
            Ok(deleted)
        })
    }

    pub fn idempotent(&self, key: &str, fingerprint: &[u8]) -> Result<Option<pb::Job>> {
        if key.is_empty() {
            return Ok(None);
        }
        let existing: Option<(Vec<u8>, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT meta,fingerprint FROM jobs WHERE idem=?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match existing {
            Some((meta, stored)) if stored == fingerprint => {
                Ok(Some(pb::Job::decode(meta.as_slice())?))
            }
            Some(_) => Err(AppError::conflict(
                "idempotency key already belongs to different input",
            )),
            None => Ok(None),
        }
    }

    pub fn insert_job(
        &mut self,
        mut meta: pb::Job,
        request: &[u8],
        key: &str,
        fingerprint: &[u8],
    ) -> Result<pb::Job> {
        self.capacity(request.len(), true)?;
        self.atomic(|store| {
        let scenario = meta
            .scenario
            .as_ref()
            .map(|s| s.scenario_id.as_str())
            .unwrap_or("");
        store.conn.execute("INSERT INTO jobs(state,scenario_id,meta,request,idem,fingerprint) VALUES(0,?1,X'',?2,?3,?4)", params![scenario, request, (!key.is_empty()).then_some(key), fingerprint])?;
        let id = store.conn.last_insert_rowid();
        meta.id = id.to_string();
        store.conn.execute("INSERT INTO job_trends(job_id,payload) VALUES(?1,X'')", [id])?;
        store.conn.execute(
            "UPDATE jobs SET meta=?1 WHERE id=?2",
            params![meta.encode_to_vec(), id],
        )?;
        store.payload_bytes += request.len() as u64;
        store.records += 1;
        Ok(meta)
        })
    }

    pub fn job(&self, id: i64) -> Result<pb::Job> {
        let bytes: Option<Vec<u8>> = self
            .conn
            .query_row("SELECT meta FROM jobs WHERE id=?1", [id], |r| r.get(0))
            .optional()?;
        Ok(pb::Job::decode(
            bytes
                .ok_or_else(|| AppError::new("not_found", "job not found"))?
                .as_slice(),
        )?)
    }

    pub fn jobs(&self, before: i64, scenario: &str, count: usize) -> Result<Vec<pb::Job>> {
        let mut stmt = self.conn.prepare("SELECT meta FROM jobs WHERE (?1=0 OR id<?1) AND (?2='' OR scenario_id=?2) ORDER BY id DESC LIMIT ?3")?;
        stmt.query_map(params![before, scenario, count as i64], |r| {
            r.get::<_, Vec<u8>>(0)
        })?
        .map(|r| Ok(pb::Job::decode(r?.as_slice())?))
        .collect()
    }

    pub fn queued(&self) -> Result<u32> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM jobs WHERE state=0", [], |r| r.get(0))?)
    }
    pub fn first_queued(&self) -> Result<Option<pb::Job>> {
        let meta: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT meta FROM jobs WHERE state=0 ORDER BY id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        meta.map(|b| Ok(pb::Job::decode(b.as_slice())?)).transpose()
    }

    pub fn payload(&self, id: i64, result: bool) -> Result<Vec<u8>> {
        let sql = if result {
            "SELECT result FROM jobs WHERE id=?1"
        } else {
            "SELECT request FROM jobs WHERE id=?1"
        };
        let value: Option<Option<Vec<u8>>> =
            self.conn.query_row(sql, [id], |r| r.get(0)).optional()?;
        value
            .ok_or_else(|| AppError::new("not_found", "job not found"))?
            .ok_or_else(|| AppError::conflict("job has no final result"))
    }

    pub fn geometry(&self, id: i64) -> Result<pb::JobGeometry> {
        let bytes: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT payload FROM job_geometry WHERE job_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        match bytes {
            Some(bytes) => Ok(pb::JobGeometry::decode(bytes.as_slice())?),
            None => Ok(pb::JobGeometry {
                job_id: id.to_string(),
                coordinate_system: "WGS84".into(),
                ..Default::default()
            }),
        }
    }
    pub fn save_geometry(&mut self, id: i64, value: &pb::JobGeometry) -> Result<()> {
        let previous: Option<usize> = self
            .conn
            .query_row(
                "SELECT length(payload) FROM job_geometry WHERE job_id=?1",
                [id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map(|n| n as usize);
        let bytes = value.encode_to_vec();
        self.capacity(
            bytes.len().saturating_sub(previous.unwrap_or(0)),
            previous.is_none(),
        )?;
        self.conn.execute("INSERT INTO job_geometry(job_id,payload) VALUES(?1,?2) ON CONFLICT(job_id) DO UPDATE SET payload=excluded.payload", params![id, bytes])?;
        self.payload_bytes = self.payload_bytes - previous.unwrap_or(0) as u64 + bytes.len() as u64;
        if previous.is_none() {
            self.records += 1;
        }
        Ok(())
    }

    pub fn update(&mut self, job: &pb::Job, result: Option<&[u8]>) -> Result<()> {
        self.atomic(|store| {
            if let Some(bytes) = result {
                if store.capacity(bytes.len(), false).is_err() {
                    let previous: Option<i64> = store
                        .conn
                        .query_row(
                            "SELECT length(payload) FROM job_trends WHERE job_id=?1",
                            [&job.id],
                            |row| row.get(0),
                        )
                        .optional()?;
                    store.conn.execute(
                        "UPDATE job_trends SET payload=X'',partial=1 WHERE job_id=?1",
                        [&job.id],
                    )?;
                    store.payload_bytes -= previous.unwrap_or(0) as u64;
                }
                store.capacity(bytes.len(), false)?;
            }
            store.conn.execute(
                "UPDATE jobs SET meta=?1,state=?2,result=coalesce(?3,result) WHERE id=?4",
                params![job.encode_to_vec(), job.state, result, job.id],
            )?;
            store.payload_bytes += result.map_or(0, |bytes| bytes.len() as u64);
            // Queued jobs from an older database begin recording when admitted.
            if job.state() == pb::JobState::Running && job.progress.is_none() {
                store.conn.execute(
                    "INSERT OR IGNORE INTO job_trends(job_id,payload) VALUES(?1,X'')",
                    [&job.id],
                )?;
            }
            store.record_trend(job)
        })
    }

    pub fn job_trend(&self, id: i64) -> Result<pb::JobTrend> {
        let job = self.job(id)?;
        let stored: Option<(Vec<u8>, bool)> = self
            .conn
            .query_row(
                "SELECT payload,partial FROM job_trends WHERE job_id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let mut trend = if let Some((bytes, partial)) = stored {
            let mut trend = pb::JobTrend::decode(bytes.as_slice())?;
            trend.partial_history = partial;
            trend
        } else {
            pb::JobTrend {
                partial_history: true,
                ..Default::default()
            }
        };
        // Legacy jobs have only a real latest/final observation. The same
        // fallback supplies the final point when the storage quota dropped it.
        if let Some(point) = crate::trend::point(&job)
            && (crate::terminal(&job) || trend.samples.is_empty())
        {
            crate::trend::append(&mut trend, point, true);
        }
        trend.job_id = job.id;
        trend.revision = job.revision;
        trend.sample_interval_ms = trend
            .sample_interval_ms
            .max(crate::trend::SAMPLE_INTERVAL_MS);
        Ok(trend)
    }

    fn record_trend(&mut self, job: &pb::Job) -> Result<()> {
        let Some(point) = crate::trend::point(job) else {
            return Ok(());
        };
        let stored: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT payload FROM job_trends WHERE job_id=?1",
                [&job.id],
                |row| row.get(0),
            )
            .optional()?;
        let previous = stored.as_ref().map_or(0, Vec::len);
        let mut trend = match stored {
            Some(bytes) => pb::JobTrend::decode(bytes.as_slice())?,
            None => {
                self.conn.execute(
                    "INSERT INTO job_trends(job_id,payload,partial) VALUES(?1,X'',1)",
                    [&job.id],
                )?;
                pb::JobTrend::default()
            }
        };
        if !crate::trend::append(&mut trend, point, crate::terminal(job)) {
            return Ok(());
        }
        let bytes = trend.encode_to_vec();
        if self
            .capacity(bytes.len().saturating_sub(previous), false)
            .is_err()
        {
            self.conn
                .execute("UPDATE job_trends SET partial=1 WHERE job_id=?1", [&job.id])?;
            return Ok(());
        }
        self.conn.execute(
            "UPDATE job_trends SET payload=?2 WHERE job_id=?1",
            params![job.id, bytes],
        )?;
        self.payload_bytes = self.payload_bytes - previous as u64 + bytes.len() as u64;
        Ok(())
    }

    pub fn delete(&mut self, id: i64) -> Result<()> {
        let job = self.job(id)?;
        if let Some(parent) = self.solver_parent(&job.id)? {
            return Err(AppError::conflict(format!(
                "plan belongs to workflow {parent}; delete the plan together with its workflow"
            )));
        }
        for task in self.tasks()? {
            if self.task_references_solver(&task, &job.id)? {
                return Err(AppError::conflict(format!(
                    "plan is referenced by retained task {}; delete that task first",
                    task.id
                )));
            }
        }
        self.delete_job_record(id)
    }

    /// The caller already proved ownership and checked external task references.
    /// All original job audit and derived-source guards still apply here.
    fn delete_job_record(&mut self, id: i64) -> Result<()> {
        let job = self.job(id)?;
        let mut history = self.conn.prepare("SELECT state FROM operation_history")?;
        for bytes in history.query_map([], |r| r.get::<_, Vec<u8>>(0))? {
            let op = pb::Operation::decode(bytes?.as_slice())?;
            if op.plan.as_ref().is_some_and(|p| p.job_id == job.id) {
                return Err(AppError::conflict(
                    "published jobs are retained for execution history",
                ));
            }
        }
        drop(history);
        let mut children = self.conn.prepare("SELECT meta FROM jobs")?;
        for bytes in children.query_map([], |r| r.get::<_, Vec<u8>>(0))? {
            if pb::Job::decode(bytes?.as_slice())?.source_job_id == job.id {
                return Err(AppError::conflict(
                    "source jobs are retained while derived jobs exist",
                ));
            }
        }
        drop(children);
        if job.execution_basis.is_some() {
            return Err(AppError::conflict(
                "execution jobs are retained for operation audit and plan replay",
            ));
        }
        if !crate::terminal(&job) {
            return Err(AppError::conflict(
                "cancel the running or queued job before deleting it",
            ));
        }
        let bytes = self.conn.query_row(
            "SELECT length(request)+coalesce(length(result),0) FROM jobs WHERE id=?1",
            [id],
            |r| r.get::<_, i64>(0),
        )? as u64;
        self.atomic(|s| {
            let geometry: Option<u64> = s
                .conn
                .query_row(
                    "SELECT length(payload) FROM job_geometry WHERE job_id=?1",
                    [id],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?
                .map(|n| n as u64);
            s.conn
                .execute("DELETE FROM job_geometry WHERE job_id=?1", [id])?;
            let trend_bytes: Option<i64> = s
                .conn
                .query_row(
                    "SELECT length(payload) FROM job_trends WHERE job_id=?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()?;
            s.conn
                .execute("DELETE FROM job_trends WHERE job_id=?1", [id])?;
            s.conn.execute("DELETE FROM jobs WHERE id=?1", [id])?;
            s.payload_bytes -= bytes + geometry.unwrap_or(0) + trend_bytes.unwrap_or(0) as u64;
            s.records -= 1 + u64::from(geometry.is_some());
            Ok(())
        })
    }

    pub fn atomic<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        let counts = (self.payload_bytes, self.records);
        self.conn.execute_batch("SAVEPOINT vrp_atomic")?;
        match f(self).and_then(|v| {
            self.conn.execute_batch("RELEASE vrp_atomic")?;
            Ok(v)
        }) {
            Ok(value) => Ok(value),
            Err(error) => {
                self.conn
                    .execute_batch("ROLLBACK TO vrp_atomic; RELEASE vrp_atomic")?;
                (self.payload_bytes, self.records) = counts;
                Err(error)
            }
        }
    }

    pub fn operation(&self, id: &str, revision: u64) -> Result<Option<pb::Operation>> {
        let bytes: Option<Vec<u8>> = if revision == 0 {
            self.conn
                .query_row("SELECT state FROM operations WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .optional()?
        } else {
            self.conn
                .query_row(
                    "SELECT state FROM operation_history WHERE id=?1 AND revision=?2",
                    params![id, revision as i64],
                    |r| r.get(0),
                )
                .optional()?
        };
        bytes
            .map(|b| Ok(pb::Operation::decode(b.as_slice())?))
            .transpose()
    }

    pub fn operation_history(
        &self,
        id: &str,
        before: u64,
        count: usize,
    ) -> Result<Vec<pb::OperationHistoryEntry>> {
        let mut query = self.conn.prepare("SELECT state,command,kind FROM operation_history WHERE id=?1 AND (?2=0 OR revision<?2) ORDER BY revision DESC LIMIT ?3")?;
        let rows = query.query_map(params![id, before as i64, count as i64], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (bytes, command, kind) = row?;
            let op = pb::Operation::decode(bytes.as_slice())?;
            let mut entry = pb::OperationHistoryEntry {
                revision: op.revision,
                at: op.at,
                kind: kind.clone(),
                plan_version: op.plan.as_ref().map_or(0, |p| p.version),
                scenario_version: op.scenario.as_ref().map_or(0, |s| s.version),
                ..Default::default()
            };
            match kind.as_str() {
                "event" => {
                    let event = pb::ExecutionEvent::decode(command.as_slice())?;
                    entry.actor = event.actor;
                    entry.at = event.at;
                    entry.ticket_ids = event
                        .tickets
                        .iter()
                        .map(|t| t.ticket_id.clone())
                        .chain(event.cargo.iter().map(|c| c.ticket_id.clone()))
                        .collect();
                    entry.agent_ids = event
                        .vehicles
                        .into_iter()
                        .map(|v| v.agent_id)
                        .chain(event.tickets.into_iter().map(|t| t.agent_id))
                        .chain(
                            event
                                .cargo
                                .into_iter()
                                .flat_map(|c| [c.from_agent_id, c.to_agent_id]),
                        )
                        .filter(|id| !id.is_empty())
                        .collect();
                    entry.ticket_ids.sort();
                    entry.ticket_ids.dedup();
                    entry.agent_ids.sort();
                    entry.agent_ids.dedup();
                }
                "start" => {
                    entry.actor = pb::StartOperationRequest::decode(command.as_slice())?.actor
                }
                "configure" => {
                    entry.actor = pb::ConfigureOperationRequest::decode(command.as_slice())?.actor
                }
                "adopt" | "auto_adopt" => {
                    entry.actor = pb::AdoptPlanRequest::decode(command.as_slice())?.actor
                }
                "manual_adjustment" => {
                    entry.actor = pb::AdjustPlanRequest::decode(command.as_slice())?.actor
                }
                "close" => {
                    entry.actor = pb::CloseOperationRequest::decode(command.as_slice())?.actor
                }
                "acknowledge" => entry
                    .agent_ids
                    .push(pb::AcknowledgePlanRequest::decode(command.as_slice())?.agent_id),
                _ => {}
            }
            entries.push(entry);
        }
        Ok(entries)
    }

    pub fn operation_event(
        &self,
        id: &str,
        key: &str,
        fingerprint: &[u8],
    ) -> Result<Option<pb::Operation>> {
        let found: Option<(Vec<u8>, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT state,fingerprint FROM operation_history WHERE id=?1 AND event_key=?2",
                params![id, key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match found {
            Some((state, hash)) if hash == fingerprint => {
                Ok(Some(pb::Operation::decode(state.as_slice())?))
            }
            Some(_) => Err(AppError::conflict(
                "event ID was already used with different content",
            )),
            None => Ok(None),
        }
    }

    pub fn save_operation(
        &mut self,
        op: &pb::Operation,
        kind: &str,
        command: &[u8],
        key: Option<&str>,
        fingerprint: &[u8],
    ) -> Result<()> {
        let id = &op
            .scenario
            .as_ref()
            .ok_or_else(|| AppError::invalid("operation has no scenario"))?
            .scenario_id;
        let old: Option<(u64, usize)> = self
            .conn
            .query_row(
                "SELECT revision,length(state) FROM operations WHERE id=?1",
                [id],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as usize)),
            )
            .optional()?;
        if old.map_or(1, |(r, _)| r + 1) != op.revision || op.revision > i64::MAX as u64 {
            return Err(AppError::conflict("operation revision changed"));
        }
        let bytes = op.encode_to_vec();
        let extra = bytes.len() * 2 + command.len();
        self.capacity(extra.saturating_sub(old.map_or(0, |(_, s)| s)), true)?;
        let count = if old.is_none() { 2 } else { 1 };
        if self.records + count > self.max_records {
            return Err(AppError::new("capacity", "operation history limit reached"));
        }
        self.atomic(|s| {
            s.conn.execute("INSERT INTO operation_history(id,revision,state,command,kind,event_key,fingerprint) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![id,op.revision as i64,bytes,command,kind,key,fingerprint])?;
            s.conn.execute("INSERT INTO operations(id,revision,state,dirty,closed,due) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,state=excluded.state,dirty=excluded.dirty,closed=excluded.closed,due=excluded.due", params![id,op.revision as i64,bytes,op.needs_replan,op.closed,op.replan_due_unix_ms])?;
            s.payload_bytes = s.payload_bytes + extra as u64 - old.map_or(0, |(_,n)| n as u64);
            s.records += count;
            Ok(())
        })
    }

    pub fn due_operations(&self, now: i64) -> Result<Vec<String>> {
        let mut query = self.conn.prepare(
            "SELECT id FROM operations WHERE dirty=1 AND closed=0 AND due<=?1 ORDER BY due,id",
        )?;
        Ok(query
            .query_map([now], |r| r.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn operation_ids(&self) -> Result<Vec<String>> {
        let mut q = self
            .conn
            .prepare("SELECT id FROM operations WHERE closed=0 ORDER BY id")?;
        Ok(q.query_map([], |r| r.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn claim_resources(&mut self, id: &str, resources: &[String]) -> Result<()> {
        self.conn
            .execute("DELETE FROM resource_claims WHERE scenario_id=?1", [id])?;
        for resource in resources.iter().filter(|v| !v.is_empty()) {
            let owner: Option<String> = self
                .conn
                .query_row(
                    "SELECT scenario_id FROM resource_claims WHERE resource_id=?1",
                    [resource],
                    |r| r.get(0),
                )
                .optional()?;
            if owner.is_some_and(|owner| owner != id) {
                return Err(AppError::conflict(
                    "physical resource is already used by another operation",
                ));
            }
            self.conn.execute(
                "INSERT OR IGNORE INTO resource_claims(resource_id,scenario_id) VALUES(?1,?2)",
                params![resource, id],
            )?;
        }
        Ok(())
    }
}
