//! 对任意合法方案做保守上界检查，避免 release 下的 i64 回绕。
use crate::constraint::{CONSTRAINT_COUNT, ConstraintId as C};
use crate::problem::{Problem, ProblemError, TicketKind, UNREACHABLE};

const LIMIT: i128 = i64::MAX as i128 / 4;

fn bounded(value: i128, name: &str) -> Result<i128, ProblemError> {
    if !(0..=LIMIT).contains(&value) {
        return Err(ProblemError(format!(
            "numeric bound for {name} exceeds safe i64 range; reduce values or weights"
        )));
    }
    Ok(value)
}

fn ceil(value: i128, unit: i128) -> i128 {
    (value + unit - 1) / unit
}

pub(crate) fn validate(p: &Problem) -> Result<(), ProblemError> {
    let n = bounded(p.tickets.len() as i128, "ticket count")?;
    if n == 0 && !p.execution.active {
        return Ok(());
    }
    let used = if p.execution.active {
        p.agents.len() as i128
    } else {
        n.min(p.agents.len() as i128)
    };
    let completed = p
        .execution
        .routes
        .iter()
        .map(|r| r.completed_tickets as i128)
        .sum::<i128>();
    let worked = p
        .execution
        .routes
        .iter()
        .map(|r| r.worked_seconds as i128)
        .sum::<i128>();
    let mut max_duration = 0i128;
    let mut max_distance = 0i128;
    let mut max_schedule = 0i128;
    for m in &p.matrices {
        for (k, &d) in m.duration.iter().enumerate() {
            if d != UNREACHABLE {
                let d = d as i128;
                max_duration = max_duration.max(d);
                max_distance = max_distance.max(m.distance[k] as i128);
                let buffer = m.buffer_permille.get(k).copied().unwrap_or(0) as i128;
                max_schedule = max_schedule.max(ceil(d * (1000 + buffer), 1000));
            }
        }
    }
    let travel = bounded((n + used) * max_duration, "travel time")?;
    let distance = bounded((n + used) * max_distance, "travel distance")?;
    let service = bounded(
        p.tickets.iter().map(|t| t.duration as i128).sum(),
        "service time",
    )?;
    let base = p
        .agents
        .iter()
        .map(|a| a.start_time)
        .chain(p.tickets.iter().map(|t| t.earliest_start))
        .chain(
            p.execution
                .past
                .iter()
                .flatten()
                .filter(|v| v.after_finish)
                .map(|v| v.finished_at),
        )
        .max()
        .unwrap_or(0)
        .max(0) as i128;
    let minimum = p
        .agents
        .iter()
        .map(|a| a.start_time)
        .chain(p.tickets.iter().map(|t| t.earliest_start))
        .min()
        .unwrap_or(0) as i128;
    let clock = bounded(base + service + (n + 1) * max_schedule, "route time")?;
    if p.day_starts.len() as i128 + clock / 86_400 > i32::MAX as i128 {
        return Err(ProblemError(
            "numeric bound for horizon day exceeds i32 range".into(),
        ));
    }
    let mut amounts = [0i128; CONSTRAINT_COUNT];
    for c in [
        C::AgentMaxTicket,
        C::AgentSkills,
        C::AgentQualification,
        C::TicketArrivalTimeSameDateWithPlanTime,
        C::SameDepo,
        C::MinimizeTicketChanging,
        C::TicketUnassigned,
    ] {
        amounts[c.index()] = n;
    }
    let latest_start = p.tickets.iter().map(|t| t.latest_start).min().unwrap_or(0) as i128;
    let latest_end = p.tickets.iter().map(|t| t.latest_end).min().unwrap_or(0) as i128;
    amounts[C::TicketStartServiceTimeMatchExpected.index()] =
        n * ceil((clock - latest_start).max(0), 60);
    amounts[C::ServiceFinishedAfterMaxEndTime.index()] = n * ceil((clock - latest_end).max(0), 60);
    amounts[C::RefTicketAfterDepTicket.index()] =
        p.precedences.len() as i128 * ceil((clock - minimum).max(0), 60).max(1) + n;
    amounts[C::RefTicketSameAgentWithDepTicket.index()] = p.precedences.len() as i128;
    amounts[C::RelationTicketsSameAgent.index()] = p.related_pairs.len() as i128;
    amounts[C::MinimizeTravelTime.index()] = travel;
    amounts[C::MinimizeTravelDistance.index()] = distance;
    let fixed: i128 = p.agents.iter().map(|a| a.fixed_cost as i128).sum();
    amounts[C::MinimizeAgentFixedCost.index()] =
        fixed + p.restricted_charge_per_hour as i128 * travel / 3600;
    amounts[C::AgentMaxTicket.index()] = n + completed;
    amounts[C::BalanceAgentLoading.index()] =
        bounded((n + completed) * (n + completed), "cumulative ticket count")?;
    let hours = ceil(travel + service + worked, 3600);
    amounts[C::BalanceAgentWorkingTime.index()] = hours * hours;
    amounts[C::UnreachableLeg.index()] = n + used;
    if let Some(end) = p.agents.iter().filter_map(|a| a.end_time).min() {
        amounts[C::AgentShiftOvertime.index()] = used * ceil((clock - end as i128).max(0), 60);
    }
    let mut delivered = vec![0i128; p.dims];
    for (d, delivered_dim) in delivered.iter_mut().enumerate() {
        let unit = bounded(p.dim_penalty_unit[d] as i128, "capacity penalty unit")?;
        let mut all = p
            .execution
            .routes
            .iter()
            .filter_map(|r| r.initial_load.as_ref())
            .map(|v| v[d] as i128)
            .sum::<i128>();
        *delivered_dim = p
            .execution
            .routes
            .iter()
            .map(|r| r.delivered.get(d).copied().unwrap_or(0) as i128)
            .sum();
        for (t, ticket) in p.tickets.iter().enumerate() {
            if ticket.kind != TicketKind::Service {
                all = bounded(all + p.demand(t as u32)[d] as i128, "load")?;
            }
            if ticket.kind == TicketKind::Delivery {
                *delivered_dim += p.demand(t as u32)[d] as i128;
            }
        }
        amounts[C::AgentCapacity.index()] = bounded(
            amounts[C::AgentCapacity.index()] + ceil(all, unit) + used,
            "capacity amount",
        )?;
        amounts[C::ExecutionConsistency.index()] += n * all;
    }
    if p.execution.active {
        let past = p
            .execution
            .past
            .iter()
            .map(|v| v.len() as i128)
            .sum::<i128>();
        let execution_time = p
            .execution
            .tickets
            .iter()
            .flat_map(|r| [r.original_start, r.committed_earliest, r.committed_latest])
            .flatten()
            .chain(p.execution.past.iter().flatten().map(|v| v.finished_at))
            .map(|v| (v as i128).abs())
            .max()
            .unwrap_or(0);
        amounts[C::ExecutionConsistency.index()] +=
            n + past + (2 * n + past) * ceil(clock + execution_time + minimum.abs(), 60);
        amounts[C::MinimizeVisitOrderChanging.index()] = n;
        amounts[C::MinimizeServiceTimeChanging.index()] =
            n * ceil(clock + execution_time + minimum.abs(), 60);
        amounts[C::RefTicketAfterDepTicket.index()] +=
            past * ceil(clock + execution_time + minimum.abs(), 60);
        amounts[C::RefTicketSameAgentWithDepTicket.index()] += past;
        amounts[C::RelationTicketsSameAgent.index()] += past;
    } else {
        amounts[C::ExecutionConsistency.index()] = 0;
    }
    // 用向上取整后的整数比率包住浮点比率，逐条路线的上界再求和。
    for a in 0..p.agents.len() {
        let mut ratio = 0;
        for (d, &q) in delivered.iter().enumerate() {
            let capacity = p.capacity(a as u32)[d] as i128;
            if capacity > 0 {
                ratio = bounded(ratio + ceil(q, capacity), "loading ratio")?;
            }
        }
        let x = bounded((10 * ratio).max(1), "loading ratio")?;
        let term = bounded(x * x, "loading ratio square")?;
        amounts[C::BalanceAgentLoadingRatio.index()] = bounded(
            amounts[C::BalanceAgentLoadingRatio.index()] + term,
            "loading ratio total",
        )?;
    }
    let mut totals = [0i128; 3];
    for c in C::ALL {
        let amount = bounded(amounts[c.index()], c.name())?;
        let w = p.weights.get(c);
        for (total, weight) in totals.iter_mut().zip([w.hard, w.medium, w.soft]) {
            *total = bounded(
                *total + amount * weight as i128,
                &format!("score ({})", c.name()),
            )?;
        }
    }
    Ok(())
}
