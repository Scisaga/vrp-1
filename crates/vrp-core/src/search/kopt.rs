//! Variable-depth exchanges, including non-sequential moves obtained by patching
//! disconnected closures. Only a complete tour is evaluated with the directed
//! business score. Removed/added edges are disjoint; no distance-gain pruning.
use super::neighborhood::Candidates;
use crate::rng::Rng;
use crate::{Change, State};

mod patch;

const NONE: usize = usize::MAX;
const BREADTH: usize = 8;
const BRANCH_BUDGET: usize = 512;
const PATCH_BUDGET: usize = 256;
const MAX_DEPTH: usize = 12;

pub(super) struct KOpt {
    nodes: Vec<u32>,
    position: Vec<usize>,
    adjacency: Vec<[usize; 2]>,
    candidates: Vec<Vec<usize>>,
    used: Vec<bool>,
    visited: Vec<bool>,
    tour: Vec<u32>,
    result: Vec<u32>,
    ranked: Vec<(u32, usize)>,
    patch_levels: Vec<patch::PatchLevel>,
    pub(super) patching_attempts: u64,
    pub(super) patched_improvements: u64,
}

struct Context<'a, 'p, F> {
    state: &'a mut State<'p>,
    stop: &'a mut F,
    evaluated: &'a mut u64,
    remaining: usize,
    patch_remaining: usize,
    max_depth: usize,
    agent: u32,
    first: usize,
}

impl KOpt {
    pub(super) fn new(tickets: usize) -> Self {
        Self {
            nodes: Vec::with_capacity(tickets),
            position: vec![NONE; tickets],
            adjacency: vec![[NONE; 2]; tickets + 1],
            candidates: (0..=tickets)
                .map(|_| Vec::with_capacity(BREADTH + 1))
                .collect(),
            used: vec![false; tickets + 1],
            visited: vec![false; tickets + 1],
            tour: Vec::with_capacity(tickets),
            result: Vec::with_capacity(tickets),
            ranked: Vec::with_capacity(tickets),
            patch_levels: (0..MAX_DEPTH / 2)
                .map(|_| patch::PatchLevel::new(tickets + 1))
                .collect(),
            patching_attempts: 0,
            patched_improvements: 0,
        }
    }

    fn prepare(&mut self, state: &State<'_>, graph: &Candidates, agent: u32) {
        self.nodes.clear();
        self.nodes.extend_from_slice(state.route(agent));
        let n = self.nodes.len();
        self.position.fill(NONE);
        self.used[..=n].fill(false);
        for (i, &t) in self.nodes.iter().enumerate() {
            self.position[t as usize] = i;
        }
        let p = state.problem();
        let a = &p.agents[agent as usize];
        let m = &p.matrices[a.matrix as usize];
        for i in 0..=n {
            self.adjacency[i] = [(i + n) % (n + 1), (i + 1) % (n + 1)];
            let row = &mut self.candidates[i];
            row.clear();
            if i < n {
                for &t in graph.neighbors(a.matrix, self.nodes[i]) {
                    let pos = self.position[t as usize];
                    if pos != NONE && pos != i {
                        row.push(pos);
                    }
                    if row.len() == BREADTH {
                        break;
                    }
                }
            }
            if row.is_empty() {
                self.ranked.clear();
                for (j, &t) in self.nodes.iter().enumerate() {
                    if i == j {
                        continue;
                    }
                    let to = p.tickets[t as usize].location;
                    let c = if i == n {
                        m.duration(a.start_location, to)
                            .min(m.duration(to, a.end_location))
                    } else {
                        let from = p.tickets[self.nodes[i] as usize].location;
                        m.duration(from, to).min(m.duration(to, from))
                    };
                    self.ranked.push((c, j));
                }
                self.ranked.sort_unstable();
                row.extend(self.ranked.iter().take(BREADTH).map(|&(_, j)| j));
            }
            if i < n {
                row.push(n);
            } // virtual depot closes the open route
        }
    }

    fn original_edge(&self, a: usize, b: usize) -> bool {
        let n = self.nodes.len() + 1;
        (a + 1) % n == b || (b + 1) % n == a
    }

