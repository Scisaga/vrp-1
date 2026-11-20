//! 每条约束的手算用例。期望值都按 docs/constraints-v2.md 的定义手工推出。

mod common;

use common::{agent, line_matrix, spec, ticket};
use vrp_core::constraint::ConstraintId as C;
use vrp_core::oracle::{self, Evaluation};
use vrp_core::problem::flag;
use vrp_core::{Problem, Score, Solution, State, TicketKind, UNREACHABLE, Weight, Weights};

/// 用 oracle 评分，同时核对增量评分给出相同的总分。
fn evaluate(problem: &Problem, routes: Vec<Vec<u32>>) -> Evaluation {
    let solution = Solution { routes };
    let evaluation = oracle::evaluate(problem, &solution);
    let state = State::new(problem, &solution).unwrap();
    assert_eq!(
        state.score(),
        evaluation.score,
        "incremental and oracle disagree"
    );
    evaluation
}

fn only(constraint: C, weight: Weight) -> Weights {
    Weights::none().with(constraint, weight)
}

const H8: i64 = 8 * 3600;

#[test]
fn schedule_waits_for_earliest_start_and_counts_return_leg() {
    let mut s = spec(4, only(C::MinimizeTravelTime, Weight::soft(1)));
    s.agents.push(agent("a", 0, H8));
    let mut t0 = ticket("t0", 1, 1800);
    t0.earliest_start = 9 * 3600;
    s.tickets.push(t0);
    s.tickets.push(ticket("t1", 3, 600));
    let p = Problem::new(s).unwrap();
    let e = evaluate(&p, vec![vec![0, 1]]);
    let route = &e.routes[0];
    // 0→1 行驶 60 秒，等到 9:00 开始，服务 30 分钟；1→3 行驶 120 秒；3→0 回程 180 秒。
    assert_eq!(
        (
            route.stops[0].arrival,
            route.stops[0].start,
            route.stops[0].end
        ),
        (H8 + 60, 32_400, 34_200)
    );
    assert_eq!(
        (
            route.stops[1].arrival,
            route.stops[1].start,
            route.stops[1].end
        ),
        (34_320, 34_320, 34_920)
    );
    assert_eq!(route.end_time, 35_100);
    assert_eq!((route.travel_time, route.travel_distance), (360, 6_000));
    assert_eq!(
        e.score,
        Score {
            hard: 0,
            medium: 0,
            soft: -360
        }
    );
}

#[test]
fn lateness_rounds_up_to_whole_minutes() {
    let w = only(C::TicketStartServiceTimeMatchExpected, Weight::soft(100))
        .with(C::ServiceFinishedAfterMaxEndTime, Weight::hard(1));
    let mut s = spec(2, w);
    s.agents.push(agent("a", 0, H8));
    let mut t = ticket("t", 1, 600);
    // 到达 8:01，比 latest_start 晚 1 秒；结束 8:11，比 latest_end 晚 61 秒。
    t.latest_start = H8 + 59;
    t.latest_end = H8 + 60 + 600 - 61;
    s.tickets.push(t);
    let p = Problem::new(s).unwrap();
    let e = evaluate(&p, vec![vec![0]]);
    assert_eq!(e.total(C::TicketStartServiceTimeMatchExpected).amount, 1);
    assert_eq!(e.total(C::ServiceFinishedAfterMaxEndTime).amount, 2);
    assert_eq!(
        e.score,
        Score {
            hard: -2,
            medium: 0,
            soft: -100
        }
    );
}

