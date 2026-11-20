use vrp_core::constraint::ConstraintId as C;
use vrp_core::problem::{UNKNOWN, UNREACHABLE};
use vrp_core::{Weight, oracle};
use vrp_model::{build_result, compile};
use vrp_proto::engine::v1 as pb;

fn le(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// 3 个地点：0→2 未知（走兜底估算），2→1 不可达。
fn request() -> pb::SolveRequest {
    let locations = vec![
        pb::Location {
            id: "depot".into(),
            lng: 121.50,
            lat: 29.87,
        },
        pb::Location {
            id: "a".into(),
            lng: 121.52,
            lat: 29.87,
        },
        pb::Location {
            id: "b".into(),
            lng: 121.50,
            lat: 29.89,
        },
    ];
    #[rustfmt::skip]
    let durations = [
        0, 300, UNKNOWN,
        300, 0, 400,
        600, UNREACHABLE, 0,
    ];
    #[rustfmt::skip]
    let distances = [
        0, 2_000, UNKNOWN,
        2_000, 0, 3_000,
        2_500, UNREACHABLE, 0,
    ];
    let weights = pb::ConstraintWeights {
        minimize_travel_distance: Some(pb::Weight {
            hard: 0,
            medium: 0,
            soft: 2,
        }),
        agent_skills_accord_with_ticket_skills: Some(pb::Weight::default()), // 显式关闭
        ..Default::default()
    };
    pb::SolveRequest {
        problem: Some(pb::Problem {
            id: "job-1".into(),
            horizon: Some(pb::Horizon {
                start_local: "2025-06-21T00:00:00".into(),
                ..Default::default()
            }),
            locations,
            matrices: vec![pb::TravelMatrix {
                profile: "car".into(),
                durations: le(&durations),
                distances: le(&distances),
                sources: vec![],
            }],
            capacity_dimensions: vec![pb::CapacityDimension {
                name: "weight".into(),
                unit: "g".into(),
                penalty_unit: 1000,
            }],
            agents: vec![pb::Agent {
                id: "agent-1".into(),
                start_location: 0,
                end_location: 0,
                start_time: 8 * 3600,
                capacity: vec![10_000],
                skills: vec!["install".into()],
                ..Default::default()
            }],
            tickets: vec![
                pb::Ticket {
                    id: "t-a".into(),
                    location: 1,
                    duration: 600,
                    latest_start: 86_000,
                    latest_end: 86_000,
                    skills_required: vec!["install".into()],
                    ..Default::default()
                },
                pb::Ticket {
                    id: "t-b".into(),
                    location: 2,
                    r#type: pb::TicketType::Delivery as i32,
                    duration: 300,
                    latest_start: 86_000,
                    latest_end: 86_000,
                    demand: vec![4_000],
                    ..Default::default()
                },
                pb::Ticket {
                    id: "t-x".into(),
                    location: 2,
                    latest_start: 86_000,
                    latest_end: 86_000,
                    skills_required: vec!["electric".into()],
                    ..Default::default()
                },
            ],
            weights: Some(weights),
            ..Default::default()
        }),
        options: None,
        initial_solution: Some(pb::Solution {
            routes: vec![pb::Route {
                agent: 0,
                tickets: vec![1, 0],
            }],
        }),
    }
}

#[test]
fn compiles_fills_unknown_edges_and_explains_the_score() {
    let compiled = compile(&request()).unwrap();
    let p = &compiled.problem;
    assert_eq!(compiled.stats.entries, 6);
    assert_eq!(
        (
            compiled.stats.exact,
            compiled.stats.fallback,
            compiled.stats.unreachable
        ),
        (4, 1, 1)
    );
    let m = &p.matrices[0];
    assert!(m.duration(0, 2) > 0 && m.duration(0, 2) < UNKNOWN);
    assert_eq!(m.duration(2, 1), UNREACHABLE);
    // 默认余量：只有兜底估算的 0→2 放大 20%。
    let buffered: Vec<usize> = (0..9).filter(|&k| m.buffer_permille[k] > 0).collect();
    assert_eq!(buffered, vec![m.index(0, 2)]);
    assert_eq!(m.buffer_permille[m.index(0, 2)], 200);
    assert_eq!(compiled.estimate_buffer, vrp_model::DEFAULT_ESTIMATE_BUFFER);

    // 未设置的约束用默认权重，显式设置的覆盖默认值。
    assert_eq!(p.weights.get(C::MinimizeTravelTime), Weight::soft(1));
    assert_eq!(p.weights.get(C::MinimizeTravelDistance), Weight::soft(2));
    assert_eq!(p.weights.get(C::AgentSkills), Weight::ZERO);
    // 技能约束关了，t-x 不被剪枝。
    assert_eq!(p.candidates[2], vec![0]);

    let solution = compiled.initial.clone().unwrap();
    let evaluation = oracle::evaluate(p, &solution);
    let result = build_result(&compiled, &solution, &evaluation, pb::SolveStats::default());
    // 路线 0 → 2（兜底估算）→ 1（不可达）→ 0。估算边按放大后的时长排班，行驶时长仍是原值。
    let d02 = m.duration(0, 2) as i64;
    let route = &result.routes[0];
    assert_eq!(route.stops[0].arrival, 8 * 3600 + (d02 * 1200 + 999) / 1000);
    assert_eq!(route.travel_time, d02 + 300);
    assert_eq!(
        result.estimated_edges,
        vec![pb::Edge {
            matrix: 0,
            from: 0,
            to: 2
        }]
    );
    assert_eq!(evaluation.total(C::UnreachableLeg).matches, 1);
    assert_eq!(
        result.unassigned,
        vec![pb::UnassignedTicket {
            ticket: 2,
            reasons: vec!["not_assigned".into()]
        }]
    );
    let explained: i64 = result.matches.iter().map(|m| m.score.unwrap().soft).sum();
    assert_eq!(explained, result.score.unwrap().soft);
}

#[test]
fn estimate_buffer_and_shift_end_follow_the_request() {
    let mut r = request();
    let problem = r.problem.as_mut().unwrap();
    let mut sources = vec![pb::EdgeSource::Exact as u8; 9];
    sources[3] = pb::EdgeSource::Model as u8; // 1→0
    problem.matrices[0].sources = sources;
    problem.estimate_buffer = Some(pb::EstimateBuffer {
        model_permille: 50,
        fallback_permille: 0,
    });
    problem.agents[0].end_time = Some(8 * 3600);
    let compiled = compile(&r).unwrap();
    let m = &compiled.problem.matrices[0];
    assert_eq!(m.buffer_permille[m.index(1, 0)], 50);
    assert_eq!(
        m.buffer_permille[m.index(0, 2)],
        0,
        "fallback buffer set to 0"
    );

    // 0→2 不放大，1→0 的 300 秒放大 5% 为 315 秒：回到终点 = 出发 + d02 + 300 + 600 + 315。
    let d02 = m.duration(0, 2) as i64;
    let solution = compiled.initial.clone().unwrap();
    let evaluation = oracle::evaluate(&compiled.problem, &solution);
    assert_eq!(evaluation.routes[0].end_time, 8 * 3600 + d02 + 1_215);
    let overtime = evaluation.total(C::AgentShiftOvertime);
    assert_eq!(overtime.amount, (d02 + 1_215 + 59) / 60);
    assert_eq!(overtime.score.soft, -100 * overtime.amount);

    // 两种余量都为 0 时不建余量表。
    let mut r = request();
    r.problem.as_mut().unwrap().estimate_buffer = Some(pb::EstimateBuffer::default());
    let compiled = compile(&r).unwrap();
    assert!(compiled.problem.matrices[0].buffer_permille.is_empty());

    let mut r = request();
    r.problem.as_mut().unwrap().estimate_buffer = Some(pb::EstimateBuffer {
        model_permille: 10_001,
        fallback_permille: 0,
    });
    assert!(compile(&r).unwrap_err().0.contains("estimate_buffer"));

    let mut r = request();
    r.problem.as_mut().unwrap().agents[0].end_time = Some(8 * 3600 - 1);
    assert!(compile(&r).unwrap_err().0.contains("end_time"));
}

#[test]
fn rejects_malformed_requests() {
    let mut r = request();
    r.problem.as_mut().unwrap().matrices[0].durations.pop();
    assert!(compile(&r).unwrap_err().0.contains("durations"));

    let mut r = request();
    r.problem.as_mut().unwrap().tickets[0].r#type = 9;
    assert!(compile(&r).unwrap_err().0.contains("type"));

    let mut r = request();
    r.initial_solution.as_mut().unwrap().routes[0].tickets = vec![0, 0];
    assert!(compile(&r).unwrap_err().0.contains("initial_solution"));

    let mut r = request();
    r.problem.as_mut().unwrap().locations[0].lat = 95.0;
    assert!(compile(&r).is_err());
}

#[test]
fn coordinates_only_requests_use_the_fallback_everywhere() {
    let mut r = request();
    r.problem.as_mut().unwrap().matrices.clear();
    let compiled = compile(&r).unwrap();
    assert_eq!(compiled.stats.fallback, 6);
    assert_eq!(compiled.problem.matrices.len(), 1);
}

#[test]
fn explicit_initial_solutions_must_preserve_pinned_assignments() {
    let mut r = request();
    let t = &mut r.problem.as_mut().unwrap().tickets[0];
    t.original_agent = Some(0);
    t.pinned = true;
    assert!(compile(&r).is_ok());
    r.initial_solution = Some(pb::Solution::default());
    assert!(compile(&r).unwrap_err().0.contains("pinned"));
    r.initial_solution = None;
    assert!(
        compile(&r).is_ok(),
        "solver will seed pins when no initial solution is supplied"
    );
}
