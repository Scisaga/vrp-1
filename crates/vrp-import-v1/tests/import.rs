use serde_json::json;
use vrp_core::constraint::ConstraintId as C;
use vrp_core::{Weight, oracle};
use vrp_import_v1::{LEGACY_LEVEL_KEY, assignment_from, import};
use vrp_proto::engine::v1 as pb;

fn sample() -> serde_json::Value {
    json!({
        "name": "demo",
        "solve_time": 15,
        "score": {"hardScore": 0, "mediumScore": -5, "softScore": -100},
        "plan": {
            "pois": [
                {"id": "depot", "location": "121.5,29.87", "loc": {"lat": 121.5, "lon": 29.87}},
                {"id": "p1", "entr_loc": {"lat": 121.52, "lon": 29.88}},
                {"id": "p2", "location": "121.53,29.89"}
            ],
            "agents": [
                {
                    "id": "e1-250621", "date": "2025-06-21", "start_loc": "depot",
                    "shift_start_time": "2025-06-21 08:00:00", "shift_off_time": "2025-06-21 20:00:00",
                    "weight": 1000, "vol": 10, "skills": ["Inst"], "qualification_level": 2,
                    "fix_cost_daily": 150.7, "tickets": ["t2", "t1"]
                },
                {"id": "virtual-1-250621", "virtual": true, "date": "2025-06-21", "start_loc": "depot", "tickets": ["t3"]}
            ],
            "tickets": [
                {
                    "id": "t1", "type": "Delv", "loc": "p1", "weight": 1.5, "vol": 0.25, "duration": "PT30M",
                    "min_start_time": "2025-06-21 09:00:00", "max_end_time": "2025-06-21 12:00:00",
                    "ref_tickets": ["t2"], "qualification_level": 1
                },
                {
                    "id": "t2", "type": "Inst", "loc": {"id": "p2", "location": "121.53,29.89"},
                    "min_start_time": "2025-06-22 09:00:00", "max_end_time": "2025-06-22 12:00:00",
                    "ref_tickets": [], "dep_tickets": ["t1"], "items": [{"sku": "x", "value": 2}]
                },
                {
                    "id": "t3", "type": "Delv_BH", "loc": "p2", "duration": "PT10M",
                    "min_start_time": "2025-06-21 10:00:00", "max_end_time": "2025-06-21 18:00:00", "pinned": true
                }
            ],
            "matrix": {"data": {"depot": {
                "p1": {"distance": 2000, "duration": 300},
                "p2": {"distance": 9223372036854775807i64, "duration": 9223372036854775807i64}
            }}},
            "constraint_configuration": {
                "name": "x",
                "agent_capacity": "1hard/0medium/0soft",
                "agent_is_virtual": "0hard/1000medium/0soft",
                "ticket_arrival_time_match_expected": "0hard/0medium/100soft",
                "agent_qualification_level_match_ticket": "1hard/0medium/0soft",
                "made_up": "1hard/0medium/0soft"
            },
            "cost_parameter": {"max_overload_ratio": 1.2, "time_restricted_traffic_charge": 50}
        }
    })
}

