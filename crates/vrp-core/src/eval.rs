//! 增量评分用的路线级计算：一次遍历得到一条路线的全部匹配量，不分配内存。
//!
//! 语义见 docs/constraints-v2.md；[`crate::oracle`] 是按定义逐条计算的独立实现，二者必须一致。

use crate::constraint::ConstraintId as C;
use crate::problem::{Problem, TicketKind, UNREACHABLE, flag};
use crate::score::Score;
use crate::solution::NONE;

/// 一条路线的各项匹配量。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RouteSummary {
    pub count: u32,
    pub travel_time: i64,
    pub travel_distance: i64,
    pub unreachable_legs: i64,
    pub service_time: i64,
    pub late_start_minutes: i64,
    pub late_finish_minutes: i64,
    pub wrong_day: i64,
    pub skills: i64,
    pub qualification: i64,
    pub depot: i64,
    pub changed: i64,
    pub capacity_excess: i64,
    pub loading_ratio_term: i64,
    pub end_time: i64,
    pub execution_violations: i64,
    pub order_changes: i64,
    pub time_changes: i64,
    pub active: bool,
    pub past_after: i64,
    pub past_same: i64,
    pub past_related: i64,
}

/// 逐维度载荷的复用缓冲区。
#[derive(Clone, Debug)]
pub struct Scratch {
    delivery: Vec<i64>,
    load: Vec<i64>,
    peak: Vec<i64>,
}

impl Scratch {
    pub fn new(dims: usize) -> Scratch {
        Scratch {
            delivery: vec![0; dims],
            load: vec![0; dims],
            peak: vec![0; dims],
        }
    }

    /// 最近一次 [`evaluate_route`] 得到的各维度载荷峰值。
    pub fn peak(&self) -> &[i64] {
        &self.peak
    }
}

/// 非负整数的向上取整除法。
#[inline]
pub fn ceil_div(a: i64, b: i64) -> i64 {
    (a + b - 1) / b
}

/// 路线经过的一条边。不可达边的时长和距离都按 0 计。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Leg {
    /// 行驶时长（秒），计入行驶成本。
    pub duration: i64,
    /// 排班时长（秒）：行驶时长加估算余量，用于推算时刻。
    pub schedule: i64,
    /// 米。
    pub distance: i64,
    pub unreachable: bool,
}

#[inline]
pub fn leg(problem: &Problem, matrix: u32, from: u32, to: u32) -> Leg {
    let m = &problem.matrices[matrix as usize];
    let i = m.index(from, to);
    let d = m.duration[i];
    if d == UNREACHABLE {
        return Leg {
            duration: 0,
            schedule: 0,
            distance: 0,
            unreachable: true,
        };
    }
    let d = d as i64;
    let extra = m
        .buffer_permille
        .get(i)
        .map_or(0, |&p| ceil_div(d * p as i64, 1000));
    Leg {
        duration: d,
        schedule: d + extra,
        distance: m.distance[i] as i64,
        unreachable: false,
    }
}

/// 计算 agent 按 `tickets` 顺序执行时的汇总；每站回调 `on_stop(ticket, arrival, start, end)`。
pub fn evaluate_route<F: FnMut(u32, i64, i64, i64)>(
    problem: &Problem,
    agent: u32,
    tickets: &[u32],
    scratch: &mut Scratch,
    on_stop: F,
) -> RouteSummary {
    evaluate_route_with_releases(problem, agent, tickets, scratch, &[], on_stop)
}