#[test]
fn capacity_peaks_are_tracked_per_dimension() {
    let mut s = spec(3, only(C::AgentCapacity, Weight::hard(1)));
    s.dims = 2;
    s.dim_penalty_unit = vec![1, 10];
    let mut a = agent("a", 0, 0);
    a.capacity = vec![10, 100];
    a.tolerance_permille = vec![200, 0]; // 重量维上限 12，体积维上限 100
    s.agents.push(a);
    let mut pickup = ticket("pickup", 1, 0);
    pickup.kind = TicketKind::Pickup;
    pickup.demand = vec![4, 0];
    let mut d1 = ticket("d1", 2, 0);
    d1.kind = TicketKind::Delivery;
    d1.demand = vec![5, 95];
    let mut d2 = ticket("d2", 2, 0);
    d2.kind = TicketKind::Delivery;
    d2.demand = vec![6, 20];
    s.tickets.extend([pickup, d1, d2]);
    let p = Problem::new(s).unwrap();
    // 重量：出发 11，先取件到 15，超 3；体积：出发 115，超 15，按 10 计 2 个单位。
    let e = evaluate(&p, vec![vec![0, 1, 2]]);
    assert_eq!(e.routes[0].peak_load, vec![15, 115]);
    assert_eq!(e.total(C::AgentCapacity).amount, 3 + 2);
    // 先送货后取件：重量峰值仍是出发时的 11，不超；体积仍超 15。
    let e = evaluate(&p, vec![vec![1, 2, 0]]);
    assert_eq!(e.routes[0].peak_load, vec![11, 115]);
    assert_eq!(e.total(C::AgentCapacity).amount, 2);
}

#[test]
fn unused_agents_pay_nothing_and_restricted_agents_pay_by_hour() {
    let w = only(C::MinimizeAgentFixedCost, Weight::soft(20))
        .with(C::BalanceAgentLoadingRatio, Weight::medium(1));
    let mut s = spec(61, w);
    s.restricted_charge_per_hour = 50;
    let mut used = agent("used", 0, 0);
    used.fixed_cost = 300;
    used.restricted = true;
    let mut idle = agent("idle", 0, 0);
    idle.fixed_cost = 999;
    s.agents.extend([used, idle]);
    s.tickets.push(ticket("far", 60, 0)); // 往返 7200 秒，限行费 100
    let p = Problem::new(s).unwrap();
    let e = evaluate(&p, vec![vec![0], vec![]]);
    assert_eq!(e.total(C::MinimizeAgentFixedCost).amount, 400);
    assert_eq!(e.total(C::BalanceAgentLoadingRatio).amount, 1);
    assert_eq!(
        e.score,
        Score {
            hard: 0,
            medium: -1,
            soft: -8_000
        }
    );
}

#[test]
fn balance_terms_use_squares() {
    let w = only(C::BalanceAgentLoading, Weight::medium(1))
        .with(C::BalanceAgentWorkingTime, Weight::medium(1))
        .with(C::BalanceAgentLoadingRatio, Weight::soft(1));
    let mut s = spec(2, w);
    s.dims = 2;
    let mut a = agent("a", 0, 0);
    a.capacity = vec![100, 0]; // 容量为 0 的维度不参与比例
    s.agents.push(a);
    for i in 0..3 {
        let mut t = ticket(&format!("t{i}"), 1, 1200);
        t.kind = TicketKind::Delivery;
        t.demand = vec![12, 50];
        s.tickets.push(t);
    }
    let p = Problem::new(s).unwrap();
    let e = evaluate(&p, vec![vec![0, 1, 2]]);
    assert_eq!(e.total(C::BalanceAgentLoading).amount, 9);
    // 行驶 120 秒 + 服务 3600 秒 → 向上取整 2 小时 → 4。
    assert_eq!(e.total(C::BalanceAgentWorkingTime).amount, 4);
    // 比例 36/100 → 10 × 0.36 = 3.6 → 3.6² = 12.96 → 12。
    assert_eq!(e.total(C::BalanceAgentLoadingRatio).amount, 12);
}

