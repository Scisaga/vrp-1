use std::collections::BTreeSet;

use vrp_core::constraint::ConstraintId;
use vrp_core::oracle::Evaluation;
use vrp_core::problem::flag;
use vrp_core::{NONE, Score, Solution};
use vrp_proto::engine::v1::{self as pb, EdgeSource};

use crate::Compiled;

fn score(s: Score) -> pb::Score {
    pb::Score {
        hard: s.hard,
        medium: s.medium,
        soft: s.soft,
    }
}

/// 只输出有工单的路线。
pub fn solution_to_proto(solution: &Solution) -> pb::Solution {
    pb::Solution {
        routes: solution
            .routes
            .iter()
            .enumerate()
            .filter(|(_, r)| !r.is_empty())
            .map(|(a, r)| pb::Route {
                agent: a as u32,
                tickets: r.clone(),
            })
            .collect(),
    }
}

/// 未派工单的原因代码：没有候选 agent 时给出剪枝原因，否则是求解后仍未派。
pub fn unassigned_reasons(compiled: &Compiled, ticket: u32) -> Vec<String> {
    let p = &compiled.problem;
    if !p.candidates[ticket as usize].is_empty() {
        return vec!["not_assigned".into()];
    }
    let rule = &p.execution.tickets[ticket as usize];
    if rule.blocked {
        return vec!["execution_blocked".into()];
    }
    if let Some(a) = rule.only_agent
        && p.execution.routes[a as usize].unavailable
    {
        return vec!["cargo_owner_unavailable".into()];
    }
    if p.execution.active && p.execution.routes.iter().all(|r| r.unavailable) {
        return vec!["no_available_vehicle".into()];
    }
    let why = p.prune_reasons[ticket as usize];
    let mut reasons = vec!["no_eligible_agent".to_string()];
    for (bit, name) in [
        (flag::SKILLS, "skills"),
        (flag::QUALIFICATION, "qualification"),
        (flag::DEPOT, "depot"),
        (flag::DAY, "day"),
        (flag::ORIGINAL, "original_agent"),
        (flag::PINNED, "pinned"),
    ] {
        if why & bit != 0 {
            reasons.push(name.into());
        }
    }
    reasons
}

/// 由全量评分结果组装 SolveResult。
pub fn build_result(
    compiled: &Compiled,
    solution: &Solution,
    evaluation: &Evaluation,
    stats: pb::SolveStats,
) -> pb::SolveResult {
    let p = &compiled.problem;
    let constraints = ConstraintId::ALL
        .into_iter()
        .filter(|&c| !p.weights.get(c).is_zero())
        .map(|c| {
            let t = evaluation.total(c);
            pb::ConstraintScore {
                name: c.name().into(),
                matches: t.matches,
                score: Some(score(t.score)),
            }
        })
        .collect();
    let matches = evaluation
        .matches
        .iter()
        .map(|m| pb::ConstraintMatch {
            constraint: m.constraint.name().into(),
            agents: m.agents.clone(),
            tickets: m.tickets.clone(),
            amount: m.amount,
            score: Some(score(m.score)),
        })
        .collect();

    let mut estimated = BTreeSet::new();
    let routes = evaluation
        .routes
        .iter()
        .map(|r| {
            let matrix = p.agents[r.agent as usize].matrix;
            let sources = &compiled.sources[matrix as usize];
            for &(from, to) in &r.legs {
                let k = from as usize * p.matrices[matrix as usize].n + to as usize;
                let source = sources[k] as i32;
                if source == EdgeSource::Model as i32 || source == EdgeSource::Fallback as i32 {
                    estimated.insert((matrix, from, to));
                }
            }
            pb::RouteDetail {
                agent: r.agent,
                stops: r
                    .stops
                    .iter()
                    .map(|s| pb::Stop {
                        ticket: s.ticket,
                        arrival: s.arrival,
                        start: s.start,
                        end: s.end,
                    })
                    .collect(),
                travel_time: r.travel_time,
                travel_distance: r.travel_distance,
                end_time: r.end_time,
                peak_load: r.peak_load.clone(),
            }
        })
        .collect();

    let unassigned = (0..p.tickets.len() as u32)
        .filter(|&t| evaluation.agent_of[t as usize] == NONE)
        .map(|t| pb::UnassignedTicket {
            ticket: t,
            reasons: unassigned_reasons(compiled, t),
        })
        .collect();

    pb::SolveResult {
        solution: Some(solution_to_proto(solution)),
        score: Some(score(evaluation.score)),
        constraints,
        matches,
        routes,
        unassigned,
        estimated_edges: estimated
            .into_iter()
            .map(|(matrix, from, to)| pb::Edge { matrix, from, to })
            .collect(),
        stats: Some(stats),
    }
}