    fn remove(&mut self, a: usize, b: usize) {
        let i = self.adjacency[a]
            .iter()
            .position(|&v| v == b)
            .expect("existing edge");
        let j = self.adjacency[b]
            .iter()
            .position(|&v| v == a)
            .expect("existing edge");
        self.adjacency[a][i] = NONE;
        self.adjacency[b][j] = NONE;
    }

    fn add(&mut self, a: usize, b: usize) {
        let i = self.adjacency[a]
            .iter()
            .position(|&v| v == NONE)
            .expect("free endpoint");
        let j = self.adjacency[b]
            .iter()
            .position(|&v| v == NONE)
            .expect("free endpoint");
        self.adjacency[a][i] = b;
        self.adjacency[b][j] = a;
    }

    fn closing_tour(&mut self) -> bool {
        let depot = self.nodes.len();
        self.visited[..=depot].fill(false);
        self.visited[depot] = true;
        self.tour.clear();
        let mut previous = depot;
        let mut current = self.adjacency[depot][0];
        while current != depot {
            if current == NONE || self.visited[current] {
                return false;
            }
            self.visited[current] = true;
            self.tour.push(self.nodes[current]);
            let next = if self.adjacency[current][0] == previous {
                self.adjacency[current][1]
            } else {
                self.adjacency[current][0]
            };
            previous = current;
            current = next;
        }
        self.tour.len() == depot
    }

    fn evaluate_tour<F: FnMut() -> bool>(&mut self, ctx: &mut Context<'_, '_, F>) -> bool {
        for _ in 0..2 {
            if (ctx.stop)() {
                return false;
            }
            let score = ctx.state.evaluate(Change {
                routes: &[(ctx.agent, &self.tour)],
            });
            *ctx.evaluated += 1;
            if score > ctx.state.score() {
                self.result.clear();
                self.result.extend_from_slice(&self.tour);
                return true;
            }
            self.tour.reverse();
        }
        false
    }

    fn visit<F: FnMut() -> bool>(
        &mut self,
        ctx: &mut Context<'_, '_, F>,
        last: usize,
        depth: usize,
    ) -> bool {
        if depth >= ctx.max_depth || ctx.remaining == 0 || (ctx.stop)() {
            return false;
        }
        for c in 0..self.candidates[last].len() {
            let next = self.candidates[last][c];
            if self.used[next] || self.original_edge(last, next) {
                continue;
            }
            let n = self.nodes.len() + 1;
            for end in [(next + n - 1) % n, (next + 1) % n] {
                if self.used[end] {
                    continue;
                }
                if ctx.remaining == 0 || (ctx.stop)() {
                    return false;
                }
                ctx.remaining -= 1;
                self.used[next] = true;
                self.used[end] = true;
                self.remove(next, end);
                self.add(last, next);
                let mut improved = false;
                if !self.original_edge(end, ctx.first) {
                    self.add(end, ctx.first);
                    if self.closing_tour() {
                        improved = self.evaluate_tour(ctx);
                    } else if depth + 3 <= ctx.max_depth && ctx.patch_remaining > 0 {
                        // A closed sequential exchange may create several cycles.
                        // Keep it tentative and search additional alternating cycles.
                        self.patching_attempts += 1;
                        improved = self.patch_components(ctx, depth + 1, 0);
                        if improved {
                            self.patched_improvements += 1;
                        }
                    }
                    self.remove(end, ctx.first);
                }
                // A worse or disconnected closure can still lead to a better deeper exchange.
                if !improved {
                    improved = self.visit(ctx, end, depth + 1);
                }
                self.remove(last, next);
                self.add(next, end);
                self.used[next] = false;
                self.used[end] = false;
                if improved {
                    return true;
                }
            }
        }
        false
    }

