mod common;
use common::{agent, spec, ticket};
use vrp_core::state::Change;
use vrp_core::{ConstraintId as C, Problem, Solution, State, Weight, Weights, oracle};

#[test]
fn multiple_predecessors_wait_and_propagate_across_unchanged_routes() {
    let mut s = spec(
        1,
        Weights::none().with(C::RefTicketAfterDepTicket, Weight::hard(1)),
    );
    s.agents = (0..4).map(|i| agent(&format!("a{i}"), 0, 0)).collect();
    s.tickets = [60, 120, 30, 15]
        .into_iter()
        .enumerate()
        .map(|(i, d)| ticket(&format!("t{i}"), 0, d))
        .collect();
    s.precedences = vec![(0, 2), (1, 2), (2, 3)];
    let p = Problem::new(s).unwrap();
    let initial = Solution {
        routes: vec![vec![0], vec![1], vec![2], vec![3]],
    };
    let e = oracle::evaluate(&p, &initial);
    assert_eq!(e.routes[2].stops[0].arrival, 0);
    assert_eq!(e.routes[2].stops[0].start, 120);
    assert_eq!(e.routes[3].stops[0].start, 150);
    assert_eq!(e.score.hard, 0);
    let mut state = State::new(&p, &initial).unwrap();
    let candidate = Solution {
        routes: vec![vec![], vec![1, 0], vec![2], vec![3]],
    };
    let change = Change {
        routes: &[(0, &[]), (1, &[1, 0])],
    };
    assert_eq!(
        state.evaluate(change),
        oracle::evaluate(&p, &candidate).score
    );
    state.apply(change);
    assert_eq!(state.summary(2).end_time, 210);
    assert_eq!(state.summary(3).end_time, 225);
    let change = Change {
        routes: &[(1, &[0])],
    };
    let e = oracle::evaluate(
        &p,
        &Solution {
            routes: vec![vec![], vec![0], vec![2], vec![3]],
        },
    );
    assert_eq!(e.total(C::RefTicketAfterDepTicket).amount, 1);
    assert_eq!(state.evaluate(change), e.score);
    assert_eq!(state.apply(change), e.score);
    assert_eq!(state.summary(3).end_time, 105);
    assert_eq!(state.recompute(), e.score);
}

#[test]
fn zero_duration_route_cycles_are_finite_and_visible_to_both_evaluators() {
    let mut s = spec(
        1,
        Weights::none().with(C::RefTicketAfterDepTicket, Weight::hard(1)),
    );
    s.agents = vec![agent("a", 0, 0), agent("b", 0, 0)];
    s.tickets = (0..4).map(|i| ticket(&format!("t{i}"), 0, 0)).collect();
    s.precedences = vec![(0, 1), (2, 3)];
    let p = Problem::new(s).unwrap();
    let mut state = State::new(
        &p,
        &Solution {
            routes: vec![vec![0, 1], vec![2, 3]],
        },
    )
    .unwrap();
    let candidate = Solution {
        routes: vec![vec![1, 2], vec![3, 0]],
    };
    let e = oracle::evaluate(&p, &candidate);
    assert_eq!(e.total(C::RefTicketAfterDepTicket).amount, 4);
    let change = Change {
        routes: &[(0, &[1, 2]), (1, &[3, 0])],
    };
    assert_eq!(state.evaluate(change), e.score);
    assert_eq!(state.apply(change), e.score);
    let fixed = Change {
        routes: &[(0, &[0, 1]), (1, &[2, 3])],
    };
    assert_eq!(state.apply(fixed).hard, 0);
}

#[test]
fn business_dependency_cycles_are_rejected_even_with_scoring_disabled() {
    let mut s = spec(1, Weights::none());
    s.agents.push(agent("a", 0, 0));
    s.tickets = (0..3).map(|i| ticket(&format!("t{i}"), 0, 0)).collect();
    s.precedences = vec![(0, 1), (1, 2), (2, 0)];
    assert!(Problem::new(s).err().unwrap().0.contains("cycle"));
}
