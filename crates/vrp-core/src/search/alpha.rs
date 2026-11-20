//! Alpha-nearness on a symmetric surrogate graph: exact minimum 1-tree sensitivity
//! at the best node penalties found by bounded Held-Karp subgradient ascent.
//! This is a candidate ordering, never a bound on the rich VRP business score.
use std::time::{Duration, Instant};

#[derive(Clone)]
struct OneTree {
    parent: Vec<usize>,
    root_edges: [usize; 2],
    degree: Vec<i64>,
    lower_bound: i128,
}

fn cost(base: &[i64], n: usize, pi: &[i64], a: usize, b: usize) -> i64 {
    base[a * n + b] + pi[a] + pi[b]
}

fn one_tree(base: &[i64], pi: &[i64], stop: &mut impl FnMut() -> bool) -> Option<OneTree> {
    let n = pi.len();
    let mut parent = vec![usize::MAX; n];
    let mut key = vec![i64::MAX; n];
    let mut used = vec![false; n];
    let mut degree = vec![0i64; n];
    key[1] = 0;
    let mut total = 0i128;
    for _ in 1..n {
        if stop() {
            return None;
        }
        let u = (1..n).filter(|&v| !used[v]).min_by_key(|&v| (key[v], v))?;
        used[u] = true;
        if parent[u] != usize::MAX {
            degree[u] += 1;
            degree[parent[u]] += 1;
            total += i128::from(key[u]);
        }
        for v in 1..n {
            let c = cost(base, n, pi, u, v);
            if !used[v] && c < key[v] {
                key[v] = c;
                parent[v] = u;
            }
        }
    }
    let mut root = (1..n)
        .map(|v| (cost(base, n, pi, 0, v), v))
        .collect::<Vec<_>>();
    root.sort_unstable();
    let root_edges = [root[0].1, root[1].1];
    for &v in &root_edges {
        degree[v] += 1;
        degree[0] += 1;
        total += i128::from(cost(base, n, pi, 0, v));
    }
    Some(OneTree {
        parent,
        root_edges,
        degree,
        lower_bound: total - 2 * pi.iter().map(|&x| i128::from(x)).sum::<i128>(),
    })
}

fn alpha_values(
    base: &[i64],
    pi: &[i64],
    tree: &OneTree,
    stop: &mut impl FnMut() -> bool,
) -> Option<Vec<i64>> {
    let n = pi.len();
    let mut adjacency = vec![Vec::new(); n];
    for v in 1..n {
        let u = tree.parent[v];
        if u != usize::MAX {
            let c = cost(base, n, pi, u, v);
            adjacency[u].push((v, c));
            adjacency[v].push((u, c));
        }
    }
    let mut alpha = vec![0; n * n];
    let root_max = tree
        .root_edges
        .iter()
        .map(|&v| cost(base, n, pi, 0, v))
        .max()
        .unwrap();
    for v in 1..n {
        let value = if tree.root_edges.contains(&v) {
            0
        } else {
            cost(base, n, pi, 0, v) - root_max
        };
        alpha[v] = value;
        alpha[v * n] = value;
    }
    let mut stack = Vec::with_capacity(n);
    for start in 1..n {
        if stop() {
            return None;
        }
        stack.clear();
        stack.push((start, usize::MAX, i64::MIN));
        while let Some((u, parent, maximum)) = stack.pop() {
            if start != u {
                alpha[start * n + u] = cost(base, n, pi, start, u) - maximum;
            }
            for &(v, c) in &adjacency[u] {
                if v != parent {
                    stack.push((v, u, maximum.max(c)));
                }
            }
        }
    }
    Some(alpha)
}

pub(super) struct Alpha {
    pub values: Vec<i64>,
    pub penalties: Vec<i64>,
    pub ascent_iterations: u32,
}

