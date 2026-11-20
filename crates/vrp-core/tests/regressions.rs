mod common;
use common::*;
use vrp_core::{Change, ConstraintId as C, Problem, Solution, State, Weight, Weights, oracle};

#[test]
fn pruning_keeps_a_better_assignment_when_unassigning_changes_the_original_agent() {
    let weights = Weights::none()
        .with(C::AgentSkills, Weight::hard(1))
        .with(C::MinimizeTicketChanging, Weight::hard(10))
        .with(C::TicketUnassigned, Weight::medium(1000));
    let mut s = spec(1, weights);
    s.agents.push(agent("a", 0, 0));
    let mut t = ticket("t", 0, 0);
    t.original_agent = Some(0);
    t.skills_required = vec![0];
    s.tickets.push(t);
    let p = Problem::new(s).unwrap();
    let assigned = Solution {
        routes: vec![vec![0]],
    };
    assert!(
        oracle::evaluate(&p, &assigned).score > oracle::evaluate(&p, &Solution::empty(1)).score
    );
    assert_eq!(p.candidates[0], vec![0]);
}

#[test]
fn service_day_does_not_prune_an_agent_who_can_wait_until_the_next_day() {
    let mut s = spec(1, Weights::default());
    s.agents.push(agent("a", 0, 23 * 3600));
    let mut t = ticket("tomorrow", 0, 60);
    t.day = 1;
    t.earliest_start = 86400;
    s.tickets.push(t);
    let p = Problem::new(s).unwrap();
    let e = oracle::evaluate(
        &p,
        &Solution {
            routes: vec![vec![0]],
        },
    );
    assert_eq!(e.score.hard, 0);
    assert_eq!(p.candidates[0], vec![0]);
}

#[test]
fn reject_overflowing_products_and_aggregate_scores_before_evaluation() {
    for (count, cost) in [(1, 10_000_000_000), (30, 100_000_000)] {
        let mut s = spec(
            1,
            Weights::none().with(C::MinimizeAgentFixedCost, Weight::soft(1_000_000_000)),
        );
        for i in 0..count {
            let mut a = agent(&format!("a{i}"), 0, 0);
            a.fixed_cost = cost;
            s.agents.push(a);
            s.tickets.push(ticket(&format!("t{i}"), 0, 0));
        }
        let error = Problem::new(s).unwrap_err();
        assert!(error.0.contains("numeric bound"), "{error}");
    }
}

#[test]
fn minimum_integer_times_and_overflowing_capacity_tolerances_are_rejected() {
    let mut s = spec(1, Weights::default());
    s.agents.push(agent("a", 0, i64::MIN));
    assert!(Problem::new(s).is_err());
    let mut s = spec(1, Weights::default());
    let mut t = ticket("t", 0, 0);
    t.latest_end = i64::MIN;
    s.tickets.push(t);
    assert!(Problem::new(s).is_err());
    let mut s = spec(1, Weights::default());
    s.dims = 1;
    let mut a = agent("a", 0, 0);
    a.capacity = vec![1_000_000_000_000_000];
    a.tolerance_permille = vec![u32::MAX];
    s.agents.push(a);
    assert!(Problem::new(s).unwrap_err().0.contains("tolerance"));
}

#[test]
fn pinned_tickets_cannot_be_omitted_or_reassigned_even_when_weights_are_zero() {
    let mut s = spec(1, Weights::none());
    s.agents.extend([agent("a", 0, 0), agent("b", 0, 0)]);
    let mut t = ticket("fixed", 0, 0);
    t.original_agent = Some(0);
    t.pinned = true;
    s.tickets.push(t);
    let p = Problem::new(s).unwrap();
    assert!(Solution::empty(2).validate(&p).is_err());
    assert!(
        Solution {
            routes: vec![vec![], vec![0]]
        }
        .validate(&p)
        .is_err()
    );
    let initial = Solution::with_pinned(&p);
    assert!(initial.validate(&p).is_ok());
    let mut state = State::new(&p, &initial).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        state.apply(Change {
            routes: &[(0, &[]), (1, &[0])],
        });
    }));
    assert!(result.is_err());
    assert_eq!(state.solution(), initial);
}
