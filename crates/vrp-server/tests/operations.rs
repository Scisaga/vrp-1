use prost::Message;
use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use vrp_proto::{app::v1 as a, engine::v1 as e};
use vrp_server::{Config, Runtime, Service, terminal};

struct Folder(PathBuf);
impl Folder {
    fn new() -> Self {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus")
            .join(format!(
                "operations-test-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Folder {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn runtime(dir: &Folder) -> Runtime {
    Runtime::open(
        offline(Config {
            data_dir: dir.0.clone(),
            budget: vrp_model::ThreadBudget { total_threads: 2 },
            ..Default::default()
        }),
        PathBuf::from(env!("CARGO_BIN_EXE_vrp-server")),
        vec!["worker-child".into(), "67108864".into()],
    )
    .unwrap()
}
fn offline(mut config: Config) -> Config {
    config.geo.auto_prefetch = false;
    config.geo.catalog_url.clear();
    config
}
fn input(delivery: bool) -> e::SolveRequest {
    let mut request = e::SolveRequest {
        problem: Some(e::Problem {
            locations: vec![
                e::Location {
                    id: "depot".into(),
                    lng: 121.0,
                    lat: 30.0,
                },
                e::Location {
                    id: "customer".into(),
                    lng: 121.01,
                    lat: 30.01,
                },
            ],
            matrices: vec![e::TravelMatrix {
                profile: "car".into(),
                durations: [0u32, 10, 10, 0]
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect(),
                distances: vec![0; 16],
                ..Default::default()
            }],
            capacity_dimensions: vec![e::CapacityDimension {
                name: "units".into(),
                unit: "unit".into(),
                penalty_unit: 1,
            }],
            agents: vec![
                e::Agent {
                    id: "a".into(),
                    resource_id: "physical-a".into(),
                    capacity: vec![10],
                    ..Default::default()
                },
                e::Agent {
                    id: "b".into(),
                    resource_id: "physical-b".into(),
                    capacity: vec![10],
                    ..Default::default()
                },
            ],
            tickets: vec![
                e::Ticket {
                    id: "one".into(),
                    location: 1,
                    r#type: if delivery {
                        e::TicketType::Delivery as i32
                    } else {
                        e::TicketType::Service as i32
                    },
                    demand: vec![2],
                    duration: 60,
                    latest_start: 80000,
                    latest_end: 80000,
                    ..Default::default()
                },
                e::Ticket {
                    id: "two".into(),
                    location: 1,
                    duration: 20,
                    latest_start: 80000,
                    latest_end: 80000,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }),
        options: Some(e::SolveOptions {
            time_limit_ms: 30,
            threads: 2,
            ..Default::default()
        }),
        ..Default::default()
    };
    vrp_model::prepare_matrices(&mut request).unwrap();
    request
}
fn start(service: &Service, delivery: bool, auto: bool) -> a::Operation {
    service
        .save_scenario(a::SaveScenarioRequest {
            display: None,
            origin: None,
            scenario_id: "live".into(),
            name: "Live synthetic case".into(),
            expected_version: 0,
            request: Some(input(delivery)),
        })
        .unwrap();
    service
        .start_operation(a::StartOperationRequest {
            scenario: Some(a::ScenarioRef {
                scenario_id: "live".into(),
                version: 1,
            }),
            policy: Some(a::OperationPolicy {
                auto_replan: auto,
                clock: a::ExecutionClock::Replay as i32,
                loading_location_id: "depot".into(),
                ..Default::default()
            }),
            ..Default::default()
        })
        .unwrap()
}
fn wait(s: &Service, id: &str) -> a::Job {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let j = s.get_job(id).unwrap();
        if terminal(&j) {
            assert!(j.has_result, "{j:?}");
            return j;
        }
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_millis(10));
    }
}

fn static_job(s: &Service, id: &str, version: u64) -> a::Job {
    let job = s
        .submit(a::SubmitJobRequest {
            source: Some(a::submit_job_request::Source::Scenario(a::ScenarioRef {
                scenario_id: id.into(),
                version,
            })),
            ..Default::default()
        })
        .unwrap();
    wait(s, &job.id)
}

#[test]
fn scenario_deletion_retains_active_and_closed_execution_history() {
    let dir = Folder::new();
    let run = runtime(&dir);
    let service = &run.service;
    let op = start(service, false, false);
    assert_eq!(
        service
            .get_scenario_metadata("live", 0)
            .unwrap()
            .summary
            .unwrap()
            .state(),
        a::ScenarioState::Executing
    );
    let request = a::DeleteScenarioRequest {
        scenario_id: "live".into(),
        expected_version: 1,
    };
    assert_eq!(
        service.delete_scenario(request.clone()).unwrap_err().code,
        "conflict"
    );
    let closed = service
        .close_operation(a::CloseOperationRequest {
            scenario_id: "live".into(),
            expected_revision: op.revision,
            actor: "test".into(),
        })
        .unwrap();
    assert!(closed.closed);
    assert_eq!(
        service
            .get_scenario_metadata("live", 0)
            .unwrap()
            .summary
            .unwrap()
            .state(),
        a::ScenarioState::Closed
    );
    assert_eq!(
        service.delete_scenario(request).unwrap_err().code,
        "conflict"
    );
    assert_eq!(service.get_operation("live", 0).unwrap(), closed);
}

#[test]
fn geometry_is_bound_to_job_result_and_preserves_solver_payloads() {
    let dir = Folder::new();
    let run = Runtime::open(
        offline(Config {
            data_dir: dir.0.clone(),
            budget: vrp_model::ThreadBudget { total_threads: 2 },
            max_payload_bytes: 65_536,
            ..Default::default()
        }),
        PathBuf::from(env!("CARGO_BIN_EXE_vrp-server")),
        vec!["worker-child".into(), "67108864".into()],
    )
    .unwrap();
    let s = &run.service;
    // Geometry storage must not depend on search scheduling within a short budget.
    let mut request = input(false);
    request.initial_solution = Some(e::Solution {
        routes: vec![e::Route {
            agent: 0,
            tickets: vec![0, 1],
        }],
    });
    s.save_scenario(a::SaveScenarioRequest {
        scenario_id: "geo".into(),
        request: Some(request),
        origin: Some(a::ScenarioOrigin {
            source_name: "synthetic".into(),
            historical_values_unverified: true,
            ..Default::default()
        }),
        ..Default::default()
    })
    .unwrap();
    let j = static_job(s, "geo", 1);
    let before = s.payload(&j.id, true).unwrap();
    let request = e::SolveRequest::decode(s.payload(&j.id, false).unwrap().as_slice()).unwrap();
    let result = e::SolveResult::decode(before.as_slice()).unwrap();
    let p = request.problem.unwrap();
    let route = &result.solution.unwrap().routes[0];
    let agent = &p.agents[route.agent as usize];
    let from = &p.locations[agent.start_location as usize];
    let to = &p.locations[p.tickets[route.tickets[0] as usize].location as usize];
    let geometry = a::JobGeometry {
        job_id: j.id.clone(),
        coordinate_system: "WGS84".into(),
        provider: "synthetic".into(),
        legs: vec![a::RouteGeometry {
            agent_id: agent.id.clone(),
            leg_index: 0,
            from_location_id: from.id.clone(),
            to_location_id: to.id.clone(),
            points: vec![
                a::GeoPoint {
                    lng: from.lng,
                    lat: from.lat,
                },
                a::GeoPoint {
                    lng: to.lng,
                    lat: to.lat,
                },
            ],
            ..Default::default()
        }],
    };
    let bytes = s.health().unwrap().payload_bytes;
    s.save_geometry(geometry.clone()).unwrap();
    assert!(s.health().unwrap().payload_bytes > bytes);
    assert_eq!(s.get_geometry(&j.id).unwrap(), geometry);
    assert_eq!(s.payload(&j.id, true).unwrap(), before);
    let after = s.health().unwrap().payload_bytes;
    s.save_geometry(geometry.clone()).unwrap();
    assert_eq!(s.health().unwrap().payload_bytes, after);
    let mut large = geometry.clone();
    large.legs[0].points = vec![large.legs[0].points[0]; 5000];
    assert_eq!(s.save_geometry(large).unwrap_err().code, "capacity");
    assert_eq!(s.health().unwrap().payload_bytes, after);
    assert_eq!(s.get_geometry(&j.id).unwrap(), geometry);
    let mut bad = geometry.clone();
    bad.legs[0].from_location_id = "other".into();
    assert_eq!(s.save_geometry(bad).unwrap_err().code, "invalid_request");
    let mut bad = geometry.clone();
    bad.coordinate_system = "GCJ-02".into();
    assert_eq!(s.save_geometry(bad).unwrap_err().code, "invalid_request");
    let mut bad = geometry.clone();
    bad.legs.push(bad.legs[0].clone());
    assert_eq!(s.save_geometry(bad).unwrap_err().code, "invalid_request");
    let another = static_job(s, "geo", 1);
    assert!(s.get_geometry(&another.id).unwrap().legs.is_empty());
    let meta = s.get_scenario_metadata("geo", 0).unwrap();
    assert_eq!(meta.origin.unwrap().source_name, "synthetic");
    assert_eq!(meta.matrix_quality.unwrap().supplied_edges, 2);
    run.shutdown();
    let run = runtime(&dir);
    assert_eq!(run.service.get_geometry(&j.id).unwrap(), geometry);
    let size = run.service.health().unwrap().payload_bytes;
    run.service.delete(&j.id).unwrap();
    assert!(run.service.health().unwrap().payload_bytes < size);
    assert_eq!(
        run.service.get_geometry(&j.id).unwrap_err().code,
        "not_found"
    );
    run.shutdown();
}

#[test]
fn continuation_freezes_problem_and_manual_candidate_is_evaluated_without_overwriting_source() {
    let dir = Folder::new();
    let run = runtime(&dir);
    let s = &run.service;
    s.save_scenario(a::SaveScenarioRequest {
        scenario_id: "live".into(),
        name: "Workbench".into(),
        request: Some(input(false)),
        ..Default::default()
    })
    .unwrap();
    let source = static_job(s, "live", 1);
    let original_input = s.payload(&source.id, false).unwrap();
    let original_result = s.payload(&source.id, true).unwrap();
    let result = e::SolveResult::decode(original_result.as_slice()).unwrap();
    let continued = s
        .continue_job(a::ContinueJobRequest {
            job_id: source.id.clone(),
            options: Some(e::SolveOptions {
                time_limit_ms: 15,
                threads: 1,
                ..Default::default()
            }),
        })
        .unwrap();
    let continued = wait(s, &continued.id);
    let frozen =
        e::SolveRequest::decode(s.payload(&continued.id, false).unwrap().as_slice()).unwrap();
    assert_eq!(frozen.problem, input(false).problem);
    assert_eq!(frozen.initial_solution, result.solution);
    assert_eq!(frozen.options.unwrap().threads, 1);
    assert_eq!(continued.kind(), a::JobKind::Optimization);
    assert_eq!(continued.source_job_id, source.id);
    let changed = e::Solution {
        routes: vec![e::Route {
            agent: 1,
            tickets: vec![1, 0],
        }],
    };
    let manual = s
        .adjust_plan(a::AdjustPlanRequest {
            job_id: source.id.clone(),
            solution: Some(changed.clone()),
            actor: "dispatcher".into(),
            reason: "swap resource and order".into(),
        })
        .unwrap();
    assert_eq!(manual.kind(), a::JobKind::ManualAdjustment);
    assert_eq!(manual.source_job_id, source.id);
    let evaluated =
        e::SolveResult::decode(s.payload(&manual.id, true).unwrap().as_slice()).unwrap();
    assert_eq!(evaluated.solution, Some(changed));
    let evaluated_input =
        e::SolveRequest::decode(s.payload(&manual.id, false).unwrap().as_slice()).unwrap();
    let compiled = vrp_model::compile(&evaluated_input).unwrap();
    let oracle = vrp_core::oracle::evaluate(&compiled.problem, compiled.initial.as_ref().unwrap());
    assert_eq!(evaluated.score.unwrap().soft, oracle.score.soft);
    assert_eq!(
        evaluated.stats.unwrap().termination_reason,
        "manual_evaluation"
    );
    assert_eq!(s.payload(&source.id, false).unwrap(), original_input);
    assert_eq!(s.payload(&source.id, true).unwrap(), original_result);
    assert!(
        s.delete(&source.id).is_err(),
        "keep lineage while children exist"
    );
    s.save_scenario(a::SaveScenarioRequest {
        scenario_id: "live".into(),
        name: "Changed".into(),
        expected_version: 1,
        request: Some(input(false)),
        ..Default::default()
    })
    .unwrap();
    assert!(
        s.continue_job(a::ContinueJobRequest {
            job_id: source.id,
            options: None
        })
        .is_err()
    );
    assert!(
        s.adjust_plan(a::AdjustPlanRequest {
            job_id: manual.id,
            solution: evaluated.solution,
            actor: "d".into(),
            reason: "stale".into()
        })
        .is_err()
    );
}

#[test]
fn adopting_static_delivery_plan_starts_atomically_without_inventing_execution_facts() {
    let dir = Folder::new();
    let run = runtime(&dir);
    let s = &run.service;
    s.save_scenario(a::SaveScenarioRequest {
        scenario_id: "live".into(),
        name: "Delivery".into(),
        request: Some(input(true)),
        ..Default::default()
    })
    .unwrap();
    let job = static_job(s, "live", 1);
    let request = a::StartOperationRequest {
        scenario: Some(a::ScenarioRef {
            scenario_id: "live".into(),
            version: 1,
        }),
        initial_job_id: job.id.clone(),
        actor: "dispatcher".into(),
        policy: Some(a::OperationPolicy {
            clock: a::ExecutionClock::Replay as i32,
            loading_location_id: "depot".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut invalid = request.clone();
    invalid.policy.as_mut().unwrap().loading_location_id = "missing".into();
    assert!(s.start_operation(invalid).is_err());
    assert!(
        s.get_operation("live", 0).is_err(),
        "failed start must not persist partial state"
    );
    let op = s.start_operation(request).unwrap();
    assert!(!op.needs_replan);
    let plan = op.plan.unwrap();
    assert_eq!(plan.version, 1);
    assert_eq!(plan.job_id, job.id);
    let route = plan
        .routes
        .iter()
        .find(|r| r.stops.iter().any(|t| t.ticket_id == "one"))
        .unwrap();
    let load = route
        .stops
        .iter()
        .position(|t| t.action == "load" && t.cargo_ticket_id == "one")
        .unwrap();
    let delivery = route
        .stops
        .iter()
        .position(|t| t.ticket_id == "one")
        .unwrap();
    assert!(load < delivery);
    assert!(route.end_time.is_some() && route.travel_time.is_some());
    assert!(
        op.tickets
            .iter()
            .all(|t| t.phase() == a::TicketPhase::Pending)
    );
    assert!(op.vehicles.iter().all(|v| v.acknowledged_plan == 0));
    assert!(
        op.cargo
            .iter()
            .all(|c| c.agent_id.is_empty() && !c.delivered)
    );
    assert!(s.delete(&job.id).is_err());
    assert!(
        s.continue_job(a::ContinueJobRequest {
            job_id: job.id,
            options: None
        })
        .is_err(),
        "static source is obsolete after execution begins"
    );
    let page = s
        .list_operation_history(a::ListOperationHistoryRequest {
            scenario_id: "live".into(),
            page_size: 1,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page.entries[0].actor, "dispatcher");
    assert_eq!(page.entries[0].plan_version, 1);
}

#[test]
fn manual_live_candidate_stays_manual_across_restart_and_old_execution_is_rejected() {
    let dir = Folder::new();
    let run = runtime(&dir);
    let s = &run.service;
    let op = start(s, false, false);
    let source = solve(s, &op);
    // Switch automatic adoption on before generating a source from the new facts.
    let op = s.get_operation("live", 0).unwrap();
    let op = s
        .configure_operation(a::ConfigureOperationRequest {
            scenario_id: "live".into(),
            expected_revision: op.revision,
            policy: Some(a::OperationPolicy {
                auto_adopt: true,
                clock: a::ExecutionClock::Replay as i32,
                ..Default::default()
            }),
            ..Default::default()
        })
        .unwrap();
    assert!(
        s.continue_job(a::ContinueJobRequest {
            job_id: source.id,
            options: None
        })
        .is_err()
    );
    // Let one automatic candidate become the published plan first.
    let candidate = solve(s, &op);
    let deadline = Instant::now() + Duration::from_secs(5);
    while s.get_operation("live", 0).unwrap().plan.is_none() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    // A new replan may auto-adopt as well. Use a static pinned invalid business
    // score so automatic adoption refuses, leaving an up-to-date evaluable source.
    let mut business = input(false);
    business.problem.as_mut().unwrap().tickets[0].pinned = true;
    business.problem.as_mut().unwrap().tickets[0].original_agent = Some(0);
    business.problem.as_mut().unwrap().tickets[0].skills_required = vec!["missing-skill".into()];
    s.save_scenario(a::SaveScenarioRequest {
        scenario_id: "live".into(),
        expected_version: 1,
        request: Some(business),
        ..Default::default()
    })
    .unwrap();
    let source = solve(s, &s.get_operation("live", 0).unwrap());
    let source_result =
        e::SolveResult::decode(s.payload(&source.id, true).unwrap().as_slice()).unwrap();
    let manual = s
        .adjust_plan(a::AdjustPlanRequest {
            job_id: source.id,
            solution: source_result.solution,
            actor: "d".into(),
            reason: "review manually".into(),
        })
        .unwrap();
    let before = s.get_operation("live", 0).unwrap();
    assert_eq!(before.plan.as_ref().unwrap().job_id, candidate.id);
    assert_eq!(before.latest_job_id, manual.id);
    run.shutdown();
    let run = runtime(&dir);
    let s = &run.service;
    let recovered = s.get_operation("live", 0).unwrap();
    assert_eq!(
        recovered, before,
        "restart must not process a manual draft as automatic work"
    );
    ticket_event(s, "two", a::TicketPhase::EnRoute, "b", 1);
    assert!(
        s.continue_job(a::ContinueJobRequest {
            job_id: manual.id,
            options: None
        })
        .is_err()
    );
    let page = s
        .list_operation_history(a::ListOperationHistoryRequest {
            scenario_id: "live".into(),
            page_size: 1,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].kind, "event");
    assert_eq!(page.entries[0].ticket_ids, vec!["two"]);
    assert_eq!(page.entries[0].agent_ids, vec!["b"]);
    assert!(page.next_before_revision > 0);
    let older = s
        .list_operation_history(a::ListOperationHistoryRequest {
            scenario_id: "live".into(),
            page_size: 2,
            before_revision: page.next_before_revision,
        })
        .unwrap();
    assert!(
        older
            .entries
            .iter()
            .all(|e| e.revision < page.entries[0].revision)
    );
}

#[test]
fn scenario_display_metadata_is_versioned_preserved_and_can_be_cleared() {
    let dir = Folder::new();
    let run = runtime(&dir);
    let s = &run.service;
    let display = a::ScenarioDisplay {
        tickets: [(
            "one".into(),
            a::ObjectLabel {
                name: "安装服务".into(),
                address: "合成地址".into(),
                note: "展示字段".into(),
            },
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let mut save = a::SaveScenarioRequest {
        scenario_id: "labels".into(),
        request: Some(input(false)),
        display: Some(display.clone()),
        ..Default::default()
    };
    s.save_scenario(save.clone()).unwrap();
    save.expected_version = 1;
    save.display = None;
    assert_eq!(
        s.save_scenario(save.clone()).unwrap().display,
        Some(display.clone())
    );
    save.expected_version = 2;
    save.display = Some(a::ScenarioDisplay::default());
    assert_eq!(
        s.save_scenario(save).unwrap().display,
        Some(a::ScenarioDisplay::default())
    );
    assert_eq!(
        s.get_scenario("labels", 1)
            .unwrap()
            .scenario
            .unwrap()
            .display,
        Some(display)
    );
}
fn solve(s: &Service, op: &a::Operation) -> a::Job {
    let j = s
        .replan(a::ReplanRequest {
            scenario_id: "live".into(),
            expected_revision: op.revision,
            options: None,
        })
        .unwrap();
    let frozen = e::SolveRequest::decode(s.payload(&j.id, false).unwrap().as_slice()).unwrap();
    vrp_model::validate_ready_matrices(&frozen)
        .expect("execution replan must freeze complete matrices");
    wait(s, &j.id)
}

#[test]
fn task_retry_reprojects_execution_and_reevaluates_manual_solution() {
    let dir = Folder::new();
    let run = runtime(&dir);
    let s = &run.service;
    s.save_scenario(a::SaveScenarioRequest {
        scenario_id: "draft".into(),
        request: Some(input(false)),
        ..Default::default()
    })
    .unwrap();
    let source = static_job(s, "draft", 1);
    let solution = e::SolveResult::decode(s.payload(&source.id, true).unwrap().as_slice())
        .unwrap()
        .solution;
    let manual = s
        .adjust_plan(a::AdjustPlanRequest {
            job_id: source.id,
            solution: solution.clone(),
            actor: "dispatcher".into(),
            reason: "retry test".into(),
        })
        .unwrap();
    let retried = s.retry_task(&manual.id).unwrap();
    let job = s.get_job(&retried.id).unwrap();
    let result = e::SolveResult::decode(s.payload(&job.id, true).unwrap().as_slice()).unwrap();
    assert_eq!(job.kind(), a::JobKind::ManualAdjustment);
    assert_eq!(result.solution, solution);
    assert_eq!(
        result.stats.unwrap().termination_reason,
        "manual_evaluation"
    );
    let op = start(s, false, false);
    let old = solve(s, &op);
    let op = s.get_operation("live", 0).unwrap();
    let current = s
        .apply_execution_event(a::ExecutionEvent {
            scenario_id: "live".into(),
            expected_revision: op.revision,
            event_id: "advance-retry".into(),
            at: 300,
            ..Default::default()
        })
        .unwrap();
    let retried = s.retry_task(&old.id).unwrap();
    let retried = wait(s, &retried.id);
    assert_eq!(retried.kind(), a::JobKind::RealtimeReplan);
    assert_eq!(retried.source_job_id, old.id);
    let basis = retried.execution_basis.unwrap();
    assert_eq!(basis.execution_version, current.execution_version);
    assert_eq!(basis.at, 300);
}
fn adopt(s: &Service, j: &a::Job) -> a::Operation {
    let op = s.get_operation("live", 0).unwrap();
    s.adopt_plan(a::AdoptPlanRequest {
        scenario_id: "live".into(),
        expected_revision: op.revision,
        expected_plan_version: op.plan.as_ref().map_or(0, |p| p.version),
        job_id: j.id.clone(),
        actor: "dispatcher".into(),
        ..Default::default()
    })
    .unwrap()
}
fn ticket_event(
    s: &Service,
    id: &str,
    phase: a::TicketPhase,
    agent: &str,
    at: i64,
) -> a::Operation {
    let op = s.get_operation("live", 0).unwrap();
    s.apply_execution_event(a::ExecutionEvent {
        scenario_id: "live".into(),
        expected_revision: op.revision,
        event_id: format!("{id}-{phase:?}-{at}"),
        at,
        actor: "test".into(),
        tickets: vec![a::TicketUpdate {
            ticket_id: id.into(),
            phase: phase as i32,
            agent_id: agent.into(),
            ..Default::default()
        }],
        ..Default::default()
    })
    .unwrap()
}

#[test]
fn service_requires_all_actual_predecessors_and_dependency_facts_are_immutable() {
    let dir = Folder::new();
    let run = runtime(&dir);
    let s = &run.service;
    start(s, false, false);
    let mut request = input(false);
    let p = request.problem.as_mut().unwrap();
    let mut third = p.tickets[0].clone();
    third.id = "three".into();
    p.tickets.push(third);
    p.precedences = vec![
        e::TicketPair {
            first: 0,
            second: 1,
        },
        e::TicketPair {
            first: 2,
            second: 1,
        },
    ];
    s.save_scenario(a::SaveScenarioRequest {
        scenario_id: "live".into(),
        expected_version: 1,
        request: Some(request.clone()),
        ..Default::default()
    })
    .unwrap();
    ticket_event(s, "two", a::TicketPhase::EnRoute, "b", 1);
    ticket_event(s, "two", a::TicketPhase::Arrived, "b", 2);
    let attempt = |at| {
        let op = s.get_operation("live", 0).unwrap();
        s.apply_execution_event(a::ExecutionEvent {
            scenario_id: "live".into(),
            expected_revision: op.revision,
            event_id: format!("start-child-{at}"),
            at,
            tickets: vec![a::TicketUpdate {
                ticket_id: "two".into(),
                phase: a::TicketPhase::Serving as i32,
                agent_id: "b".into(),
                ..Default::default()
            }],
            ..Default::default()
        })
    };
    let before = s.get_operation("live", 0).unwrap();
    assert!(attempt(3).unwrap_err().message.contains("predecessor"));
    assert_eq!(
        s.get_operation("live", 0).unwrap().revision,
        before.revision
    );
    ticket_event(s, "one", a::TicketPhase::EnRoute, "a", 3);
    ticket_event(s, "one", a::TicketPhase::Arrived, "a", 4);
    ticket_event(s, "one", a::TicketPhase::Serving, "a", 5);
    ticket_event(s, "one", a::TicketPhase::Completed, "a", 6);
    assert!(attempt(7).unwrap_err().message.contains("three"));
    ticket_event(s, "three", a::TicketPhase::EnRoute, "a", 7);
    ticket_event(s, "three", a::TicketPhase::Arrived, "a", 8);
    ticket_event(s, "three", a::TicketPhase::Serving, "a", 9);
    let op = s.get_operation("live", 0).unwrap();
    // Child is first in the batch: both facts become effective atomically.
    s.apply_execution_event(a::ExecutionEvent {
        scenario_id: "live".into(),
        expected_revision: op.revision,
        event_id: "finish-and-start".into(),
        at: 10,
        tickets: vec![
            a::TicketUpdate {
                ticket_id: "two".into(),
                phase: a::TicketPhase::Serving as i32,
                agent_id: "b".into(),
                ..Default::default()
            },
            a::TicketUpdate {
                ticket_id: "three".into(),
                phase: a::TicketPhase::Completed as i32,
                agent_id: "a".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    })
    .unwrap();
    request.problem.as_mut().unwrap().precedences.clear();
    assert!(
        s.save_scenario(a::SaveScenarioRequest {
            scenario_id: "live".into(),
            expected_version: 2,
            request: Some(request),
            ..Default::default()
        })
        .is_err()
    );
}

#[test]
fn serving_and_completed_work_are_facts_and_old_candidates_cannot_be_adopted() {
    let dir = Folder::new();
    let run = runtime(&dir);
    let s = &run.service;
    let op = start(s, false, false);
    let j = solve(s, &op);
    let adopted = adopt(s, &j);
    let agent = adopted
        .plan
        .as_ref()
        .unwrap()
        .routes
        .iter()
        .find(|r| r.stops.iter().any(|t| t.ticket_id == "one"))
        .unwrap()
        .agent_id
        .clone();
    ticket_event(s, "one", a::TicketPhase::EnRoute, &agent, 1);
    ticket_event(s, "one", a::TicketPhase::Arrived, &agent, 11);
    let op = ticket_event(s, "one", a::TicketPhase::Serving, &agent, 12);
    let j = solve(s, &op);
    let projected = e::SolveRequest::decode(s.payload(&j.id, false).unwrap().as_slice()).unwrap();
    let p = projected.problem.unwrap();
    assert!(!p.tickets.iter().any(|t| t.id == "one"));
    let ai = p.agents.iter().position(|a| a.id == agent).unwrap();
    assert!(p.agents[ai].start_time >= 72);
    assert_eq!(p.execution.unwrap().routes[ai].completed_tickets, 1);
    let finished = ticket_event(s, "one", a::TicketPhase::Completed, &agent, 80);
    assert!(
        s.adopt_plan(a::AdoptPlanRequest {
            scenario_id: "live".into(),
            expected_revision: finished.revision,
            expected_plan_version: 1,
            job_id: j.id,
            actor: "test".into(),
            ..Default::default()
        })
        .is_err()
    );
    let j = solve(s, &finished);
    let p = e::SolveRequest::decode(s.payload(&j.id, false).unwrap().as_slice())
        .unwrap()
        .problem
        .unwrap();
    assert!(!p.tickets.iter().any(|t| t.id == "one"));
    assert_eq!(
        s.get_operation("live", 0)
            .unwrap()
            .vehicles
            .iter()
            .find(|a| a.agent_id == agent)
            .unwrap()
            .service_seconds,
        68
    );
    let adopted = adopt(s, &j);
    assert_eq!(adopted.plan.unwrap().version, 2);
}

#[test]
fn cargo_is_loaded_explicitly_cancelled_delivery_stays_on_vehicle_and_transfer_is_atomic() {
    let dir = Folder::new();
    let run = runtime(&dir);
    let s = &run.service;
    let op = start(s, true, false);
    let j = solve(s, &op);
    let adopted = adopt(s, &j);
    assert!(
        adopted
            .plan
            .unwrap()
            .routes
            .iter()
            .any(|r| r.stops.iter().any(|s| s.action == "load"))
    );
    let op = s.get_operation("live", 0).unwrap();
    let load = a::ExecutionEvent {
        scenario_id: "live".into(),
        expected_revision: op.revision,
        event_id: "load-one".into(),
        at: 1,
        cargo: vec![a::CargoUpdate {
            ticket_id: "one".into(),
            to_agent_id: "a".into(),
            location_id: "depot".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let loaded = s.apply_execution_event(load.clone()).unwrap();
    assert_eq!(s.apply_execution_event(load.clone()).unwrap(), loaded);
    let mut conflict = load;
    conflict.at = 2;
    assert!(s.apply_execution_event(conflict).is_err());
    let cancelled = ticket_event(s, "one", a::TicketPhase::Cancelled, "", 2);
    assert_eq!(cancelled.cargo[0].agent_id, "a");
    let j = solve(s, &cancelled);
    let p = e::SolveRequest::decode(s.payload(&j.id, false).unwrap().as_slice())
        .unwrap()
        .problem
        .unwrap();
    assert_eq!(
        p.execution.unwrap().routes[0]
            .initial_load
            .as_ref()
            .unwrap()
            .values,
        vec![2]
    );
    assert!(p.tickets.iter().any(|t| t.id == "@vrp/unload/one"));
    let op = s.get_operation("live", 0).unwrap();
    let moved = s
        .apply_execution_event(a::ExecutionEvent {
            scenario_id: "live".into(),
            expected_revision: op.revision,
            event_id: "transfer".into(),
            at: 3,
            cargo: vec![a::CargoUpdate {
                ticket_id: "one".into(),
                from_agent_id: "a".into(),
                to_agent_id: "b".into(),
                location_id: "depot".into(),
            }],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(moved.cargo[0].agent_id, "b");
    let bad = s.apply_execution_event(a::ExecutionEvent {
        scenario_id: "live".into(),
        expected_revision: moved.revision,
        event_id: "bad-transfer".into(),
        at: 4,
        cargo: vec![a::CargoUpdate {
            ticket_id: "one".into(),
            from_agent_id: "a".into(),
            to_agent_id: "b".into(),
            location_id: "depot".into(),
        }],
        ..Default::default()
    });
    assert!(bad.is_err());
    assert_eq!(s.get_operation("live", 0).unwrap().revision, moved.revision);
}

#[test]
fn operations_recover_and_physical_resources_are_exclusive() {
    let dir = Folder::new();
    let run = runtime(&dir);
    let op = start(&run.service, false, false);
    run.service
        .save_scenario(a::SaveScenarioRequest {
            display: None,
            scenario_id: "other".into(),
            name: "other".into(),
            request: Some(input(false)),
            ..Default::default()
        })
        .unwrap();
    assert!(
        run.service
            .start_operation(a::StartOperationRequest {
                scenario: Some(a::ScenarioRef {
                    scenario_id: "other".into(),
                    version: 1
                }),
                ..Default::default()
            })
            .is_err()
    );
    run.shutdown();
    let run = runtime(&dir);
    assert_eq!(run.service.get_operation("live", 0).unwrap(), op);
    assert_eq!(run.service.get_operation("live", 1).unwrap(), op);
}

#[test]
fn urgent_insert_reroutes_a_driving_vehicle_and_adoption_and_ack_are_versioned() {
    let dir = Folder::new();
    let run = runtime(&dir);
    let s = &run.service;
    let op = start(s, false, false);
    let job = solve(s, &op);
    let initial = adopt(s, &job);
    ticket_event(s, "one", a::TicketPhase::EnRoute, "a", 1);
    let mut request = input(false);
    request.problem.as_mut().unwrap().tickets.push(e::Ticket {
        id: "urgent".into(),
        location: 1,
        duration: 10,
        latest_start: 200,
        latest_end: 300,
        ..Default::default()
    });
    s.save_scenario(a::SaveScenarioRequest {
        display: None,
        origin: None,
        scenario_id: "live".into(),
        name: "live".into(),
        expected_version: 1,
        request: Some(request),
    })
    .unwrap();
    let op = s.get_operation("live", 0).unwrap();
    let j = solve(s, &op);
    let before = s.get_operation("live", 0).unwrap();
    let command = a::AdoptPlanRequest {
        scenario_id: "live".into(),
        expected_revision: before.revision,
        expected_plan_version: 1,
        job_id: j.id,
        actor: "dispatch".into(),
        ..Default::default()
    };
    let (one, two) = thread::scope(|scope| {
        let first = scope.spawn(|| s.adopt_plan(command.clone()));
        let second = scope.spawn(|| s.adopt_plan(command.clone()));
        (first.join().unwrap(), second.join().unwrap())
    });
    assert_ne!(one.is_ok(), two.is_ok());
    let rerouted = ticket_event(s, "urgent", a::TicketPhase::EnRoute, "a", 2);
    assert_eq!(
        rerouted
            .tickets
            .iter()
            .find(|t| t.ticket_id == "one")
            .unwrap()
            .phase(),
        a::TicketPhase::Pending
    );
    assert_eq!(
        rerouted
            .tickets
            .iter()
            .find(|t| t.ticket_id == "urgent")
            .unwrap()
            .agent_id,
        "a"
    );
    assert!(
        s.acknowledge_plan(a::AcknowledgePlanRequest {
            scenario_id: "live".into(),
            expected_revision: rerouted.revision,
            plan_version: initial.plan.unwrap().version,
            agent_id: "a".into()
        })
        .is_err()
    );
    let ack = a::AcknowledgePlanRequest {
        scenario_id: "live".into(),
        expected_revision: rerouted.revision,
        plan_version: 2,
        agent_id: "a".into(),
    };
    let acked = s.acknowledge_plan(ack.clone()).unwrap();
    assert_eq!(s.acknowledge_plan(ack.clone()).unwrap(), acked);
    let mut duplicate = ack;
    duplicate.expected_revision = acked.revision;
    assert_eq!(s.acknowledge_plan(duplicate).unwrap(), acked);
    assert!(
        s.apply_execution_event(a::ExecutionEvent {
            scenario_id: "live".into(),
            expected_revision: acked.revision,
            event_id: "late".into(),
            at: 1,
            vehicles: vec![a::VehicleUpdate {
                agent_id: "a".into(),
                phase: Some(a::VehiclePhase::Available as i32),
                ..Default::default()
            }],
            ..Default::default()
        })
        .is_err()
    );
}

#[test]
fn delivery_prefix_includes_loading_and_impossible_time_commitment_cannot_be_overridden() {
    let dir = Folder::new();
    let run = runtime(&dir);
    let s = &run.service;
    let op = start(s, true, false);
    let op = s
        .configure_operation(a::ConfigureOperationRequest {
            scenario_id: "live".into(),
            expected_revision: op.revision,
            policy: op.policy,
            route_commitments: vec![a::RouteCommitment {
                agent_id: "a".into(),
                prefix: vec!["one".into(), "two".into()],
            }],
            ..Default::default()
        })
        .unwrap();
    let j = solve(s, &op);
    let plan = adopt(s, &j).plan.unwrap();
    let route = plan.routes.iter().find(|r| r.agent_id == "a").unwrap();
    assert_eq!(
        route
            .stops
            .iter()
            .map(|s| s.ticket_id.as_str())
            .collect::<Vec<_>>(),
        vec!["@vrp/load/one", "one", "two"]
    );
    let op = s.get_operation("live", 0).unwrap();
    let op = s
        .configure_operation(a::ConfigureOperationRequest {
            scenario_id: "live".into(),
            expected_revision: op.revision,
            policy: op.policy,
            route_commitments: op.route_commitments,
            time_commitments: vec![a::TimeCommitment {
                ticket_id: "one".into(),
                agent_id: "a".into(),
                latest: Some(1),
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
    let j = solve(s, &op);
    let op = s.get_operation("live", 0).unwrap();
    let error = s
        .adopt_plan(a::AdoptPlanRequest {
            scenario_id: "live".into(),
            expected_revision: op.revision,
            expected_plan_version: 1,
            job_id: j.id,
            actor: "test".into(),
            accept_business_violations: true,
            reason: "cannot override physics".into(),
        })
        .unwrap_err();
    assert!(error.message.contains("physical"));
}

#[test]
fn partial_service_fault_and_cargo_handoff_preserve_actual_work() {
    let dir = Folder::new();
    let run = runtime(&dir);
    let s = &run.service;
    let op = start(s, true, false);
    s.apply_execution_event(a::ExecutionEvent {
        scenario_id: "live".into(),
        expected_revision: op.revision,
        event_id: "load".into(),
        at: 1,
        cargo: vec![a::CargoUpdate {
            ticket_id: "one".into(),
            to_agent_id: "a".into(),
            location_id: "depot".into(),
            ..Default::default()
        }],
        ..Default::default()
    })
    .unwrap();
    ticket_event(s, "one", a::TicketPhase::EnRoute, "a", 2);
    ticket_event(s, "one", a::TicketPhase::Arrived, "a", 12);
    ticket_event(s, "one", a::TicketPhase::Serving, "a", 13);
    let op = s.get_operation("live", 0).unwrap();
    let op = s
        .apply_execution_event(a::ExecutionEvent {
            scenario_id: "live".into(),
            expected_revision: op.revision,
            event_id: "fault".into(),
            at: 33,
            vehicles: vec![a::VehicleUpdate {
                agent_id: "a".into(),
                phase: Some(a::VehiclePhase::Unavailable as i32),
                ..Default::default()
            }],
            tickets: vec![a::TicketUpdate {
                ticket_id: "one".into(),
                phase: a::TicketPhase::Exception as i32,
                note: "interrupted after 20 seconds".into(),
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(op.vehicles[0].service_seconds, 20);
    assert_eq!(op.tickets[0].remaining_service_seconds, Some(40));
    let job = solve(s, &op);
    let projected = e::SolveRequest::decode(s.payload(&job.id, false).unwrap().as_slice()).unwrap();
    assert!(projected.problem.unwrap().execution.unwrap().routes[0].unavailable);
    let op = s.get_operation("live", 0).unwrap();
    let transferred = s
        .apply_execution_event(a::ExecutionEvent {
            scenario_id: "live".into(),
            expected_revision: op.revision,
            event_id: "handoff".into(),
            at: 34,
            vehicles: vec![a::VehicleUpdate {
                agent_id: "b".into(),
                position: Some(input(true).problem.unwrap().locations[1].clone()),
                ..Default::default()
            }],
            cargo: vec![a::CargoUpdate {
                ticket_id: "one".into(),
                from_agent_id: "a".into(),
                to_agent_id: "b".into(),
                location_id: "customer".into(),
            }],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(transferred.cargo[0].agent_id, "b");
    ticket_event(s, "one", a::TicketPhase::Pending, "", 35);
    ticket_event(s, "one", a::TicketPhase::EnRoute, "b", 36);
    ticket_event(s, "one", a::TicketPhase::Arrived, "b", 37);
    ticket_event(s, "one", a::TicketPhase::Serving, "b", 38);
    let done = ticket_event(s, "one", a::TicketPhase::Completed, "b", 50);
    assert!(done.cargo[0].delivered);
    assert_eq!(done.vehicles[0].completed_tickets, 0);
    assert_eq!(done.vehicles[1].completed_tickets, 1);
    assert_eq!(done.vehicles[1].service_seconds, 12);
}

#[test]
fn auto_replanning_of_other_scenes_is_not_blocked_by_telemetry_on_a_running_scene() {
    let dir = Folder::new();
    let run = runtime(&dir);
    let s = &run.service;
    let op = start(s, false, false);
    let mut policy = op.policy.clone().unwrap();
    policy.auto_replan = true;
    let op = s
        .configure_operation(a::ConfigureOperationRequest {
            scenario_id: "live".into(),
            expected_revision: op.revision,
            policy: Some(policy),
            ..Default::default()
        })
        .unwrap();
    let slow = s
        .replan(a::ReplanRequest {
            scenario_id: "live".into(),
            expected_revision: op.revision,
            options: Some(e::SolveOptions {
                threads: 1,
                time_limit_ms: 30000,
                ..Default::default()
            }),
        })
        .unwrap();
    let op = s.get_operation("live", 0).unwrap();
    s.apply_execution_event(a::ExecutionEvent {
        scenario_id: "live".into(),
        expected_revision: op.revision,
        event_id: "gps".into(),
        at: 1,
        vehicles: vec![a::VehicleUpdate {
            agent_id: "a".into(),
            position: Some(e::Location {
                id: "gps".into(),
                lat: 30.0,
                lng: 121.001,
            }),
            ..Default::default()
        }],
        ..Default::default()
    })
    .unwrap();
    thread::sleep(Duration::from_millis(270));
    let mut other = input(false);
    for agent in &mut other.problem.as_mut().unwrap().agents {
        agent.resource_id.push_str("-other");
    }
    other.options.as_mut().unwrap().threads = 1;
    s.save_scenario(a::SaveScenarioRequest {
        display: None,
        scenario_id: "other".into(),
        name: "other".into(),
        request: Some(other),
        ..Default::default()
    })
    .unwrap();
    s.start_operation(a::StartOperationRequest {
        scenario: Some(a::ScenarioRef {
            scenario_id: "other".into(),
            version: 1,
        }),
        policy: Some(a::OperationPolicy {
            auto_replan: true,
            auto_adopt: true,
            clock: a::ExecutionClock::Replay as i32,
            ..Default::default()
        }),
        ..Default::default()
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let other = s.get_operation("other", 0).unwrap();
        if other.plan.is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "{other:?}");
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!terminal(&s.get_job(&slow.id).unwrap()));
    assert!(s.health().unwrap().reserved_threads <= 2);
    s.cancel(&slow.id).unwrap();
}

#[test]
fn queue_full_and_restart_retain_pending_replanning_and_execution_facts() {
    let dir = Folder::new();
    let config = offline(Config {
        data_dir: dir.0.clone(),
        budget: vrp_model::ThreadBudget { total_threads: 2 },
        max_queued: 1,
        ..Default::default()
    });
    let open = || {
        Runtime::open(
            config.clone(),
            PathBuf::from(env!("CARGO_BIN_EXE_vrp-server")),
            vec!["worker-child".into(), "67108864".into()],
        )
        .unwrap()
    };
    let run = open();
    let s = &run.service;
    let mut request = input(false);
    request.options.as_mut().unwrap().time_limit_ms = 30_000;
    let submit = || {
        s.submit(a::SubmitJobRequest {
            source: Some(a::submit_job_request::Source::Request(Box::new(
                request.clone(),
            ))),
            ..Default::default()
        })
        .unwrap()
    };
    let first = submit();
    let end = Instant::now() + Duration::from_secs(10);
    while s.get_job(&first.id).unwrap().state() == a::JobState::Queued {
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_millis(10));
    }
    let queued = submit();
    start(s, false, true);
    let reported = ticket_event(s, "one", a::TicketPhase::EnRoute, "a", 1);
    thread::sleep(Duration::from_millis(350));
    let pending = s.get_operation("live", 0).unwrap();
    assert!(pending.needs_replan);
    assert!(pending.latest_job_id.is_empty());
    run.shutdown();
    let run = open();
    let s = &run.service;
    s.cancel(&queued.id).unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    let op = loop {
        let op = s.get_operation("live", 0).unwrap();
        if !op.latest_job_id.is_empty() {
            break op;
        }
        assert!(Instant::now() < end, "{op:?}");
        thread::sleep(Duration::from_millis(10));
    };
    wait(s, &op.latest_job_id);
    assert_eq!(op.tickets, reported.tickets);
    assert!(op.plan.is_none());
}

#[test]
fn completed_dependencies_survive_location_reordering_and_keep_original_weights() {
    let dir = Folder::new();
    let run = runtime(&dir);
    let s = &run.service;
    start(s, false, false);
    let mut request = input(false);
    let p = request.problem.as_mut().unwrap();
    p.precedences = vec![e::TicketPair {
        first: 0,
        second: 1,
    }];
    p.related_pairs = p.precedences.clone();
    p.weights = Some(e::ConstraintWeights {
        ref_ticket_after_dep_ticket: Some(e::Weight {
            hard: 1,
            ..Default::default()
        }),
        ref_ticket_same_agent_with_dep_ticket: Some(e::Weight {
            soft: 7,
            ..Default::default()
        }),
        ..Default::default()
    });
    s.save_scenario(a::SaveScenarioRequest {
        display: None,
        scenario_id: "live".into(),
        expected_version: 1,
        request: Some(request.clone()),
        ..Default::default()
    })
    .unwrap();
    ticket_event(s, "one", a::TicketPhase::EnRoute, "a", 1);
    ticket_event(s, "one", a::TicketPhase::Arrived, "a", 2);
    ticket_event(s, "one", a::TicketPhase::Serving, "a", 3);
    ticket_event(s, "one", a::TicketPhase::Completed, "a", 20);
    let p = request.problem.as_mut().unwrap();
    p.locations.swap(0, 1);
    for agent in &mut p.agents {
        agent.start_location = 1;
        agent.end_location = 1;
    }
    for ticket in &mut p.tickets {
        ticket.location = 0;
    }
    s.save_scenario(a::SaveScenarioRequest {
        display: None,
        scenario_id: "live".into(),
        expected_version: 2,
        request: Some(request),
        ..Default::default()
    })
    .unwrap();
    let op = s.get_operation("live", 0).unwrap();
    let job = solve(s, &op);
    let projected = e::SolveRequest::decode(s.payload(&job.id, false).unwrap().as_slice()).unwrap();
    let compiled = vrp_model::compile(&projected).unwrap();
    let solution = vrp_core::Solution {
        routes: vec![vec![], vec![0]],
    };
    let evaluation = vrp_core::oracle::evaluate(&compiled.problem, &solution);
    assert_eq!(
        evaluation
            .total(vrp_core::ConstraintId::RefTicketSameAgentWithDepTicket)
            .score
            .soft,
        -7
    );
    assert_eq!(
        evaluation
            .total(vrp_core::ConstraintId::RelationTicketsSameAgent)
            .score
            .medium,
        -50
    );
    assert_eq!(
        evaluation
            .total(vrp_core::ConstraintId::ExecutionConsistency)
            .amount,
        0
    );
    assert_eq!(
        vrp_core::State::new(&compiled.problem, &solution)
            .unwrap()
            .score(),
        evaluation.score
    );
}

#[test]
fn explicit_zero_remaining_work_survives_exception_and_resume() {
    let dir = Folder::new();
    let run = runtime(&dir);
    let s = &run.service;
    start(s, false, false);
    let op = s.get_operation("live", 0).unwrap();
    s.apply_execution_event(a::ExecutionEvent {
        scenario_id: "live".into(),
        expected_revision: op.revision,
        event_id: "zero".into(),
        at: 1,
        tickets: vec![a::TicketUpdate {
            ticket_id: "one".into(),
            phase: a::TicketPhase::Exception as i32,
            remaining_service_seconds: Some(0),
            note: "waiting for acceptance, no service work remaining".into(),
            ..Default::default()
        }],
        ..Default::default()
    })
    .unwrap();
    let op = ticket_event(s, "one", a::TicketPhase::Pending, "", 2);
    let job = solve(s, &op);
    let p = e::SolveRequest::decode(s.payload(&job.id, false).unwrap().as_slice())
        .unwrap()
        .problem
        .unwrap();
    assert_eq!(
        p.tickets.iter().find(|t| t.id == "one").unwrap().duration,
        0
    );
    assert_eq!(op.tickets[0].phase(), a::TicketPhase::Pending);
}
