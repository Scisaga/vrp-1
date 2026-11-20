//! 增量评分状态：缓存每条路线的罚分和每张工单的时间，一次改动只重算受影响的路线和对。

use crate::eval::{self, RouteSummary, Scratch};
use crate::problem::{PairRef, Problem};
use crate::score::Score;
use crate::solution::{NONE, Solution, SolutionError};
use crate::{
    constraint::ConstraintId as C,
    timing::{self, Timing},
};

/// 一次改动：给出若干 agent 的新路线。
///
/// 旧路线上、但不在任何新路线里的工单变为未派；原本未派、出现在新路线里的工单变为已派。
/// 从未改动的路线上挪走工单是非法的：工单的来源路线必须同时出现在改动里。
#[derive(Clone, Copy, Debug)]
pub struct Change<'a> {
    pub routes: &'a [(u32, &'a [u32])],
}

pub struct State<'p> {
    problem: &'p Problem,
    routes: Vec<Vec<u32>>,
    agent_of: Vec<u32>,
    start: Vec<i64>,
    end: Vec<i64>,
    summaries: Vec<RouteSummary>,
    route_scores: Vec<Score>,
    total: Score,
    // 以下为复用的临时数据。
    scratch: Scratch,
    mark: Vec<u32>,
    agent_mark: Vec<u32>,
    generation: u32,
    touched: Vec<u32>,
    tmp_agent: Vec<u32>,
    tmp_start: Vec<i64>,
    tmp_end: Vec<i64>,
    related_mark: Vec<u32>,
    precedence_mark: Vec<u32>,
    pending: Vec<(u32, RouteSummary, Score)>,
    timing: Timing,
    timing_blocked: i64,
    pending_blocked: i64,
}

impl<'p> State<'p> {
    pub fn new(problem: &'p Problem, solution: &Solution) -> Result<State<'p>, SolutionError> {
        solution.validate(problem)?;
        let n_tickets = problem.tickets.len();
        let n_agents = problem.agents.len();
        let mut state = State {
            problem,
            routes: solution.routes.clone(),
            agent_of: solution.agent_of(n_tickets),
            start: vec![0; n_tickets],
            end: vec![0; n_tickets],
            summaries: vec![RouteSummary::default(); n_agents],
            route_scores: vec![Score::ZERO; n_agents],
            total: Score::ZERO,
            scratch: Scratch::new(problem.dims),
            mark: vec![0; n_tickets],
            agent_mark: vec![0; n_agents],
            generation: 0,
            touched: Vec::new(),
            tmp_agent: vec![NONE; n_tickets],
            tmp_start: vec![0; n_tickets],
            tmp_end: vec![0; n_tickets],
            related_mark: vec![0; problem.related_pairs.len()],
            precedence_mark: vec![0; problem.precedences.len()],
            pending: Vec::new(),
            timing: Timing::new(n_tickets),
            timing_blocked: 0,
            pending_blocked: 0,
        };
        state.recompute();
        Ok(state)
    }

    pub fn problem(&self) -> &'p Problem {
        self.problem
    }

    #[inline]
    pub fn score(&self) -> Score {
        self.total
    }

    #[inline]
    pub fn route(&self, agent: u32) -> &[u32] {
        &self.routes[agent as usize]
    }

    #[inline]
    pub fn agent_of(&self, ticket: u32) -> Option<u32> {
        let a = self.agent_of[ticket as usize];
        (a != NONE).then_some(a)
    }

    pub fn summary(&self, agent: u32) -> &RouteSummary {
        &self.summaries[agent as usize]
    }

    pub fn solution(&self) -> Solution {
        Solution {
            routes: self.routes.clone(),
        }
    }

    /// 从头重算全部缓存，返回总分。
    pub fn recompute(&mut self) -> Score {
        let p = self.problem;
        let mut total = Score::ZERO;
        let dependencies = timing::enabled(p);
        if dependencies {
            self.timing.compute(p, |a| &self.routes[a]);
        }
        self.timing_blocked = if dependencies { self.timing.blocked } else { 0 };
        total += Score::penalty(
            self.timing_blocked,
            p.weights.get(C::RefTicketAfterDepTicket),
        );
        for a in 0..p.agents.len() {
            let (start, end) = (&mut self.start, &mut self.end);
            let summary = eval::evaluate_route_with_releases(
                p,
                a as u32,
                &self.routes[a],
                &mut self.scratch,
                if dependencies {
                    &self.timing.start
                } else {
                    &[]
                },
                |t, _, s, e| {
                    start[t as usize] = s;
                    end[t as usize] = e;
                },
            );
            let score = eval::route_score(p, a as u32, &summary);
            self.summaries[a] = summary;
            self.route_scores[a] = score;
            total += score;
        }
        for t in 0..p.tickets.len() {
            if self.agent_of[t] == NONE {
                total += eval::unassigned_score(p, t as u32);
            }
        }
        for &(a, b) in &p.related_pairs {
            total += eval::related_score(p, self.agent_of[a as usize], self.agent_of[b as usize]);
        }
        for &(a, b) in &p.precedences {
            total += eval::precedence_score(p, self.end_view(a, false), self.start_view(b, false));
        }
        self.total = total;
        total
    }