    pub(super) fn improve(
        &mut self,
        state: &mut State<'_>,
        graph: &Candidates,
        rng: &mut Rng,
        max_depth: usize,
        mut stop: impl FnMut() -> bool,
        evaluated: &mut u64,
    ) -> bool {
        let t = rng.below(state.problem().tickets.len()) as u32;
        let Some(agent) = state.agent_of(t) else {
            return false;
        };
        if state.route(agent).len() < 3 || stop() {
            return false;
        }
        self.prepare(state, graph, agent);
        let n = self.nodes.len() + 1;
        let first = rng.below(n);
        let mut ctx = Context {
            state,
            stop: &mut stop,
            evaluated,
            remaining: BRANCH_BUDGET,
            patch_remaining: PATCH_BUDGET,
            // Patching can cut another original edge incident to a used node.
            // Its total edge budget is not restricted to distinct endpoint pairs.
            max_depth: max_depth.min(MAX_DEPTH).min(n),
            agent,
            first,
        };
        self.used[first] = true;
        for second in [(first + n - 1) % n, (first + 1) % n] {
            self.used[second] = true;
            self.remove(first, second);
            let improved = self.visit(&mut ctx, second, 1);
            self.add(first, second);
            self.used[second] = false;
            if improved {
                ctx.state.apply(Change {
                    routes: &[(agent, &self.result)],
                });
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentSpec, ConstraintId as C, Matrix, Problem, ProblemSpec, Solution, TicketSpec, Weight,
        Weights, oracle,
    };

    fn cycle_problem(n: usize, cheap: &[(usize, usize)]) -> (Problem, Solution) {
        let mut costs = vec![1000; n * n];
        for a in 0..n {
            costs[a * n + a] = 0;
            let b = (a + 1) % n;
            costs[a * n + b] = 10;
            costs[b * n + a] = 10;
        }
        for &(a, b) in cheap {
            costs[a * n + b] = 1;
            costs[b * n + a] = 1;
        }
        let p = Problem::new(ProblemSpec {
            matrices: vec![Matrix {
                n,
                duration: costs.clone(),
                distance: costs,
                buffer_permille: vec![],
            }],
            agents: vec![AgentSpec {
                start_location: (n - 1) as u32,
                end_location: (n - 1) as u32,
                ..AgentSpec::default()
            }],
            tickets: (0..n - 1)
                .map(|i| TicketSpec {
                    location: i as u32,
                    original_agent: Some(0),
                    pinned: true,
                    ..TicketSpec::default()
                })
                .collect(),
            weights: Weights::none().with(C::MinimizeTravelTime, Weight::soft(1)),
            ..ProblemSpec::default()
        })
        .unwrap();
        (
            p,
            Solution {
                routes: vec![(0..n as u32 - 1).collect()],
            },
        )
    }

    fn candidate_tours(
        values: &mut [u32],
        at: usize,
        allowed: &impl Fn(u32, u32) -> bool,
        f: &mut impl FnMut(&[u32]),
    ) {
        if at == values.len() {
            if allowed(values[at - 1], at as u32) {
                f(values);
            }
        } else {
            let previous = if at == 0 {
                values.len() as u32
            } else {
                values[at - 1]
            };
            for i in at..values.len() {
                if !allowed(previous, values[i]) {
                    continue;
                }
                values.swap(at, i);
                candidate_tours(values, at + 1, allowed, f);
                values.swap(at, i);
            }
        }
    }

    // Independent graph check on complete tours, without the search adjacency.
    fn exchange_shape(route: &[u32]) -> (usize, usize) {
        use std::collections::BTreeSet;
        let n = route.len() + 1;
        let edge = |a: usize, b: usize| (a.min(b), a.max(b));
        let original: BTreeSet<_> = (0..n).map(|i| edge(i, (i + 1) % n)).collect();
        let cycle: Vec<_> = std::iter::once(n - 1)
            .chain(route.iter().map(|&t| t as usize))
            .collect();
        let changed: BTreeSet<_> = (0..n).map(|i| edge(cycle[i], cycle[(i + 1) % n])).collect();
        let cuts = original.difference(&changed).count();
        let mut adjacency = vec![Vec::new(); n];
        for &(a, b) in original.symmetric_difference(&changed) {
            adjacency[a].push(b);
            adjacency[b].push(a);
        }
        let mut seen = vec![false; n];
        let mut components = 0;
        for first in 0..n {
            if seen[first] || adjacency[first].is_empty() {
                continue;
            }
            components += 1;
            let mut stack = vec![first];
            while let Some(node) = stack.pop() {
                if !seen[node] {
                    seen[node] = true;
                    stack.extend(adjacency[node].iter().copied());
                }
            }
        }
        (cuts, components)
    }

    #[test]
    fn non_sequential_four_opt_escapes_a_sequential_local_minimum() {
        let (p, initial) = cycle_problem(12, &[(0, 7), (1, 6), (3, 10), (4, 9)]);
        let score = oracle::evaluate(&p, &initial).score;
        assert_eq!(score.soft, -120);
        let mut better_tours = 0;
        // Exhaust all tours containing only original/cheap edges. Any other
        // edge costs 1000 (> the initial tour), so cannot improve. Every better
        // tour requires two disjoint alternating cycles, not a sequential move.
        candidate_tours(
            &mut initial.routes[0].clone(),
            0,
            &|a, b| p.matrices[0].duration(a, b) < 120,
            &mut |route| {
                let candidate = Solution {
                    routes: vec![route.to_vec()],
                };
                if oracle::evaluate(&p, &candidate).score > score {
                    assert_eq!(exchange_shape(route), (4, 2));
                    better_tours += 1;
                }
            },
        );
        assert_eq!(better_tours, 2); // The same undirected tour in both directions.
        let graph = Candidates::build(&p, || false);
        let mut kopt = KOpt::new(11);
        let mut evaluated = 0;
        let mut state = State::new(&p, &initial).unwrap();
        assert!(!kopt.improve(
            &mut state,
            &graph,
            &mut Rng::new(0),
            3,
            || false,
            &mut evaluated
        ));
        for seed in 0..16 {
            if kopt.improve(
                &mut state,
                &graph,
                &mut Rng::new(seed),
                4,
                || false,
                &mut evaluated,
            ) {
                break;
            }
        }
        assert_eq!(state.score().soft, -84);
        assert_eq!(kopt.patched_improvements, 1);
        assert!(kopt.patching_attempts > 0);
        assert_eq!(exchange_shape(state.route(0)), (4, 2));
        state.solution().validate(&p).unwrap();
        assert_eq!(state.score(), oracle::evaluate(&p, &state.solution()).score);
    }

    #[test]
    fn cheaper_patched_routes_cannot_override_hard_time_windows() {
        let (mut p, initial) = cycle_problem(12, &[(0, 7), (1, 6), (3, 10), (4, 9)]);
        p.weights = p
            .weights
            .with(C::TicketStartServiceTimeMatchExpected, Weight::hard(1));
        for (i, ticket) in p.tickets.iter_mut().enumerate() {
            ticket.earliest_start = (i as i64 + 1) * 10;
            ticket.latest_start = ticket.earliest_start;
        }
        let score = oracle::evaluate(&p, &initial).score;
        assert_eq!(score.hard, 0);
        // Both cheaper orientations violate at least one hard window.
        let mut cheaper = 0;
        candidate_tours(
            &mut initial.routes[0].clone(),
            0,
            &|a, b| p.matrices[0].duration(a, b) < 120,
            &mut |route| {
                let result = oracle::evaluate(
                    &p,
                    &Solution {
                        routes: vec![route.to_vec()],
                    },
                );
                if result.score.soft > score.soft {
                    cheaper += 1;
                    assert!(result.score.hard < 0);
                }
            },
        );
        assert_eq!(cheaper, 2);
        let graph = Candidates::build(&p, || false);
        let mut kopt = KOpt::new(11);
        let mut state = State::new(&p, &initial).unwrap();
        let mut evaluated = 0;
        for seed in 0..16 {
            assert!(!kopt.improve(
                &mut state,
                &graph,
                &mut Rng::new(seed),
                12,
                || false,
                &mut evaluated
            ));
        }
        assert!(kopt.patching_attempts > 0);
        assert_eq!(kopt.patched_improvements, 0);
        assert_eq!(state.solution(), initial);
        assert_eq!(state.score(), score);
    }

    fn three_cycles(kopt: &mut KOpt, state: &State<'_>, graph: &Candidates) {
        kopt.prepare(state, graph, 0);
        for (a, b) in [(3, 4), (7, 8), (11, 0)] {
            kopt.remove(a, b);
        }
        for (a, b) in [(3, 0), (7, 4), (11, 8)] {
            kopt.add(a, b);
        }
        assert!(!kopt.closing_tour());
    }

    fn edges(kopt: &KOpt) -> Vec<[usize; 2]> {
        kopt.adjacency[..=kopt.nodes.len()]
            .iter()
            .map(|row| {
                let mut pair = *row;
                pair.sort_unstable();
                pair
            })
            .collect()
    }

    #[test]
    fn one_alternating_cycle_patches_three_subtours_with_six_total_cuts() {
        let (p, initial) = cycle_problem(12, &[(3, 0), (7, 4), (11, 8), (1, 4), (5, 8), (9, 0)]);
        let graph = Candidates::build(&p, || false);
        let mut state = State::new(&p, &initial).unwrap();
        let mut kopt = KOpt::new(11);
        three_cycles(&mut kopt, &state, &graph);
        let before = edges(&kopt);
        let mut evaluated = 0;
        let mut ctx = Context {
            state: &mut state,
            stop: &mut || false,
            evaluated: &mut evaluated,
            remaining: 0,
            patch_remaining: PATCH_BUDGET,
            max_depth: 6,
            agent: 0,
            first: 0,
        };
        assert!(kopt.patch_components(&mut ctx, 3, 0));
        assert_eq!(edges(&kopt), before);
        state.apply(Change {
            routes: &[(0, &kopt.result)],
        });
        assert_eq!(state.score().soft, -66);
        assert_eq!(exchange_shape(state.route(0)).0, 6);
        assert_eq!(state.score(), oracle::evaluate(&p, &state.solution()).score);
        state.solution().validate(&p).unwrap();
    }

    #[test]
    fn multiple_patches_respect_total_depth_budget_and_restore_on_cancellation() {
        let bridges = [(0, 5), (1, 4), (2, 9), (3, 8)];
        let mut cheap = vec![(3, 0), (7, 4), (11, 8)];
        cheap.extend(bridges);
        let (p, initial) = cycle_problem(12, &cheap);
        let graph = Candidates::build(&p, || false);
        let mut kopt = KOpt::new(11);
        let mut accepted = 0;
        for (depth, budget, stop_at) in [(6, PATCH_BUDGET, usize::MAX), (7, 3, usize::MAX)]
            .into_iter()
            .chain((1..100).map(|s| (7, PATCH_BUDGET, s)))
            .chain([(7, PATCH_BUDGET, usize::MAX)])
        {
            let mut state = State::new(&p, &initial).unwrap();
            three_cycles(&mut kopt, &state, &graph);
            // No B-C candidate edge: a single alternating cycle cannot join all
            // three components. Two separate patch cycles are necessary.
            for row in &mut kopt.candidates {
                row.clear();
            }
            for (a, b) in bridges {
                kopt.candidates[a].push(b);
                kopt.candidates[b].push(a);
            }
            let before = edges(&kopt);
            let mut calls = 0;
            let mut stop = || {
                calls += 1;
                calls >= stop_at
            };
            let mut evaluated = 0;
            let mut ctx = Context {
                state: &mut state,
                stop: &mut stop,
                evaluated: &mut evaluated,
                remaining: 0,
                patch_remaining: budget,
                max_depth: depth,
                agent: 0,
                first: 0,
            };
            let improved = kopt.patch_components(&mut ctx, 3, 0);
            assert_eq!(edges(&kopt), before);
            assert_eq!(state.solution(), initial); // Nothing committed mid-search.
            if depth == 6 || budget == 3 {
                assert!(!improved);
            }
            if stop_at == usize::MAX && depth == 7 && budget == PATCH_BUDGET {
                assert!(improved);
            }
            if improved {
                accepted += 1;
                state.apply(Change {
                    routes: &[(0, &kopt.result)],
                });
                state.solution().validate(&p).unwrap();
                assert_eq!(state.score().soft, -57);
                assert_eq!(exchange_shape(state.route(0)).0, 7);
                assert_eq!(state.score(), oracle::evaluate(&p, &state.solution()).score);
            }
        }
        assert!(accepted > 0);
    }

    fn three_opt_trap() -> (Problem, Solution) {
        let n = 6;
        let mut costs = vec![100; n * n];
        for a in 0..n {
            costs[a * n + a] = 0;
            let b = (a + 1) % n;
            costs[a * n + b] = 10;
            costs[b * n + a] = 10;
        }
        for (a, b) in [(1, 3), (2, 5), (4, 0)] {
            costs[a * n + b] = 1;
            costs[b * n + a] = 1;
        }
        let p = Problem::new(ProblemSpec {
            matrices: vec![Matrix {
                n,
                duration: costs.clone(),
                distance: costs,
                buffer_permille: vec![],
            }],
            agents: vec![AgentSpec::default()],
            tickets: (1..n)
                .map(|i| TicketSpec {
                    location: i as u32,
                    original_agent: Some(0),
                    pinned: true,
                    ..TicketSpec::default()
                })
                .collect(),
            weights: Weights::none().with(C::MinimizeTravelTime, Weight::soft(1)),
            ..ProblemSpec::default()
        })
        .unwrap();
        (
            p,
            Solution {
                routes: vec![(0..5).collect()],
            },
        )
    }

    #[test]
    fn three_opt_crosses_a_two_opt_local_minimum_without_moving_pins() {
        let (p, initial) = three_opt_trap();
        let initial_score = oracle::evaluate(&p, &initial).score;
        assert_eq!(initial_score.soft, -60);
        // Exhaustively verify no single segment reversal improves this route.
        for a in 0..5 {
            for b in a + 1..5 {
                let mut s = initial.clone();
                s.routes[0][a..=b].reverse();
                assert!(oracle::evaluate(&p, &s).score <= initial_score);
            }
        }
        let graph = Candidates::build(&p, || false);
        let mut moves = 0;
        let mut kopt = KOpt::new(5);
        let mut state = State::new(&p, &initial).unwrap();
        assert!(!kopt.improve(
            &mut state,
            &graph,
            &mut Rng::new(0),
            2,
            || false,
            &mut moves
        ));
        assert!(kopt.improve(
            &mut state,
            &graph,
            &mut Rng::new(0),
            3,
            || false,
            &mut moves
        ));
        state.solution().validate(&p).unwrap();
        assert_eq!(state.score().soft, -33);
        assert_eq!(state.score(), oracle::evaluate(&p, &state.solution()).score);
        assert!(moves > 0);
    }

    #[test]
    fn cancelled_exchange_does_not_mutate_the_current_solution() {
        let (p, initial) = three_opt_trap();
        let graph = Candidates::build(&p, || false);
        let mut state = State::new(&p, &initial).unwrap();
        let mut calls = 0;
        let mut moves = 0;
        KOpt::new(5).improve(
            &mut state,
            &graph,
            &mut Rng::new(0),
            5,
            || {
                calls += 1;
                calls > 4
            },
            &mut moves,
        );
        assert_eq!(state.solution(), initial);
        assert_eq!(state.score(), oracle::evaluate(&p, &initial).score);
    }

    #[test]
    fn directed_matrix_moves_are_monotone_and_match_the_full_oracle() {
        let mut rng = Rng::new(981);
        for _ in 0..12 {
            let n = 10;
            let costs: Vec<u32> = (0..n * n)
                .map(|i| {
                    if i / n == i % n {
                        0
                    } else {
                        rng.below(1000) as u32
                    }
                })
                .collect();
            let p = Problem::new(ProblemSpec {
                matrices: vec![Matrix {
                    n,
                    duration: costs.clone(),
                    distance: costs,
                    buffer_permille: vec![],
                }],
                agents: vec![AgentSpec::default()],
                tickets: (1..n)
                    .map(|i| TicketSpec {
                        location: i as u32,
                        ..TicketSpec::default()
                    })
                    .collect(),
                weights: Weights::default(),
                ..ProblemSpec::default()
            })
            .unwrap();
            let mut state = State::new(
                &p,
                &Solution {
                    routes: vec![(0..9).collect()],
                },
            )
            .unwrap();
            let graph = Candidates::build(&p, || false);
            let mut kopt = KOpt::new(9);
            let mut evaluated = 0;
            for _ in 0..30 {
                let before = state.score();
                let depth = 2 + rng.below(11);
                kopt.improve(
                    &mut state,
                    &graph,
                    &mut rng,
                    depth,
                    || false,
                    &mut evaluated,
                );
                state.solution().validate(&p).unwrap();
                assert!(state.score() >= before);
                assert_eq!(state.score(), oracle::evaluate(&p, &state.solution()).score);
            }
        }
    }
}
