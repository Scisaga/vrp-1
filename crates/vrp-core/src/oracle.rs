//! 全量评分：按 docs/constraints-v2.md 逐条约束直接计算，并产出解释明细。
//!
//! 这里刻意不复用 [`crate::eval`] 的路线汇总，作为增量评分的独立对照。

use crate::constraint::{CONSTRAINT_COUNT, ConstraintId as C, Weight};
use crate::problem::{Problem, TicketKind, UNREACHABLE, flag};
use crate::score::Score;
use crate::solution::{NONE, Solution};

/// 一条计分明细。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub constraint: C,
    pub agents: Vec<u32>,
    pub tickets: Vec<u32>,
    /// 乘权重之前的量。
    pub amount: i64,
    pub score: Score,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConstraintTotal {
    pub matches: u64,
    pub amount: i64,
    pub score: Score,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stop {
    pub ticket: u32,
    pub arrival: i64,
    pub start: i64,
    pub end: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteDetail {
    pub agent: u32,
    pub stops: Vec<Stop>,
    pub travel_time: i64,
    pub travel_distance: i64,
    pub end_time: i64,
    pub peak_load: Vec<i64>,
    /// 经过的边（含回程），(from, to) 为地点下标。
    pub legs: Vec<(u32, u32)>,
    pub unreachable_legs: i64,
}

#[derive(Clone, Debug)]
pub struct Evaluation {
    pub score: Score,
    pub totals: [ConstraintTotal; CONSTRAINT_COUNT],
    /// 只含权重非 0 且匹配量大于 0 的明细。
    pub matches: Vec<Match>,
    /// 只含已使用的 agent。
    pub routes: Vec<RouteDetail>,
    /// 每张工单所在的 agent，未派为 [`NONE`]。
    pub agent_of: Vec<u32>,
}

impl Evaluation {
    pub fn total(&self, constraint: C) -> ConstraintTotal {
        self.totals[constraint.index()]
    }
}

struct Recorder<'p> {
    problem: &'p Problem,
    totals: [ConstraintTotal; CONSTRAINT_COUNT],
    matches: Vec<Match>,
}

impl Recorder<'_> {
    fn add(&mut self, constraint: C, amount: i64, agents: &[u32], tickets: &[u32]) {
        if amount <= 0 {
            return;
        }
        let weight: Weight = self.problem.weights.get(constraint);
        let score = Score::penalty(amount, weight);
        let total = &mut self.totals[constraint.index()];
        total.matches += 1;
        total.amount += amount;
        total.score += score;
        if !weight.is_zero() {
            self.matches.push(Match {
                constraint,
                agents: agents.to_vec(),
                tickets: tickets.to_vec(),
                amount,
                score,
            });
        }
    }
}

fn minutes_late(actual: i64, deadline: i64) -> i64 {
    if actual > deadline {
        (actual - deadline + 59) / 60
    } else {
        0
    }
}

/// 按定义计算一条路线的时间表和载荷。
fn schedule(problem: &Problem, agent: u32, tickets: &[u32], releases: &[i64]) -> RouteDetail {
    let a = &problem.agents[agent as usize];
    let m = &problem.matrices[a.matrix as usize];
    let mut detail = RouteDetail {
        agent,
        stops: Vec::with_capacity(tickets.len()),
        travel_time: 0,
        travel_distance: 0,
        end_time: a.start_time,
        peak_load: vec![0; problem.dims],
        legs: Vec::with_capacity(tickets.len() + 1),
        unreachable_legs: 0,
    };
    // 记下行驶成本，返回推算时刻用的排班时长：估算边按放大后的时长计。
    let drive = |from: u32, to: u32, detail: &mut RouteDetail| -> i64 {
        detail.legs.push((from, to));
        let d = m.duration(from, to);
        if d == UNREACHABLE {
            detail.unreachable_legs += 1;
            return 0;
        }
        detail.travel_time += d as i64;
        detail.travel_distance += m.distance(from, to) as i64;
        let permille = m
            .buffer_permille
            .get(m.index(from, to))
            .map_or(0, |&p| p as i64);
        (d as i64 * (1000 + permille) + 999) / 1000
    };
    let mut clock = a.start_time;
    let mut here = a.start_location;
    for &t in tickets {
        let tk = &problem.tickets[t as usize];
        let arrival = clock + drive(here, tk.location, &mut detail);
        let past = problem.execution.past[t as usize]
            .iter()
            .filter(|p| p.after_finish)
            .map(|p| p.finished_at)
            .max()
            .unwrap_or(i64::MIN);
        let start = arrival
            .max(tk.earliest_start)
            .max(past)
            .max(releases.get(t as usize).copied().unwrap_or(i64::MIN));
        let end = start + tk.duration;
        detail.stops.push(Stop {
            ticket: t,
            arrival,
            start,
            end,
        });
        clock = end;
        here = tk.location;
    }
    detail.end_time = clock
        + if problem.execution.routes[agent as usize].unavailable {
            0
        } else {
            drive(here, a.end_location, &mut detail)
        };

    // 载荷剖面：出发时装上全部配送货物，逐站卸货或装货。
    for d in 0..problem.dims {
        let static_load: i64 = tickets
            .iter()
            .filter(|&&t| problem.tickets[t as usize].kind == TicketKind::Delivery)
            .map(|&t| problem.demand(t)[d])
            .sum();
        let mut load = problem.execution.routes[agent as usize]
            .initial_load
            .as_ref()
            .map_or(static_load, |v| v[d]);
        let mut peak = load;
        for &t in tickets {
            match problem.tickets[t as usize].kind {
                TicketKind::Delivery => load -= problem.demand(t)[d],
                TicketKind::Pickup => load += problem.demand(t)[d],
                TicketKind::Service => {}
            }
            peak = peak.max(load);
        }
        detail.peak_load[d] = peak;
    }
    detail
}

