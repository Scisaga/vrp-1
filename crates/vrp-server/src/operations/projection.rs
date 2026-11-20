use super::*;
use vrp_core::constraint::ConstraintId as C;

fn rule(context: &mut e::ExecutionContext, t: u32) -> &mut e::ExecutionTicket {
    if let Some(i) = context.tickets.iter().position(|v| v.ticket == t) {
        return &mut context.tickets[i];
    }
    context.tickets.push(e::ExecutionTicket {
        ticket: t,
        ..Default::default()
    });
    context.tickets.last_mut().unwrap()
}
fn add_amount(to: &mut [i64], from: &[i64], sign: i64) -> Result<()> {
    for (a, b) in to.iter_mut().zip(from) {
        *a = a
            .checked_add(
                b.checked_mul(sign)
                    .ok_or_else(|| AppError::invalid("cargo overflow"))?,
            )
            .ok_or_else(|| AppError::invalid("cargo overflow"))?;
    }
    if to.iter().any(|v| *v < 0 || *v > 1_000_000_000_000_000) {
        return Err(AppError::conflict(
            "physical cargo quantities are inconsistent",
        ));
    }
    Ok(())
}
fn append_location(p: &mut e::Problem, mut loc: e::Location, agent: &str) -> u32 {
    if let Some(i) = p
        .locations
        .iter()
        .position(|v| v.id == loc.id && v.lng == loc.lng && v.lat == loc.lat)
    {
        return i as u32;
    }
    loc.id = format!("@vrp/position/{agent}");
    p.locations.push(loc);
    (p.locations.len() - 1) as u32
}
fn expand_matrices(p: &mut e::Problem, old: usize) {
    let n = p.locations.len();
    if n == old {
        return;
    }
    for m in &mut p.matrices {
        for bytes in [&mut m.durations, &mut m.distances] {
            let mut expanded = Vec::with_capacity(n * n * 4);
            for i in 0..n {
                for j in 0..n {
                    if i < old && j < old {
                        expanded
                            .extend_from_slice(&bytes[(i * old + j) * 4..(i * old + j + 1) * 4]);
                    } else {
                        expanded.extend_from_slice(
                            &(if i == j { 0u32 } else { u32::MAX - 1 }).to_le_bytes(),
                        );
                    }
                }
            }
            *bytes = expanded;
        }
        let mut source = vec![e::EdgeSource::Fallback as u8; n * n];
        for i in 0..n {
            source[i * n + i] = e::EdgeSource::Exact as u8;
        }
        for i in 0..old {
            for j in 0..old {
                source[i * n + j] = m.sources.get(i * old + j).copied().unwrap_or(0);
            }
        }
        m.sources = source;
    }
}

