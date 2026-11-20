//! Offline preparation of a private, non-operating demo database.
//! Usage: cargo run -p vrp-server --example prepare_demo -- <data-dir>
//! Rewrites native identifiers consistently, never coordinates or numeric input.
use prost::Message;
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, error::Error, fs::OpenOptions, path::Path};
use vrp_proto::{app::v1 as app, engine::v1 as engine};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
type StoredJobRow = (i64, String, Vec<u8>, Vec<u8>, Option<Vec<u8>>);

#[derive(Default)]
struct Names {
    entries: BTreeMap<String, BTreeMap<String, String>>,
}
impl Names {
    fn name(&mut self, space: &str, original: &str, prefix: &str) -> String {
        if original.is_empty() {
            return String::new();
        }
        let entries = self.entries.entry(space.into()).or_default();
        let next = format!("{prefix}{:03}", entries.len() + 1);
        entries.entry(original.into()).or_insert(next).clone()
    }
    fn rename_request(
        &mut self,
        scene: &str,
        ordinal: usize,
        input: &mut engine::SolveRequest,
    ) -> Result<()> {
        let p = input.problem.as_mut().ok_or("missing problem")?;
        if p.execution.is_some() {
            return Err(
                "prepare execution datasets separately; execution boundaries must stay immutable"
                    .into(),
            );
        }
        p.id = format!("SCENE-{ordinal:03}");
        for l in &mut p.locations {
            l.id = self.name(&format!("{scene}/locations"), &l.id, "L");
        }
        for a in &mut p.agents {
            a.id = self.name(&format!("{scene}/agents"), &a.id, "R");
            a.resource_id = self.name("physical_resources", &a.resource_id, "RES");
            a.depot_id = self.name("depots", &a.depot_id, "D");
            for skill in &mut a.skills {
                *skill = self.name("skills", skill, "技能");
            }
            a.qualification_levels = a
                .qualification_levels
                .iter()
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .map(|(key, value)| (self.name("qualifications", key, "资质"), *value))
                .collect();
        }
        for t in &mut p.tickets {
            t.id = self.name(&format!("{scene}/tickets"), &t.id, "T");
            t.depot_id = self.name("depots", &t.depot_id, "D");
            for skill in &mut t.skills_required {
                *skill = self.name("skills", skill, "技能");
            }
            t.qualification_required = t
                .qualification_required
                .iter()
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .map(|(key, value)| (self.name("qualifications", key, "资质"), *value))
                .collect();
        }
        for (i, matrix) in p.matrices.iter_mut().enumerate() {
            matrix.profile = format!("车型{:02}", i + 1);
        }
        Ok(())
    }
}

fn display(input: &engine::SolveRequest) -> app::ScenarioDisplay {
    let p = input.problem.as_ref().unwrap();
    let object = |name| app::ObjectLabel {
        name,
        ..Default::default()
    };
    app::ScenarioDisplay {
        tickets: p
            .tickets
            .iter()
            .map(|v| {
                (
                    v.id.clone(),
                    object(format!("工单{}", v.id.trim_start_matches('T'))),
                )
            })
            .collect(),
        agents: p
            .agents
            .iter()
            .map(|v| {
                (
                    v.id.clone(),
                    object(format!("资源{}", v.id.trim_start_matches('R'))),
                )
            })
            .collect(),
        locations: p
            .locations
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let depot = p
                    .agents
                    .iter()
                    .any(|a| a.start_location as usize == i || a.end_location as usize == i);
                (
                    v.id.clone(),
                    object(format!(
                        "{}{}",
                        if depot { "出发点" } else { "服务点" },
                        v.id.trim_start_matches('L')
                    )),
                )
            })
            .collect(),
    }
}