    /// 改动后的总分，不修改状态。
    pub fn evaluate(&mut self, change: Change<'_>) -> Score {
        if !self.permitted(change) {
            return Score {
                hard: i64::MIN / 2,
                medium: 0,
                soft: 0,
            };
        }
        self.total + self.prepare(change)
    }

    /// 应用改动，返回新的总分。
    pub fn apply(&mut self, change: Change<'_>) -> Score {
        if !self.permitted(change) {
            return self.total;
        }
        let delta = self.prepare(change);
        for (agent, summary, score) in self.pending.drain(..) {
            self.summaries[agent as usize] = summary;
            self.route_scores[agent as usize] = score;
        }
        for &(agent, tickets) in change.routes {
            let route = &mut self.routes[agent as usize];
            route.clear();
            route.extend_from_slice(tickets);
        }
        for &t in &self.touched {
            let i = t as usize;
            self.agent_of[i] = self.tmp_agent[i];
            self.start[i] = self.tmp_start[i];
            self.end[i] = self.tmp_end[i];
        }
        self.total += delta;
        self.timing_blocked = self.pending_blocked;
        self.total
    }

    /// 计算改动带来的分数变化，新路线的汇总留在 `pending`，工单的新值留在 tmp 数组。
    fn prepare(&mut self, change: Change<'_>) -> Score {
        if timing::enabled(self.problem) {
            return self.prepare_dependencies(change);
        }
        let p = self.problem;
        self.next_generation();
        let generation = self.generation;
        self.touched.clear();
        self.pending.clear();

        // 旧路线上的工单先视为未派。
        for &(agent, _) in change.routes {
            assert_ne!(
                self.agent_mark[agent as usize], generation,
                "agent appears twice in change"
            );
            self.agent_mark[agent as usize] = generation;
            for &t in &self.routes[agent as usize] {
                let i = t as usize;
                if self.mark[i] != generation {
                    self.mark[i] = generation;
                    self.touched.push(t);
                }
                self.tmp_agent[i] = NONE;
            }
        }

        let mut delta = Score::ZERO;
        for &(agent, tickets) in change.routes {
            let (mark, touched) = (&mut self.mark, &mut self.touched);
            let (tmp_agent, tmp_start, tmp_end) =
                (&mut self.tmp_agent, &mut self.tmp_start, &mut self.tmp_end);
            let agent_of = &self.agent_of;
            let summary =
                eval::evaluate_route(p, agent, tickets, &mut self.scratch, |t, _, s, e| {
                    let i = t as usize;
                    if mark[i] != generation {
                        assert!(
                            agent_of[i] == NONE,
                            "ticket {t} moved from a route that is not part of the change"
                        );
                        mark[i] = generation;
                        touched.push(t);
                        tmp_agent[i] = NONE;
                    }
                    assert!(
                        tmp_agent[i] == NONE,
                        "ticket {t} appears twice in the change"
                    );
                    tmp_agent[i] = agent;
                    tmp_start[i] = s;
                    tmp_end[i] = e;
                });
            let score = eval::route_score(p, agent, &summary);
            delta += score - self.route_scores[agent as usize];
            self.pending.push((agent, summary, score));
        }

        for &t in &self.touched {
            let i = t as usize;
            if p.tickets[i].pinned {
                assert_eq!(
                    Some(self.tmp_agent[i]),
                    p.tickets[i].original_agent,
                    "pinned ticket cannot be moved or unassigned"
                );
            }
            let was = self.agent_of[i] != NONE;
            let now = self.tmp_agent[i] != NONE;
            if was && !now {
                delta += eval::unassigned_score(p, t);
            } else if !was && now {
                delta -= eval::unassigned_score(p, t);
            }
        }

        for k in 0..self.touched.len() {
            let t = self.touched[k];
            for &pair in &p.ticket_pairs[t as usize] {
                match pair {
                    PairRef::Related(i) => {
                        if self.related_mark[i as usize] == generation {
                            continue;
                        }
                        self.related_mark[i as usize] = generation;
                        let (a, b) = p.related_pairs[i as usize];
                        let old = eval::related_score(
                            p,
                            self.agent_of[a as usize],
                            self.agent_of[b as usize],
                        );
                        let new = eval::related_score(p, self.agent_view(a), self.agent_view(b));
                        delta += new - old;
                    }
                    PairRef::Precedence(i) => {
                        if self.precedence_mark[i as usize] == generation {
                            continue;
                        }
                        self.precedence_mark[i as usize] = generation;
                        let (a, b) = p.precedences[i as usize];
                        let old = eval::precedence_score(
                            p,
                            self.end_view(a, false),
                            self.start_view(b, false),
                        );
                        let new = eval::precedence_score(
                            p,
                            self.end_view(a, true),
                            self.start_view(b, true),
                        );
                        delta += new - old;
                    }
                }
            }
        }
        delta
    }

