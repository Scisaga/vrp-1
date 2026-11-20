use super::*;

pub(crate) fn apply_event(
    op: &mut a::Operation,
    p: &e::Problem,
    event: &a::ExecutionEvent,
    wall: i64,
) -> Result<()> {
    if !(0..=1_000_000_000_000).contains(&event.at)
        || event.actor.len() > 256
        || event.vehicles.len() > 200
        || event.tickets.len() > 1000
        || event.cargo.len() > 1000
    {
        return Err(AppError::invalid("execution event exceeds bounds"));
    }
    if op.policy.as_ref().unwrap().clock() == a::ExecutionClock::Live
        && event.at > at(op, wall) + 30
    {
        return Err(AppError::conflict(
            "execution event is ahead of the live clock",
        ));
    }
    let mut seen = HashSet::new();
    for update in &event.vehicles {
        if !seen.insert(&update.agent_id) {
            return Err(AppError::invalid("duplicate vehicle update"));
        }
        let vehicle = op
            .vehicles
            .iter_mut()
            .find(|v| v.agent_id == update.agent_id)
            .ok_or_else(|| AppError::invalid("unknown vehicle"))?;
        if event.at < vehicle.observed_at {
            return Err(AppError::conflict(
                "vehicle event is older than observed state",
            ));
        }
        if let Some(phase) = update.phase {
            a::VehiclePhase::try_from(phase)
                .map_err(|_| AppError::invalid("unknown vehicle phase"))?;
            vehicle.phase = phase;
        }
        if let Some(position) = &update.position {
            if !position.lat.is_finite()
                || !position.lng.is_finite()
                || position.lat.abs() > 90.0
                || position.lng.abs() > 180.0
            {
                return Err(AppError::invalid("invalid vehicle position"));
            }
            vehicle.position = Some(position.clone());
        }
        if let Some(time) = update.available_at {
            if time < event.at || time > 1_000_000_000_000 {
                return Err(AppError::invalid(
                    "available_at must be at or after event time",
                ));
            }
            vehicle.available_at = time;
        }
        if let Some(time) = update.travel_seconds {
            if time < vehicle.travel_seconds || time > 1_000_000_000_000 {
                return Err(AppError::invalid(
                    "cumulative travel_seconds cannot decrease",
                ));
            }
            vehicle.travel_seconds = time;
        }
        vehicle.observed_at = event.at;
        if vehicle.phase() == a::VehiclePhase::Travelling || vehicle.travel_seconds > 0 {
            vehicle.fixed_cost_paid = true;
        }
    }
    seen.clear();
    for update in &event.cargo {
        if !seen.insert(&update.ticket_id) || update.from_agent_id == update.to_agent_id {
            return Err(AppError::invalid("duplicate or empty cargo movement"));
        }
        let ci = op
            .cargo
            .iter()
            .position(|c| c.ticket_id == update.ticket_id)
            .ok_or_else(|| AppError::invalid("unknown cargo"))?;
        let cargo = &op.cargo[ci];
        if p.tickets
            .iter()
            .any(|t| t.id == cargo.ticket_id && t.r#type == e::TicketType::Pickup as i32)
            && !op
                .tickets
                .iter()
                .any(|t| t.ticket_id == cargo.ticket_id && t.phase() == a::TicketPhase::Completed)
        {
            return Err(AppError::conflict(
                "pickup cargo becomes available only after confirmed service completion",
            ));
        }
        if cargo.delivered || cargo.agent_id != update.from_agent_id {
            return Err(AppError::conflict(
                "cargo ownership changed or was already delivered",
            ));
        }
        let loc = p
            .locations
            .iter()
            .find(|l| l.id == update.location_id)
            .ok_or_else(|| AppError::invalid("unknown cargo movement location"))?;
        if update.from_agent_id.is_empty() && cargo.location_id != loc.id {
            return Err(AppError::conflict("cargo is not stored at this location"));
        }
        for id in [&update.from_agent_id, &update.to_agent_id]
            .into_iter()
            .filter(|id| !id.is_empty())
        {
            let v = op
                .vehicles
                .iter_mut()
                .find(|v| v.agent_id == *id)
                .ok_or_else(|| AppError::invalid("unknown cargo vehicle"))?;
            if event.at < v.observed_at
                || v.phase() == a::VehiclePhase::Travelling
                || !v.position.as_ref().is_some_and(|pos| {
                    (pos.lng - loc.lng).abs() < 0.000001 && (pos.lat - loc.lat).abs() < 0.000001
                })
            {
                return Err(AppError::conflict(
                    "cargo transfer requires stopped, co-located vehicles with current observations",
                ));
            }
            v.observed_at = event.at;
        }
        let owner_serving = op
            .tickets
            .iter()
            .any(|t| t.ticket_id == update.ticket_id && t.phase() == a::TicketPhase::Serving);
        if owner_serving {
            return Err(AppError::conflict(
                "cannot transfer cargo while its order is being serviced",
            ));
        }
        op.cargo[ci].agent_id = update.to_agent_id.clone();
        op.cargo[ci].location_id = loc.id.clone();
    }
    seen.clear();
    for update in &event.tickets {
        if !seen.insert(&update.ticket_id) || update.note.len() > 2048 {
            return Err(AppError::invalid(
                "duplicate ticket update or oversized note",
            ));
        }
        let phase = a::TicketPhase::try_from(update.phase)
            .map_err(|_| AppError::invalid("unknown ticket phase"))?;
        let ti = op
            .tickets
            .iter()
            .position(|t| t.ticket_id == update.ticket_id)
            .ok_or_else(|| AppError::invalid("unknown execution ticket"))?;
        let original = op.tickets[ti].clone();
        if event.at < original.observed_at {
            return Err(AppError::conflict(
                "ticket event is older than observed state",
            ));
        }
        let valid = matches!(
            (original.phase(), phase),
            (
                a::TicketPhase::Pending,
                a::TicketPhase::EnRoute | a::TicketPhase::Cancelled | a::TicketPhase::Exception
            ) | (
                a::TicketPhase::EnRoute,
                a::TicketPhase::EnRoute
                    | a::TicketPhase::Arrived
                    | a::TicketPhase::Cancelled
                    | a::TicketPhase::Exception
            ) | (
                a::TicketPhase::Arrived,
                a::TicketPhase::EnRoute
                    | a::TicketPhase::Serving
                    | a::TicketPhase::Cancelled
                    | a::TicketPhase::Exception
            ) | (
                a::TicketPhase::Serving,
                a::TicketPhase::Serving | a::TicketPhase::Completed | a::TicketPhase::Exception
            ) | (
                a::TicketPhase::Exception,
                a::TicketPhase::Pending | a::TicketPhase::Cancelled
            )
        );
        if !valid {
            return Err(AppError::conflict(
                "invalid ticket transition; completed facts cannot be rewritten and serving work must finish or report an exception",
            ));
        }
        if phase == a::TicketPhase::Exception && update.note.is_empty() {
            return Err(AppError::invalid("exception requires a note"));
        }
        let ticket = p
            .tickets
            .iter()
            .find(|t| t.id == update.ticket_id)
            .ok_or_else(|| AppError::conflict("ticket is absent from business input"))?;
        let agent = if !update.agent_id.is_empty() {
            update.agent_id.clone()
        } else {
            original.agent_id.clone()
        };
        let requires_agent = matches!(
            phase,
            a::TicketPhase::EnRoute
                | a::TicketPhase::Arrived
                | a::TicketPhase::Serving
                | a::TicketPhase::Completed
        );
        if requires_agent && !op.vehicles.iter().any(|v| v.agent_id == agent) {
            return Err(AppError::invalid("transition requires a known agent"));
        }
        if !matches!(
            original.phase(),
            a::TicketPhase::Pending | a::TicketPhase::Exception
        ) && original.agent_id != agent
            && phase != a::TicketPhase::EnRoute
        {
            return Err(AppError::conflict(
                "cannot change the executing agent through a status update",
            ));
        }
        if phase == a::TicketPhase::EnRoute
            && original.agent_id != agent
            && !original.agent_id.is_empty()
        {
            let old = op
                .vehicles
                .iter_mut()
                .find(|v| v.agent_id == original.agent_id)
                .unwrap();
            if event.at < old.observed_at {
                return Err(AppError::conflict(
                    "reassignment predates old vehicle observation",
                ));
            }
            if matches!(
                old.phase(),
                a::VehiclePhase::Travelling | a::VehiclePhase::Waiting
            ) {
                old.phase = a::VehiclePhase::Available as i32;
                old.available_at = event.at;
            }
            old.observed_at = event.at;
        }
        if requires_agent {
            if op.tickets.iter().any(|t| {
                t.ticket_id != ticket.id
                    && t.agent_id == agent
                    && t.phase() == a::TicketPhase::Serving
            }) {
                return Err(AppError::conflict(
                    "vehicle is already serving another ticket",
                ));
            }
            if phase == a::TicketPhase::EnRoute {
                // Explicit departure to another stop supersedes the old, not-yet-started leg.
                for t in &mut op.tickets {
                    if t.ticket_id != ticket.id
                        && t.agent_id == agent
                        && matches!(t.phase(), a::TicketPhase::EnRoute | a::TicketPhase::Arrived)
                    {
                        t.phase = a::TicketPhase::Pending as i32;
                        t.agent_id.clear();
                        t.observed_at = event.at;
                    }
                }
            }
        }
        let remaining = update.remaining_service_seconds.unwrap_or_else(|| {
            if original.phase() == a::TicketPhase::Serving {
                (original
                    .remaining_service_seconds
                    .unwrap_or(ticket.duration)
                    - (event.at - original.observed_at))
                    .max(0)
            } else if let Some(remaining) = original.remaining_service_seconds {
                remaining
            } else {
                ticket.duration
            }
        });
        if !(0..=1_000_000_000_000).contains(&remaining) {
            return Err(AppError::invalid("remaining service time out of range"));
        }
        if phase == a::TicketPhase::Serving && original.phase() != a::TicketPhase::Serving {
            for pair in p
                .precedences
                .iter()
                .filter(|pair| p.tickets[pair.second as usize].id == ticket.id)
            {
                let predecessor = &p.tickets[pair.first as usize].id;
                if !op.tickets.iter().any(|status| {
                    status.ticket_id == *predecessor
                        && status.phase() == a::TicketPhase::Completed
                        && status.finished_at.is_some_and(|at| at <= event.at)
                }) && !event.tickets.iter().any(|update| {
                    update.ticket_id == *predecessor
                        && update.phase == a::TicketPhase::Completed as i32
                }) {
                    return Err(AppError::conflict(format!(
                        "predecessor {predecessor} must be completed before starting this ticket"
                    )));
                }
            }
        }
        if phase == a::TicketPhase::Serving
            && ticket.r#type == e::TicketType::Delivery as i32
            && !op
                .cargo
                .iter()
                .any(|c| c.ticket_id == ticket.id && c.agent_id == agent && !c.delivered)
        {
            return Err(AppError::conflict(
                "delivery cargo is not on the servicing vehicle",
            ));
        }
        if let Some(v) = op.vehicles.iter_mut().find(|v| v.agent_id == agent) {
            if event.at < v.observed_at {
                return Err(AppError::conflict(
                    "ticket event predates the vehicle observation",
                ));
            }
            if matches!(phase, a::TicketPhase::EnRoute | a::TicketPhase::Serving)
                && matches!(
                    v.phase(),
                    a::VehiclePhase::Unavailable | a::VehiclePhase::Paused
                )
            {
                return Err(AppError::conflict("vehicle is unavailable"));
            }
            match phase {
                a::TicketPhase::EnRoute => {
                    v.phase = a::VehiclePhase::Travelling as i32;
                    v.fixed_cost_paid = true;
                }
                a::TicketPhase::Arrived => {
                    v.position = Some(p.locations[ticket.location as usize].clone());
                    v.phase = a::VehiclePhase::Waiting as i32;
                }
                a::TicketPhase::Serving => {
                    v.phase = a::VehiclePhase::Servicing as i32;
                    v.available_at = event.at + remaining;
                }
                a::TicketPhase::Completed | a::TicketPhase::Exception => {
                    if let Some(start) = original.service_started_at {
                        v.service_seconds = v
                            .service_seconds
                            .checked_add(event.at - start)
                            .ok_or_else(|| AppError::invalid("service work overflow"))?;
                    }
                    if phase == a::TicketPhase::Completed {
                        v.completed_tickets = v
                            .completed_tickets
                            .checked_add(1)
                            .ok_or_else(|| AppError::invalid("completed count overflow"))?;
                        if ticket.r#type == e::TicketType::Delivery as i32 {
                            for (d, q) in v.delivered.iter_mut().zip(&ticket.demand) {
                                *d = d.checked_add(*q).ok_or_else(|| {
                                    AppError::invalid("delivered amount overflow")
                                })?;
                            }
                        }
                    }
                    if v.phase() != a::VehiclePhase::Unavailable {
                        v.phase = a::VehiclePhase::Available as i32;
                    }
                    v.available_at = event.at;
                }
                a::TicketPhase::Cancelled
                    if matches!(
                        original.phase(),
                        a::TicketPhase::EnRoute | a::TicketPhase::Arrived
                    ) && matches!(
                        v.phase(),
                        a::VehiclePhase::Travelling | a::VehiclePhase::Waiting
                    ) =>
                {
                    v.phase = a::VehiclePhase::Available as i32;
                    v.available_at = event.at;
                }
                _ => {}
            }
            v.observed_at = event.at;
        }
        let state = &mut op.tickets[ti];
        state.phase = phase as i32;
        state.agent_id = agent.clone();
        state.observed_at = event.at;
        state.note = update.note.clone();
        if phase == a::TicketPhase::Serving {
            state.service_started_at = Some(original.service_started_at.unwrap_or(event.at));
            state.remaining_service_seconds = Some(remaining);
        }
        if phase == a::TicketPhase::Exception {
            state.service_started_at = None;
            state.remaining_service_seconds = Some(remaining);
        }
        if phase == a::TicketPhase::Pending {
            state.agent_id.clear();
        }
        if phase == a::TicketPhase::Completed {
            state.finished_at = Some(event.at);
            state.remaining_service_seconds = Some(0);
            if let Some(cargo) = op.cargo.iter_mut().find(|c| c.ticket_id == ticket.id) {
                if ticket.r#type == e::TicketType::Delivery as i32 {
                    cargo.agent_id.clear();
                    cargo.delivered = true;
                }
                if ticket.r#type == e::TicketType::Pickup as i32 {
                    cargo.agent_id = agent;
                }
                cargo.location_id = p.locations[ticket.location as usize].id.clone();
            }
        }
        if phase == a::TicketPhase::Cancelled {
            for lock in &mut op.route_commitments {
                lock.prefix.retain(|t| t != &ticket.id);
            }
            op.time_commitments.retain(|t| t.ticket_id != ticket.id);
        }
    }
    for vehicle in &op.vehicles {
        let serving = op
            .tickets
            .iter()
            .any(|t| t.agent_id == vehicle.agent_id && t.phase() == a::TicketPhase::Serving);
        if (vehicle.phase() == a::VehiclePhase::Servicing && !serving)
            || (serving
                && !matches!(
                    vehicle.phase(),
                    a::VehiclePhase::Servicing
                        | a::VehiclePhase::Unavailable
                        | a::VehiclePhase::Paused
                ))
        {
            return Err(AppError::conflict(
                "vehicle service state must agree with its serving ticket; report a completion or exception first",
            ));
        }
    }
    let current = at(op, wall).max(event.at);
    op.at = current;
    op.clock_unix_ms = wall;
    let significant = !event.tickets.is_empty()
        || !event.cargo.is_empty()
        || event
            .vehicles
            .iter()
            .any(|v| v.phase.is_some() || v.available_at.is_some());
    mark_dirty(op, wall, significant);
    op.revision += 1;
    validate_commitments(op, p)?;
    Ok(())
}