fn title(meta: &app::Scenario, input: &engine::SolveRequest, ordinal: usize) -> String {
    let source = meta
        .origin
        .as_ref()
        .map(|o| o.source_name.as_str())
        .unwrap_or("");
    if source.ends_with("public-demo/beijing-score-progress.json") {
        return "北京 · 多仓调度".into();
    }
    if source.ends_with("public-demo/here-nz-auckland-complex.json") {
        return "奥克兰 · 多仓调度".into();
    }
    let p = input.problem.as_ref().unwrap();
    let service = p
        .tickets
        .iter()
        .all(|t| t.r#type == engine::TicketType::Service as i32);
    let delivery = p
        .tickets
        .iter()
        .all(|t| t.r#type != engine::TicketType::Service as i32);
    format!(
        "{} {ordinal:03}",
        if service {
            "上门服务"
        } else if delivery {
            "配送调度"
        } else {
            "混合调度"
        }
    )
}

fn check_numeric_input(before: &engine::SolveRequest, after: &engine::SolveRequest) -> Result<()> {
    let old = before.problem.as_ref().unwrap();
    let new = after.problem.as_ref().unwrap();
    // Equality after restoring ONLY the intentionally renamed fields protects
    // every numeric field, entity order, relation index and raw matrix byte.
    let mut restored = after.clone();
    let p = restored.problem.as_mut().unwrap();
    p.id.clone_from(&old.id);
    for (a, b) in p.locations.iter_mut().zip(&old.locations) {
        a.id.clone_from(&b.id);
    }
    for (a, b) in p.agents.iter_mut().zip(&old.agents) {
        a.id.clone_from(&b.id);
        a.resource_id.clone_from(&b.resource_id);
        a.depot_id.clone_from(&b.depot_id);
        a.skills.clone_from(&b.skills);
        a.qualification_levels.clone_from(&b.qualification_levels);
    }
    for (a, b) in p.tickets.iter_mut().zip(&old.tickets) {
        a.id.clone_from(&b.id);
        a.depot_id.clone_from(&b.depot_id);
        a.skills_required.clone_from(&b.skills_required);
        a.qualification_required
            .clone_from(&b.qualification_required);
    }
    for (a, b) in p.matrices.iter_mut().zip(&old.matrices) {
        a.profile.clone_from(&b.profile);
    }
    if &restored != before {
        return Err("a numeric value, coordinate or reference changed".into());
    }
    // Compare candidate eligibility as well: skill and qualification renames
    // must retain their equality relationships, not merely vector lengths.
    let a = vrp_model::compile(before)?;
    let b = vrp_model::compile(after)?;
    if a.problem.candidates != b.problem.candidates
        || a.stats != b.stats
        || old.locations.len() != new.locations.len()
    {
        return Err("eligibility or matrix quality changed".into());
    }
    Ok(())
}

fn run(dir: &Path) -> Result<()> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("server.lock"))?;
    lock.try_lock()
        .map_err(|_| "stop the server using this data directory first")?;
    if !dir.join("tasks.sqlite").is_file() {
        return Err("database does not exist".into());
    }
    let mut conn = Connection::open(dir.join("tasks.sqlite"))?;
    conn.execute_batch("PRAGMA secure_delete=ON;")?;
    let prepared: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='demo_preparation_v1')",
        [],
        |r| r.get(0),
    )?;
    if prepared {
        println!("Demo identifiers already prepared; no changes.");
        return Ok(());
    }
    let operating: i64 = conn.query_row("SELECT (SELECT count(*) FROM operations)+(SELECT count(*) FROM operation_history)+(SELECT count(*) FROM resource_claims)", [], |r| r.get(0))?;
    if operating != 0 {
        return Err("this offline demo tool requires a database without execution history; do not rewrite live operations".into());
    }
    let pending: i64 = conn.query_row(
        "SELECT count(*) FROM jobs WHERE state IN (0,1,2)",
        [],
        |r| r.get(0),
    )?;
    if pending != 0 {
        return Err("queued/running jobs must finish before demo preparation".into());
    }
    let scenarios: Vec<(String, i64, Vec<u8>, Vec<u8>)> = conn
        .prepare("SELECT id,version,meta,request FROM scenarios ORDER BY id,version")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let jobs: Vec<StoredJobRow> = conn
        .prepare("SELECT id,scenario_id,meta,request,result FROM jobs ORDER BY id")?
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<std::result::Result<_, _>>()?;
    let mut names = Names::default();
    let mut ordinals = BTreeMap::new();
    let tx = conn.transaction()?;
    for (id, version, meta_bytes, request_bytes) in &scenarios {
        let mut meta = app::Scenario::decode(meta_bytes.as_slice())?;
        let before = engine::SolveRequest::decode(request_bytes.as_slice())?;
        let next = ordinals.len() + 1;
        let ordinal = *ordinals.entry(id.clone()).or_insert(next);
        let mut after = before.clone();
        names.rename_request(id, ordinal, &mut after)?;
        check_numeric_input(&before, &after)?;
        meta.name = title(&meta, &after, ordinal);
        meta.display = Some(display(&after));
        if let Some(origin) = meta.origin.as_mut() {
            origin.source_name = format!("历史场景 {ordinal:03}");
            origin.coordinate_note = "保留导入的精确坐标和在途矩阵".into();
            let count = origin.warnings.len();
            origin.warnings = if count == 0 {
                vec![]
            } else {
                vec![format!(
                    "原始导入报告包含 {count} 条提示，详见项目私有语料目录"
                )]
            };
        }
        tx.execute(
            "UPDATE scenarios SET meta=?1,request=?2 WHERE id=?3 AND version=?4",
            params![meta.encode_to_vec(), after.encode_to_vec(), id, version],
        )?;
    }
    for (id, scenario, meta_bytes, request_bytes, result_bytes) in &jobs {
        let meta = app::Job::decode(meta_bytes.as_slice())?;
        if meta.kind != app::JobKind::Planning as i32
            && meta.kind != app::JobKind::Replanning as i32
        {
            return Err("prepare this demo before creating optimization or execution tasks".into());
        }
        let ordinal = *ordinals.get(scenario).ok_or("job has no scenario")?;
        let before = engine::SolveRequest::decode(request_bytes.as_slice())?;
        let mut after = before.clone();
        names.rename_request(scenario, ordinal, &mut after)?;
        check_numeric_input(&before, &after)?;
        if let Some(bytes) = result_bytes {
            let result = engine::SolveResult::decode(bytes.as_slice())?;
            let mut with_solution = before.clone();
            with_solution.initial_solution.clone_from(&result.solution);
            let old = vrp_model::compile(&with_solution)?;
            with_solution = after.clone();
            with_solution.initial_solution.clone_from(&result.solution);
            let new = vrp_model::compile(&with_solution)?;
            let a = vrp_core::oracle::evaluate(
                &old.problem,
                old.initial.as_ref().ok_or("missing saved solution")?,
            )
            .score;
            let b = vrp_core::oracle::evaluate(
                &new.problem,
                new.initial.as_ref().ok_or("missing saved solution")?,
            )
            .score;
            if a != b {
                return Err("saved plan score changed after renaming".into());
            }
        }
        let bytes = after.encode_to_vec();
        let mut hash = Sha256::new();
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
        if meta.kind != 0 {
            hash.update(meta.kind.to_le_bytes());
        }
        if let Some(reference) = &meta.scenario {
            hash.update(reference.encode_to_vec());
        }
        tx.execute(
            "UPDATE jobs SET request=?1,fingerprint=?2 WHERE id=?3",
            params![bytes, hash.finalize().as_slice(), id],
        )?;
        let geometry: Option<Vec<u8>> = tx
            .query_row(
                "SELECT payload FROM job_geometry WHERE job_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(bytes) = geometry {
            let mut geometry = app::JobGeometry::decode(bytes.as_slice())?;
            for leg in &mut geometry.legs {
                leg.agent_id = names.name(&format!("{scenario}/agents"), &leg.agent_id, "R");
                leg.from_location_id =
                    names.name(&format!("{scenario}/locations"), &leg.from_location_id, "L");
                leg.to_location_id =
                    names.name(&format!("{scenario}/locations"), &leg.to_location_id, "L");
            }
            tx.execute(
                "UPDATE job_geometry SET payload=?1 WHERE job_id=?2",
                params![geometry.encode_to_vec(), id],
            )?;
        }
    }
    tx.execute_batch("CREATE TABLE demo_preparation_v1 (completed INTEGER NOT NULL); INSERT INTO demo_preparation_v1 VALUES (1);")?;
    tx.commit()?;
    conn.execute_batch(
        "PRAGMA wal_checkpoint(TRUNCATE); VACUUM; PRAGMA wal_checkpoint(TRUNCATE);",
    )?;
    println!(
        "Prepared {} scenarios / {} versions / {} saved jobs; coordinates, matrices, eligibility and saved scores preserved.",
        ordinals.len(),
        scenarios.len(),
        jobs.len()
    );
    Ok(())
}