pub(crate) fn project(
    input: &e::SolveRequest,
    op: &a::Operation,
    now: i64,
    options: Option<e::SolveOptions>,
) -> Result<e::SolveRequest> {
    let source = problem(input)?;
    let mut p = source.clone();
    let old_locations = p.locations.len();
    let phases: HashMap<_, _> = op
        .tickets
        .iter()
        .map(|t| (t.ticket_id.as_str(), t))
        .collect();
    let mut mapping = vec![None; source.tickets.len()];
    p.tickets.clear();
    p.related_pairs.clear();
    p.precedences.clear();
    let mut context = e::ExecutionContext::default();
    for (i, t) in source.tickets.iter().enumerate() {
        let status = phases
            .get(t.id.as_str())
            .ok_or_else(|| AppError::conflict("business ticket has no execution state"))?;
        if matches!(
            status.phase(),
            a::TicketPhase::Serving | a::TicketPhase::Completed | a::TicketPhase::Cancelled
        ) {
            continue;
        }
        let new = p.tickets.len() as u32;
        mapping[i] = Some(new);
        let mut t = t.clone();
        if let Some(remaining) = status.remaining_service_seconds {
            t.duration = remaining;
        }
        if status.phase() == a::TicketPhase::Exception {
            rule(&mut context, new).blocked = true;
            t.pinned = false;
        }
        p.tickets.push(t);
    }
    let agent_index: HashMap<String, u32> = p
        .agents
        .iter()
        .enumerate()
        .map(|(i, v)| (v.id.clone(), i as u32))
        .collect();
    let ticket_index: HashMap<String, u32> = p
        .tickets
        .iter()
        .enumerate()
        .map(|(i, v)| (v.id.clone(), i as u32))
        .collect();
    for i in 0..p.agents.len() {
        let id = p.agents[i].id.clone();
        let actual = op
            .vehicles
            .iter()
            .find(|v| v.agent_id == id)
            .ok_or_else(|| AppError::conflict("vehicle has no execution state"))?;
        let mut load = vec![0; p.capacity_dimensions.len()];
        for cargo in &op.cargo {
            if cargo.agent_id == id && !cargo.delivered {
                add_amount(&mut load, &cargo.quantities, 1)?;
            }
        }
        let mut delivered = actual.delivered.clone();
        delivered.resize(p.capacity_dimensions.len(), 0);
        let mut count = actual.completed_tickets;
        let mut work = actual.travel_seconds + actual.service_seconds;
        let mut available = actual.available_at.max(now);
        let mut position = actual
            .position
            .clone()
            .ok_or_else(|| AppError::conflict("vehicle position is missing"))?;
        for busy in op
            .tickets
            .iter()
            .filter(|t| t.agent_id == id && t.phase() == a::TicketPhase::Serving)
        {
            let t = source
                .tickets
                .iter()
                .find(|t| t.id == busy.ticket_id)
                .ok_or_else(|| AppError::conflict("serving ticket disappeared"))?;
            let remaining = (busy.remaining_service_seconds.unwrap_or(t.duration)
                - (now - busy.observed_at).max(0))
            .max(0);
            available = available.max(now + remaining);
            work = work
                .checked_add((now - busy.service_started_at.unwrap_or(now)).max(0) + remaining)
                .ok_or_else(|| AppError::invalid("work time overflow"))?;
            count += 1;
            position = source.locations[t.location as usize].clone();
            if t.r#type == e::TicketType::Delivery as i32 {
                add_amount(&mut load, &t.demand, -1)?;
                add_amount(&mut delivered, &t.demand, 1)?;
            }
            if t.r#type == e::TicketType::Pickup as i32 {
                add_amount(&mut load, &t.demand, 1)?;
            }
        }
        let start = append_location(&mut p, position, &id);
        p.agents[i].start_location = start;
        p.agents[i].start_time = available;
        context.routes.push(e::ExecutedRoute {
            agent: i as u32,
            initial_load: Some(e::LoadVector { values: load }),
            completed_tickets: count,
            worked_seconds: work,
            delivered,
            fixed_cost_paid: actual.fixed_cost_paid,
            unavailable: matches!(
                actual.phase(),
                a::VehiclePhase::Unavailable | a::VehiclePhase::Paused
            ),
            must_return: actual.fixed_cost_paid,
            locked_prefix: Vec::new(),
        });
    }
    // Planned loading and unloading are actual visits, never phantom inventory.
    for cargo in &op.cargo {
        if cargo.delivered {
            continue;
        }
        if let Some(&t) = ticket_index.get(&cargo.ticket_id) {
            if p.tickets[t as usize].r#type != e::TicketType::Delivery as i32 {
                continue;
            }
            if let Some(&owner) = agent_index.get(&cargo.agent_id) {
                rule(&mut context, t).only_agent = Some(owner);
            } else if cargo.agent_id.is_empty() {
                let loc = p
                    .locations
                    .iter()
                    .position(|l| l.id == cargo.location_id)
                    .ok_or_else(|| AppError::conflict("cargo origin disappeared"))?;
                let helper = p.tickets.len() as u32;
                let customer = p.tickets[t as usize].clone();
                p.tickets.push(e::Ticket {
                    id: format!("@vrp/load/{}", cargo.ticket_id),
                    location: loc as u32,
                    r#type: e::TicketType::Pickup as i32,
                    demand: cargo.quantities.clone(),
                    earliest_start: now,
                    latest_start: customer.latest_start,
                    latest_end: customer.latest_end,
                    day: customer.day,
                    ..Default::default()
                });
                rule(&mut context, helper).auxiliary = true;
                rule(&mut context, t).load_before = Some(helper);
            } else {
                return Err(AppError::conflict("cargo belongs to an unknown vehicle"));
            }
        } else if !cargo.agent_id.is_empty() {
            let owner = *agent_index
                .get(&cargo.agent_id)
                .ok_or_else(|| AppError::conflict("cargo vehicle disappeared"))?;
            if phases
                .get(cargo.ticket_id.as_str())
                .is_some_and(|v| v.phase() == a::TicketPhase::Serving)
            {
                continue;
            }
            let agent = &p.agents[owner as usize];
            let helper = p.tickets.len() as u32;
            p.tickets.push(e::Ticket {
                id: format!("@vrp/unload/{}", cargo.ticket_id),
                location: agent.end_location,
                r#type: e::TicketType::Delivery as i32,
                demand: cargo.quantities.clone(),
                earliest_start: now,
                latest_start: 1_000_000_000_000,
                latest_end: 1_000_000_000_000,
                day: agent.day,
                original_agent: Some(owner),
                pinned: !context.routes[owner as usize].unavailable,
                ..Default::default()
            });
            let blocked = context.routes[owner as usize].unavailable;
            let r = rule(&mut context, helper);
            r.auxiliary = true;
            r.only_agent = Some(owner);
            r.blocked = blocked;
        }
    }
    for lock in &op.route_commitments {
        let owner = agent_index[&lock.agent_id];
        for t in &lock.prefix {
            if let Some(&t) = ticket_index.get(t) {
                if let Some(load) = rule(&mut context, t).load_before {
                    p.tickets[load as usize].pinned = true;
                    p.tickets[load as usize].original_agent = Some(owner);
                    rule(&mut context, load).only_agent = Some(owner);
                    context.routes[owner as usize].locked_prefix.push(load);
                }
                p.tickets[t as usize].pinned = true;
                p.tickets[t as usize].original_agent = Some(owner);
                context.routes[owner as usize].locked_prefix.push(t);
            }
        }
    }
    for lock in &op.time_commitments {
        if let Some(&t) = ticket_index.get(&lock.ticket_id) {
            let owner = agent_index[&lock.agent_id];
            p.tickets[t as usize].pinned = true;
            p.tickets[t as usize].original_agent = Some(owner);
            let r = rule(&mut context, t);
            r.only_agent = Some(owner);
            r.committed_earliest = lock.earliest;
            r.committed_latest = lock.latest;
        }
    }
    for pair in &source.related_pairs {
        if let (Some(first), Some(second)) =
            (mapping[pair.first as usize], mapping[pair.second as usize])
        {
            p.related_pairs.push(e::TicketPair { first, second });
        }
        for (active, past) in [(pair.first, pair.second), (pair.second, pair.first)] {
            if let Some(ticket) = mapping[active as usize] {
                let status = phases[source.tickets[past as usize].id.as_str()];
                if matches!(
                    status.phase(),
                    a::TicketPhase::Completed | a::TicketPhase::Serving
                ) {
                    context.past_dependencies.push(e::PastDependency {
                        ticket,
                        agent: agent_index[&status.agent_id],
                        same_agent: true,
                        related: true,
                        ..Default::default()
                    });
                }
            }
        }
    }
    let weights = vrp_model::weights_from_proto(source.weights.as_ref());
    for pair in &source.precedences {
        match (mapping[pair.first as usize], mapping[pair.second as usize]) {
            (Some(first), Some(second)) => p.precedences.push(e::TicketPair { first, second }),
            (None, Some(ticket)) => {
                let first = &source.tickets[pair.first as usize];
                let status = phases[first.id.as_str()];
                if status.phase() == a::TicketPhase::Completed {
                    context.past_dependencies.push(e::PastDependency {
                        ticket,
                        agent: agent_index[&status.agent_id],
                        finished_at: status.finished_at.unwrap(),
                        same_agent: !weights.get(C::RefTicketSameAgentWithDepTicket).is_zero(),
                        after_finish: !weights.get(C::RefTicketAfterDepTicket).is_zero(),
                        related: false,
                    });
                } else if status.phase() == a::TicketPhase::Serving {
                    context.past_dependencies.push(e::PastDependency {
                        ticket,
                        agent: agent_index[&status.agent_id],
                        finished_at: now
                            + (status.remaining_service_seconds.unwrap_or(first.duration)
                                - (now - status.observed_at).max(0))
                            .max(0),
                        same_agent: !weights.get(C::RefTicketSameAgentWithDepTicket).is_zero(),
                        after_finish: !weights.get(C::RefTicketAfterDepTicket).is_zero(),
                        related: false,
                    });
                } else if !weights.get(C::RefTicketAfterDepTicket).is_zero() {
                    rule(&mut context, ticket).blocked = true;
                }
            }
            _ => {}
        }
    }
    let mut routes = vec![Vec::new(); p.agents.len()];
    if let Some(plan) = &op.plan {
        for route in &plan.routes {
            if let Some(&a) = agent_index.get(&route.agent_id) {
                let mut pos = 0;
                for stop in &route.stops {
                    if let Some(&t) = ticket_index.get(&stop.ticket_id) {
                        routes[a as usize].push(t);
                        let r = rule(&mut context, t);
                        r.original_position = Some(pos);
                        r.original_start = Some(stop.start);
                        if !p.tickets[t as usize].pinned {
                            p.tickets[t as usize].original_agent = Some(a);
                        }
                        pos += 1;
                    }
                }
            }
        }
    } else if let Some(initial) = &input.initial_solution {
        for route in &initial.routes {
            for &t in &route.tickets {
                if let Some(t) = mapping[t as usize] {
                    routes[route.agent as usize].push(t);
                }
            }
        }
    }
    // Seed all eligible customer work and its material collection. The solver may
    // subsequently unassign infeasible work; auxiliary visits carry no unmet-order penalty.
    let mut used = HashSet::new();
    let rules: HashMap<u32, e::ExecutionTicket> =
        context.tickets.iter().map(|v| (v.ticket, *v)).collect();
    for (a, route) in routes.iter_mut().enumerate() {
        route.retain(|t| {
            !context.routes[a].unavailable
                && rules
                    .get(t)
                    .is_none_or(|r| !r.blocked && r.only_agent.is_none_or(|o| o == a as u32))
                && (!p.tickets[*t as usize].pinned
                    || p.tickets[*t as usize].original_agent == Some(a as u32))
                && used.insert(*t)
        });
    }
    for t in 0..p.tickets.len() as u32 {
        if used.contains(&t)
            || rules
                .get(&t)
                .is_some_and(|r| r.blocked || r.auxiliary && !p.tickets[t as usize].pinned)
        {
            continue;
        }
        let required = if p.tickets[t as usize].pinned {
            p.tickets[t as usize].original_agent
        } else {
            rules.get(&t).and_then(|r| r.only_agent)
        };
        let target = (0..p.agents.len())
            .filter(|&a| !context.routes[a].unavailable && required.is_none_or(|r| r as usize == a))
            .min_by_key(|&a| {
                (
                    p.tickets[t as usize]
                        .skills_required
                        .iter()
                        .filter(|s| !p.agents[a].skills.contains(s))
                        .count(),
                    routes[a].len(),
                )
            });
        if let Some(a) = target {
            routes[a].push(t);
            used.insert(t);
        }
    }
    for (a, route) in routes.iter_mut().enumerate() {
        let prefix = &context.routes[a].locked_prefix;
        route.retain(|t| !prefix.contains(t));
        let mut ordered = prefix.clone();
        ordered.append(route);
        let mut material = Vec::new();
        for t in ordered {
            if let Some(load) = rules.get(&t).and_then(|r| r.load_before)
                && !material.contains(&load)
            {
                material.push(load);
            }
            if !material.contains(&t) {
                material.push(t);
            }
        }
        *route = material;
    }
    expand_matrices(&mut p, old_locations);
    p.execution = Some(context);
    let result = e::SolveRequest {
        problem: Some(p),
        options: options.or_else(|| op.policy.as_ref().and_then(|p| p.options)),
        initial_solution: Some(e::Solution {
            routes: routes
                .into_iter()
                .enumerate()
                .filter(|(_, r)| !r.is_empty())
                .map(|(a, tickets)| e::Route {
                    agent: a as u32,
                    tickets,
                })
                .collect(),
        }),
    };
    vrp_model::compile(&result).map_err(|v| AppError::conflict(v.0))?;
    Ok(result)
}