/// n >= 3, symmetric nonnegative base costs. Reserve 40% of the local budget
/// for exact sensitivity computation after the bounded ascent phase.
pub(super) fn build(
    base: &[i64],
    n: usize,
    budget: Duration,
    mut stop: impl FnMut() -> bool,
) -> Option<Alpha> {
    if n < 3 || stop() {
        return None;
    }
    let start = Instant::now();
    let ascent_budget = budget.mul_f64(0.6);
    let upper_bound: i128 = (0..n).map(|i| i128::from(base[i * n + (i + 1) % n])).sum();
    let mut pi = vec![0i64; n];
    let mut best_pi = pi.clone();
    let mut best = one_tree(base, &pi, &mut stop)?;
    let mut scale = 100i128;
    let mut stale = 0;
    let mut iterations = 0;
    for _ in 0..64 {
        if start.elapsed() >= ascent_budget || stop() {
            break;
        }
        let tree = one_tree(base, &pi, &mut stop)?;
        iterations += 1;
        if tree.lower_bound > best.lower_bound {
            best_pi.clone_from(&pi);
            best = tree.clone();
            stale = 0;
        } else {
            stale += 1;
        }
        let norm: i128 = tree.degree.iter().map(|d| i128::from(d - 2).pow(2)).sum();
        if norm == 0 || tree.lower_bound >= upper_bound {
            break;
        }
        if stale >= 8 {
            scale = (scale / 2).max(1);
            stale = 0;
        }
        let step = ((upper_bound - tree.lower_bound) * scale / (100 * norm)).max(1);
        for (value, degree) in pi.iter_mut().zip(tree.degree) {
            *value = (i128::from(*value) + step * i128::from(degree - 2))
                .clamp(-1_000_000_000_000, 1_000_000_000_000) as i64;
        }
    }
    let values = alpha_values(base, &best_pi, &best, &mut stop)?;
    Some(Alpha {
        values,
        penalties: best_pi,
        ascent_iterations: iterations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    // Independent exhaustive oracle: enumerate every tree on 1..n plus two root edges.
    fn brute(base: &[i64], pi: &[i64]) -> (i128, Vec<i128>) {
        let n = pi.len();
        let edges: Vec<_> = (1..n)
            .flat_map(|a| (a + 1..n).map(move |b| (a, b)))
            .collect();
        let mut best = i128::MAX;
        let mut forced = vec![i128::MAX; n * n];
        for mask in 0u32..(1 << edges.len()) {
            if mask.count_ones() as usize != n - 2 {
                continue;
            }
            let mut component: Vec<_> = (0..n).collect();
            let chosen: Vec<_> = edges
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, &e)| e)
                .collect();
            for &(a, b) in &chosen {
                let (from, to) = (component[a], component[b]);
                for c in &mut component {
                    if *c == from {
                        *c = to;
                    }
                }
            }
            if component[1..].iter().any(|&c| c != component[1]) {
                continue;
            }
            for a in 1..n {
                for b in a + 1..n {
                    let all: Vec<_> = chosen.iter().copied().chain([(0, a), (0, b)]).collect();
                    let total: i128 = all
                        .iter()
                        .map(|&(u, v)| i128::from(cost(base, n, pi, u, v)))
                        .sum();
                    best = best.min(total);
                    for (u, v) in all {
                        forced[u * n + v] = forced[u * n + v].min(total);
                        forced[v * n + u] = forced[v * n + u].min(total);
                    }
                }
            }
        }
        (best, forced)
    }

    #[test]
    fn exact_alpha_matches_every_forced_edge_of_enumerated_one_trees() {
        let mut rng = Rng::new(78);
        for n in 3..=6 {
            for _ in 0..8 {
                let mut base = vec![0; n * n];
                for a in 0..n {
                    for b in a + 1..n {
                        let c = rng.below(20) as i64;
                        base[a * n + b] = c;
                        base[b * n + a] = c;
                    }
                }
                let pi: Vec<_> = (0..n).map(|_| rng.below(31) as i64 - 15).collect();
                let tree = one_tree(&base, &pi, &mut || false).unwrap();
                let alpha = alpha_values(&base, &pi, &tree, &mut || false).unwrap();
                let (best, forced) = brute(&base, &pi);
                assert_eq!(
                    tree.lower_bound,
                    best - 2 * pi.iter().map(|&x| i128::from(x)).sum::<i128>()
                );
                for a in 0..n {
                    for b in 0..n {
                        if a != b {
                            assert_eq!(i128::from(alpha[a * n + b]), forced[a * n + b] - best);
                            assert!(alpha[a * n + b] >= 0);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn ascent_preserves_or_improves_the_bound_and_cancellation_is_observed() {
        let n = 7;
        let mut base = vec![0; n * n];
        for a in 0..n {
            for b in a + 1..n {
                let c = ((a * 37 + b * 53) % 100 + 1) as i64;
                base[a * n + b] = c;
                base[b * n + a] = c;
            }
        }
        let initial = one_tree(&base, &vec![0; n], &mut || false).unwrap();
        let result = build(&base, n, Duration::from_secs(1), || false).unwrap();
        let improved = one_tree(&base, &result.penalties, &mut || false).unwrap();
        assert!(improved.lower_bound >= initial.lower_bound);
        assert!(result.ascent_iterations > 0);
        assert!(build(&base, n, Duration::from_secs(1), || true).is_none());
    }
}
