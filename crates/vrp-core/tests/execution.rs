use std::sync::atomic::AtomicBool;
use std::time::Duration;
use vrp_core::{execution::*, *};

fn problem() -> Problem {
    Problem::new(ProblemSpec {
        dims: 1,
        matrices: vec![Matrix {
            n: 4,
            duration: vec![10; 16],
            distance: vec![100; 16],
            buffer_permille: vec![],
        }],
        agents: (0..2)
            .map(|i| AgentSpec {
                id: format!("a{i}"),
                capacity: vec![10],
                max_tickets: 5,
                fixed_cost: 100,
                ..Default::default()
            })
            .collect(),
        tickets: vec![
            TicketSpec {
                id: "delivery".into(),
                kind: TicketKind::Delivery,
                demand: vec![3],
                location: 1,
                ..Default::default()
            },
            TicketSpec {
                id: "pickup".into(),
                kind: TicketKind::Pickup,
                demand: vec![4],
                location: 2,
                ..Default::default()
            },
            TicketSpec {
                id: "committed".into(),
                original_agent: Some(0),
                pinned: true,
                duration: 60,
                location: 1,
                ..Default::default()
            },
            TicketSpec {
                id: "load".into(),
                kind: TicketKind::Pickup,
                demand: vec![2],
                ..Default::default()
            },
            TicketSpec {
                id: "new-delivery".into(),
                kind: TicketKind::Delivery,
                demand: vec![2],
                location: 3,
                ..Default::default()
            },
        ],
        execution: Some(ExecutionSpec {
            routes: vec![
                (
                    0,
                    ExecutedRoute {
                        initial_load: Some(vec![5]),
                        completed_tickets: 3,
                        worked_seconds: 4000,
                        delivered: vec![2],
                        fixed_cost_paid: true,
                        locked_prefix: vec![2],
                        ..Default::default()
                    },
                ),
                (
                    1,
                    ExecutedRoute {
                        initial_load: Some(vec![0]),
                        ..Default::default()
                    },
                ),
            ],
            tickets: vec![
                (
                    0,
                    ExecutionTicket {
                        only_agent: Some(0),
                        original_position: Some(1),
                        original_start: Some(100),
                        ..Default::default()
                    },
                ),
                (
                    3,
                    ExecutionTicket {
                        auxiliary: true,
                        ..Default::default()
                    },
                ),
                (
                    4,
                    ExecutionTicket {
                        load_before: Some(3),
                        ..Default::default()
                    },
                ),
            ],
            past_dependencies: vec![PastDependency {
                ticket: 1,
                agent: 1,
                finished_at: 200,
                same_agent: true,
                after_finish: true,
                related: false,
            }],
        }),
        weights: Weights::default()
            .with(ConstraintId::BalanceAgentWorkingTime, Weight::soft(2))
            .with(ConstraintId::RefTicketAfterDepTicket, Weight::medium(3))
            .with(
                ConstraintId::RefTicketSameAgentWithDepTicket,
                Weight::soft(4),
            ),
        ..Default::default()
    })
    .unwrap()
}

#[test]
fn live_moves_preserve_locks_and_match_the_independent_oracle() {
    let p = problem();
    let initial = Solution {
        routes: vec![vec![2, 0, 1], vec![3, 4]],
    };
    let mut state = State::new(&p, &initial).unwrap();
    assert_eq!(state.score(), oracle::evaluate(&p, &initial).score);
    let before = state.solution();
    state.apply(Change {
        routes: &[(0, &[0, 2, 1])],
    });
    assert_eq!(state.solution(), before);
    let mut rng = rng::Rng::new(42);
    for _ in 0..1000 {
        let mut candidate = state.solution();
        let t = rng.below(5) as u32;
        if t == 2 {
            continue;
        }
        for route in &mut candidate.routes {
            route.retain(|v| *v != t);
        }
        let a = rng.below(2);
        let pos = rng.below(candidate.routes[a].len() + 1);
        candidate.routes[a].insert(pos, t);
        let change = Change {
            routes: &[(0, &candidate.routes[0]), (1, &candidate.routes[1])],
        };
        if candidate.validate(&p).is_err() {
            let old = state.solution();
            state.apply(change);
            assert_eq!(state.solution(), old);
            continue;
        }
        let expected = oracle::evaluate(&p, &candidate).score;
        assert_eq!(state.evaluate(change), expected);
        assert_eq!(state.apply(change), expected);
    }
}

#[test]
fn cargo_cannot_appear_on_a_departed_vehicle_and_helpers_do_not_count_as_orders() {
    let p = problem();
    let invalid = Solution {
        routes: vec![vec![2, 0], vec![4]],
    };
    assert!(
        oracle::evaluate(&p, &invalid)
            .total(ConstraintId::ExecutionConsistency)
            .amount
            > 0
    );
    let valid = Solution {
        routes: vec![vec![2, 0], vec![3, 4]],
    };
    let evaluation = oracle::evaluate(&p, &valid);
    assert_eq!(
        evaluation.total(ConstraintId::ExecutionConsistency).amount,
        0
    );
    assert_eq!(evaluation.total(ConstraintId::AgentMaxTicket).amount, 0);
    assert_eq!(
        evaluation.total(ConstraintId::BalanceAgentLoading).amount,
        26
    ); // (3+2)^2 + 1^2
    assert_eq!(evaluation.routes[0].peak_load, vec![5]);
    assert_eq!(State::new(&p, &valid).unwrap().score(), evaluation.score);
}

#[test]
fn all_search_strategies_keep_the_committed_prefix() {
    let p = problem();
    for strategy in [
        SearchStrategy::Baseline,
        SearchStrategy::Candidate,
        SearchStrategy::Alpha,
        SearchStrategy::AlphaKOpt,
    ] {
        let result = solve(
            &p,
            None,
            &SearchOptions {
                threads: 2,
                time_limit: Some(Duration::from_millis(35)),
                strategy,
                ..Default::default()
            },
            &AtomicBool::new(false),
        )
        .unwrap();
        result.solution.validate(&p).unwrap();
        assert_eq!(result.solution.routes[0][0], 2);
        assert_eq!(result.score, oracle::evaluate(&p, &result.solution).score);
    }
}

#[test]
fn unavailable_empty_vehicle_still_counts_real_cargo_and_work() {
    let p = Problem::new(ProblemSpec {
        dims: 1,
        matrices: vec![Matrix {
            n: 1,
            duration: vec![0],
            distance: vec![0],
            buffer_permille: vec![],
        }],
        agents: vec![AgentSpec {
            capacity: vec![2],
            ..Default::default()
        }],
        execution: Some(ExecutionSpec {
            routes: vec![(
                0,
                ExecutedRoute {
                    initial_load: Some(vec![5]),
                    worked_seconds: 3601,
                    unavailable: true,
                    ..Default::default()
                },
            )],
            ..Default::default()
        }),
        weights: Weights::default().with(ConstraintId::BalanceAgentWorkingTime, Weight::soft(1)),
        ..Default::default()
    })
    .unwrap();
    let solution = Solution::empty(1);
    let evaluation = oracle::evaluate(&p, &solution);
    assert_eq!(evaluation.total(ConstraintId::AgentCapacity).amount, 3);
    assert_eq!(
        evaluation
            .total(ConstraintId::BalanceAgentWorkingTime)
            .amount,
        4
    );
    assert_eq!(evaluation.routes[0].travel_time, 0);
    assert_eq!(State::new(&p, &solution).unwrap().score(), evaluation.score);
}
