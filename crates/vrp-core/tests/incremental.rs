//! 差分测试：随机问题 + 随机改动，增量评分必须与 oracle 完全一致。

use vrp_core::constraint::ConstraintId;
use vrp_core::rng::Rng;
use vrp_core::{
    AgentSpec, Change, Matrix, Problem, ProblemSpec, Solution, State, TicketKind, TicketSpec,
    UNREACHABLE, Weight, Weights, oracle,
};

/// 3 种资质里随机挑一些（每种 1/den 的概率），等级 0 到 3。
fn random_levels(rng: &mut Rng, den: usize) -> Vec<(u32, f64)> {
    let mut levels = Vec::new();
    for key in 0..3u32 {
        if rng.chance(1, den) {
            levels.push((key, rng.range(0, 3) as f64));
        }
    }
    levels
}

/// 随机问题：所有约束都打开，权重随机，覆盖多日、多矩阵、不可达边、估算余量、班次结束、关联和依赖。
fn random_problem(rng: &mut Rng) -> Problem {
    let n_locations = 2 + rng.below(12);
    let n_agents = 1 + rng.below(5);
    let n_tickets = rng.below(25);
    let dims = rng.below(3);
    let n_matrices = 1 + rng.below(2);

    let matrices = (0..n_matrices)
        .map(|_| {
            let mut duration = vec![0; n_locations * n_locations];
            let mut distance = vec![0; n_locations * n_locations];
            for i in 0..n_locations {
                for j in 0..n_locations {
                    if i == j {
                        continue;
                    }
                    let k = i * n_locations + j;
                    if rng.chance(1, 40) {
                        duration[k] = UNREACHABLE;
                        distance[k] = UNREACHABLE;
                    } else {
                        duration[k] = rng.range(30, 4_000) as u32;
                        distance[k] = rng.range(100, 60_000) as u32;
                    }
                }
            }
            // 一半的矩阵带估算余量：约三分之一的边放大 0–50%。
            let buffer_permille = if rng.chance(1, 2) {
                (0..n_locations * n_locations)
                    .map(|_| {
                        if rng.chance(1, 3) {
                            rng.range(0, 500) as u16
                        } else {
                            0
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };
            Matrix {
                n: n_locations,
                duration,
                distance,
                buffer_permille,
            }
        })
        .collect();

    let mut weights = Weights::none();
    for c in ConstraintId::ALL {
        let mut pick = || {
            if rng.chance(1, 2) {
                rng.range(0, 50)
            } else {
                0
            }
        };
        weights.set(
            c,
            Weight {
                hard: pick(),
                medium: pick(),
                soft: pick(),
            },
        );
    }

    let agents = (0..n_agents)
        .map(|a| {
            let start_time = rng.range(0, 30_000) + if rng.chance(1, 2) { 86_400 } else { 0 };
            AgentSpec {
                id: format!("a{a}"),
                day: rng.below(2) as i32,
                start_location: rng.below(n_locations) as u32,
                end_location: rng.below(n_locations) as u32,
                start_time,
                end_time: rng.chance(2, 3).then(|| start_time + rng.range(0, 40_000)),
                capacity: (0..dims).map(|_| rng.range(0, 60)).collect(),
                tolerance_permille: (0..dims).map(|_| rng.range(0, 300) as u32).collect(),
                max_tickets: rng.below(6) as u32,
                fixed_cost: rng.range(0, 500),
                restricted: rng.chance(1, 3),
                skills: (0..4u32).filter(|_| rng.chance(1, 2)).collect(),
                qualifications: random_levels(rng, 2),
                depot: rng.chance(2, 3).then(|| rng.below(2) as u32),
                matrix: rng.below(n_matrices) as u32,
            }
        })
        .collect();

    let tickets = (0..n_tickets)
        .map(|t| {
            let earliest = rng.range(0, 120_000);
            let latest_start = earliest + rng.range(0, 20_000);
            let original_agent = rng.chance(1, 3).then(|| rng.below(n_agents) as u32);
            TicketSpec {
                id: format!("t{t}"),
                location: rng.below(n_locations) as u32,
                kind: match rng.below(3) {
                    0 => TicketKind::Service,
                    1 => TicketKind::Delivery,
                    _ => TicketKind::Pickup,
                },
                duration: rng.range(0, 5_000),
                earliest_start: earliest,
                latest_start,
                latest_end: latest_start + rng.range(0, 8_000),
                day: rng.below(2) as i32,
                demand: (0..dims).map(|_| rng.range(0, 20)).collect(),
                skills_required: (0..4u32).filter(|_| rng.chance(1, 4)).collect(),
                qualification_required: random_levels(rng, 4),
                depot: rng.chance(1, 2).then(|| rng.below(2) as u32),
                original_agent,
                pinned: original_agent.is_some() && rng.chance(1, 4),
            }
        })
        .collect();

    let mut pairs = |count: usize| -> Vec<(u32, u32)> {
        if n_tickets < 2 {
            return Vec::new();
        }
        (0..count)
            .filter_map(|_| {
                let (a, b) = (rng.below(n_tickets) as u32, rng.below(n_tickets) as u32);
                (a != b).then_some((a, b))
            })
            .collect()
    };
    let related_pairs = pairs(n_tickets / 2);
    // Business dependencies must be acyclic; randomized route order can still
    // create joint graph cycles, exercising their bounded violation handling.
    let precedences = pairs(n_tickets / 3)
        .into_iter()
        .map(|(a, b)| (a.min(b), a.max(b)))
        .collect();

    Problem::new(ProblemSpec {
        execution: None,
        dims,
        dim_penalty_unit: (0..dims).map(|_| rng.range(1, 5)).collect(),
        matrices,
        agents,
        tickets,
        related_pairs,
        precedences,
        weights,
        restricted_charge_per_hour: rng.range(0, 100),
        day_starts: if rng.chance(1, 2) {
            vec![]
        } else {
            vec![0, 80_000, 170_000]
        },
    })
    .unwrap()
}

fn random_solution(rng: &mut Rng, p: &Problem) -> Solution {
    let mut solution = Solution::empty(p.agents.len());
    for t in 0..p.tickets.len() as u32 {
        if p.tickets[t as usize].pinned {
            solution.routes[p.tickets[t as usize].original_agent.unwrap() as usize].push(t);
        } else if rng.chance(3, 4) {
            let route = &mut solution.routes[rng.below(p.agents.len())];
            let at = rng.below(route.len() + 1);
            route.insert(at, t);
        }
    }
    solution
}

/// 随机改动：派/撤一张工单、跨路线移动、交换、路线内反转一段。
fn random_change(rng: &mut Rng, state: &State<'_>) -> Vec<(u32, Vec<u32>)> {
    let p = state.problem();
    let n_agents = p.agents.len() as u32;
    let a = rng.below(n_agents as usize) as u32;
    let b = rng.below(n_agents as usize) as u32;
    let mut ra = state.route(a).to_vec();
    match rng.below(5) {
        0 => {
            // 派一张未派工单。
            let unassigned: Vec<u32> = (0..p.tickets.len() as u32)
                .filter(|&t| state.agent_of(t).is_none())
                .collect();
            if !unassigned.is_empty() {
                let t = unassigned[rng.below(unassigned.len())];
                let at = rng.below(ra.len() + 1);
                ra.insert(at, t);
            }
            vec![(a, ra)]
        }
        1 => {
            // 撤掉一张。
            if !ra.is_empty() {
                let i = rng.below(ra.len());
                if !p.tickets[ra[i] as usize].pinned {
                    ra.remove(i);
                }
            }
            vec![(a, ra)]
        }
        2 if a != b && !ra.is_empty() => {
            // 从 a 移到 b。
            let i = rng.below(ra.len());
            if p.tickets[ra[i] as usize].pinned {
                return vec![(a, ra)];
            }
            let t = ra.remove(i);
            let mut rb = state.route(b).to_vec();
            let at = rng.below(rb.len() + 1);
            rb.insert(at, t);
            vec![(a, ra), (b, rb)]
        }
        3 if a != b => {
            // 交换 a、b 各一张。
            let mut rb = state.route(b).to_vec();
            if !ra.is_empty() && !rb.is_empty() {
                let (i, j) = (rng.below(ra.len()), rng.below(rb.len()));
                if !p.tickets[ra[i] as usize].pinned && !p.tickets[rb[j] as usize].pinned {
                    std::mem::swap(&mut ra[i], &mut rb[j]);
                }
            }
            vec![(a, ra), (b, rb)]
        }
        _ => {
            // 路线内反转一段。
            if ra.len() > 1 {
                let i = rng.below(ra.len());
                let j = rng.below(ra.len());
                ra[i.min(j)..=i.max(j)].reverse();
            }
            vec![(a, ra)]
        }
    }
}

#[test]
fn incremental_score_matches_oracle() {
    let mut rng = Rng::new(20_261_028);
    let mut checked = 0;
    for _ in 0..300 {
        let p = random_problem(&mut rng);
        let initial = random_solution(&mut rng, &p);
        let mut state = State::new(&p, &initial).unwrap();
        assert_eq!(state.score(), oracle::evaluate(&p, &initial).score);
        for _ in 0..60 {
            let change = random_change(&mut rng, &state);
            let borrowed: Vec<(u32, &[u32])> =
                change.iter().map(|(a, r)| (*a, r.as_slice())).collect();
            let predicted = state.evaluate(Change { routes: &borrowed });
            let applied = state.apply(Change { routes: &borrowed });
            assert_eq!(predicted, applied, "evaluate and apply disagree");
            let expected = oracle::evaluate(&p, &state.solution()).score;
            assert_eq!(
                applied, expected,
                "incremental score drifted from the oracle"
            );
            checked += 1;
        }
        assert_eq!(state.recompute(), state.score());
    }
    assert_eq!(checked, 300 * 60);
}

#[test]
fn per_constraint_totals_add_up_to_the_score() {
    let mut rng = Rng::new(7);
    for _ in 0..200 {
        let p = random_problem(&mut rng);
        let solution = random_solution(&mut rng, &p);
        let e = oracle::evaluate(&p, &solution);
        let sum: vrp_core::Score = e.totals.iter().map(|t| t.score).sum();
        assert_eq!(sum, e.score);
        let explained: vrp_core::Score = e.matches.iter().map(|m| m.score).sum();
        assert_eq!(explained, e.score, "matches must explain the whole score");
    }
}
