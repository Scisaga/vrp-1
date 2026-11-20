//! Offline workflow acceptance: every test disables provider HTTP completely.
use prost::Message;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use vrp_model::ThreadBudget;
use vrp_proto::{app::v1 as pb, engine::v1 as engine};
use vrp_server::{Config, Runtime, Service, terminal};

struct Fixture {
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus")
            .canonicalize()
            .unwrap();
        let dir = root.join(format!(
            "geo-task-test-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        Self { dir }
    }
    fn config(&self) -> Config {
        let mut c = Config {
            data_dir: self.dir.clone(),
            budget: ThreadBudget { total_threads: 1 },
            max_running: 1,
            max_queued: 8,
            max_request_bytes: 4 * 1024 * 1024,
            memory_budget_bytes: 768 * 1024 * 1024,
            host_reserve_bytes: 64 * 1024 * 1024,
            isolation: vrp_worker::IsolationOptions {
                memory_limit_bytes: Some(128 * 1024 * 1024),
                max_frame_bytes: 8 * 1024 * 1024,
                progress_interval: Duration::from_millis(10),
                ..Default::default()
            },
            ..Config::default()
        };
        c.geo.auto_prefetch = false;
        c.geo.catalog_url.clear();
        c.geo.road_wait = Duration::ZERO;
        c.geo.worker_memory_bytes = 128 * 1024 * 1024;
        c
    }
    fn start(&self) -> Runtime {
        start(self.config())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}
fn start(config: Config) -> Runtime {
    let limit = config.isolation.max_frame_bytes;
    Runtime::open(
        config,
        env!("CARGO_BIN_EXE_vrp-server").into(),
        vec!["worker-child".into(), limit.to_string().into()],
    )
    .unwrap()
}
fn input(millis: u64) -> engine::SolveRequest {
    let locations = vec![
        engine::Location {
            id: "depot".into(),
            lng: 10.0,
            lat: 10.0,
        },
        engine::Location {
            id: "visit-a".into(),
            lng: 10.01,
            lat: 10.01,
        },
        engine::Location {
            id: "visit-b".into(),
            lng: 10.02,
            lat: 10.01,
        },
    ];
    let unknown = u32::MAX - 1;
    let values: Vec<_> = (0..9)
        .flat_map(|k| if k / 3 == k % 3 { 0_u32 } else { unknown }.to_le_bytes())
        .collect();
    engine::SolveRequest {
        problem: Some(engine::Problem {
            locations,
            agents: vec![engine::Agent {
                id: "vehicle".into(),
                start_location: 0,
                end_location: 0,
                ..Default::default()
            }],
            tickets: (1..=2)
                .map(|location| engine::Ticket {
                    id: format!("ticket-{location}"),
                    location,
                    duration: 60,
                    original_agent: Some(0),
                    pinned: true,
                    latest_start: 86400,
                    latest_end: 86400,
                    ..Default::default()
                })
                .collect(),
            matrices: vec![engine::TravelMatrix {
                profile: "car".into(),
                durations: values.clone(),
                distances: values,
                ..Default::default()
            }],
            ..Default::default()
        }),
        options: Some(engine::SolveOptions {
            threads: 1,
            time_limit_ms: millis,
            seed: 7,
            ..Default::default()
        }),
        ..Default::default()
    }
}
fn save(service: &Service, id: &str, request: engine::SolveRequest) -> pb::Scenario {
    service
        .save_scenario(pb::SaveScenarioRequest {
            scenario_id: id.into(),
            name: id.into(),
            request: Some(request),
            ..Default::default()
        })
        .unwrap()
}
fn plan(id: &str, version: u64, key: &str) -> pb::GeoPlanRequest {
    pb::GeoPlanRequest {
        scenario_id: id.into(),
        expected_version: version,
        idempotency_key: key.into(),
        ..Default::default()
    }
}
fn task_terminal(task: &pb::Task) -> bool {
    matches!(
        task.state(),
        pb::TaskState::Succeeded
            | pb::TaskState::Partial
            | pb::TaskState::Failed
            | pb::TaskState::Cancelled
            | pb::TaskState::Interrupted
    )
}
fn wait_task(service: &Service, id: &str, test: impl Fn(&pb::Task) -> bool) -> pb::Task {
    let start = Instant::now();
    loop {
        let task = service.get_task(id).unwrap();
        if test(&task) {
            return task;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "task did not reach expected state: {task:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn wait_job(service: &Service, id: &str, test: impl Fn(&pb::Job) -> bool) -> pb::Job {
    let start = Instant::now();
    loop {
        let job = service.get_job(id).unwrap();
        if test(&job) {
            return job;
        }
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "job did not reach expected state: {job:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn blocker(service: &Service) -> pb::Job {
    let mut request = input(5000);
    vrp_model::prepare_matrices(&mut request).unwrap();
    let job = service
        .submit(pb::SubmitJobRequest {
            source: Some(pb::submit_job_request::Source::Request(Box::new(request))),
            ..Default::default()
        })
        .unwrap();
    wait_job(service, &job.id, |j| {
        j.progress.as_ref().is_some_and(|p| p.search.is_some())
    });
    job
}

#[test]
fn fallback_workflow_materializes_matrix_before_solve_and_retains_frozen_versions() {
    let fixture = Fixture::new();
    let runtime = fixture.start();
    let s = &runtime.service;
    let original = input(30);
    let scene = save(s, "offline", original.clone());
    assert!(!scene.matrix_ready);
    let error = s
        .submit(pb::SubmitJobRequest {
            source: Some(pb::submit_job_request::Source::Scenario(pb::ScenarioRef {
                scenario_id: scene.id.clone(),
                version: scene.version,
            })),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(error.code, "matrix_not_ready");
    let request = plan(&scene.id, scene.version, "offline-plan");
    let task = s.plan_with_roads(request.clone()).unwrap();
    let done = wait_task(s, &task.id, task_terminal);
    assert!(
        matches!(
            done.state(),
            pb::TaskState::Succeeded | pb::TaskState::Partial
        ),
        "{done:?}"
    );
    assert!(!done.result_job_id.is_empty());
    assert_eq!(done.result_scenario_version, 2);
    let ready = s.get_scenario(&scene.id, 2).unwrap();
    assert!(ready.scenario.unwrap().matrix_ready);
    let prepared = ready.request.unwrap();
    vrp_model::validate_ready_matrices(&prepared).unwrap();
    let matrix = &prepared.problem.as_ref().unwrap().matrices[0];
    for k in 0..9 {
        let duration = u32::from_le_bytes(matrix.durations[k * 4..k * 4 + 4].try_into().unwrap());
        let distance = u32::from_le_bytes(matrix.distances[k * 4..k * 4 + 4].try_into().unwrap());
        assert!(duration < u32::MAX - 1 && distance < u32::MAX - 1);
        if k / 3 != k % 3 {
            assert!(duration > 0 && distance > 0);
            assert_eq!(matrix.sources[k], engine::EdgeSource::Fallback as u8);
        }
    }
    assert_eq!(
        s.get_scenario(&scene.id, 1).unwrap().request.unwrap(),
        original
    );
    let frozen =
        engine::SolveRequest::decode(s.payload(&done.result_job_id, false).unwrap().as_slice())
            .unwrap();
    assert_eq!(frozen, prepared);
    assert!(s.get_geometry(&done.result_job_id).unwrap().legs.is_empty());
    let children: Vec<_> = done
        .child_ids
        .iter()
        .map(|id| s.get_task(id).unwrap())
        .collect();
    assert!(
        children
            .iter()
            .any(|t| t.kind() == pb::TaskKind::MatrixPrepare
                && !t.artifacts.is_empty()
                && !t.warnings.is_empty())
    );
    assert!(
        children
            .iter()
            .any(|t| t.kind() == pb::TaskKind::GeometryGenerate)
    );
    let solver = s.get_task(&done.result_job_id).unwrap();
    assert_eq!(solver.kind(), pb::TaskKind::Solve);
    assert_eq!(solver.parent_id, done.id);
    let listed = s
        .list_tasks(pb::ListTasksRequest {
            scenario_id: scene.id.clone(),
            page_size: 100,
            ..Default::default()
        })
        .unwrap();
    assert!(listed.tasks.iter().any(|t| t.id == done.id));
    assert!(
        listed
            .tasks
            .iter()
            .any(|t| t.id == done.result_job_id && t.kind() == pb::TaskKind::Solve)
    );
    // A repeated request must return the original task even after the workflow
    // itself advanced the current business version.
    assert_eq!(s.plan_with_roads(request.clone()).unwrap().id, task.id);
    let mut conflict = request;
    conflict.connector_speed_kmh = 20.0;
    assert_eq!(s.plan_with_roads(conflict).unwrap_err().code, "conflict");
}

#[test]
fn editing_during_preparation_retains_matrix_artifact_without_overwriting_new_scene() {
    let fixture = Fixture::new();
    let runtime = fixture.start();
    let s = &runtime.service;
    let old = input(30);
    let scene = save(s, "editing", old.clone());
    let busy = blocker(s);
    let task = s.plan_with_roads(plan(&scene.id, 1, "edit-plan")).unwrap();
    wait_task(s, &task.id, |t| t.state() == pb::TaskState::Running);
    let mut edited = old.clone();
    edited.options.as_mut().unwrap().seed = 99;
    assert_eq!(
        s.save_scenario(pb::SaveScenarioRequest {
            scenario_id: scene.id.clone(),
            expected_version: 1,
            request: Some(edited.clone()),
            ..Default::default()
        })
        .unwrap()
        .version,
        2
    );
    s.cancel(&busy.id).unwrap();
    wait_job(s, &busy.id, terminal);
    let done = wait_task(s, &task.id, task_terminal);
    assert_eq!(done.state(), pb::TaskState::Failed);
    assert_eq!(done.error.unwrap().code, "conflict");
    assert!(done.result_job_id.is_empty());
    assert_eq!(
        s.get_scenario(&scene.id, 0).unwrap().request.unwrap(),
        edited
    );
    assert_eq!(s.get_scenario(&scene.id, 1).unwrap().request.unwrap(), old);
    assert!(
        done.child_ids
            .iter()
            .map(|id| s.get_task(id).unwrap())
            .any(|t| t.kind() == pb::TaskKind::MatrixPrepare
                && t.artifacts.iter().any(|a| a.kind == "solve_input"))
    );
}

#[test]
fn cancelled_preparation_recovers_as_interrupted_and_retry_uses_retained_input() {
    let fixture = Fixture::new();
    let config = fixture.config();
    let runtime = start(config.clone());
    let s = &runtime.service;
    let original = input(30);
    let scene = save(s, "retry", original.clone());
    let busy = blocker(s);
    let task = s.plan_with_roads(plan(&scene.id, 1, "retry-plan")).unwrap();
    wait_task(s, &task.id, |t| t.state() == pb::TaskState::Running);
    s.cancel_task(&task.id).unwrap();
    let cancelled = wait_task(s, &task.id, task_terminal);
    assert_eq!(cancelled.state(), pb::TaskState::Cancelled);
    assert_eq!(
        s.get_scenario(&scene.id, 0)
            .unwrap()
            .scenario
            .unwrap()
            .version,
        1
    );
    s.cancel(&busy.id).unwrap();
    wait_job(s, &busy.id, terminal);
    runtime.shutdown();
    // Emulate a durable running record left by a hard stop only after all real
    // child processes have been reaped; no process is deliberately orphaned.
    let connection = rusqlite::Connection::open(fixture.dir.join("tasks.sqlite")).unwrap();
    let number = task
        .id
        .strip_prefix("task-")
        .unwrap()
        .parse::<i64>()
        .unwrap();
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT meta FROM generic_tasks WHERE id=?1",
            [number],
            |r| r.get(0),
        )
        .unwrap();
    let mut interrupted = pb::Task::decode(bytes.as_slice()).unwrap();
    interrupted.state = pb::TaskState::Running as i32;
    interrupted.finished_unix_ms = 0;
    interrupted.error = None;
    connection
        .execute(
            "UPDATE generic_tasks SET meta=?1 WHERE id=?2",
            rusqlite::params![interrupted.encode_to_vec(), number],
        )
        .unwrap();
    drop(connection);
    let runtime = start(config);
    let s = &runtime.service;
    let recovered = s.get_task(&task.id).unwrap();
    assert_eq!(recovered.state(), pb::TaskState::Interrupted);
    assert_eq!(
        s.get_scenario(&scene.id, 1).unwrap().request.unwrap(),
        original
    );
    let retry = s.retry_task(&task.id).unwrap();
    assert_ne!(retry.id, task.id);
    assert_eq!(retry.source_task_id, task.id);
    let done = wait_task(s, &retry.id, task_terminal);
    assert!(
        matches!(
            done.state(),
            pb::TaskState::Succeeded | pb::TaskState::Partial
        ),
        "{done:?}"
    );
    assert!(!done.result_job_id.is_empty());
    assert_eq!(
        s.get_task(&task.id).unwrap().state(),
        pb::TaskState::Interrupted
    );
}

#[test]
fn geometry_failure_preserves_successful_solver_and_marks_parent_partial() {
    let fixture = Fixture::new();
    let runtime = fixture.start();
    let s = &runtime.service;
    let scene = save(s, "partial", input(1200));
    let task = s
        .plan_with_roads(plan(&scene.id, 1, "partial-plan"))
        .unwrap();
    let solving = wait_task(s, &task.id, |t| {
        t.progress.as_ref().is_some_and(|p| p.phase == "solving")
    });
    let matrix = solving
        .child_ids
        .iter()
        .map(|id| s.get_task(id).unwrap())
        .find(|t| t.kind() == pb::TaskKind::MatrixPrepare)
        .unwrap();
    let (_, network_path) = s.task_artifact(&matrix.id, "network").unwrap();
    std::fs::write(network_path, b"deliberately damaged graph cache").unwrap();
    let done = wait_task(s, &task.id, task_terminal);
    assert_eq!(done.state(), pb::TaskState::Partial, "{done:?}");
    assert!(!done.result_job_id.is_empty());
    assert!(s.get_job(&done.result_job_id).unwrap().has_result);
    assert!(!done.warnings.is_empty());
    let request =
        engine::SolveRequest::decode(s.payload(&done.result_job_id, false).unwrap().as_slice())
            .unwrap();
    vrp_model::validate_ready_matrices(&request).unwrap();
    assert!(
        s.get_scenario(&scene.id, 2)
            .unwrap()
            .scenario
            .unwrap()
            .matrix_ready
    );
}

#[test]
fn geometry_cannot_attach_another_coordinate_snapshot_to_a_frozen_job() {
    let fixture = Fixture::new();
    let runtime = fixture.start();
    let s = &runtime.service;
    let scene = save(s, "snapshot", input(30));
    let parent = s
        .plan_with_roads(plan(&scene.id, 1, "snapshot-plan"))
        .unwrap();
    let done = wait_task(s, &parent.id, task_terminal);
    assert!(matches!(
        done.state(),
        pb::TaskState::Succeeded | pb::TaskState::Partial
    ));
    let matrix = done
        .child_ids
        .iter()
        .map(|id| s.get_task(id).unwrap())
        .find(|t| t.kind() == pb::TaskKind::MatrixPrepare)
        .unwrap();
    let prepared = s.get_scenario(&scene.id, 2).unwrap().request.unwrap();
    let geometry_input = |job_id: String| pb::SubmitTaskRequest {
        input: Some(pb::TaskInput {
            input: Some(pb::task_input::Input::Geometry(pb::GeometryGenerateInput {
                job_id,
                network_artifact: matrix.id.clone(),
                ..Default::default()
            })),
            ..Default::default()
        }),
        ..Default::default()
    };
    // A matching frozen matrix can reuse its graph for another solver result.
    let matching = s
        .submit(pb::SubmitJobRequest {
            source: Some(pb::submit_job_request::Source::Request(Box::new(
                prepared.clone(),
            ))),
            ..Default::default()
        })
        .unwrap();
    wait_job(s, &matching.id, terminal);
    let geometry = s.submit_task(geometry_input(matching.id)).unwrap();
    assert_eq!(
        wait_task(s, &geometry.id, task_terminal).state(),
        pb::TaskState::Succeeded
    );

    let mut changed = prepared;
    changed.problem.as_mut().unwrap().locations[1].lng += 0.1;
    let foreign = s
        .submit(pb::SubmitJobRequest {
            source: Some(pb::submit_job_request::Source::Request(Box::new(changed))),
            ..Default::default()
        })
        .unwrap();
    wait_job(s, &foreign.id, terminal);
    match s.submit_task(geometry_input(foreign.id.clone())) {
        Err(error) => assert_eq!(error.code, "conflict"),
        Ok(task) => assert_eq!(
            wait_task(s, &task.id, task_terminal).state(),
            pb::TaskState::Failed,
            "a foreign graph must never be attached successfully"
        ),
    }
    assert!(s.get_job(&foreign.id).unwrap().has_result);
}

#[test]
fn history_plan_delete_keeps_saved_matrix_and_reusable_road_snapshot() {
    let fixture = Fixture::new();
    let runtime = fixture.start();
    let s = &runtime.service;
    let scene = save(s, "delete-plan", input(30));
    let parent = s
        .plan_with_roads(plan(&scene.id, 1, "delete-plan-workflow"))
        .unwrap();
    let done = wait_task(s, &parent.id, task_terminal);
    assert_eq!(done.state(), pb::TaskState::Succeeded, "{done:?}");
    let matrix = done
        .child_ids
        .iter()
        .map(|id| s.get_task(id).unwrap())
        .find(|t| t.kind() == pb::TaskKind::MatrixPrepare)
        .unwrap();
    let geometry = done
        .child_ids
        .iter()
        .map(|id| s.get_task(id).unwrap())
        .find(|t| t.kind() == pb::TaskKind::GeometryGenerate)
        .unwrap();
    let frozen = s.get_scenario(&scene.id, 2).unwrap().request.unwrap();
    let network = s.task_artifact(&matrix.id, "network").unwrap().1;
    let fingerprint = vrp_geo::RoadNetwork::load(&network).unwrap().fingerprint;
    s.delete_task(&done.result_job_id).unwrap();
    assert!(s.get_job(&done.result_job_id).is_err());
    assert!(s.get_task(&parent.id).is_err());
    assert!(s.get_task(&geometry.id).is_err());
    assert!(!fixture.dir.join("task-artifacts").join(&parent.id).exists());
    assert!(
        !fixture
            .dir
            .join("task-artifacts")
            .join(&geometry.id)
            .exists()
    );
    let retained = s.get_task(&matrix.id).unwrap();
    assert!(retained.parent_id.is_empty());
    assert_eq!(retained.scenario.unwrap().version, 2);
    assert_eq!(
        s.get_scenario(&scene.id, 2).unwrap().request.unwrap(),
        frozen
    );
    assert_eq!(
        vrp_geo::RoadNetwork::load(&network).unwrap().fingerprint,
        fingerprint
    );
    assert_eq!(s.delete_task(&matrix.id).unwrap_err().code, "conflict");
    // The retained graph still matches the saved matrix and can render a later
    // result without preparing a different road snapshot.
    let job = s
        .submit(pb::SubmitJobRequest {
            source: Some(pb::submit_job_request::Source::Scenario(pb::ScenarioRef {
                scenario_id: scene.id.clone(),
                version: 2,
            })),
            ..Default::default()
        })
        .unwrap();
    wait_job(s, &job.id, terminal);
    let geometry = s
        .submit_task(pb::SubmitTaskRequest {
            input: Some(pb::TaskInput {
                input: Some(pb::task_input::Input::Geometry(pb::GeometryGenerateInput {
                    job_id: job.id,
                    network_artifact: matrix.id.clone(),
                    ..Default::default()
                })),
                ..Default::default()
            }),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        wait_task(s, &geometry.id, task_terminal).state(),
        pb::TaskState::Succeeded
    );
    assert_eq!(
        s.get_scenario_metadata(&scene.id, 2)
            .unwrap()
            .geo
            .unwrap()
            .task_id,
        matrix.id
    );
}

#[test]
fn deleting_a_scene_collects_its_terminal_geo_workflow_and_matrix_artifacts() {
    let fixture = Fixture::new();
    let runtime = fixture.start();
    let s = &runtime.service;
    let scene = save(s, "delete-geo-scene", input(30));
    let parent = s
        .plan_with_roads(plan(&scene.id, 1, "delete-geo-scene-workflow"))
        .unwrap();
    let done = wait_task(s, &parent.id, task_terminal);
    assert_eq!(done.state(), pb::TaskState::Succeeded, "{done:?}");
    let ids = done
        .child_ids
        .iter()
        .filter(|id| id.starts_with("task-"))
        .cloned()
        .chain(std::iter::once(parent.id))
        .collect::<Vec<_>>();
    s.delete_scenario(pb::DeleteScenarioRequest {
        scenario_id: scene.id.clone(),
        expected_version: 2,
    })
    .unwrap();
    assert!(s.get_scenario(&scene.id, 0).is_err());
    assert!(s.get_job(&done.result_job_id).is_err());
    for id in ids {
        assert!(s.get_task(&id).is_err());
        assert!(!fixture.dir.join("task-artifacts").join(id).exists());
    }
}

#[test]
fn both_plan_delete_routes_refuse_external_geometry_until_it_is_collected() {
    let fixture = Fixture::new();
    let runtime = fixture.start();
    let s = &runtime.service;
    let scene = save(s, "referenced-plan", input(30));
    let parent = s
        .plan_with_roads(plan(&scene.id, 1, "referenced-plan-workflow"))
        .unwrap();
    let done = wait_task(s, &parent.id, task_terminal);
    assert_eq!(done.state(), pb::TaskState::Succeeded, "{done:?}");
    let matrix = done
        .child_ids
        .iter()
        .map(|id| s.get_task(id).unwrap())
        .find(|t| t.kind() == pb::TaskKind::MatrixPrepare)
        .unwrap();
    let external = s
        .submit_task(pb::SubmitTaskRequest {
            input: Some(pb::TaskInput {
                input: Some(pb::task_input::Input::Geometry(pb::GeometryGenerateInput {
                    job_id: done.result_job_id.clone(),
                    network_artifact: matrix.id.clone(),
                    ..Default::default()
                })),
                ..Default::default()
            }),
            ..Default::default()
        })
        .unwrap();
    wait_task(s, &external.id, task_terminal);
    for error in [
        s.delete(&done.result_job_id).unwrap_err(),
        s.delete_task(&done.result_job_id).unwrap_err(),
    ] {
        assert_eq!(error.code, "conflict");
        assert!(error.message.contains(&external.id));
    }
    assert_eq!(
        s.get_task(&matrix.id).unwrap().parent_id,
        parent.id,
        "failed deletion must roll back snapshot promotion"
    );
    assert!(s.get_job(&done.result_job_id).unwrap().has_result);
    s.delete_task(&external.id).unwrap();
    s.delete(&done.result_job_id).unwrap();
    assert!(s.get_task(&matrix.id).unwrap().parent_id.is_empty());
}
