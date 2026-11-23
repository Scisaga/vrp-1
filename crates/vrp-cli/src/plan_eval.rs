//! Evaluate externally supplied route assignments against an immutable request.
use super::{Args, read_json};
use prost::Message;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::{fs, path::Path};
use vrp_core::{NONE, oracle};
use vrp_proto::engine::v1 as pb;

fn assignments(path: &str) -> Result<Vec<(String, Vec<String>)>, String> {
    let value = read_json(Path::new(path))?;
    let routes = value
        .get("routes")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{path}: expected an object with a routes array"))?;
    let mut agents = HashSet::new();
    let mut assigned = HashSet::new();
    let mut result = Vec::with_capacity(routes.len());
    for route in routes {
        let id = route
            .get("agentId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or("route requires a nonempty agentId")?;
        if !agents.insert(id) {
            return Err(format!("duplicate agent ID: {id}"));
        }
        let tickets = route
            .get("ticketIds")
            .and_then(Value::as_array)
            .ok_or("route requires a ticketIds array")?;
        let mut ids = Vec::with_capacity(tickets.len());
        for ticket in tickets {
            let id = ticket
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("ticket IDs must be nonempty strings")?;
            if !assigned.insert(id) {
                return Err(format!("duplicate assignment: {id}"));
            }
            ids.push(id.to_owned());
        }
        result.push((id.to_owned(), ids));
    }
    Ok(result)
}

pub(super) fn run(args: &[String]) -> Result<(), String> {
    let args = Args::parse(args)?;
    let [input, candidate] = args.positional[..] else {
        return Err("usage: vrp eval-plan <frozen request> <ordered routes.json> [--baseline <routes.json>] [--urgent-ticket <id>]".into());
    };
    for &(option, _) in &args.options {
        if !["baseline", "urgent-ticket"].contains(&option) {
            return Err(format!("unknown eval-plan option --{option}"));
        }
    }
    let path = Path::new(input);
    let bytes = fs::read(path).map_err(|error| format!("{input}: {error}"))?;
    let mut request: pb::SolveRequest = if path.extension().is_some_and(|s| s == "json") {
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?
    } else {
        pb::SolveRequest::decode(bytes.as_slice()).map_err(|error| error.to_string())?
    };
    vrp_model::validate_ready_matrices(&request).map_err(|e| e.0)?;
    let problem = request
        .problem
        .as_ref()
        .ok_or("frozen request has no problem")?;
    let agents: HashMap<_, _> = problem
        .agents
        .iter()
        .enumerate()
        .map(|(i, agent)| (agent.id.as_str(), i as u32))
        .collect();
    let tickets: HashMap<_, _> = problem
        .tickets
        .iter()
        .enumerate()
        .map(|(i, ticket)| (ticket.id.as_str(), i as u32))
        .collect();
    let routes = assignments(candidate)?
        .into_iter()
        .map(|(agent_id, ticket_ids)| {
            let agent = *agents
                .get(agent_id.as_str())
                .ok_or_else(|| format!("unknown agent: {agent_id}"))?;
            let tickets = ticket_ids
                .into_iter()
                .map(|id| {
                    tickets
                        .get(id.as_str())
                        .copied()
                        .ok_or_else(|| format!("unknown ticket: {id}"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, String>(pb::Route { agent, tickets })
        })
        .collect::<Result<Vec<_>, _>>()?;
    request.initial_solution = Some(pb::Solution { routes });
    // compile enforces pins and exact locked prefixes. Invalid assignments fail;
    // the evaluator never completes a partial plan or repairs an illegal route.
    let compiled = vrp_model::compile(&request).map_err(|e| e.0)?;
    let solution = compiled
        .initial
        .as_ref()
        .ok_or("compiled assignment is missing")?;
    let evaluation = oracle::evaluate(&compiled.problem, solution);
    let result =
        vrp_model::build_result(&compiled, solution, &evaluation, pb::SolveStats::default());
    let is_business = |index: usize| !problem.tickets[index].id.starts_with("@vrp/");
    let assigned = evaluation
        .agent_of
        .iter()
        .enumerate()
        .filter(|&(index, &agent)| is_business(index) && agent != NONE)
        .count();
    let unassigned = result
        .unassigned
        .iter()
        .filter(|row| is_business(row.ticket as usize))
        .map(|row| &problem.tickets[row.ticket as usize].id)
        .collect::<Vec<_>>();
    let urgent = args
        .option("urgent-ticket")
        .map(|id| {
            let index = tickets
                .get(id)
                .copied()
                .ok_or_else(|| format!("unknown urgent ticket: {id}"))?;
            let deadline = problem.tickets[index as usize].latest_start;
            let stop = result.routes.iter().find_map(|route| {
                route
                    .stops
                    .iter()
                    .find(|stop| stop.ticket == index)
                    .map(|stop| (route, stop))
            });
            Ok::<_, String>(json!({
                "ticketId": id, "deadlineSeconds": deadline,
                "agentId": stop.map(|(route, _)| &problem.agents[route.agent as usize].id),
                "arrivalSeconds": stop.map(|(_, stop)| stop.arrival),
                "serviceStartSeconds": stop.map(|(_, stop)| stop.start),
                "onTime": stop.is_some_and(|(_, stop)| stop.arrival <= deadline)
            }))
        })
        .transpose()?;
    let reassigned = args
        .option("baseline")
        .map(|path| {
            let baseline = assignments(path)?;
            let mut count = 0;
            for (agent, ids) in baseline {
                for id in ids {
                    if let Some(&index) = tickets.get(id.as_str()) {
                        let assigned = evaluation.agent_of[index as usize];
                        if is_business(index as usize)
                            && assigned != NONE
                            && problem.agents[assigned as usize].id != agent
                        {
                            count += 1;
                        }
                    }
                }
            }
            Ok::<_, String>(count)
        })
        .transpose()?;
    let hard = result
        .constraints
        .iter()
        .filter(|c| c.score.as_ref().is_some_and(|s| s.hard < 0))
        .map(|c| json!({"constraint": c.name, "matches": c.matches}))
        .collect::<Vec<_>>();
    let qualified = evaluation.score.hard == 0
        && unassigned.is_empty()
        && urgent.as_ref().is_none_or(|value| value["onTime"] == true);
    println!("{}", serde_json::to_string_pretty(&json!({
        "qualified": qualified, "assigned": assigned,
        "unassigned": unassigned.len(), "unassignedTicketIds": unassigned,
        "hardViolations": hard,
        "score": {"hard": evaluation.score.hard, "medium": evaluation.score.medium, "soft": evaluation.score.soft},
        "travelMeters": result.routes.iter().map(|route| route.travel_distance).sum::<i64>(),
        "travelSeconds": result.routes.iter().map(|route| route.travel_time).sum::<i64>(),
        "protectedTasksPreserved": true,
        "reassigned": reassigned, "urgentTicket": urgent
    })).map_err(|error| error.to_string())?);
    Ok(())
}