// Independent reference propagation: repeatedly select stations whose route
// predecessor and all assigned business predecessors have been scheduled.
fn dependency_times(p: &Problem, solution: &Solution, agent_of: &[u32]) -> (Vec<i64>, Vec<u32>) {
    if p.precedences.is_empty() || p.weights.get(C::RefTicketAfterDepTicket).is_zero() {
        return (Vec::new(), Vec::new());
    }
    let n = p.tickets.len();
    let mut parents: Vec<Vec<(usize, i64)>> = vec![Vec::new(); n];
    let mut times: Vec<i64> = p
        .tickets
        .iter()
        .enumerate()
        .map(|(t, tk)| {
            tk.earliest_start.max(
                p.execution.past[t]
                    .iter()
                    .filter(|v| v.after_finish)
                    .map(|v| v.finished_at)
                    .max()
                    .unwrap_or(i64::MIN),
            )
        })
        .collect();
    for (a, route) in solution.routes.iter().enumerate() {
        let spec = &p.agents[a];
        let matrix = &p.matrices[spec.matrix as usize];
        let mut from = spec.start_location;
        for (i, &t) in route.iter().enumerate() {
            let k = matrix.index(from, p.tickets[t as usize].location);
            let d = matrix.duration[k];
            let travel = if d == UNREACHABLE {
                0
            } else {
                (d as i64 * (1000 + matrix.buffer_permille.get(k).copied().unwrap_or(0) as i64)
                    + 999)
                    / 1000
            };
            if i == 0 {
                times[t as usize] = times[t as usize].max(spec.start_time + travel);
            } else {
                let previous = route[i - 1] as usize;
                parents[t as usize].push((previous, p.tickets[previous].duration + travel));
            }
            from = p.tickets[t as usize].location;
        }
    }
    for &(a, b) in &p.precedences {
        if agent_of[a as usize] != NONE && agent_of[b as usize] != NONE {
            parents[b as usize].push((a as usize, p.tickets[a as usize].duration));
        }
    }
    let mut done: Vec<bool> = agent_of.iter().map(|&a| a == NONE).collect();
    loop {
        let mut progress = false;
        for t in 0..n {
            if !done[t] && parents[t].iter().all(|&(before, _)| done[before]) {
                for &(before, lag) in &parents[t] {
                    times[t] = times[t].max(times[before] + lag);
                }
                done[t] = true;
                progress = true;
            }
        }
        if !progress {
            break;
        }
    }
    let blocked: Vec<u32> = (0..n).filter(|&t| !done[t]).map(|t| t as u32).collect();
    if blocked.is_empty() {
        (times, blocked)
    } else {
        (Vec::new(), blocked)
    }
}