    // Propagate times over the joint graph, but only re-evaluate route summaries
    // whose assignments/order or times changed. Dependency-free requests retain
    // the original strictly local fast path.
    fn prepare_dependencies(&mut self, change: Change<'_>) -> Score {
        let p = self.problem;
        self.next_generation();
        let generation = self.generation;
        self.touched.clear();
        self.pending.clear();
        for &(a, _) in change.routes {
            assert_ne!(
                self.agent_mark[a as usize], generation,
                "agent appears twice in change"
            );
            self.agent_mark[a as usize] = generation;
        }
        self.timing.compute(p, |a| {
            change
                .routes
                .iter()
                .find(|(id, _)| *id as usize == a)
                .map_or(self.routes[a].as_slice(), |(_, tickets)| *tickets)
        });
        self.pending_blocked = self.timing.blocked;
        let mut delta = Score::penalty(
            self.pending_blocked - self.timing_blocked,
            p.weights.get(C::RefTicketAfterDepTicket),
        );
        for t in 0..p.tickets.len() {
            let old = self.agent_of[t];
            let new = self.timing.agent[t];
            let changed_route = (old != NONE && self.agent_mark[old as usize] == generation)
                || (new != NONE && self.agent_mark[new as usize] == generation);
            let end = self.timing.start[t] + p.tickets[t].duration;
            if changed_route
                || old != new
                || new != NONE && (self.start[t] != self.timing.start[t] || self.end[t] != end)
            {
                self.mark[t] = generation;
                self.touched.push(t as u32);
                self.tmp_agent[t] = new;
                self.tmp_start[t] = self.timing.start[t];
                self.tmp_end[t] = end;
                if p.tickets[t].pinned {
                    assert_eq!(
                        Some(new),
                        p.tickets[t].original_agent,
                        "pinned ticket cannot be moved or unassigned"
                    );
                }
                if old == NONE && new != NONE {
                    delta -= eval::unassigned_score(p, t as u32);
                }
                if old != NONE && new == NONE {
                    delta += eval::unassigned_score(p, t as u32);
                }
            }
        }
        for a in 0..p.agents.len() {
            let tickets = change
                .routes
                .iter()
                .find(|(id, _)| *id as usize == a)
                .map_or(self.routes[a].as_slice(), |(_, tickets)| *tickets);
            if self.agent_mark[a] != generation
                && !tickets.iter().any(|&t| self.mark[t as usize] == generation)
            {
                continue;
            }
            let summary = eval::evaluate_route_with_releases(
                p,
                a as u32,
                tickets,
                &mut self.scratch,
                &self.timing.start,
                |_, _, _, _| {},
            );
            let score = eval::route_score(p, a as u32, &summary);
            delta += score - self.route_scores[a];
            self.pending.push((a as u32, summary, score));
        }
        for &(a, b) in &p.related_pairs {
            delta +=
                eval::related_score(
                    p,
                    self.timing.agent[a as usize],
                    self.timing.agent[b as usize],
                ) - eval::related_score(p, self.agent_of[a as usize], self.agent_of[b as usize]);
        }
        for &(a, b) in &p.precedences {
            delta += eval::precedence_score(p, self.end_view(a, true), self.start_view(b, true))
                - eval::precedence_score(p, self.end_view(a, false), self.start_view(b, false));
        }
        delta
    }

    fn permitted(&self, change: Change<'_>) -> bool {
        change
            .routes
            .iter()
            .all(|&(a, route)| self.problem.execution.permits_route(a, route))
    }

    fn next_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            // 溢出后清零所有标记，避免与很久以前的代数混淆。
            self.mark.fill(0);
            self.agent_mark.fill(0);
            self.related_mark.fill(0);
            self.precedence_mark.fill(0);
            self.generation = 1;
        }
    }

    /// 改动后的 agent：被改动涉及的工单取临时值，其余取当前值。
    #[inline]
    fn agent_view(&self, t: u32) -> u32 {
        let i = t as usize;
        if self.mark[i] == self.generation {
            self.tmp_agent[i]
        } else {
            self.agent_of[i]
        }
    }

    #[inline]
    fn end_view(&self, t: u32, after_change: bool) -> (u32, i64) {
        let i = t as usize;
        if after_change && self.mark[i] == self.generation {
            (self.tmp_agent[i], self.tmp_end[i])
        } else {
            (self.agent_of[i], self.end[i])
        }
    }

    #[inline]
    fn start_view(&self, t: u32, after_change: bool) -> (u32, i64) {
        let i = t as usize;
        if after_change && self.mark[i] == self.generation {
            (self.tmp_agent[i], self.tmp_start[i])
        } else {
            (self.agent_of[i], self.start[i])
        }
    }
}
