//! 候选只引导采样，不删掉非近邻移动。所有新路线仍通过完整业务增量评分。
use super::alpha;
use crate::rng::Rng;
use crate::{Problem, State};
use std::time::{Duration, Instant};

const NEIGHBORS: usize = 16;

/// 以 min(去程时长, 回程时长) 排序的工单近邻。不是 LKH 的 alpha-nearness。
/// 按实际使用的矩阵缓存，每个矩阵、工单最多 16 项；不会按线程复制。
pub(super) struct Candidates {
    by_matrix: Vec<Vec<Vec<u32>>>,
    pub alpha_profiles: u32,
    pub ascent_iterations: u32,
}

impl Candidates {
    pub(super) fn build(p: &Problem, mut stop: impl FnMut() -> bool) -> Self {
        let mut result = Self {
            by_matrix: vec![Vec::new(); p.matrices.len()],
            alpha_profiles: 0,
            ascent_iterations: 0,
        };
        let mut edges = Vec::with_capacity(p.tickets.len());
        for agent in &p.agents {
            let rows = &mut result.by_matrix[agent.matrix as usize];
            if !rows.is_empty() {
                continue;
            }
            let matrix = &p.matrices[agent.matrix as usize];
            for (t, ticket) in p.tickets.iter().enumerate() {
                if stop() {
                    return result;
                }
                edges.clear();
                for (other, neighbor) in p.tickets.iter().enumerate() {
                    if other.is_multiple_of(256) && stop() {
                        return result;
                    }
                    if t == other {
                        continue;
                    }
                    let cost = matrix
                        .duration(ticket.location, neighbor.location)
                        .min(matrix.duration(neighbor.location, ticket.location));
                    edges.push((cost, other as u32));
                }
                if edges.len() > NEIGHBORS {
                    edges.select_nth_unstable(NEIGHBORS);
                    edges.truncate(NEIGHBORS);
                }
                edges.sort_unstable();
                rows.push(edges.iter().map(|&(_, other)| other).collect());
            }
        }
        result
    }

    pub(super) fn build_alpha(
        p: &Problem,
        budget: Duration,
        mut stop: impl FnMut() -> bool,
    ) -> Self {
        let started = Instant::now();
        let mut result = Self::build(p, || started.elapsed() >= budget || stop());
        let n = p.tickets.len();
        if n < 3 {
            return result;
        }
        let profiles = result
            .by_matrix
            .iter()
            .filter(|rows| rows.len() == n)
            .count();
        let mut remaining_profiles = profiles;
        for (matrix_id, rows) in result.by_matrix.iter_mut().enumerate() {
            if rows.len() != n {
                continue;
            }
            if started.elapsed() >= budget || stop() {
                break;
            }
            let profile_budget =
                budget.saturating_sub(started.elapsed()) / remaining_profiles.max(1) as u32;
            remaining_profiles -= 1;
            let profile_started = Instant::now();
            let mut base = vec![0i64; n * n];
            let matrix = &p.matrices[matrix_id];
            for a in 0..n {
                if started.elapsed() >= budget || stop() {
                    return result;
                }
                for b in a + 1..n {
                    let (x, y) = (p.tickets[a].location, p.tickets[b].location);
                    let cost = i64::from(matrix.duration(x, y).min(matrix.duration(y, x)));
                    base[a * n + b] = cost;
                    base[b * n + a] = cost;
                }
            }
            let local_budget = profile_budget.saturating_sub(profile_started.elapsed());
            let Some(alpha) = alpha::build(&base, n, local_budget, || {
                started.elapsed() >= budget || profile_started.elapsed() >= profile_budget || stop()
            }) else {
                continue;
            };
            let mut ranked = Vec::with_capacity(n - 1);
            for (a, row) in rows.iter_mut().enumerate() {
                ranked.clear();
                for b in 0..n {
                    if a != b {
                        ranked.push((
                            alpha.values[a * n + b],
                            base[a * n + b] + alpha.penalties[a] + alpha.penalties[b],
                            b as u32,
                        ));
                    }
                }
                if ranked.len() > NEIGHBORS {
                    ranked.select_nth_unstable(NEIGHBORS);
                    ranked.truncate(NEIGHBORS);
                }
                ranked.sort_unstable();
                row.clear();
                row.extend(ranked.iter().map(|&(_, _, b)| b));
            }
            result.alpha_profiles += 1;
            result.ascent_iterations += alpha.ascent_iterations;
        }
        result
    }

    pub(super) fn neighbors(&self, matrix: u32, ticket: u32) -> &[u32] {
        self.by_matrix
            .get(matrix as usize)
            .and_then(|rows| rows.get(ticket as usize))
            .map_or(&[], Vec::as_slice)
    }

    fn pick(&self, state: &State<'_>, t: u32, rng: &mut Rng) -> Option<u32> {
        // 保留 20% 全域探索；静态技能、日期和近邻都不是安全硬剪枝条件。
        if rng.chance(1, 5) {
            return None;
        }
        let p = state.problem();
        let agent = state
            .agent_of(t)
            .unwrap_or_else(|| rng.below(p.agents.len()) as u32);
        let rows = &self.by_matrix[p.agents[agent as usize].matrix as usize];
        let neighbors = rows.get(t as usize)?;
        (!neighbors.is_empty()).then(|| neighbors[rng.below(neighbors.len())])
    }
}