pub fn evaluate(problem: &Problem, solution: &Solution) -> Evaluation {
    solution
        .validate(problem)
        .expect("oracle requires a valid solution");
    let agent_of = solution.agent_of(problem.tickets.len());
    let (releases, blocked) = dependency_times(problem, solution, &agent_of);
    let mut rec = Recorder {
        problem,
        totals: [ConstraintTotal::default(); CONSTRAINT_COUNT],
        matches: Vec::new(),
    };
    let mut start = vec![0i64; problem.tickets.len()];
    let mut end = vec![0i64; problem.tickets.len()];
    let mut routes = Vec::new();
    rec.add(
        C::RefTicketAfterDepTicket,
        blocked.len() as i64,
        &[],
        &blocked,
    );

    for (a, tickets) in solution.routes.iter().enumerate() {
        let execution = &problem.execution.routes[a];
        if tickets.is_empty()
            && execution.completed_tickets == 0
            && !execution.must_return
            && execution.worked_seconds == 0
            && !execution.fixed_cost_paid
            && execution
                .initial_load
                .as_ref()
                .is_none_or(|load| load.iter().all(|&v| v == 0))
        {
            continue;
        }
        let agent = a as u32;
        let detail = schedule(problem, agent, tickets, &releases);
        for stop in &detail.stops {
            start[stop.ticket as usize] = stop.start;
            end[stop.ticket as usize] = stop.end;
        }
        let spec = &problem.agents[a];
        let ag = [agent];

        // agent 级约束。
        let mut excess = 0;
        for d in 0..problem.dims {
            let over = detail.peak_load[d] - problem.limit(agent)[d];
            if over > 0 {
                let unit = problem.dim_penalty_unit[d];
                excess += (over + unit - 1) / unit;
            }
        }
        rec.add(C::AgentCapacity, excess, &ag, tickets);
        if spec.max_tickets > 0 {
            rec.add(
                C::AgentMaxTicket,
                execution.completed_tickets as i64
                    + tickets
                        .iter()
                        .filter(|&&t| !problem.execution.tickets[t as usize].auxiliary)
                        .count() as i64
                    - spec.max_tickets as i64,
                &ag,
                tickets,
            );
        }
        rec.add(C::MinimizeTravelTime, detail.travel_time, &ag, &[]);
        rec.add(C::MinimizeTravelDistance, detail.travel_distance, &ag, &[]);
        let restricted = if spec.restricted {
            (problem.restricted_charge_per_hour as i128 * detail.travel_time as i128 / 3600) as i64
        } else {
            0
        };
        rec.add(
            C::MinimizeAgentFixedCost,
            if execution.fixed_cost_paid {
                restricted
            } else {
                spec.fixed_cost + restricted
            },
            &ag,
            &[],
        );
        let count = execution.completed_tickets as i64
            + tickets
                .iter()
                .filter(|&&t| !problem.execution.tickets[t as usize].auxiliary)
                .count() as i64;
        rec.add(C::BalanceAgentLoading, count * count, &ag, &[]);
        let mut ratio = 0.0f64;
        for d in 0..problem.dims {
            let capacity = problem.capacity(agent)[d];
            if capacity > 0 {
                let delivered: i64 = tickets
                    .iter()
                    .filter(|&&t| problem.tickets[t as usize].kind == TicketKind::Delivery)
                    .map(|&t| problem.demand(t)[d])
                    .sum();
                ratio += (delivered + execution.delivered.get(d).copied().unwrap_or(0)) as f64
                    / capacity as f64;
            }
        }
        let x = (10.0 * ratio).max(1.0);
        rec.add(
            C::BalanceAgentLoadingRatio,
            (x * x).floor() as i64,
            &ag,
            &[],
        );
        let service: i64 = tickets
            .iter()
            .map(|&t| problem.tickets[t as usize].duration)
            .sum();
        let hours = (detail.travel_time + service + execution.worked_seconds + 3599) / 3600;
        rec.add(C::BalanceAgentWorkingTime, hours * hours, &ag, &[]);
        rec.add(C::UnreachableLeg, detail.unreachable_legs, &ag, &[]);
        if let Some(shift_end) = spec.end_time {
            rec.add(
                C::AgentShiftOvertime,
                minutes_late(detail.end_time, shift_end),
                &ag,
                &[],
            );
        }

        // 站点级约束。
        let mut business_position = 0;
        for (position, stop) in detail.stops.iter().enumerate() {
            let t = stop.ticket;
            let tk = &problem.tickets[t as usize];
            let tt = [t];
            let rule = &problem.execution.tickets[t as usize];
            if !rule.auxiliary {
                rec.add(
                    C::MinimizeVisitOrderChanging,
                    rule.original_position
                        .is_some_and(|p| p != business_position) as i64,
                    &ag,
                    &tt,
                );
                business_position += 1;
                if let Some(prior) = rule.original_start {
                    rec.add(
                        C::MinimizeServiceTimeChanging,
                        ((stop.start - prior).abs() + 59) / 60,
                        &ag,
                        &tt,
                    );
                }
            }
            if let Some(load) = rule.load_before {
                rec.add(
                    C::ExecutionConsistency,
                    (!detail.stops[..position].iter().any(|s| s.ticket == load)) as i64,
                    &ag,
                    &tt,
                );
            }
            if let Some(v) = rule.committed_earliest {
                rec.add(
                    C::ExecutionConsistency,
                    minutes_late(v, stop.start),
                    &ag,
                    &tt,
                );
            }
            if let Some(v) = rule.committed_latest {
                rec.add(
                    C::ExecutionConsistency,
                    minutes_late(stop.start, v),
                    &ag,
                    &tt,
                );
            }
            for past in &problem.execution.past[t as usize] {
                rec.add(
                    if past.related {
                        C::RelationTicketsSameAgent
                    } else {
                        C::RefTicketSameAgentWithDepTicket
                    },
                    (past.same_agent && past.agent != agent) as i64,
                    &ag,
                    &tt,
                );
                if past.after_finish {
                    rec.add(
                        C::RefTicketAfterDepTicket,
                        minutes_late(past.finished_at, stop.start),
                        &ag,
                        &tt,
                    );
                }
            }
            if rule.auxiliary {
                continue;
            }
            let f = problem.static_flags(t, agent);
            rec.add(C::AgentSkills, (f & flag::SKILLS != 0) as i64, &ag, &tt);
            rec.add(
                C::AgentQualification,
                (f & flag::QUALIFICATION != 0) as i64,
                &ag,
                &tt,
            );
            rec.add(C::SameDepo, (f & flag::DEPOT != 0) as i64, &ag, &tt);
            rec.add(
                C::ServiceFinishedAfterMaxEndTime,
                minutes_late(stop.end, tk.latest_end),
                &ag,
                &tt,
            );
            rec.add(
                C::TicketStartServiceTimeMatchExpected,
                minutes_late(stop.start, tk.latest_start),
                &ag,
                &tt,
            );
            rec.add(
                C::TicketArrivalTimeSameDateWithPlanTime,
                (problem.day_of(stop.start) != tk.day) as i64,
                &ag,
                &tt,
            );
            rec.add(
                C::MinimizeTicketChanging,
                tk.original_agent.is_some_and(|o| o != agent) as i64,
                &ag,
                &tt,
            );
        }
        for d in 0..problem.dims {
            let mut load = execution.initial_load.as_ref().map_or_else(
                || {
                    tickets
                        .iter()
                        .filter(|&&t| problem.tickets[t as usize].kind == TicketKind::Delivery)
                        .map(|&t| problem.demand(t)[d])
                        .sum()
                },
                |v| v[d],
            );
            for &t in tickets {
                match problem.tickets[t as usize].kind {
                    TicketKind::Delivery => {
                        load -= problem.demand(t)[d];
                        rec.add(C::ExecutionConsistency, (-load).max(0), &ag, &[t]);
                    }
                    TicketKind::Pickup => load += problem.demand(t)[d],
                    TicketKind::Service => {}
                }
            }
        }
        routes.push(detail);
    }

    // 未派工单。
    for (t, ticket) in problem.tickets.iter().enumerate() {
        if agent_of[t] == NONE && !problem.execution.tickets[t].auxiliary {
            let tt = [t as u32];
            rec.add(C::TicketUnassigned, 1, &[], &tt);
            rec.add(
                C::MinimizeTicketChanging,
                ticket.original_agent.is_some() as i64,
                &[],
                &tt,
            );
        }
    }

    // 关联对与依赖对：两张都已派才计。
    for &(a, b) in &problem.related_pairs {
        let (x, y) = (agent_of[a as usize], agent_of[b as usize]);
        if x != NONE && y != NONE {
            rec.add(
                C::RelationTicketsSameAgent,
                (x != y) as i64,
                &[x, y],
                &[a, b],
            );
        }
    }
    for &(first, second) in &problem.precedences {
        let (x, y) = (agent_of[first as usize], agent_of[second as usize]);
        if x == NONE && y != NONE {
            rec.add(C::RefTicketAfterDepTicket, 1, &[y], &[first, second]);
        }
        if x != NONE && y != NONE {
            rec.add(
                C::RefTicketSameAgentWithDepTicket,
                (x != y) as i64,
                &[x, y],
                &[first, second],
            );
            rec.add(
                C::RefTicketAfterDepTicket,
                minutes_late(end[first as usize], start[second as usize]),
                &[x, y],
                &[first, second],
            );
        }
    }

    let score = rec.totals.iter().map(|t| t.score).sum();
    Evaluation {
        score,
        totals: rec.totals,
        matches: rec.matches,
        routes,
        agent_of,
    }
}
