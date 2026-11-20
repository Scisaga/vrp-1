//! Reusable joint route/dependency timing for incremental evaluation.
use crate::{constraint::ConstraintId as C, eval, problem::Problem, solution::NONE};

pub(crate) fn enabled(p: &Problem) -> bool {
    !p.precedences.is_empty() && !p.weights.get(C::RefTicketAfterDepTicket).is_zero()
}

pub(crate) fn past_release(p: &Problem, ticket: usize) -> i64 {
    p.execution.past[ticket]
        .iter()
        .filter(|v| v.after_finish)
        .map(|v| v.finished_at)
        .max()
        .unwrap_or(i64::MIN)
}

pub(crate) struct Timing {
    pub start: Vec<i64>,
    pub agent: Vec<u32>,
    pub blocked: i64,
    base: Vec<i64>,
    edges: Vec<Vec<(usize, i64)>>,
    indegree: Vec<usize>,
    queue: Vec<usize>,
}

impl Timing {
    pub fn new(n: usize) -> Self {
        Self {
            start: vec![0; n],
            agent: vec![NONE; n],
            blocked: 0,
            base: vec![0; n],
            edges: vec![Vec::new(); n],
            indegree: vec![0; n],
            queue: Vec::with_capacity(n),
        }
    }

    pub fn compute<'a>(&mut self, p: &Problem, route: impl Fn(usize) -> &'a [u32]) {
        self.agent.fill(NONE);
        self.indegree.fill(0);
        self.queue.clear();
        for edges in &mut self.edges {
            edges.clear();
        }
        let mut assigned = 0;
        for (a, spec) in p.agents.iter().enumerate() {
            let mut previous: Option<usize> = None;
            let mut here = spec.start_location;
            let mut clock = spec.start_time;
            for &ticket in route(a) {
                let t = ticket as usize;
                assert_eq!(self.agent[t], NONE, "duplicate ticket in timing input");
                self.agent[t] = a as u32;
                assigned += 1;
                let travel = eval::leg(p, spec.matrix, here, p.tickets[t].location).schedule;
                let lower = p.tickets[t].earliest_start.max(past_release(p, t));
                self.base[t] = (clock + travel).max(lower);
                self.start[t] = lower;
                if let Some(before) = previous {
                    self.edges[before].push((t, p.tickets[before].duration + travel));
                    self.indegree[t] += 1;
                } else {
                    self.start[t] = self.start[t].max(spec.start_time + travel);
                }
                previous = Some(t);
                clock = self.base[t] + p.tickets[t].duration;
                here = p.tickets[t].location;
            }
        }
        for &(first, second) in &p.precedences {
            let (a, b) = (first as usize, second as usize);
            if self.agent[a] != NONE && self.agent[b] != NONE {
                self.edges[a].push((b, p.tickets[a].duration));
                self.indegree[b] += 1;
            }
        }
        for t in 0..self.agent.len() {
            if self.agent[t] != NONE && self.indegree[t] == 0 {
                self.queue.push(t);
            }
        }
        let mut cursor = 0;
        while cursor < self.queue.len() {
            let t = self.queue[cursor];
            cursor += 1;
            for &(next, lag) in &self.edges[t] {
                self.start[next] = self.start[next].max(self.start[t] + lag);
                self.indegree[next] -= 1;
                if self.indegree[next] == 0 {
                    self.queue.push(next);
                }
            }
        }
        self.blocked = (assigned - self.queue.len()) as i64;
        // Infeasible route ordering gets bounded baseline times and an explicit
        // violation, including zero-duration cycles. Never iterate waits forever.
        if self.blocked != 0 {
            self.start.copy_from_slice(&self.base);
        }
    }
}