#[test]
fn related_pairs_are_unordered_and_need_both_assigned() {
    let mut s = spec(2, only(C::RelationTicketsSameAgent, Weight::medium(50)));
    s.agents.extend([agent("a", 0, 0), agent("b", 0, 0)]);
    s.tickets
        .extend([ticket("x", 1, 0), ticket("y", 1, 0), ticket("z", 1, 0)]);
    s.related_pairs = vec![(0, 1), (1, 0), (0, 2)];
    let p = Problem::new(s).unwrap();
    assert_eq!(p.related_pairs, vec![(0, 1), (0, 2)]);
    let e = evaluate(&p, vec![vec![0], vec![1]]);
    assert_eq!(e.total(C::RelationTicketsSameAgent).matches, 1);
    assert_eq!(e.score.medium, -50);
}

#[test]
fn precedence_requires_start_after_dependency_ends() {
    let w = only(C::RefTicketAfterDepTicket, Weight::hard(1))
        .with(C::RefTicketSameAgentWithDepTicket, Weight::medium(1));
    let mut s = spec(2, w);
    s.agents.extend([agent("a", 0, H8), agent("b", 0, H8)]);
    s.tickets
        .extend([ticket("first", 1, 1800), ticket("second", 1, 600)]);
    s.precedences = vec![(0, 1)];
    let p = Problem::new(s).unwrap();
    // 不同 agent 同时出发：second 等 first 8:31 结束后才开始。
    let e = evaluate(&p, vec![vec![0], vec![1]]);
    assert_eq!(
        e.score,
        Score {
            hard: 0,
            medium: -1,
            soft: 0
        }
    );
    assert_eq!(e.routes[1].stops[0].arrival, H8 + 60);
    assert_eq!(e.routes[1].stops[0].start, H8 + 1860);
    // 同一 agent、先 first 后 second：没有违反。
    let e = evaluate(&p, vec![vec![0, 1], vec![]]);
    assert_eq!(e.score, Score::ZERO);
}

#[test]
fn unassigned_ticket_with_original_agent_counts_as_changed() {
    let w = only(C::TicketUnassigned, Weight::medium(1000))
        .with(C::MinimizeTicketChanging, Weight::soft(1000));
    let mut s = spec(2, w);
    s.agents.extend([agent("a", 0, 0), agent("b", 0, 0)]);
    let mut kept = ticket("kept", 1, 0);
    kept.original_agent = Some(0);
    let mut moved = ticket("moved", 1, 0);
    moved.original_agent = Some(0);
    let mut dropped = ticket("dropped", 1, 0);
    dropped.original_agent = Some(1);
    s.tickets
        .extend([kept, moved, dropped, ticket("new", 1, 0)]);
    let p = Problem::new(s).unwrap();
    let e = evaluate(&p, vec![vec![0], vec![1]]);
    assert_eq!(e.total(C::TicketUnassigned).matches, 2);
    assert_eq!(e.total(C::MinimizeTicketChanging).matches, 2);
    assert_eq!(
        e.score,
        Score {
            hard: 0,
            medium: -2000,
            soft: -2000
        }
    );
}

#[test]
fn unreachable_legs_cost_nothing_but_are_penalized() {
    let w = only(C::UnreachableLeg, Weight::hard(1)).with(C::MinimizeTravelTime, Weight::soft(1));
    let mut s = spec(3, w);
    let m = &mut s.matrices[0];
    let i = m.index(1, 2);
    m.duration[i] = UNREACHABLE;
    m.distance[i] = UNREACHABLE;
    s.agents.push(agent("a", 0, 0));
    s.tickets.extend([ticket("t1", 1, 0), ticket("t2", 2, 0)]);
    let p = Problem::new(s).unwrap();
    // 0→1 60 秒，1→2 不可达按 0 计，2→0 回程 120 秒。
    let e = evaluate(&p, vec![vec![0, 1]]);
    assert_eq!(
        e.score,
        Score {
            hard: -1,
            medium: 0,
            soft: -180
        }
    );
}