#[test]
fn maps_v1_scenarios_to_v2_requests() {
    let imported = import(&sample()).unwrap();
    let p = imported.request.problem.as_ref().unwrap();

    let coords: Vec<(&str, f64, f64)> = p
        .locations
        .iter()
        .map(|l| (l.id.as_str(), l.lng, l.lat))
        .collect();
    assert_eq!(
        coords,
        vec![
            ("depot", 121.5, 29.87),
            ("p1", 121.52, 29.88),
            ("p2", 121.53, 29.89)
        ]
    );

    assert_eq!(p.agents.len(), 1, "virtual agents are dropped");
    let a = &p.agents[0];
    assert_eq!(
        (a.resource_id.as_str(), a.start_time, a.end_time),
        ("e1", 28_800, Some(72_000))
    );
    assert_eq!(a.capacity, vec![1_000_000, 10_000]);
    assert_eq!(a.capacity_tolerance_permille, vec![200, 0]);
    assert_eq!(a.fixed_cost, 150);
    assert_eq!(a.qualification_levels.get(LEGACY_LEVEL_KEY), Some(&2.0));

    let t = &p.tickets;
    assert_eq!(t[0].r#type, pb::TicketType::Delivery as i32);
    assert_eq!(
        (t[0].earliest_start, t[0].latest_start, t[0].latest_end),
        (32_400, 43_200, 43_200)
    );
    assert_eq!(t[0].demand, vec![1_500, 250]);
    assert_eq!(
        t[0].qualification_required.get(LEGACY_LEVEL_KEY),
        Some(&1.0)
    );
    assert_eq!(
        (t[1].duration, t[1].day, t[1].earliest_start),
        (1_200, 1, 118_800)
    );
    assert_eq!(t[2].r#type, pb::TicketType::Pickup as i32);
    assert_eq!(
        (
            t[0].original_agent,
            t[1].original_agent,
            t[2].original_agent
        ),
        (Some(0), Some(0), None)
    );
    assert!(
        !t[2].pinned,
        "a pinned ticket without original agent is unpinned"
    );

    assert_eq!(
        p.precedences,
        vec![pb::TicketPair {
            first: 0,
            second: 1
        }]
    );
    assert!(p.related_pairs.is_empty());
    let routes = &imported.request.initial_solution.as_ref().unwrap().routes;
    assert_eq!(
        routes,
        &vec![pb::Route {
            agent: 0,
            tickets: vec![1, 0]
        }]
    );
    assert_eq!(
        imported.recorded_score.as_deref(),
        Some("0hard/-5medium/-100soft")
    );
    assert_eq!(
        imported.request.options.as_ref().unwrap().time_limit_ms,
        15_000
    );
    assert!(imported.warnings.iter().any(|w| w.contains("made_up")));
    assert!(imported.warnings.iter().any(|w| w.contains("unpinned")));

    let compiled = vrp_model::compile(&imported.request).unwrap();
    let w = &compiled.problem.weights;
    assert_eq!(w.get(C::TicketUnassigned), Weight::medium(1000));
    assert_eq!(
        w.get(C::TicketStartServiceTimeMatchExpected),
        Weight::soft(100)
    );
    assert_eq!(w.get(C::AgentQualification), Weight::hard(1));
    assert!(compiled.problem.related_pairs.is_empty());
    // depot→p1 来自 v1 矩阵，depot→p2 是 Long.MAX_VALUE，其余缺失：都由兜底补齐。
    assert_eq!(compiled.problem.matrices[0].duration(0, 1), 300);
    assert_eq!(compiled.stats.exact, 1);
    assert_eq!(compiled.stats.fallback, 5);

    let evaluation = oracle::evaluate(&compiled.problem, compiled.initial.as_ref().unwrap());
    // t2 计划在第 1 天，却由第 0 天的 agent 在第 0 天服务。
    assert_eq!(
        evaluation
            .total(C::TicketArrivalTimeSameDateWithPlanTime)
            .matches,
        1
    );
    assert_eq!(evaluation.total(C::TicketUnassigned).matches, 1);
}

#[test]
fn dependency_mirrors_and_downstream_only_records_do_not_imply_same_agent() {
    let mut source = sample();
    source["plan"]["constraint_configuration"]["ref_ticket_after_dep_ticket"] =
        json!("0hard/0medium/0soft");
    let imported = import(&source).unwrap();
    let p = imported.request.problem.unwrap();
    assert_eq!(
        p.precedences,
        vec![pb::TicketPair {
            first: 0,
            second: 1
        }]
    );
    assert!(p.related_pairs.is_empty());
    assert_eq!(
        p.weights.unwrap().ref_ticket_after_dep_ticket.unwrap().hard,
        1
    );
    source["plan"]["tickets"][1]["dep_tickets"] = json!([]);
    assert_eq!(
        import(&source)
            .unwrap()
            .request
            .problem
            .unwrap()
            .precedences
            .len(),
        1
    );
    source["plan"]["tickets"][1]["dep_tickets"] = json!(["t1"]);
    source["plan"]["constraint_configuration"]["relation_tickets_same_agent"] =
        json!("0hard/55medium/0soft");
    let p = import(&source).unwrap().request.problem.unwrap();
    assert_eq!(p.precedences.len(), 1);
    assert_eq!(
        p.related_pairs,
        vec![pb::TicketPair {
            first: 0,
            second: 1
        }]
    );
    assert_eq!(
        p.weights
            .unwrap()
            .relation_tickets_same_agent
            .unwrap()
            .medium,
        55
    );
}

#[test]
fn reads_an_assignment_from_another_file() {
    let imported = import(&sample()).unwrap();
    let other = json!({
        "agents": [{"id": "e1-250621", "tickets": [{"id": "t1"}, "t3", "nope"]}],
        "tickets": [{"id": "t2", "agent": "e1-250621", "arrival_time": "2025-06-21 15:00:00"}]
    });
    let (solution, warnings) = assignment_from(&other, &imported).unwrap();
    assert_eq!(
        solution.routes,
        vec![pb::Route {
            agent: 0,
            tickets: vec![0, 2, 1]
        }]
    );
    assert_eq!(warnings.len(), 1);
}

#[test]
fn shift_end_before_shift_start_is_dropped() {
    let mut v = sample();
    // v1 结果文件会把开始时刻改到路线结束前后，可能晚于班次结束。
    v["plan"]["agents"][0]["shift_start_time"] = json!("2025-06-21 21:00:00");
    let imported = import(&v).unwrap();
    let a = &imported.request.problem.as_ref().unwrap().agents[0];
    assert_eq!((a.start_time, a.end_time), (75_600, None));
    assert!(
        imported
            .warnings
            .iter()
            .any(|w| w.contains("shift_off_time"))
    );

    v["plan"]["agents"][0]
        .as_object_mut()
        .unwrap()
        .remove("shift_off_time");
    let imported = import(&v).unwrap();
    assert_eq!(
        imported.request.problem.as_ref().unwrap().agents[0].end_time,
        None
    );
}

#[test]
fn recorded_original_agents_take_precedence() {
    let mut v = sample();
    v["plan"]["tickets"][0]["original_agent"] = json!("e1-250621");
    v["plan"]["tickets"][2]["original_agent"] = json!("e1-250621");
    let imported = import(&v).unwrap();
    let t = &imported.request.problem.as_ref().unwrap().tickets;
    assert_eq!(
        (
            t[0].original_agent,
            t[1].original_agent,
            t[2].original_agent
        ),
        (Some(0), None, Some(0))
    );
    assert!(t[2].pinned);
}