pub(crate) fn remap_solution(
    from: &e::SolveRequest,
    solution: &e::Solution,
    into: &e::SolveRequest,
) -> Result<vrp_core::Solution> {
    let old = problem(from)?;
    let new = problem(into)?;
    let agents: HashMap<_, _> = new
        .agents
        .iter()
        .enumerate()
        .map(|(i, a)| (a.id.as_str(), i))
        .collect();
    let tickets: HashMap<_, _> = new
        .tickets
        .iter()
        .enumerate()
        .map(|(i, t)| (t.id.as_str(), i as u32))
        .collect();
    let mut result = vrp_core::Solution::empty(new.agents.len());
    for route in &solution.routes {
        let agent = old
            .agents
            .get(route.agent as usize)
            .ok_or_else(|| AppError::conflict("result has unknown agent"))?;
        if let Some(&a) = agents.get(agent.id.as_str()) {
            for &t in &route.tickets {
                let t = old
                    .tickets
                    .get(t as usize)
                    .ok_or_else(|| AppError::conflict("result has unknown ticket"))?;
                if let Some(&t) = tickets.get(t.id.as_str()) {
                    result.routes[a].push(t);
                }
            }
        }
    }
    Ok(result)
}
pub(crate) fn published(
    input: &e::SolveRequest,
    evaluation: &vrp_core::oracle::Evaluation,
    op: &a::Operation,
    job: &str,
    actor: &str,
    wall: i64,
) -> Result<a::PublishedPlan> {
    let p = problem(input)?;
    let old: HashMap<_, _> = op
        .plan
        .iter()
        .flat_map(|p| &p.routes)
        .flat_map(|r| {
            r.stops
                .iter()
                .enumerate()
                .map(move |(i, s)| (s.ticket_id.as_str(), (r.agent_id.as_str(), i, s.start)))
        })
        .collect();
    let mut changed = Vec::new();
    let routes = evaluation
        .routes
        .iter()
        .map(|r| {
            let agent_id = p.agents[r.agent as usize].id.clone();
            let stops = r
                .stops
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let t = &p.tickets[s.ticket as usize];
                    let (action, cargo) = if let Some(id) = t.id.strip_prefix("@vrp/load/") {
                        ("load", id)
                    } else if let Some(id) = t.id.strip_prefix("@vrp/unload/") {
                        ("unload", id)
                    } else {
                        ("service", "")
                    };
                    if old.get(t.id.as_str()).copied() != Some((agent_id.as_str(), i, s.start)) {
                        changed.push(t.id.clone());
                    }
                    a::PlanStop {
                        ticket_id: t.id.clone(),
                        arrival: s.arrival,
                        start: s.start,
                        end: s.end,
                        action: action.into(),
                        cargo_ticket_id: cargo.into(),
                        location_id: p.locations[t.location as usize].id.clone(),
                    }
                })
                .collect();
            a::PlanRoute {
                agent_id,
                stops,
                start_time: Some(p.agents[r.agent as usize].start_time),
                end_time: Some(r.end_time),
                travel_time: Some(r.travel_time),
                travel_distance: Some(r.travel_distance),
            }
        })
        .collect::<Vec<_>>();
    for id in old.keys() {
        if !routes
            .iter()
            .any(|r| r.stops.iter().any(|s| s.ticket_id == *id))
        {
            changed.push((*id).into());
        }
    }
    changed.sort();
    changed.dedup();
    Ok(a::PublishedPlan {
        version: op.plan.as_ref().map_or(1, |p| p.version + 1),
        job_id: job.into(),
        execution_version: op.execution_version,
        routes,
        score: Some(e::Score {
            hard: evaluation.score.hard,
            medium: evaluation.score.medium,
            soft: evaluation.score.soft,
        }),
        adopted_unix_ms: wall,
        actor: actor.into(),
        changed_tickets: changed,
    })
}
