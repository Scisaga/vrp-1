//! Business execution state and deterministic projection. Persistence is in Store.
mod events;
mod projection;
use crate::{AppError, Result};
pub(crate) use events::apply_event;
pub(crate) use projection::{project, published, remap_solution};
use std::collections::{HashMap, HashSet};
use vrp_proto::{app::v1 as a, engine::v1 as e};

pub(crate) fn problem(request: &e::SolveRequest) -> Result<&e::Problem> {
    request
        .problem
        .as_ref()
        .ok_or_else(|| AppError::invalid("problem is required"))
}
pub(crate) fn validate_business(request: &e::SolveRequest) -> Result<()> {
    let p = problem(request)?;
    if p.execution.is_some() {
        return Err(AppError::invalid(
            "operation business input must not contain a derived execution context",
        ));
    }
    let mut resources = HashSet::new();
    for agent in &p.agents {
        if agent.resource_id.is_empty() || !resources.insert(&agent.resource_id) {
            return Err(AppError::invalid(
                "live vehicles require distinct nonempty resource_id; use one active agent per physical resource",
            ));
        }
    }
    for ids in [
        p.agents.iter().map(|v| &v.id).collect::<Vec<_>>(),
        p.tickets.iter().map(|v| &v.id).collect(),
        p.locations.iter().map(|v| &v.id).collect(),
    ] {
        let mut seen = HashSet::new();
        for id in ids {
            if id.is_empty() || id.len() > 256 || id.starts_with("@vrp/") || !seen.insert(id) {
                return Err(AppError::invalid(
                    "live entities need unique nonempty stable IDs (reserved prefix @vrp/)",
                ));
            }
        }
    }
    vrp_model::compile(request).map_err(|v| AppError::invalid(v.0))?;
    Ok(())
}
pub(crate) fn policy(
    value: Option<a::OperationPolicy>,
    default: Option<&e::SolveOptions>,
) -> Result<a::OperationPolicy> {
    let mut p = value.unwrap_or_else(|| a::OperationPolicy {
        auto_replan: true,
        ..Default::default()
    });
    if a::ExecutionClock::try_from(p.clock).is_err() || p.debounce_ms > 5000 {
        return Err(AppError::invalid(
            "invalid clock or debounce_ms (maximum 5000)",
        ));
    }
    if p.debounce_ms == 0 {
        p.debounce_ms = 250;
    }
    if p.options.is_none() {
        p.options = default.cloned();
    }
    vrp_model::resolve_options(p.options.as_ref()).map_err(|v| AppError::invalid(v.0))?;
    Ok(p)
}
pub(crate) fn at(op: &a::Operation, wall: i64) -> i64 {
    if op
        .policy
        .as_ref()
        .is_some_and(|v| v.clock() == a::ExecutionClock::Live)
    {
        op.at
            .saturating_add(wall.saturating_sub(op.clock_unix_ms).max(0) / 1000)
    } else {
        op.at
    }
}
pub(crate) fn mark_dirty(op: &mut a::Operation, wall: i64, significant: bool) {
    if significant {
        op.execution_version += 1;
    }
    op.plan_stale = op.plan.is_some();
    op.planning_error = None;
    if op.policy.as_ref().is_some_and(|v| v.auto_replan) {
        if !op.needs_replan {
            op.replan_due_unix_ms = wall + i64::from(op.policy.as_ref().unwrap().debounce_ms);
        }
        op.needs_replan = true;
    } else {
        op.needs_replan = false;
    }
}
pub(crate) fn require_revision(op: &a::Operation, revision: u64) -> Result<()> {
    if op.closed {
        return Err(AppError::conflict("operation is closed"));
    }
    if op.revision != revision {
        return Err(AppError::conflict(format!(
            "current operation revision is {}",
            op.revision
        )));
    }
    if op.revision >= i64::MAX as u64 - 1 || op.execution_version >= i64::MAX as u64 - 1 {
        return Err(AppError::conflict("operation version exhausted"));
    }
    Ok(())
}
pub(crate) fn initial(
    reference: a::ScenarioRef,
    input: &e::SolveRequest,
    start: i64,
    value: Option<a::OperationPolicy>,
    wall: i64,
) -> Result<a::Operation> {
    validate_business(input)?;
    if !(0..=1_000_000_000_000).contains(&start) {
        return Err(AppError::invalid("at is outside horizon-second range"));
    }
    let mut op = a::Operation {
        scenario: Some(reference),
        revision: 1,
        execution_version: 1,
        at: start,
        clock_unix_ms: wall,
        policy: Some(policy(value, input.options.as_ref())?),
        ..Default::default()
    };
    synchronize(&mut op, None, input)?;
    mark_dirty(&mut op, wall, false);
    Ok(op)
}
pub(crate) fn synchronize(
    op: &mut a::Operation,
    previous: Option<&e::SolveRequest>,
    next: &e::SolveRequest,
) -> Result<()> {
    validate_business(next)?;
    let p = problem(next)?;
    if let Some(old) = previous {
        let old = problem(old)?;
        if old.capacity_dimensions != p.capacity_dimensions || old.horizon != p.horizon {
            return Err(AppError::conflict(
                "execution horizon and quantity units cannot change",
            ));
        }
        for before in &old.agents {
            if let Some(after) = p.agents.iter().find(|a| a.id == before.id)
                && (before.resource_id != after.resource_id || before.day != after.day)
            {
                return Err(AppError::conflict(
                    "execution vehicle resource identity and day cannot change",
                ));
            }
        }
        for ticket in &op.tickets {
            let before = old.tickets.iter().find(|v| v.id == ticket.ticket_id);
            let after = p.tickets.iter().find(|v| v.id == ticket.ticket_id);
            if after.is_none()
                && !matches!(
                    ticket.phase(),
                    a::TicketPhase::Completed | a::TicketPhase::Cancelled
                )
            {
                return Err(AppError::conflict(
                    "cancel an unfinished ticket through an execution event before removing it",
                ));
            }
            if matches!(
                ticket.phase(),
                a::TicketPhase::Serving | a::TicketPhase::Completed
            ) && after.is_some()
                && !before
                    .zip(after)
                    .is_some_and(|(b, a)| same_ticket_fact(old, p, b, a))
            {
                return Err(AppError::conflict(
                    "serving/completed ticket facts cannot be rewritten",
                ));
            }
            if let (Some(before), Some(after)) = (before, after)
                && (before.demand != after.demand || before.r#type != after.r#type)
            {
                return Err(AppError::conflict(
                    "cargo quantity/type of an existing execution ticket cannot change",
                ));
            }
        }
    }
    let now = op.at;
    for agent in &p.agents {
        if !op.vehicles.iter().any(|v| v.agent_id == agent.id) {
            op.vehicles.push(a::VehicleExecution {
                agent_id: agent.id.clone(),
                position: Some(p.locations[agent.start_location as usize].clone()),
                observed_at: now,
                available_at: agent.start_time.max(now),
                delivered: vec![0; p.capacity_dimensions.len()],
                ..Default::default()
            });
        }
    }
    if op
        .vehicles
        .iter()
        .any(|v| !p.agents.iter().any(|a| a.id == v.agent_id))
    {
        return Err(AppError::conflict(
            "retain execution vehicles in the scenario; mark unavailable instead of deleting",
        ));
    }
    for ticket in &p.tickets {
        if op.tickets.iter().any(|v| v.ticket_id == ticket.id) {
            continue;
        }
        op.tickets.push(a::TicketExecution {
            ticket_id: ticket.id.clone(),
            observed_at: now,
            ..Default::default()
        });
        if ticket.r#type != e::TicketType::Service as i32 {
            let origin = if ticket.r#type == e::TicketType::Delivery as i32 {
                let configured = &op.policy.as_ref().unwrap().loading_location_id;
                if !configured.is_empty() {
                    configured.clone()
                } else if p.locations.iter().any(|l| l.id == ticket.depot_id) {
                    ticket.depot_id.clone()
                } else {
                    let starts: HashSet<_> = p.agents.iter().map(|v| v.start_location).collect();
                    if starts.len() != 1 {
                        return Err(AppError::invalid(
                            "delivery cargo needs an explicit loading_location_id when depots differ",
                        ));
                    }
                    p.locations[*starts.iter().next().unwrap() as usize]
                        .id
                        .clone()
                }
            } else {
                p.locations[ticket.location as usize].id.clone()
            };
            if !p.locations.iter().any(|v| v.id == origin) {
                return Err(AppError::invalid(
                    "cargo loading location is not in the scenario",
                ));
            }
            let mut quantities = ticket.demand.clone();
            quantities.resize(p.capacity_dimensions.len(), 0);
            op.cargo.push(a::Cargo {
                ticket_id: ticket.id.clone(),
                location_id: origin,
                quantities,
                ..Default::default()
            });
        }
    }
    validate_commitments(op, p)?;
    if op.cargo.iter().any(|c| {
        !c.delivered && c.agent_id.is_empty() && !p.locations.iter().any(|l| l.id == c.location_id)
    }) {
        return Err(AppError::conflict("retain locations that hold cargo"));
    }
    Ok(())
}