pub(super) struct MoveBuffers {
    pub a: u32,
    pub b: Option<u32>,
    pub first: Vec<u32>,
    pub second: Vec<u32>,
}

fn insertion_at(route: &[u32], anchor: Option<u32>, rng: &mut Rng) -> usize {
    match anchor.and_then(|t| route.iter().position(|&x| x == t)) {
        Some(at) => at + rng.below(2),
        None => rng.below(route.len() + 1),
    }
}

impl MoveBuffers {
    pub(super) fn new(n: usize) -> Self {
        Self {
            a: 0,
            b: None,
            first: Vec::with_capacity(n),
            second: Vec::with_capacity(n),
        }
    }

    pub(super) fn generate(
        &mut self,
        state: &State<'_>,
        candidates: Option<&Candidates>,
        rng: &mut Rng,
    ) -> bool {
        let p = state.problem();
        let t = rng.below(p.tickets.len()) as u32;
        let anchor = candidates.and_then(|c| c.pick(state, t, rng));
        self.b = None;
        self.first.clear();
        self.second.clear();
        match rng.below(if candidates.is_some() { 4 } else { 3 }) {
            0 => {
                let from = state.agent_of(t);
                let choices = &p.candidates[t as usize];
                let to = anchor
                    .and_then(|other| state.agent_of(other))
                    .filter(|&a| !p.tickets[t as usize].pinned || Some(a) == from)
                    .or_else(|| choices.get(rng.below(choices.len() + 1)).copied());
                if to.is_none() && (from.is_none() || p.tickets[t as usize].pinned) {
                    return false;
                }
                if let Some(from) = from {
                    self.a = from;
                    self.first
                        .extend(state.route(from).iter().copied().filter(|&x| x != t));
                    if let Some(to) = to {
                        if to == from {
                            let at = insertion_at(&self.first, anchor, rng);
                            self.first.insert(at, t);
                        } else {
                            self.b = Some(to);
                            self.second.extend_from_slice(state.route(to));
                            let at = insertion_at(&self.second, anchor, rng);
                            self.second.insert(at, t);
                        }
                    }
                } else if let Some(to) = to {
                    self.a = to;
                    self.first.extend_from_slice(state.route(to));
                    let at = insertion_at(&self.first, anchor, rng);
                    self.first.insert(at, t);
                }
            }
            1 => {
                let other = anchor.unwrap_or_else(|| rng.below(p.tickets.len()) as u32);
                if t == other {
                    return false;
                }
                let (Some(a), Some(b)) = (state.agent_of(t), state.agent_of(other)) else {
                    return false;
                };
                if a != b && (p.tickets[t as usize].pinned || p.tickets[other as usize].pinned) {
                    return false;
                }
                self.a = a;
                self.first.extend_from_slice(state.route(a));
                let i = self.first.iter().position(|&x| x == t).unwrap();
                if a == b {
                    let j = self.first.iter().position(|&x| x == other).unwrap();
                    self.first.swap(i, j);
                } else {
                    self.b = Some(b);
                    self.second.extend_from_slice(state.route(b));
                    let j = self.second.iter().position(|&x| x == other).unwrap();
                    std::mem::swap(&mut self.first[i], &mut self.second[j]);
                }
            }
            2 => {
                let Some(a) = state.agent_of(t) else {
                    return false;
                };
                if state.route(a).len() < 2 {
                    return false;
                }
                self.a = a;
                self.first.extend_from_slice(state.route(a));
                let i = self.first.iter().position(|&x| x == t).unwrap();
                let j = anchor
                    .and_then(|x| self.first.iter().position(|&y| x == y))
                    .unwrap_or_else(|| rng.below(self.first.len()));
                // 非对称矩阵也必须重算反转段内部的所有有向边。
                self.first[i.min(j)..=i.max(j)].reverse();
            }
            _ => {
                // Or-opt：移动 1..3 张连续工单，保留段内方向。可跨路线。
                let Some(from) = state.agent_of(t) else {
                    return false;
                };
                let route = state.route(from);
                let start = route.iter().position(|&x| x == t).unwrap();
                let end = start + 1 + rng.below((route.len() - start).min(3));
                let segment = &route[start..end];
                let to = anchor
                    .and_then(|x| state.agent_of(x))
                    .unwrap_or_else(|| rng.below(p.agents.len()) as u32);
                if from != to && segment.iter().any(|&x| p.tickets[x as usize].pinned) {
                    return false;
                }
                self.a = from;
                self.first.extend_from_slice(&route[..start]);
                self.first.extend_from_slice(&route[end..]);
                let destination = if from == to {
                    &mut self.first
                } else {
                    self.b = Some(to);
                    self.second.extend_from_slice(state.route(to));
                    &mut self.second
                };
                let at = insertion_at(destination, anchor, rng);
                // 追加后旋转，复用缓冲区且不创建临时段。
                destination.extend_from_slice(segment);
                destination[at..].rotate_right(segment.len());
            }
        }
        true
    }