pub(crate) fn evaluate_route_with_releases<F: FnMut(u32, i64, i64, i64)>(
    problem: &Problem,
    agent: u32,
    tickets: &[u32],
    scratch: &mut Scratch,
    releases: &[i64],
    mut on_stop: F,
) -> RouteSummary {
    let a = &problem.agents[agent as usize];
    let execution = &problem.execution.routes[agent as usize];
    let mut s = RouteSummary {
        end_time: a.start_time,
        count: execution.completed_tickets,
        active: !tickets.is_empty()
            || execution.completed_tickets > 0
            || execution.must_return
            || execution.worked_seconds > 0
            || execution.fixed_cost_paid
            || execution
                .initial_load
                .as_ref()
                .is_some_and(|load| load.iter().any(|&v| v > 0)),
        ..RouteSummary::default()
    };
    let dims = problem.dims;
    scratch.delivery.fill(0);
    if !s.active {
        scratch.peak.fill(0);
        return s;
    }
    for &t in tickets {
        if problem.tickets[t as usize].kind == TicketKind::Delivery {
            for (acc, q) in scratch.delivery.iter_mut().zip(problem.demand(t)) {
                *acc += q;
            }
        }
    }
    scratch.load.copy_from_slice(
        execution
            .initial_load
            .as_deref()
            .unwrap_or(&scratch.delivery),
    );
    scratch.peak.copy_from_slice(&scratch.load);

    let mut prev = a.start_location;
    let mut departure = a.start_time;
    let mut business_position = 0;
    for (position, &t) in tickets.iter().enumerate() {
        let tk = &problem.tickets[t as usize];
        let rule = &problem.execution.tickets[t as usize];
        let edge = leg(problem, a.matrix, prev, tk.location);
        s.unreachable_legs += edge.unreachable as i64;
        s.travel_time += edge.duration;
        s.travel_distance += edge.distance;
        let arrival = departure + edge.schedule;
        let start = arrival
            .max(tk.earliest_start)
            .max(crate::timing::past_release(problem, t as usize))
            .max(releases.get(t as usize).copied().unwrap_or(i64::MIN));
        let end = start + tk.duration;
        if !rule.auxiliary {
            s.count += 1;
            s.order_changes += rule
                .original_position
                .is_some_and(|p| p != business_position) as i64;
            business_position += 1;
            if let Some(prior) = rule.original_start {
                s.time_changes += ceil_div((start - prior).abs(), 60);
            }
        }
        if let Some(load) = rule.load_before {
            s.execution_violations += (!tickets[..position].contains(&load)) as i64;
        }
        if let Some(v) = rule.committed_earliest {
            s.execution_violations += ceil_div((v - start).max(0), 60);
        }
        if let Some(v) = rule.committed_latest {
            s.execution_violations += ceil_div((start - v).max(0), 60);
        }
        for past in &problem.execution.past[t as usize] {
            if past.same_agent && past.agent != agent {
                if past.related {
                    s.past_related += 1;
                } else {
                    s.past_same += 1;
                }
            }
            if past.after_finish {
                s.past_after += ceil_div((past.finished_at - start).max(0), 60);
            }
        }
        s.service_time += tk.duration;
        if !rule.auxiliary && start > tk.latest_start {
            s.late_start_minutes += ceil_div(start - tk.latest_start, 60);
        }
        if !rule.auxiliary && end > tk.latest_end {
            s.late_finish_minutes += ceil_div(end - tk.latest_end, 60);
        }
        if !rule.auxiliary && problem.day_of(start) != tk.day {
            s.wrong_day += 1;
        }
        let f = problem.static_flags(t, agent);
        if !rule.auxiliary {
            s.skills += (f & flag::SKILLS != 0) as i64;
            s.qualification += (f & flag::QUALIFICATION != 0) as i64;
            s.depot += (f & flag::DEPOT != 0) as i64;
            s.changed += tk.original_agent.is_some_and(|o| o != agent) as i64;
        }
        match tk.kind {
            TicketKind::Delivery => {
                for (l, q) in scratch.load.iter_mut().zip(problem.demand(t)) {
                    *l -= q;
                    if *l < 0 {
                        s.execution_violations += -*l;
                    }
                }
            }
            TicketKind::Pickup => {
                for d in 0..dims {
                    scratch.load[d] += problem.demand(t)[d];
                    scratch.peak[d] = scratch.peak[d].max(scratch.load[d]);
                }
            }
            TicketKind::Service => {}
        }
        on_stop(t, arrival, start, end);
        departure = end;
        prev = tk.location;
    }
    let edge = if execution.unavailable {
        Leg {
            duration: 0,
            schedule: 0,
            distance: 0,
            unreachable: false,
        }
    } else {
        leg(problem, a.matrix, prev, a.end_location)
    };
    s.unreachable_legs += edge.unreachable as i64;
    s.travel_time += edge.duration;
    s.travel_distance += edge.distance;
    s.end_time = departure + edge.schedule;

    let limit = problem.limit(agent);
    let capacity = problem.capacity(agent);
    let mut ratio = 0.0f64;
    for d in 0..dims {
        let excess = scratch.peak[d] - limit[d];
        if excess > 0 {
            s.capacity_excess += ceil_div(excess, problem.dim_penalty_unit[d]);
        }
        if capacity[d] > 0 {
            ratio += (scratch.delivery[d] + execution.delivered.get(d).copied().unwrap_or(0))
                as f64
                / capacity[d] as f64;
        }
    }
    let x = (10.0 * ratio).max(1.0);
    s.loading_ratio_term = (x * x).floor() as i64;
    s
}