#[test]
fn shift_overtime_counts_minutes_after_returning() {
    let mut s = spec(3, only(C::AgentShiftOvertime, Weight::soft(100)));
    // 8:00 出发，0→2 行驶 120 秒，服务 10 分钟，回程 120 秒：8:14 回到终点。
    let mut late = agent("late", 0, H8);
    late.end_time = Some(H8 + 779);
    let mut on_time = agent("on_time", 0, H8);
    on_time.end_time = Some(H8 + 840);
    let mut idle = agent("idle", 0, H8);
    idle.end_time = Some(H8);
    s.agents
        .extend([late, on_time, idle, agent("unbounded", 0, H8)]);
    s.tickets.extend([
        ticket("x", 2, 600),
        ticket("y", 2, 600),
        ticket("z", 2, 600),
    ]);
    let p = Problem::new(s).unwrap();
    let e = evaluate(&p, vec![vec![0], vec![1], vec![], vec![2]]);
    assert_eq!(e.routes[0].end_time, H8 + 840);
    // 只有 late 超时 61 秒，按 2 分钟计；没派单的和不限班次的 agent 不计。
    assert_eq!(e.total(C::AgentShiftOvertime).matches, 1);
    assert_eq!(e.total(C::AgentShiftOvertime).amount, 2);
    assert_eq!(
        e.score,
        Score {
            hard: 0,
            medium: 0,
            soft: -200
        }
    );
}

#[test]
fn estimated_edges_are_buffered_in_the_schedule_only() {
    let w = only(C::TicketStartServiceTimeMatchExpected, Weight::soft(100))
        .with(C::MinimizeTravelTime, Weight::soft(1));
    let mut s = spec(2, w);
    // 只有 0→1 是估算边，放大 12.5%：60 秒按 ⌈67.5⌉ = 68 秒排班。
    let m = &mut s.matrices[0];
    m.buffer_permille = vec![0; 4];
    let i = m.index(0, 1);
    m.buffer_permille[i] = 125;
    s.agents.push(agent("a", 0, H8));
    let mut t = ticket("t", 1, 0);
    t.latest_start = H8 + 60;
    s.tickets.push(t);
    let p = Problem::new(s).unwrap();
    let e = evaluate(&p, vec![vec![0]]);
    let route = &e.routes[0];
    assert_eq!(route.stops[0].arrival, H8 + 68);
    // 回程 1→0 是实测边，不放大。
    assert_eq!(route.end_time, H8 + 128);
    // 行驶成本仍按原始时长 60 + 60；迟到 8 秒按 1 分钟计。
    assert_eq!(route.travel_time, 120);
    assert_eq!(e.total(C::TicketStartServiceTimeMatchExpected).amount, 1);
    assert_eq!(
        e.score,
        Score {
            hard: 0,
            medium: 0,
            soft: -220
        }
    );
}

#[test]
fn service_day_is_compared_by_horizon_day_index() {
    let mut s = spec(
        2,
        only(C::TicketArrivalTimeSameDateWithPlanTime, Weight::hard(1)),
    );
    s.agents.push(agent("a", 0, 23 * 3600));
    let mut today = ticket("today", 1, 0);
    today.day = 0;
    let mut late_night = ticket("spills", 1, 0);
    late_night.day = 0;
    late_night.earliest_start = 86_400 + 60; // 只能次日服务
    s.tickets.extend([today, late_night]);
    let p = Problem::new(s).unwrap();
    let e = evaluate(&p, vec![vec![0, 1]]);
    assert_eq!(e.total(C::TicketArrivalTimeSameDateWithPlanTime).matches, 1);
}

#[test]
fn static_flags_cover_skills_qualifications_and_depots() {
    let mut s = spec(2, Weights::default());
    let mut a = agent("a", 0, 0);
    a.skills = vec![0, 2];
    a.qualifications = vec![(0, 2.0)];
    a.depot = Some(0);
    s.agents.push(a);
    let mut ok = ticket("ok", 1, 0);
    ok.skills_required = vec![2];
    ok.qualification_required = vec![(0, 2.0)];
    ok.depot = Some(0);
    let mut bad = ticket("bad", 1, 0);
    bad.skills_required = vec![1, 2];
    bad.qualification_required = vec![(0, 3.0)];
    bad.depot = Some(1);
    let mut no_depot = ticket("no_depot", 1, 0);
    no_depot.qualification_required = vec![(5, 0.0)];
    s.tickets.extend([ok, bad, no_depot]);
    let p = Problem::new(s).unwrap();
    assert_eq!(p.static_flags(0, 0), 0);
    assert_eq!(
        p.static_flags(1, 0),
        flag::SKILLS | flag::QUALIFICATION | flag::DEPOT
    );
    assert_eq!(p.static_flags(2, 0), flag::QUALIFICATION);
}