use rusqlite::OptionalExtension;
fn main() -> Result<()> {
    let dir = std::env::args_os()
        .nth(1)
        .ok_or("usage: prepare_demo <data-dir>")?;
    run(Path::new(&dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_physical_resources_skills_and_location_indices_survive() {
        let mut names = Names::default();
        let make = || engine::SolveRequest {
            problem: Some(engine::Problem {
                id: "private-request".into(),
                locations: vec![engine::Location {
                    id: "private-place".into(),
                    lng: 121.47,
                    lat: 31.23,
                }],
                agents: vec![engine::Agent {
                    id: "private-agent".into(),
                    resource_id: "same-physical-resource".into(),
                    skills: vec!["private-skill".into()],
                    ..Default::default()
                }],
                tickets: vec![engine::Ticket {
                    id: "private-ticket".into(),
                    skills_required: vec!["private-skill".into()],
                    latest_start: 3600,
                    latest_end: 7200,
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let original = make();
        let mut first = original.clone();
        let mut second = make();
        names.rename_request("one", 1, &mut first).unwrap();
        names.rename_request("two", 2, &mut second).unwrap();
        check_numeric_input(&original, &first).unwrap();
        let a = first.problem.unwrap();
        let b = second.problem.unwrap();
        assert_eq!(a.agents[0].resource_id, b.agents[0].resource_id);
        assert_eq!(a.agents[0].skills, a.tickets[0].skills_required);
        assert_eq!(a.locations[0].lng, 121.47);
        assert_eq!(a.tickets[0].location, 0);
        assert_eq!(a.tickets[0].id, "T001");
        assert_eq!(a.locations[0].id, "L001");
    }
}