    pub(super) fn kick(&mut self, state: &State<'_>, rng: &mut Rng) -> bool {
        let t = rng.below(state.problem().tickets.len()) as u32;
        let Some(a) = state.agent_of(t) else {
            return false;
        };
        let route = state.route(a);
        if route.len() < 4 {
            return false;
        }
        let i = 1 + rng.below(route.len() - 3);
        let j = i + 1 + rng.below(route.len() - i - 2);
        let k = j + 1 + rng.below(route.len() - j - 1);
        self.a = a;
        self.b = None;
        self.first.clear();
        // A|B|C|D -> A|D|C|B，换掉四条连接边，不反转任何片段。
        self.first.extend_from_slice(&route[..i]);
        self.first.extend_from_slice(&route[k..]);
        self.first.extend_from_slice(&route[j..k]);
        self.first.extend_from_slice(&route[i..j]);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentSpec, Change, Matrix, ProblemSpec, Solution, TicketKind, TicketSpec, Weights, oracle,
    };

    fn asymmetric_problem() -> (Problem, Solution) {
        let n = 13;
        let duration: Vec<u32> = (0..n * n)
            .map(|i| {
                if i / n == i % n {
                    0
                } else {
                    (i * 37 % 500 + 1) as u32
                }
            })
            .collect();
        let mut reverse = duration.clone();
        for i in 0..n {
            for j in 0..n {
                reverse[i * n + j] = duration[j * n + i];
            }
        }
        let matrices = [duration, reverse]
            .into_iter()
            .map(|v| Matrix {
                n,
                distance: v.clone(),
                duration: v,
                buffer_permille: vec![],
            })
            .collect();
        let agents = (0..3)
            .map(|a| AgentSpec {
                id: format!("a{a}"),
                matrix: a % 2,
                capacity: vec![10],
                ..AgentSpec::default()
            })
            .collect();
        let tickets = (0..36)
            .map(|t| TicketSpec {
                id: format!("t{t}"),
                location: t % n as u32,
                duration: 30,
                earliest_start: (t * 20) as i64,
                latest_start: 1000,
                latest_end: 1100,
                demand: vec![1],
                kind: if t % 2 == 0 {
                    TicketKind::Delivery
                } else {
                    TicketKind::Pickup
                },
                pinned: t % 11 == 0,
                original_agent: Some(t % 3),
                ..TicketSpec::default()
            })
            .collect();
        let p = Problem::new(ProblemSpec {
            dims: 1,
            matrices,
            agents,
            tickets,
            weights: Weights::default(),
            related_pairs: vec![(0, 1), (10, 23)],
            precedences: vec![(2, 3), (12, 24)],
            ..ProblemSpec::default()
        })
        .unwrap();
        let solution = Solution {
            routes: (0..3)
                .map(|a| (0..36).filter(|t| t % 3 == a).collect())
                .collect(),
        };
        (p, solution)
    }

    #[test]
    fn generated_moves_and_kicks_preserve_structure_and_match_the_oracle() {
        let (p, initial) = asymmetric_problem();
        let candidates = Candidates::build(&p, || false);
        for graph in [None, Some(&candidates)] {
            let mut state = State::new(&p, &initial).unwrap();
            let mut rng = Rng::new(17);
            let mut buffers = MoveBuffers::new(p.tickets.len());
            let mut applied = 0;
            for step in 0..2000 {
                let generated = if step % 11 == 0 {
                    buffers.kick(&state, &mut rng)
                } else {
                    buffers.generate(&state, graph, &mut rng)
                };
                if !generated {
                    continue;
                }
                let routes = [
                    (buffers.a, buffers.first.as_slice()),
                    (buffers.b.unwrap_or(buffers.a), buffers.second.as_slice()),
                ];
                let change = Change {
                    routes: &routes[..if buffers.b.is_some() { 2 } else { 1 }],
                };
                let predicted = state.evaluate(change);
                assert_eq!(state.apply(change), predicted);
                let solution = state.solution();
                solution.validate(&p).unwrap();
                assert_eq!(
                    predicted,
                    oracle::evaluate(&p, &solution).score,
                    "step {step}"
                );
                applied += 1;
            }
            assert!(applied > 500);
        }
    }

    #[test]
    fn candidate_cache_is_bounded_and_can_stop_during_construction() {
        let (p, _) = asymmetric_problem();
        let graph = Candidates::build(&p, || false);
        assert_eq!(graph.by_matrix.len(), 2);
        for rows in &graph.by_matrix {
            assert_eq!(rows.len(), p.tickets.len());
            for (t, row) in rows.iter().enumerate() {
                assert_eq!(row.len(), NEIGHBORS);
                assert!(!row.contains(&(t as u32)));
                let mut unique = row.clone();
                unique.sort_unstable();
                unique.dedup();
                assert_eq!(unique.len(), row.len());
            }
        }
        let stopped = Candidates::build(&p, || true);
        assert!(stopped.by_matrix.iter().all(Vec::is_empty));
    }
}