#[test]
fn only_structurally_pinned_assignments_prune_candidates() {
    let build = |weights: Weights| {
        let mut s = spec(2, weights);
        let mut skilled = agent("skilled", 0, 0);
        skilled.skills = vec![0];
        let mut tomorrow = agent("tomorrow", 0, 86_400);
        tomorrow.day = 1;
        tomorrow.skills = vec![0];
        s.agents.extend([skilled, agent("plain", 0, 0), tomorrow]);
        let mut t = ticket("t", 1, 0);
        t.skills_required = vec![0];
        let mut pinned = ticket("pinned", 1, 0);
        pinned.original_agent = Some(1);
        pinned.pinned = true;
        s.tickets.extend([t, pinned]);
        Problem::new(s).unwrap()
    };
    let p = build(Weights::default());
    assert_eq!(p.candidates[0], vec![0, 1, 2]);
    assert_eq!(p.prune_reasons[0], 0);
    assert_eq!(p.candidates[1], vec![1]);
    assert_eq!(p.prune_reasons[1], flag::PINNED);

    let p = build(Weights::default().with(C::TicketUnassigned, Weight::hard(1)));
    assert_eq!(p.candidates[0], vec![0, 1, 2]);
    assert_eq!(p.candidates[1], vec![1]);
}

#[test]
fn invalid_specs_are_rejected() {
    let mut s = spec(2, Weights::default());
    s.agents.push(agent("a", 0, 0));
    let mut t = ticket("t", 1, 0);
    t.pinned = true;
    s.tickets.push(t);
    assert!(Problem::new(s).unwrap_err().0.contains("pinned"));

    let mut s = spec(2, Weights::default());
    s.matrices[0].duration[1] = vrp_core::problem::UNKNOWN;
    s.matrices[0].distance[1] = vrp_core::problem::UNKNOWN;
    assert!(Problem::new(s).unwrap_err().0.contains("unknown"));

    let mut s = spec(2, Weights::default());
    s.tickets.push(ticket("t", 1, 0));
    s.related_pairs = vec![(0, 3)];
    assert!(Problem::new(s).is_err());

    let s = spec(
        2,
        Weights::default().with(C::MinimizeTravelTime, Weight::soft(-1)),
    );
    assert!(Problem::new(s).is_err());

    let mut s = spec(2, Weights::default());
    s.matrices.push(line_matrix(3));
    assert!(Problem::new(s).is_err());

    let mut s = spec(2, Weights::default());
    let mut a = agent("a", 0, H8);
    a.end_time = Some(H8 - 1);
    s.agents.push(a);
    assert!(Problem::new(s).unwrap_err().0.contains("end_time"));

    let mut s = spec(2, Weights::default());
    s.matrices[0].buffer_permille = vec![0; 3];
    assert!(Problem::new(s).unwrap_err().0.contains("buffer"));
}

#[test]
fn explicit_day_starts_handle_irregular_days() {
    let mut s = spec(1, Weights::default());
    s.day_starts = vec![0, 82_800, 169_200]; // 第 0 天只有 23 小时
    let p = Problem::new(s).unwrap();
    assert_eq!(p.day_of(82_799), 0);
    assert_eq!(p.day_of(82_800), 1);
    assert_eq!(p.day_of(169_200 + 86_400), 3);
    assert_eq!(p.day_of(-1), -1);
}
