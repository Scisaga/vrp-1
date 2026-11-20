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
            "server-test-{}-{}-{}",
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
        let mut config = Config {
            data_dir: self.dir.clone(),
            budget: ThreadBudget { total_threads: 2 },
            max_running: 2,
            max_queued: 2,
            max_request_bytes: 8 * 1024 * 1024,
            memory_budget_bytes: 1024 * 1024 * 1024,
            host_reserve_bytes: 128 * 1024 * 1024,
            isolation: vrp_worker::IsolationOptions {
                max_frame_bytes: 16 * 1024 * 1024,
                memory_limit_bytes: Some(256 * 1024 * 1024),
                progress_interval: Duration::from_millis(10),
                ..Default::default()
            },
            ..Config::default()
        };
        config.geo.auto_prefetch = false;
        config.geo.catalog_url.clear();
        config
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
fn input(threads: u32, millis: u64) -> engine::SolveRequest {
    let mut request = engine::SolveRequest {
        problem: Some(engine::Problem {
            locations: vec![engine::Location::default()],
            matrices: vec![engine::TravelMatrix {
                durations: vec![0; 4],
                distances: vec![0; 4],
                ..Default::default()
            }],
            agents: vec![engine::Agent::default()],
            tickets: vec![engine::Ticket {
                original_agent: Some(0),
                pinned: true,
                latest_start: 86400,
                latest_end: 86400,
                ..Default::default()
            }],
            ..Default::default()
        }),
        options: Some(engine::SolveOptions {
            threads,
            time_limit_ms: millis,
            seed: u64::MAX,
            ..Default::default()
        }),
        ..Default::default()
    };
    vrp_model::prepare_matrices(&mut request).unwrap();
    request
}
fn submit(service: &Service, threads: u32, millis: u64) -> pb::Job {
    service
        .submit(pb::SubmitJobRequest {
            source: Some(pb::submit_job_request::Source::Request(Box::new(input(
                threads, millis,
            )))),
            ..Default::default()
        })
        .unwrap()
}
fn wait(service: &Service, id: &str, matches: impl Fn(&pb::Job) -> bool) -> pb::Job {
    let start = Instant::now();
    loop {
        let job = service.get_job(id).unwrap();
        if matches(&job) {
            return job;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "job did not reach expected state: {job:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn searching(job: &pb::Job) -> bool {
    job.progress.as_ref().is_some_and(|p| p.search.is_some())
}

#[test]
fn solve_submission_rejects_unprepared_matrices_without_consuming_storage() {
    let fixture = Fixture::new();
    let runtime = fixture.start();
    let service = &runtime.service;
    let before = service.health().unwrap().payload_bytes;
    let mut missing = input(1, 20);
    missing.problem.as_mut().unwrap().matrices.clear();
    let error = service
        .submit(pb::SubmitJobRequest {
            source: Some(pb::submit_job_request::Source::Request(Box::new(
                missing.clone(),
            ))),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(error.code, "matrix_not_ready");
    assert_eq!(service.health().unwrap().payload_bytes, before);
    let mut unknown = input(1, 20);
    let matrix = &mut unknown.problem.as_mut().unwrap().matrices[0];
    matrix
        .durations
        .copy_from_slice(&vrp_core::problem::UNKNOWN.to_le_bytes());
    matrix
        .distances
        .copy_from_slice(&vrp_core::problem::UNKNOWN.to_le_bytes());
    let error = service
        .submit(pb::SubmitJobRequest {
            source: Some(pb::submit_job_request::Source::Request(Box::new(unknown))),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(error.code, "matrix_not_ready");
    assert_eq!(service.health().unwrap().payload_bytes, before);
    // Saving an editable business scenario may precede matrix preparation, but
    // a solve must still refuse that unprepared immutable scenario version.
    let scene = service
        .save_scenario(pb::SaveScenarioRequest {
            scenario_id: "needs-matrix".into(),
            request: Some(missing),
            ..Default::default()
        })
        .unwrap();
    let stored = service.health().unwrap().payload_bytes;
    let error = service
        .submit(pb::SubmitJobRequest {
            source: Some(pb::submit_job_request::Source::Scenario(pb::ScenarioRef {
                scenario_id: scene.id,
                version: scene.version,
            })),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(error.code, "matrix_not_ready");
    assert_eq!(service.health().unwrap().payload_bytes, stored);
    assert_eq!(service.health().unwrap().reserved_threads, 0);
}

#[test]
fn scenario_versions_idempotency_and_frozen_job_input() {
    let fixture = Fixture::new();
    let runtime = fixture.start();
    let service = &runtime.service;
    let original = input(1, 80);
    let mut save = pb::SaveScenarioRequest {
        scenario_id: "north".into(),
        name: "North".into(),
        request: Some(original.clone()),
        ..Default::default()
    };
    assert_eq!(service.save_scenario(save.clone()).unwrap().version, 1);
    assert_eq!(
        service.save_scenario(save.clone()).unwrap_err().code,
        "conflict"
    );
    save.expected_version = 1;
    save.request
        .as_mut()
        .unwrap()
        .options
        .as_mut()
        .unwrap()
        .seed = 12;
    assert_eq!(service.save_scenario(save).unwrap().version, 2);
    let job_request = pb::SubmitJobRequest {
        kind: 0,
        options: None,
        idempotency_key: "north-run".into(),
        source: Some(pb::submit_job_request::Source::Scenario(pb::ScenarioRef {
            scenario_id: "north".into(),
            version: 1,
        })),
    };
    let job = service.submit(job_request.clone()).unwrap();
    assert_eq!(service.submit(job_request.clone()).unwrap().id, job.id);
    let mut conflict = job_request.clone();
    conflict.source = Some(pb::submit_job_request::Source::Scenario(pb::ScenarioRef {
        scenario_id: "north".into(),
        version: 2,
    }));
    assert_eq!(service.submit(conflict).unwrap_err().code, "conflict");
    assert_eq!(
        engine::SolveRequest::decode(service.payload(&job.id, false).unwrap().as_slice()).unwrap(),
        original
    );
    let done = wait(service, &job.id, terminal);
    assert_eq!(done.state(), pb::JobState::Succeeded);
    assert_eq!(done.stats.unwrap().seed, u64::MAX);
    let result =
        engine::SolveResult::decode(service.payload(&job.id, true).unwrap().as_slice()).unwrap();
    assert_eq!(result.score.unwrap().hard, 0);
    assert_eq!(service.submit(job_request.clone()).unwrap().id, job.id);
    service.delete(&job.id).unwrap();
    assert_eq!(service.get_job(&job.id).unwrap_err().code, "not_found");
    assert_ne!(service.submit(job_request).unwrap().id, job.id);
}

#[test]
fn concurrent_scenarios_share_budget_and_queue_is_bounded() {
    let fixture = Fixture::new();
    let runtime = fixture.start();
    let service = &runtime.service;
    let a = submit(service, 2, 5000);
    wait(service, &a.id, searching);
    let b = submit(service, 1, 300);
    let c = submit(service, 1, 300);
    let request = pb::SubmitJobRequest {
        source: Some(pb::submit_job_request::Source::Request(Box::new(input(
            1, 10,
        )))),
        ..Default::default()
    };
    assert_eq!(service.submit(request).unwrap_err().code, "capacity");
    assert_eq!(service.health().unwrap().reserved_threads, 2);
    service.cancel(&a.id).unwrap();
    let done_a = wait(service, &a.id, terminal);
    assert_eq!(done_a.state(), pb::JobState::Cancelled);
    assert!(done_a.has_result);
    wait(service, &b.id, searching);
    wait(service, &c.id, searching);
    let status = service.health().unwrap();
    assert_eq!(status.running_jobs, 2);
    assert_eq!(status.reserved_threads, 2);
    for id in [&b.id, &c.id] {
        let done = wait(service, id, terminal);
        assert_eq!(done.state(), pb::JobState::Succeeded);
        assert!(done.started_unix_ms >= done_a.finished_unix_ms);
    }
    // Terminal publication may precede final process/permit cleanup.
    let cleanup = Instant::now();
    while service.health().unwrap().reserved_threads != 0 {
        assert!(cleanup.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn fifo_does_not_bypass_a_large_job_and_queued_cancel_releases_capacity() {
    let fixture = Fixture::new();
    let runtime = fixture.start();
    let service = &runtime.service;
    let a = submit(service, 1, 5000);
    wait(service, &a.id, searching);
    let b = submit(service, 2, 5000);
    let c = submit(service, 1, 50);
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(
        service.get_job(&b.id).unwrap().state(),
        pb::JobState::Queued
    );
    assert_eq!(
        service.get_job(&c.id).unwrap().state(),
        pb::JobState::Queued
    );
    let cancelled = service.cancel(&b.id).unwrap();
    assert_eq!(cancelled.state(), pb::JobState::Cancelled);
    assert!(!cancelled.has_result);
    assert_eq!(service.payload(&b.id, true).unwrap_err().code, "conflict");
    assert_eq!(
        wait(service, &c.id, terminal).state(),
        pb::JobState::Succeeded
    );
    assert_eq!(
        service.get_job(&a.id).unwrap().state(),
        pb::JobState::Running
    );
    service.cancel(&a.id).unwrap();
    wait(service, &a.id, terminal);
}

#[test]
fn invalid_input_fails_in_child_and_next_job_runs() {
    let fixture = Fixture::new();
    let runtime = fixture.start();
    let service = &runtime.service;
    let mut invalid = input(1, 30);
    invalid.problem.as_mut().unwrap().tickets[0].location = 99;
    let job = service
        .submit(pb::SubmitJobRequest {
            source: Some(pb::submit_job_request::Source::Request(Box::new(invalid))),
            ..Default::default()
        })
        .unwrap();
    let failed = wait(service, &job.id, terminal);
    assert_eq!(failed.state(), pb::JobState::Failed);
    assert_eq!(failed.error.unwrap().code, "invalid_request");
    assert!(!failed.has_result);
    let next = submit(service, 2, 30);
    assert_eq!(
        wait(service, &next.id, terminal).state(),
        pb::JobState::Succeeded
    );
    assert_eq!(service.health().unwrap().reserved_threads, 0);
}

#[test]
fn restart_retains_queue_marks_interrupted_and_locks_the_data_directory() {
    let fixture = Fixture::new();
    let config = fixture.config();
    let runtime = start(config.clone());
    let duplicate = Runtime::open(
        config.clone(),
        env!("CARGO_BIN_EXE_vrp-server").into(),
        vec![],
    );
    assert!(matches!(duplicate, Err(e) if e.code == "conflict"));
    let a = submit(&runtime.service, 2, 5000);
    wait(&runtime.service, &a.id, searching);
    let b = submit(&runtime.service, 1, 30);
    runtime.shutdown();
    // Emulate the durable state left by a hard process stop. The old worker has
    // already been reaped, so this test never leaves orphan processes.
    let conn = rusqlite::Connection::open(fixture.dir.join("tasks.sqlite")).unwrap();
    let bytes: Vec<u8> = conn
        .query_row("SELECT meta FROM jobs WHERE id=?1", [&a.id], |r| r.get(0))
        .unwrap();
    let mut unfinished = pb::Job::decode(bytes.as_slice()).unwrap();
    unfinished.state = pb::JobState::Running as i32;
    unfinished.has_result = false;
    unfinished.finished_unix_ms = 0;
    conn.execute(
        "UPDATE jobs SET state=1,meta=?1,result=NULL WHERE id=?2",
        rusqlite::params![unfinished.encode_to_vec(), a.id],
    )
    .unwrap();
    drop(conn);
    let restarted = start(config);
    let interrupted = restarted.service.get_job(&a.id).unwrap();
    assert_eq!(interrupted.state(), pb::JobState::Interrupted);
    assert_eq!(interrupted.error.unwrap().code, "server_restarted");
    assert_eq!(
        wait(&restarted.service, &b.id, terminal).state(),
        pb::JobState::Succeeded
    );
}

#[test]
fn memory_storage_and_thread_limits_reject_impossible_requests() {
    let fixture = Fixture::new();
    let mut invalid = fixture.config();
    invalid.memory_budget_bytes = invalid.host_reserve_bytes;
    assert!(Runtime::open(invalid, env!("CARGO_BIN_EXE_vrp-server").into(), vec![]).is_err());
    let mut config = fixture.config();
    config.max_records = 1;
    let runtime = start(config);
    let service = &runtime.service;
    let too_many = pb::SubmitJobRequest {
        source: Some(pb::submit_job_request::Source::Request(Box::new(input(
            3, 10,
        )))),
        ..Default::default()
    };
    assert_eq!(
        service.submit(too_many).unwrap_err().code,
        "invalid_request"
    );
    let first = submit(service, 1, 30);
    wait(service, &first.id, terminal);
    let second = pb::SubmitJobRequest {
        source: Some(pb::submit_job_request::Source::Request(Box::new(input(
            1, 10,
        )))),
        ..Default::default()
    };
    assert_eq!(service.submit(second).unwrap_err().code, "capacity");
    service.delete(&first.id).unwrap();
    assert_eq!(service.health().unwrap().payload_bytes, 0);
    let next = submit(service, 1, 20);
    assert_eq!(
        wait(service, &next.id, terminal).state(),
        pb::JobState::Succeeded
    );
}

mod http_tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        extract::ConnectInfo,
        http::{Request, StatusCode},
        response::Response,
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    // Authentication cases model remote requests. The shared call helper models
    // real local direct connections unless this outer layer replaces its peer.
    fn remote_router(service: Service, token: String) -> Router {
        vrp_server::http::router(service, Some(token)).layer(axum::Extension(ConnectInfo(
            "192.0.2.10:1234".parse::<std::net::SocketAddr>().unwrap(),
        )))
    }

    #[tokio::test]
    async fn trend_http_is_authenticated_and_supports_json_and_binary() {
        let fixture = Fixture::new();
        let runtime = fixture.start();
        let job = submit(&runtime.service, 1, 1_200);
        let done = wait(&runtime.service, &job.id, terminal);
        let router = remote_router(runtime.service.clone(), "secret".into());
        let uri = format!("/v1/jobs/{}/trend", job.id);
        let response = call(&router, "GET", &uri, vec![], &[]).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        drop(bytes(response).await);
        let auth = [("authorization", "Bearer secret")];
        let response = call(&router, "GET", &uri, vec![], &auth).await;
        assert_eq!(response.status(), StatusCode::OK);
        let trend: pb::JobTrend = serde_json::from_slice(&bytes(response).await).unwrap();
        assert_eq!(trend.job_id, done.id);
        assert_eq!(trend.revision, done.revision);
        assert!(trend.samples.len() >= 2);
        assert!(!trend.partial_history);
        let response = call(
            &router,
            "GET",
            &uri,
            vec![],
            &[
                ("authorization", "Bearer secret"),
                ("accept", "application/protobuf"),
            ],
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            pb::JobTrend::decode(bytes(response).await.as_slice()).unwrap(),
            trend
        );
        let response = call(&router, "GET", "/v1/jobs/999999/trend", vec![], &auth).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn scenario_deletion_requires_authentication_and_a_current_version() {
        let fixture = Fixture::new();
        let runtime = fixture.start();
        runtime
            .service
            .save_scenario(pb::SaveScenarioRequest {
                scenario_id: "remove".into(),
                request: Some(input(1, 20)),
                ..Default::default()
            })
            .unwrap();
        let router = remote_router(runtime.service.clone(), "secret".into());
        let auth = [("authorization", "Bearer secret")];
        for (path, headers, expected) in [
            (
                "/v1/scenarios/remove?expected_version=1",
                &[][..],
                StatusCode::UNAUTHORIZED,
            ),
            ("/v1/scenarios/remove", &auth[..], StatusCode::BAD_REQUEST),
            (
                "/v1/scenarios/remove?expected_version=2",
                &auth[..],
                StatusCode::CONFLICT,
            ),
            (
                "/v1/scenarios/remove?scenario_id=other&expected_version=1",
                &auth[..],
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let response = call(&router, "DELETE", path, vec![], headers).await;
            assert_eq!(response.status(), expected, "{path}");
            drop(bytes(response).await);
        }
        let response = call(&router, "GET", "/v1/scenarios", vec![], &auth).await;
        let page: pb::ListScenariosResponse =
            serde_json::from_slice(&bytes(response).await).unwrap();
        assert_eq!(page.scenarios[0].summary.as_ref().unwrap().ticket_count, 1);
        let response = call(
            &router,
            "DELETE",
            "/v1/scenarios/remove?expected_version=1",
            vec![],
            &auth,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let _: pb::Empty = serde_json::from_slice(&bytes(response).await).unwrap();
        let response = call(&router, "GET", "/v1/scenarios/remove", vec![], &auth).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    async fn tcp_request(
        address: std::net::SocketAddr,
        method: &str,
        path: &str,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> (StatusCode, Vec<u8>) {
        let socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let (mut client, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(socket))
                .await
                .unwrap();
        let driver = tokio::spawn(connection);
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", address.to_string())
            .header("content-type", content_type)
            .header("connection", "close")
            .body(http_body_util::Full::new(axum::body::Bytes::from(bytes)))
            .unwrap();
        let response = client.send_request(request).await.unwrap();
        let status = response.status();
        let data = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        drop(client);
        driver.await.unwrap().unwrap();
        (status, data)
    }

    #[tokio::test]
    async fn real_http_listener_accepts_binary_scenarios_and_serves_job_results() {
        let fixture = Fixture::new();
        let runtime = fixture.start();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = vrp_server::http::router(
            runtime.service.clone(),
            Some("test-listener-bearer-token".into()),
        );
        let (shutdown, stop) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = stop.await;
            })
            .await
            .unwrap();
        });
        let scenario = pb::SaveScenarioRequest {
            scenario_id: "north".into(),
            name: "North".into(),
            request: Some(input(1, 50)),
            ..Default::default()
        };
        let (status, data) = tcp_request(
            address,
            "POST",
            "/v1/scenarios",
            "application/protobuf",
            scenario.encode_to_vec(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let saved: pb::Scenario = serde_json::from_slice(&data).unwrap();
        assert_eq!(saved.version, 1);
        let (status, data) = tcp_request(
            address,
            "GET",
            "/v1/scenarios/north?version=1",
            "application/json",
            vec![],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_slice::<pb::ScenarioVersion>(&data)
                .unwrap()
                .request,
            scenario.request
        );
        let request = pb::SubmitJobRequest {
            source: Some(pb::submit_job_request::Source::Scenario(pb::ScenarioRef {
                scenario_id: "north".into(),
                version: 1,
            })),
            ..Default::default()
        };
        let (status, data) = tcp_request(
            address,
            "POST",
            "/v1/jobs",
            "application/protobuf",
            request.encode_to_vec(),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let job: pb::Job = serde_json::from_slice(&data).unwrap();
        assert_eq!(
            wait(&runtime.service, &job.id, terminal).state(),
            pb::JobState::Succeeded
        );
        let (status, data) = tcp_request(
            address,
            "GET",
            &format!("/v1/jobs/{}/result", job.id),
            "application/json",
            vec![],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            engine::SolveResult::decode(data.as_slice())
                .unwrap()
                .stats
                .unwrap()
                .seed,
            u64::MAX
        );
        shutdown.send(()).unwrap();
        server.await.unwrap();
    }

    async fn call(
        router: &Router,
        method: &str,
        path: &str,
        bytes: Vec<u8>,
        headers: &[(&str, &str)],
    ) -> Response {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .extension(ConnectInfo(
                "127.0.0.1:1234".parse::<std::net::SocketAddr>().unwrap(),
            ))
            .header("host", "localhost")
            .header("content-type", "application/json");
        for (key, value) in headers {
            request = request.header(*key, *value);
        }
        router
            .clone()
            .oneshot(request.body(Body::from(bytes)).unwrap())
            .await
            .unwrap()
    }
    async fn bytes(response: Response) -> Vec<u8> {
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec()
    }

    #[tokio::test]
    async fn workbench_shell_native_import_and_execution_http_contract() {
        let fixture = Fixture::new();
        let runtime = fixture.start();
        let router = remote_router(runtime.service.clone(), "test-browser-secret".into());
        for path in [
            "/",
            "/app.js",
            "/schedule.js",
            "/progress.js",
            "/brand.svg",
            "/forms.js",
            "/style.css",
        ] {
            let response = call(&router, "GET", path, vec![], &[]).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert!(response.headers().contains_key("content-security-policy"));
            assert!(!bytes(response).await.is_empty());
        }
        let headers = [("authorization", "Bearer test-browser-secret")];
        let mut request = input(1, 30);
        let p = request.problem.as_mut().unwrap();
        p.agents[0].id = "vehicle".into();
        p.agents[0].resource_id = "physical-vehicle".into();
        p.tickets[0].id = "ticket".into();
        p.locations[0].id = "depot".into();
        let import = pb::ImportScenarioRequest {
            scenario_id: "scene".into(),
            name: "Native".into(),
            request_bytes: request.encode_to_vec(),
            ..Default::default()
        };
        let response = call(
            &router,
            "POST",
            "/v1/scenarios/import",
            serde_json::to_vec(&import).unwrap(),
            &headers,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        drop(bytes(response).await);
        let start = pb::StartOperationRequest {
            scenario: Some(pb::ScenarioRef {
                scenario_id: "scene".into(),
                version: 1,
            }),
            policy: Some(pb::OperationPolicy {
                clock: pb::ExecutionClock::Replay as i32,
                ..Default::default()
            }),
            ..Default::default()
        };
        let response = call(
            &router,
            "POST",
            "/v1/operations/start",
            serde_json::to_vec(&start).unwrap(),
            &headers,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let op: pb::Operation = serde_json::from_slice(&bytes(response).await).unwrap();
        let metadata = call(
            &router,
            "GET",
            "/v1/scenarios/scene/metadata",
            vec![],
            &headers,
        )
        .await;
        assert_eq!(metadata.status(), StatusCode::OK);
        let metadata: pb::Scenario = serde_json::from_slice(&bytes(metadata).await).unwrap();
        assert_eq!(metadata.id, "scene");
        assert_eq!(metadata.version, 1);
        let event = pb::ExecutionEvent {
            scenario_id: "scene".into(),
            expected_revision: op.revision,
            event_id: "departure".into(),
            at: 1,
            tickets: vec![pb::TicketUpdate {
                ticket_id: "ticket".into(),
                agent_id: "vehicle".into(),
                phase: pb::TicketPhase::EnRoute as i32,
                ..Default::default()
            }],
            ..Default::default()
        };
        let response = call(
            &router,
            "POST",
            "/v1/operations/scene/events",
            serde_json::to_vec(&event).unwrap(),
            &headers,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let saved = bytes(response).await;
        let retry = call(
            &router,
            "POST",
            "/v1/operations/scene/events",
            serde_json::to_vec(&event).unwrap(),
            &headers,
        )
        .await;
        assert_eq!(saved, bytes(retry).await);
        let historical = call(
            &router,
            "GET",
            "/v1/operations/scene?revision=1",
            vec![],
            &headers,
        )
        .await;
        let first: pb::Operation = serde_json::from_slice(&bytes(historical).await).unwrap();
        assert_eq!(first, op);
        let response = call(
            &router,
            "GET",
            "/v1/operations/scene/history?page_size=1",
            vec![],
            &headers,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let page: pb::ListOperationHistoryResponse =
            serde_json::from_slice(&bytes(response).await).unwrap();
        assert_eq!(page.entries[0].kind, "event");
        assert_eq!(page.entries[0].ticket_ids, vec!["ticket"]);
        assert!(page.next_before_revision > 0);
        let denied = call(&router, "GET", "/v1/operations/scene", vec![], &[]).await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn protojson_binary_download_and_sse_reconnect() {
        let fixture = Fixture::new();
        let runtime = fixture.start();
        let router = vrp_server::http::router(runtime.service.clone(), None);
        let input = input(1, 200);
        let request = pb::SubmitJobRequest {
            source: Some(pb::submit_job_request::Source::Request(Box::new(
                input.clone(),
            ))),
            ..Default::default()
        };
        let json = serde_json::to_vec(&request).unwrap();
        assert!(String::from_utf8_lossy(&json).contains("\"18446744073709551615\""));
        let response = call(&router, "POST", "/v1/jobs", json, &[]).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let job: pb::Job = serde_json::from_slice(&bytes(response).await).unwrap();
        let stream = call(
            &router,
            "GET",
            &format!("/v1/jobs/{}/events", job.id),
            vec![],
            &[("last-event-id", &format!("{}:0", job.id))],
        )
        .await;
        assert_eq!(stream.headers()["content-type"], "text/event-stream");
        let events = String::from_utf8(bytes(stream).await).unwrap();
        assert!(events.contains("event: snapshot"));
        assert!(events.contains("JOB_STATE_SUCCEEDED"));
        let snapshots: Vec<pb::Job> = events
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(|json| serde_json::from_str(json).unwrap())
            .collect();
        assert!(
            snapshots
                .windows(2)
                .all(|pair| pair[0].revision < pair[1].revision)
        );
        let done = snapshots.last().unwrap();
        assert!(terminal(done));
        let replay = call(
            &router,
            "GET",
            &format!("/v1/jobs/{}/events", job.id),
            vec![],
            &[("last-event-id", &format!("{}:{}", job.id, done.revision))],
        )
        .await;
        assert!(
            String::from_utf8(bytes(replay).await)
                .unwrap()
                .contains("JOB_STATE_SUCCEEDED")
        );
        let bad_event = call(
            &router,
            "GET",
            &format!("/v1/jobs/{}/events", job.id),
            vec![],
            &[("last-event-id", "wrong:99")],
        )
        .await;
        assert_eq!(bad_event.status(), StatusCode::BAD_REQUEST);
        drop(bad_event);
        let archive = call(
            &router,
            "GET",
            &format!("/v1/jobs/{}/request", job.id),
            vec![],
            &[],
        )
        .await;
        assert_eq!(archive.headers()["content-type"], "application/protobuf");
        assert_eq!(
            engine::SolveRequest::decode(bytes(archive).await.as_slice()).unwrap(),
            input
        );
        let result = call(
            &router,
            "GET",
            &format!("/v1/jobs/{}/result", job.id),
            vec![],
            &[("accept", "application/json")],
        )
        .await;
        let value: engine::SolveResult = serde_json::from_slice(&bytes(result).await).unwrap();
        assert_eq!(value.stats.unwrap().seed, u64::MAX);
        let list = call(&router, "GET", "/v1/jobs?page_size=1", vec![], &[]).await;
        assert_eq!(list.status(), StatusCode::OK);
        let list: pb::ListJobsResponse = serde_json::from_slice(&bytes(list).await).unwrap();
        assert_eq!(list.jobs.len(), 1);
    }

    #[tokio::test]
    async fn auth_origin_body_limits_and_unknown_fields_are_checked() {
        let fixture = Fixture::new();
        let mut config = fixture.config();
        config.max_request_bytes = 1024;
        let runtime = start(config);
        let token = "test-token-with-32-characters-abc";
        let router = remote_router(runtime.service.clone(), token.into());
        let denied = call(&router, "GET", "/healthz", vec![], &[]).await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            serde_json::from_slice::<pb::Error>(&bytes(denied).await)
                .unwrap()
                .code,
            "unauthorized"
        );
        let bearer = format!("Bearer {token}");
        let auth = [("authorization", bearer.as_str())];
        let map_denied = call(&router, "GET", "/v1/map-config", vec![], &[]).await;
        assert_eq!(map_denied.status(), StatusCode::UNAUTHORIZED);
        drop(map_denied);
        let map_config = call(&router, "GET", "/v1/map-config", vec![], &auth).await;
        assert_eq!(map_config.status(), StatusCode::OK);
        let policy = map_config.headers()["content-security-policy"]
            .to_str()
            .unwrap();
        assert!(policy.contains("https://tile.openstreetmap.org"));
        assert_eq!(
            map_config.headers()["referrer-policy"],
            "strict-origin-when-cross-origin"
        );
        assert!(
            serde_json::from_slice::<pb::MapConfig>(&bytes(map_config).await)
                .unwrap()
                .raster_url
                .contains("openstreetmap")
        );
        for path in [
            "/map.js",
            "/vendor/maplibre-gl.mjs",
            "/vendor/maplibre-gl-worker.mjs",
            "/vendor/maplibre-gl-shared.mjs",
            "/vendor/pmtiles.js",
        ] {
            assert_eq!(
                call(&router, "GET", path, vec![], &[]).await.status(),
                StatusCode::OK
            );
        }
        let cross = call(
            &router,
            "POST",
            "/v1/jobs/1/cancel",
            vec![],
            &[
                ("authorization", &bearer),
                ("origin", "https://other.example"),
            ],
        )
        .await;
        assert_eq!(cross.status(), StatusCode::FORBIDDEN);
        let large = call(&router, "POST", "/v1/jobs", vec![b' '; 1025], &auth).await;
        assert_eq!(large.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let unknown = call(
            &router,
            "POST",
            "/v1/jobs",
            br#"{"unknownField":1}"#.to_vec(),
            &auth,
        )
        .await;
        assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
        let ready = call(&router, "GET", "/healthz", vec![], &auth).await;
        assert_eq!(ready.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn token_is_optional_only_for_direct_loopback_access() {
        let fixture = Fixture::new();
        let runtime = fixture.start();
        let router = vrp_server::http::router(
            runtime.service.clone(),
            Some("test-direct-loopback-token".into()),
        );
        for (peer, host, extra_header, authorization, expected) in [
            ("127.0.0.1:1234", "127.0.0.1:8080", None, None, 200),
            ("127.0.0.1:1234", "localhost:8080", None, None, 200),
            ("[::1]:1234", "[::1]:8080", None, None, 200),
            ("192.0.2.10:1234", "192.0.2.20:8080", None, None, 401),
            ("127.0.0.1:1234", "192.0.2.20:8080", None, None, 401),
            ("192.0.2.10:1234", "127.0.0.1:8080", None, None, 401),
            ("192.0.2.10:1234", "localhost:8080", None, None, 401),
            (
                "127.0.0.1:1234",
                "127.0.0.1:8080",
                Some(("forwarded", "for=192.0.2.10")),
                None,
                401,
            ),
            (
                "127.0.0.1:1234",
                "127.0.0.1:8080",
                Some(("x-forwarded-for", "192.0.2.10")),
                None,
                401,
            ),
            (
                "127.0.0.1:1234",
                "127.0.0.1:8080",
                Some(("x-forwarded-host", "192.0.2.20:8080")),
                None,
                401,
            ),
            (
                "192.0.2.10:1234",
                "192.0.2.20:8080",
                Some(("x-forwarded-for", "127.0.0.1")),
                None,
                401,
            ),
            (
                "192.0.2.10:1234",
                "192.0.2.20:8080",
                None,
                Some("Bearer test-direct-loopback-token"),
                200,
            ),
            (
                "192.0.2.10:1234",
                "192.0.2.20:8080",
                None,
                Some("Bearer incorrect-token"),
                401,
            ),
            (
                "127.0.0.1:1234",
                "127.0.0.1:8080",
                Some(("x-forwarded-for", "192.0.2.10")),
                Some("Bearer test-direct-loopback-token"),
                200,
            ),
            (
                "127.0.0.1:1234",
                "127.0.0.1:8080",
                Some(("origin", "http://192.0.2.20:8080")),
                None,
                403,
            ),
        ] {
            let mut request = Request::builder()
                .uri("/healthz")
                .header("host", host)
                .extension(ConnectInfo(peer.parse::<std::net::SocketAddr>().unwrap()));
            if let Some((name, value)) = extra_header {
                request = request.header(name, value);
            }
            if let Some(value) = authorization {
                request = request.header("authorization", value);
            }
            let response = router
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), expected, "{peer} -> {host}");
        }
    }

    #[tokio::test]
    async fn slow_response_bodies_hold_admission_and_event_disconnect_does_not_cancel() {
        let fixture = Fixture::new();
        let runtime = fixture.start();
        let router = vrp_server::http::router(runtime.service.clone(), None);
        let mut held = Vec::new();
        for _ in 0..4 {
            let response = call(&router, "GET", "/healthz", vec![], &[]).await;
            assert_eq!(response.status(), StatusCode::OK);
            held.push(response);
        }
        assert_eq!(
            call(&router, "GET", "/healthz", vec![], &[]).await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        drop(held);
        assert_eq!(
            call(&router, "GET", "/healthz", vec![], &[]).await.status(),
            StatusCode::OK
        );
        let job = submit(&runtime.service, 1, 50);
        let stream = call(
            &router,
            "GET",
            &format!("/v1/jobs/{}/events", job.id),
            vec![],
            &[],
        )
        .await;
        drop(stream);
        assert_eq!(
            wait(&runtime.service, &job.id, terminal).state(),
            pb::JobState::Succeeded
        );
    }
}

#[test]
fn deleting_a_scenario_removes_all_versions_derived_jobs_and_geometry_and_releases_capacity() {
    let fixture = Fixture::new();
    let mut config = fixture.config();
    config.max_records = 7;
    let runtime = start(config);
    let service = &runtime.service;
    let keep = service
        .save_scenario(pb::SaveScenarioRequest {
            scenario_id: "keep".into(),
            request: Some(input(1, 30)),
            ..Default::default()
        })
        .unwrap();
    let kept_bytes = service.health().unwrap().payload_bytes;
    let mut request = input(1, 30);
    request.problem.as_mut().unwrap().tickets[0].id = "one".into();
    let mut save = pb::SaveScenarioRequest {
        scenario_id: "remove".into(),
        request: Some(request),
        ..Default::default()
    };
    service.save_scenario(save.clone()).unwrap();
    save.expected_version = 1;
    let mut ticket = save
        .request
        .as_ref()
        .unwrap()
        .problem
        .as_ref()
        .unwrap()
        .tickets[0]
        .clone();
    ticket.id = "two".into();
    save.request
        .as_mut()
        .unwrap()
        .problem
        .as_mut()
        .unwrap()
        .tickets
        .push(ticket.clone());
    service.save_scenario(save.clone()).unwrap();
    let source = service
        .submit(pb::SubmitJobRequest {
            idempotency_key: "remove-source".into(),
            source: Some(pb::submit_job_request::Source::Scenario(pb::ScenarioRef {
                scenario_id: "remove".into(),
                version: 2,
            })),
            ..Default::default()
        })
        .unwrap();
    assert!(wait(service, &source.id, terminal).has_result);
    let child = service
        .continue_job(pb::ContinueJobRequest {
            job_id: source.id.clone(),
            options: None,
        })
        .unwrap();
    assert!(wait(service, &child.id, terminal).has_result);
    service
        .save_geometry(pb::JobGeometry {
            job_id: child.id.clone(),
            coordinate_system: "WGS84".into(),
            ..Default::default()
        })
        .unwrap();
    let summary = service
        .get_scenario_metadata("remove", 0)
        .unwrap()
        .summary
        .unwrap();
    assert_eq!(
        (
            summary.ticket_count,
            summary.agent_count,
            summary.job_count,
            summary.version_count
        ),
        (2, 1, 2, 2)
    );
    assert_eq!(summary.state(), pb::ScenarioState::HasPlan);
    save.expected_version = 2;
    ticket.id = "three".into();
    save.request
        .as_mut()
        .unwrap()
        .problem
        .as_mut()
        .unwrap()
        .tickets
        .push(ticket);
    service.save_scenario(save).unwrap();
    let summary = service
        .list_scenarios(pb::ListScenariosRequest::default())
        .unwrap()
        .scenarios
        .into_iter()
        .find(|scene| scene.id == "remove")
        .unwrap()
        .summary
        .unwrap();
    assert_eq!((summary.ticket_count, summary.version_count), (3, 3));
    assert_eq!(summary.state(), pb::ScenarioState::StalePlan);
    let extra = pb::SaveScenarioRequest {
        scenario_id: "extra".into(),
        request: Some(input(1, 20)),
        ..Default::default()
    };
    assert_eq!(
        service.save_scenario(extra.clone()).unwrap_err().code,
        "capacity"
    );
    let before = service.health().unwrap().payload_bytes;
    assert_eq!(
        service
            .delete_scenario(pb::DeleteScenarioRequest {
                scenario_id: "remove".into(),
                expected_version: 2,
            })
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(service.health().unwrap().payload_bytes, before);
    assert!(service.get_job(&child.id).is_ok());
    service
        .delete_scenario(pb::DeleteScenarioRequest {
            scenario_id: "remove".into(),
            expected_version: 3,
        })
        .unwrap();
    for version in 0..=3 {
        assert_eq!(
            service.get_scenario("remove", version).unwrap_err().code,
            "not_found"
        );
    }
    for job in [source.id, child.id] {
        assert_eq!(service.get_job(&job).unwrap_err().code, "not_found");
        assert_eq!(service.get_geometry(&job).unwrap_err().code, "not_found");
    }
    assert_eq!(service.health().unwrap().payload_bytes, kept_bytes);
    assert_eq!(service.get_scenario_metadata("keep", 0).unwrap(), keep);
    let conn = rusqlite::Connection::open(fixture.dir.join("tasks.sqlite")).unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM job_geometry", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(conn);
    service.save_scenario(extra).unwrap();
}

#[test]
fn active_scenario_jobs_are_reported_and_prevent_deletion_until_stopped() {
    let fixture = Fixture::new();
    let runtime = fixture.start();
    let service = &runtime.service;
    for id in ["running", "queued"] {
        service
            .save_scenario(pb::SaveScenarioRequest {
                scenario_id: id.into(),
                request: Some(input(2, 5000)),
                ..Default::default()
            })
            .unwrap();
    }
    let submit_scene = |id: &str| {
        service
            .submit(pb::SubmitJobRequest {
                source: Some(pb::submit_job_request::Source::Scenario(pb::ScenarioRef {
                    scenario_id: id.into(),
                    version: 1,
                })),
                ..Default::default()
            })
            .unwrap()
    };
    let running = submit_scene("running");
    wait(service, &running.id, |job| {
        job.state() == pb::JobState::Running
    });
    let queued = submit_scene("queued");
    assert_eq!(
        service
            .get_scenario_metadata("running", 0)
            .unwrap()
            .summary
            .unwrap()
            .state(),
        pb::ScenarioState::Solving
    );
    assert_eq!(
        service
            .get_scenario_metadata("queued", 0)
            .unwrap()
            .summary
            .unwrap()
            .state(),
        pb::ScenarioState::Queued
    );
    for id in ["running", "queued"] {
        assert_eq!(
            service
                .delete_scenario(pb::DeleteScenarioRequest {
                    scenario_id: id.into(),
                    expected_version: 1
                })
                .unwrap_err()
                .code,
            "conflict"
        );
    }
    service.cancel(&queued.id).unwrap();
    service.cancel(&running.id).unwrap();
    wait(service, &running.id, terminal);
    for id in ["running", "queued"] {
        service
            .delete_scenario(pb::DeleteScenarioRequest {
                scenario_id: id.into(),
                expected_version: 1,
            })
            .unwrap();
    }
}

#[test]
fn legacy_summaries_are_backfilled_without_changing_versions_times_or_requests() {
    let fixture = Fixture::new();
    let original;
    {
        let runtime = fixture.start();
        runtime
            .service
            .save_scenario(pb::SaveScenarioRequest {
                scenario_id: "legacy".into(),
                name: "Legacy".into(),
                request: Some(input(1, 20)),
                ..Default::default()
            })
            .unwrap();
        original = runtime.service.get_scenario("legacy", 1).unwrap();
    }
    {
        let conn = rusqlite::Connection::open(fixture.dir.join("tasks.sqlite")).unwrap();
        let mut meta = original.scenario.clone().unwrap();
        meta.summary = None;
        conn.execute(
            "UPDATE scenarios SET meta=?1 WHERE id='legacy'",
            [meta.encode_to_vec()],
        )
        .unwrap();
    }
    let runtime = fixture.start();
    let restored = runtime.service.get_scenario("legacy", 1).unwrap();
    assert_eq!(restored, original);
    let summary = runtime
        .service
        .list_scenarios(pb::ListScenariosRequest::default())
        .unwrap()
        .scenarios[0]
        .summary
        .unwrap();
    assert_eq!(
        (
            summary.ticket_count,
            summary.agent_count,
            summary.version_count
        ),
        (1, 1, 1)
    );
    let conn = rusqlite::Connection::open(fixture.dir.join("tasks.sqlite")).unwrap();
    let bytes: Vec<u8> = conn
        .query_row("SELECT meta FROM scenarios WHERE id='legacy'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(
        pb::Scenario::decode(bytes.as_slice())
            .unwrap()
            .summary
            .is_some()
    );
}

#[test]
fn a_retained_job_reference_rolls_back_the_entire_scenario_deletion() {
    let fixture = Fixture::new();
    let runtime = fixture.start();
    let service = &runtime.service;
    service
        .save_scenario(pb::SaveScenarioRequest {
            scenario_id: "remove".into(),
            request: Some(input(1, 20)),
            ..Default::default()
        })
        .unwrap();
    let source = service
        .submit(pb::SubmitJobRequest {
            source: Some(pb::submit_job_request::Source::Scenario(pb::ScenarioRef {
                scenario_id: "remove".into(),
                version: 1,
            })),
            ..Default::default()
        })
        .unwrap();
    wait(service, &source.id, terminal);
    let child = service
        .continue_job(pb::ContinueJobRequest {
            job_id: source.id.clone(),
            options: None,
        })
        .unwrap();
    wait(service, &child.id, terminal);
    let shape = service
        .save_geometry(pb::JobGeometry {
            job_id: child.id.clone(),
            coordinate_system: "WGS84".into(),
            ..Default::default()
        })
        .unwrap();
    let unrelated = submit(service, 1, 20);
    let mut retained = wait(service, &unrelated.id, terminal);
    // Exercise a retained reference from another source, as may exist in an
    // older database. Deleting the child succeeds before the parent is blocked.
    retained.source_job_id = source.id.clone();
    let conn = rusqlite::Connection::open(fixture.dir.join("tasks.sqlite")).unwrap();
    conn.execute(
        "UPDATE jobs SET meta=?1 WHERE id=?2",
        rusqlite::params![retained.encode_to_vec(), retained.id],
    )
    .unwrap();
    drop(conn);
    let before = service.health().unwrap().payload_bytes;
    assert_eq!(
        service
            .delete_scenario(pb::DeleteScenarioRequest {
                scenario_id: "remove".into(),
                expected_version: 1,
            })
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(service.health().unwrap().payload_bytes, before);
    assert!(service.get_scenario("remove", 1).is_ok());
    assert!(service.get_job(&source.id).is_ok());
    assert!(service.get_job(&child.id).is_ok());
    assert_eq!(service.get_geometry(&child.id).unwrap(), shape);
    assert_eq!(service.get_job(&retained.id).unwrap(), retained);
}

#[test]
fn task_trend_is_persisted_without_a_browser_and_survives_restart_and_deletion() {
    let fixture = Fixture::new();
    let id;
    let saved;
    {
        let runtime = fixture.start();
        let job = submit(&runtime.service, 1, 1_600);
        let done = wait(&runtime.service, &job.id, terminal);
        assert!(done.has_result);
        saved = runtime.service.get_job_trend(&job.id).unwrap();
        assert!(!saved.partial_history);
        assert!(saved.samples.len() >= 2, "{saved:?}");
        assert_eq!(saved.samples.last().unwrap().best_score, done.score);
        assert_eq!(
            saved.samples.last().unwrap().elapsed_ms,
            done.stats.unwrap().elapsed_ms
        );
        assert!(
            saved
                .samples
                .windows(2)
                .all(|pair| pair[0].elapsed_ms < pair[1].elapsed_ms)
        );
        id = job.id;
    }
    let runtime = fixture.start();
    assert_eq!(runtime.service.get_job_trend(&id).unwrap(), saved);
    runtime.service.delete(&id).unwrap();
    assert_eq!(
        runtime.service.get_job_trend(&id).unwrap_err().code,
        "not_found"
    );
    let conn = rusqlite::Connection::open(fixture.dir.join("tasks.sqlite")).unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM job_trends", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(runtime.service.health().unwrap().payload_bytes, 0);
}

#[test]
fn old_tasks_have_explicitly_partial_trends_after_schema_migration() {
    let fixture = Fixture::new();
    let done;
    let original_input;
    let original_result;
    {
        let runtime = fixture.start();
        let job = submit(&runtime.service, 1, 40);
        done = wait(&runtime.service, &job.id, terminal);
        original_input = runtime.service.payload(&job.id, false).unwrap();
        original_result = runtime.service.payload(&job.id, true).unwrap();
    }
    {
        let conn = rusqlite::Connection::open(fixture.dir.join("tasks.sqlite")).unwrap();
        conn.execute_batch("DROP TABLE job_trends; PRAGMA user_version=3;")
            .unwrap();
    }
    let runtime = fixture.start();
    let trend = runtime.service.get_job_trend(&done.id).unwrap();
    assert!(trend.partial_history);
    assert_eq!(trend.samples.len(), 1);
    assert_eq!(trend.samples[0].elapsed_ms, done.stats.unwrap().elapsed_ms);
    assert_eq!(trend.samples[0].best_score, done.score);
    assert_eq!(
        runtime.service.payload(&done.id, false).unwrap(),
        original_input
    );
    assert_eq!(
        runtime.service.payload(&done.id, true).unwrap(),
        original_result
    );
    let conn = rusqlite::Connection::open(fixture.dir.join("tasks.sqlite")).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        6
    );
}

#[test]
fn final_result_takes_precedence_over_trend_storage_under_a_small_quota() {
    let baseline = Fixture::new();
    let result_size;
    {
        let runtime = baseline.start();
        let job = submit(&runtime.service, 1, 1_600);
        assert!(wait(&runtime.service, &job.id, terminal).has_result);
        result_size = runtime.service.payload(&job.id, true).unwrap().len();
    }
    let fixture = Fixture::new();
    let mut config = fixture.config();
    config.max_payload_bytes = (input(1, 1_600).encoded_len() + result_size + 16) as u64;
    let limit = config.max_payload_bytes;
    let runtime = start(config);
    let job = submit(&runtime.service, 1, 1_600);
    let done = wait(&runtime.service, &job.id, terminal);
    assert_eq!(done.state(), pb::JobState::Succeeded, "{done:?}");
    let trend = runtime.service.get_job_trend(&job.id).unwrap();
    assert!(trend.partial_history);
    assert_eq!(trend.samples.last().unwrap().best_score, done.score);
    assert!(runtime.service.health().unwrap().payload_bytes <= limit);
    assert!(runtime.service.payload(&job.id, true).is_ok());
}

#[test]
fn task_retry_rejects_running_solver_and_preserves_frozen_optimization() {
    let fixture = Fixture::new();
    let runtime = fixture.start();
    let service = &runtime.service;
    let pending = submit(service, 1, 2_000);
    assert_eq!(
        service.retry_task(&pending.id).unwrap_err().code,
        "conflict"
    );
    service.cancel(&pending.id).unwrap();
    wait(service, &pending.id, terminal);
    let source = submit(service, 1, 30);
    assert!(wait(service, &source.id, terminal).has_result);
    let optimized = service
        .continue_job(pb::ContinueJobRequest {
            job_id: source.id,
            options: None,
        })
        .unwrap();
    assert!(wait(service, &optimized.id, terminal).has_result);
    let frozen = service.payload(&optimized.id, false).unwrap();
    let retried = service.retry_task(&optimized.id).unwrap();
    let done = wait(service, &retried.id, terminal);
    assert_eq!(done.kind(), pb::JobKind::Optimization);
    assert_eq!(done.source_job_id, optimized.id);
    assert_eq!(service.payload(&done.id, false).unwrap(), frozen);
}
