use std::fmt;

use crate::problem::Problem;

/// 下标数组里表示"无"的值，例如未派工单的 agent。
pub const NONE: u32 = u32::MAX;

/// 方案：每个 agent 一条路线（按访问顺序的工单下标），未出现在任何路线里的工单视为未派。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Solution {
    pub routes: Vec<Vec<u32>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolutionError(pub String);

impl fmt::Display for SolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SolutionError {}

impl Solution {
    pub fn empty(n_agents: usize) -> Solution {
        Solution {
            routes: vec![Vec::new(); n_agents],
        }
    }

    /// 无显式初始解时使用：按工单下标把固定工单放入原 agent，其他工单未派。
    pub fn with_pinned(problem: &Problem) -> Solution {
        let mut solution = Self::empty(problem.agents.len());
        for (a, execution) in problem.execution.routes.iter().enumerate() {
            solution.routes[a].extend_from_slice(&execution.locked_prefix);
        }
        for (t, ticket) in problem.tickets.iter().enumerate() {
            if ticket.pinned
                && !solution.routes[ticket.original_agent.unwrap() as usize].contains(&(t as u32))
            {
                solution.routes[ticket.original_agent.expect("validated pinned agent") as usize]
                    .push(t as u32);
            }
        }
        solution
    }

    pub fn validate(&self, problem: &Problem) -> Result<(), SolutionError> {
        if self.routes.len() != problem.agents.len() {
            return Err(SolutionError(format!(
                "solution has {} routes but the problem has {} agents",
                self.routes.len(),
                problem.agents.len()
            )));
        }
        let mut seen = vec![false; problem.tickets.len()];
        for (a, route) in self.routes.iter().enumerate() {
            if route.iter().any(|&t| t as usize >= problem.tickets.len()) {
                return Err(SolutionError("unknown ticket in route".into()));
            }
            if !problem.execution.permits_route(a as u32, route) {
                return Err(SolutionError(
                    "route conflicts with execution restrictions or locked prefix".into(),
                ));
            }
            for &t in route {
                let slot = seen.get_mut(t as usize).ok_or_else(|| {
                    SolutionError(format!("route of agent {a} refers to unknown ticket {t}"))
                })?;
                if *slot {
                    return Err(SolutionError(format!("ticket {t} appears more than once")));
                }
                *slot = true;
                let ticket = &problem.tickets[t as usize];
                if ticket.pinned && ticket.original_agent != Some(a as u32) {
                    return Err(SolutionError(format!(
                        "pinned ticket {t} must remain on its original agent"
                    )));
                }
            }
        }
        for (t, ticket) in problem.tickets.iter().enumerate() {
            if ticket.pinned && !seen[t] {
                return Err(SolutionError(format!("pinned ticket {t} must be assigned")));
            }
        }
        Ok(())
    }

    /// 每张工单所在的 agent，未派为 [`NONE`]。
    pub fn agent_of(&self, n_tickets: usize) -> Vec<u32> {
        let mut agent_of = vec![NONE; n_tickets];
        for (a, route) in self.routes.iter().enumerate() {
            for &t in route {
                agent_of[t as usize] = a as u32;
            }
        }
        agent_of
    }

    pub fn assigned_count(&self) -> usize {
        self.routes.iter().map(Vec::len).sum()
    }
}