// Compare stable identities and coordinates, not array offsets that may change
// when a business version reorders entities.
fn same_ticket_fact(
    old: &e::Problem,
    new: &e::Problem,
    before: &e::Ticket,
    after: &e::Ticket,
) -> bool {
    let mut left = before.clone();
    let mut right = after.clone();
    left.location = 0;
    right.location = 0;
    left.original_agent = None;
    right.original_agent = None;
    left == right
        && predecessor_ids(old, &before.id) == predecessor_ids(new, &after.id)
        && old.locations[before.location as usize] == new.locations[after.location as usize]
        && before.original_agent.map(|a| &old.agents[a as usize].id)
            == after.original_agent.map(|a| &new.agents[a as usize].id)
}
pub(crate) fn validate_commitments(op: &a::Operation, p: &e::Problem) -> Result<()> {
    let agents: HashSet<_> = p.agents.iter().map(|v| v.id.as_str()).collect();
    let phases: HashMap<_, _> = op
        .tickets
        .iter()
        .map(|t| (t.ticket_id.as_str(), t.phase()))
        .collect();
    let mut routes = HashSet::new();
    let mut tickets = HashSet::new();
    for lock in &op.route_commitments {
        if !agents.contains(lock.agent_id.as_str()) || !routes.insert(&lock.agent_id) {
            return Err(AppError::invalid("unknown or duplicate commitment agent"));
        }
        for t in &lock.prefix {
            if !phases.contains_key(t.as_str())
                || !tickets.insert(t)
                || phases[t.as_str()] == a::TicketPhase::Cancelled
            {
                return Err(AppError::invalid(
                    "unknown, cancelled or duplicate prefix ticket",
                ));
            }
        }
    }
    tickets.clear();
    for lock in &op.time_commitments {
        if !agents.contains(lock.agent_id.as_str())
            || !phases.contains_key(lock.ticket_id.as_str())
            || !tickets.insert(&lock.ticket_id)
            || lock.earliest.zip(lock.latest).is_some_and(|(a, b)| a > b)
            || [lock.earliest, lock.latest]
                .into_iter()
                .flatten()
                .any(|v| !(0..=1_000_000_000_000).contains(&v))
        {
            return Err(AppError::invalid("invalid time commitment"));
        }
        if let Some(prefix_owner) = op
            .route_commitments
            .iter()
            .find(|v| v.prefix.contains(&lock.ticket_id))
            && prefix_owner.agent_id != lock.agent_id
        {
            return Err(AppError::conflict(
                "time and prefix commitments disagree on agent",
            ));
        }
    }
    Ok(())
}

fn predecessor_ids<'a>(p: &'a e::Problem, ticket: &str) -> Vec<&'a str> {
    let mut ids: Vec<_> = p
        .precedences
        .iter()
        .filter(|pair| p.tickets[pair.second as usize].id == ticket)
        .map(|pair| p.tickets[pair.first as usize].id.as_str())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}
