//! Immutable execution boundary. No event processing or external I/O lives here.
use crate::{ConstraintId as C, Problem, ProblemError, Weight};

#[derive(Clone, Debug, Default)]
pub struct ExecutedRoute {
    pub initial_load: Option<Vec<i64>>,
    pub completed_tickets: u32,
    pub worked_seconds: i64,
    pub delivered: Vec<i64>,
    pub fixed_cost_paid: bool,
    pub unavailable: bool,
    pub must_return: bool,
    pub locked_prefix: Vec<u32>,
}
#[derive(Clone, Debug, Default)]
pub struct ExecutionTicket {
    pub only_agent: Option<u32>,
    pub blocked: bool,
    pub auxiliary: bool,
    pub load_before: Option<u32>,
    pub original_position: Option<u32>,
    pub original_start: Option<i64>,
    pub committed_earliest: Option<i64>,
    pub committed_latest: Option<i64>,
}
#[derive(Clone, Debug)]
pub struct PastDependency {
    pub ticket: u32,
    pub agent: u32,
    pub finished_at: i64,
    pub same_agent: bool,
    pub after_finish: bool,
    pub related: bool,
}
#[derive(Clone, Debug, Default)]
pub struct ExecutionSpec {
    pub routes: Vec<(u32, ExecutedRoute)>,
    pub tickets: Vec<(u32, ExecutionTicket)>,
    pub past_dependencies: Vec<PastDependency>,
}
#[derive(Clone, Debug, Default)]
pub struct Execution {
    pub active: bool,
    pub routes: Vec<ExecutedRoute>,
    pub tickets: Vec<ExecutionTicket>,
    pub past: Vec<Vec<PastDependency>>,
}

impl Execution {
    pub fn compile(spec: Option<ExecutionSpec>, p: &Problem) -> Result<Self, ProblemError> {
        let mut result = Self {
            active: spec.is_some(),
            routes: vec![ExecutedRoute::default(); p.agents.len()],
            tickets: vec![ExecutionTicket::default(); p.tickets.len()],
            past: vec![Vec::new(); p.tickets.len()],
        };
        let Some(spec) = spec else { return Ok(result) };
        let fail = |s: &str| ProblemError(format!("execution: {s}"));
        if p.weights.get(C::ExecutionConsistency) != Weight::hard(1) {
            return Err(fail("execution_consistency must have weight 1 hard"));
        }
        let mut seen = std::collections::HashSet::new();
        for (a, mut route) in spec.routes {
            if a as usize >= p.agents.len() || !seen.insert(a) {
                return Err(fail("unknown or duplicate route"));
            }
            if !(0..=1_000_000_000_000).contains(&route.worked_seconds)
                || route.completed_tickets > 1_000_000_000
            {
                return Err(fail("cumulative work out of range"));
            }
            for values in route
                .initial_load
                .iter_mut()
                .chain(std::iter::once(&mut route.delivered))
            {
                if values.len() > p.dims
                    || values
                        .iter()
                        .any(|&v| !(0..=1_000_000_000_000_000).contains(&v))
                {
                    return Err(fail("load or delivered vector out of range"));
                }
                values.resize(p.dims, 0);
            }
            let mut prefix_seen = std::collections::HashSet::new();
            for &t in &route.locked_prefix {
                if t as usize >= p.tickets.len()
                    || !prefix_seen.insert(t)
                    || !p.tickets[t as usize].pinned
                    || p.tickets[t as usize].original_agent != Some(a)
                {
                    return Err(fail(
                        "locked prefix must contain distinct pinned tickets of its agent",
                    ));
                }
            }
            result.routes[a as usize] = route;
        }
        seen.clear();
        for (t, rule) in spec.tickets {
            if t as usize >= p.tickets.len()
                || !seen.insert(t)
                || rule
                    .only_agent
                    .is_some_and(|a| a as usize >= p.agents.len())
                || rule
                    .load_before
                    .is_some_and(|v| v as usize >= p.tickets.len() || v == t)
                || [
                    rule.original_start,
                    rule.committed_earliest,
                    rule.committed_latest,
                ]
                .into_iter()
                .flatten()
                .any(|v| !(-1_000_000_000_000..=1_000_000_000_000).contains(&v))
                || rule
                    .committed_earliest
                    .zip(rule.committed_latest)
                    .is_some_and(|(a, b)| a > b)
            {
                return Err(fail("invalid or duplicate ticket rule"));
            }
            result.tickets[t as usize] = rule;
        }
        for past in spec.past_dependencies {
            if past.ticket as usize >= p.tickets.len()
                || past.agent as usize >= p.agents.len()
                || !(-1_000_000_000_000..=1_000_000_000_000).contains(&past.finished_at)
            {
                return Err(fail("past dependency out of range"));
            }
            result.past[past.ticket as usize].push(past);
        }
        Ok(result)
    }

    pub fn permits_ticket(&self, agent: u32, ticket: u32) -> bool {
        let r = &self.tickets[ticket as usize];
        !self.routes[agent as usize].unavailable
            && !r.blocked
            && r.only_agent.is_none_or(|a| a == agent)
    }
    pub fn permits_route(&self, agent: u32, route: &[u32]) -> bool {
        !self.active
            || (route.starts_with(&self.routes[agent as usize].locked_prefix)
                && route.iter().all(|&t| self.permits_ticket(agent, t)))
    }
}