/// 一条路线的 agent 级与站点级罚分。没派单的 agent 不计罚。
pub fn route_score(problem: &Problem, agent: u32, s: &RouteSummary) -> Score {
    if !s.active {
        return Score::ZERO;
    }
    let w = &problem.weights;
    let a = &problem.agents[agent as usize];
    let count = s.count as i64;
    let mut score = Score::penalty(s.capacity_excess, w.get(C::AgentCapacity));
    if a.max_tickets > 0 && s.count > a.max_tickets {
        score += Score::penalty((s.count - a.max_tickets) as i64, w.get(C::AgentMaxTicket));
    }
    score += Score::penalty(s.skills, w.get(C::AgentSkills));
    score += Score::penalty(s.qualification, w.get(C::AgentQualification));
    score += Score::penalty(
        s.late_finish_minutes,
        w.get(C::ServiceFinishedAfterMaxEndTime),
    );
    score += Score::penalty(
        s.late_start_minutes,
        w.get(C::TicketStartServiceTimeMatchExpected),
    );
    score += Score::penalty(s.wrong_day, w.get(C::TicketArrivalTimeSameDateWithPlanTime));
    score += Score::penalty(s.travel_time, w.get(C::MinimizeTravelTime));
    score += Score::penalty(s.travel_distance, w.get(C::MinimizeTravelDistance));
    score += Score::penalty(
        fixed_cost(problem, agent, s.travel_time),
        w.get(C::MinimizeAgentFixedCost),
    );
    score += Score::penalty(s.depot, w.get(C::SameDepo));
    score += Score::penalty(count * count, w.get(C::BalanceAgentLoading));
    score += Score::penalty(s.loading_ratio_term, w.get(C::BalanceAgentLoadingRatio));
    let hours = ceil_div(
        s.travel_time + s.service_time + problem.execution.routes[agent as usize].worked_seconds,
        3600,
    );
    score += Score::penalty(hours * hours, w.get(C::BalanceAgentWorkingTime));
    score += Score::penalty(s.changed, w.get(C::MinimizeTicketChanging));
    score += Score::penalty(s.unreachable_legs, w.get(C::UnreachableLeg));
    score += Score::penalty(s.execution_violations, w.get(C::ExecutionConsistency));
    score += Score::penalty(s.order_changes, w.get(C::MinimizeVisitOrderChanging));
    score += Score::penalty(s.time_changes, w.get(C::MinimizeServiceTimeChanging));
    score += Score::penalty(s.past_after, w.get(C::RefTicketAfterDepTicket));
    score += Score::penalty(s.past_same, w.get(C::RefTicketSameAgentWithDepTicket));
    score += Score::penalty(s.past_related, w.get(C::RelationTicketsSameAgent));
    if let Some(shift_end) = a.end_time
        && s.end_time > shift_end
    {
        score += Score::penalty(
            ceil_div(s.end_time - shift_end, 60),
            w.get(C::AgentShiftOvertime),
        );
    }
    score
}

/// 已使用的 agent 的固定成本（含限行费）。
#[inline]
pub fn fixed_cost(problem: &Problem, agent: u32, travel_time: i64) -> i64 {
    let a = &problem.agents[agent as usize];
    let restricted = if a.restricted {
        (problem.restricted_charge_per_hour as i128 * travel_time as i128 / 3600) as i64
    } else {
        0
    };
    if problem.execution.routes[agent as usize].fixed_cost_paid {
        restricted
    } else {
        a.fixed_cost + restricted
    }
}

/// 未派工单的罚分：未派本身，以及有原指派时的改派。
#[inline]
pub fn unassigned_score(problem: &Problem, ticket: u32) -> Score {
    if problem.execution.tickets[ticket as usize].auxiliary {
        return Score::ZERO;
    }
    let w = &problem.weights;
    let mut score = Score::penalty(1, w.get(C::TicketUnassigned));
    if problem.tickets[ticket as usize].original_agent.is_some() {
        score += Score::penalty(1, w.get(C::MinimizeTicketChanging));
    }
    score
}

/// 关联对的罚分：两张都已派且 agent 不同。
#[inline]
pub fn related_score(problem: &Problem, agent_a: u32, agent_b: u32) -> Score {
    if agent_a != NONE && agent_b != NONE && agent_a != agent_b {
        Score::penalty(1, problem.weights.get(C::RelationTicketsSameAgent))
    } else {
        Score::ZERO
    }
}

/// 依赖对的罚分。`first` 取 (agent, 结束服务)，`second` 取 (agent, 开始服务)。
#[inline]
pub fn precedence_score(problem: &Problem, first: (u32, i64), second: (u32, i64)) -> Score {
    if second.0 == NONE {
        return Score::ZERO;
    }
    if first.0 == NONE {
        return Score::penalty(1, problem.weights.get(C::RefTicketAfterDepTicket));
    }
    let w = &problem.weights;
    let mut score = Score::ZERO;
    if first.0 != second.0 {
        score += Score::penalty(1, w.get(C::RefTicketSameAgentWithDepTicket));
    }
    if first.1 > second.1 {
        score += Score::penalty(
            ceil_div(first.1 - second.1, 60),
            w.get(C::RefTicketAfterDepTicket),
        );
    }
    score
}
