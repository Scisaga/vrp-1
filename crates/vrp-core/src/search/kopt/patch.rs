//! Patch a 2-factor by one or more alternating cycles (Helsgaun 2009, §5.2).
//! Each alternating cycle cuts one retained original edge in each visited
//! component and joins those components. Search starts in the smallest cycle.
//! Temporary graphs never enter State; every mutation is undone on all exits.
use super::{Context, KOpt, NONE};

pub(super) struct PatchLevel {
    component: Vec<usize>,
    sizes: Vec<usize>,
    selected: Vec<bool>,
}

impl PatchLevel {
    pub(super) fn new(nodes: usize) -> Self {
        Self {
            component: vec![NONE; nodes],
            sizes: vec![0; nodes],
            selected: vec![false; nodes],
        }
    }
}

impl KOpt {
    fn label_cycles(
        &mut self,
        level: usize,
        stop: &mut impl FnMut() -> bool,
    ) -> Option<(usize, usize)> {
        let n = self.nodes.len() + 1;
        let scratch = &mut self.patch_levels[level];
        scratch.component[..n].fill(NONE);
        scratch.sizes[..n].fill(0);
        scratch.selected[..n].fill(false);
        let mut count = 0;
        for first in 0..n {
            if scratch.component[first] != NONE {
                continue;
            }
            let mut previous = NONE;
            let mut current = first;
            loop {
                if scratch.sizes[count].is_multiple_of(256) && stop() {
                    return None;
                }
                if current >= n || scratch.component[current] != NONE {
                    return None;
                }
                let [a, b] = self.adjacency[current];
                if a >= n || b >= n || a == b || a == current || b == current {
                    return None;
                }
                scratch.component[current] = count;
                scratch.sizes[count] += 1;
                let next = if previous == NONE || b == previous {
                    a
                } else if a == previous {
                    b
                } else {
                    return None;
                };
                previous = current;
                current = next;
                if current == first {
                    if !self.adjacency[first].contains(&previous) {
                        return None;
                    }
                    break;
                }
            }
            count += 1;
        }
        let shortest = (0..count).min_by_key(|&i| scratch.sizes[i])?;
        Some((count, shortest))
    }

    fn patch_branch<F: FnMut() -> bool>(ctx: &mut Context<'_, '_, F>) -> bool {
        if ctx.patch_remaining == 0 || (ctx.stop)() {
            return false;
        }
        ctx.patch_remaining -= 1;
        true
    }

    pub(super) fn patch_components<F: FnMut() -> bool>(
        &mut self,
        ctx: &mut Context<'_, '_, F>,
        depth: usize,
        level: usize,
    ) -> bool {
        if ctx.patch_remaining == 0 || depth + 2 > ctx.max_depth || (ctx.stop)() {
            return false;
        }
        let Some((count, shortest)) = self.label_cycles(level, ctx.stop) else {
            return false;
        };
        // At least one more original edge must be cut in every component.
        if count < 2 || depth + count > ctx.max_depth {
            return false;
        }
        for first in 0..=self.nodes.len() {
            if (ctx.stop)() || ctx.patch_remaining == 0 {
                return false;
            }
            if self.patch_levels[level].component[first] != shortest {
                continue;
            }
            for second in self.adjacency[first] {
                if !self.original_edge(first, second) {
                    continue;
                }
                if !Self::patch_branch(ctx) {
                    return false;
                }
                self.patch_levels[level].selected[shortest] = true;
                self.remove(first, second);
                let improved = self.patch_chain(ctx, level, first, second, depth + 1, count);
                self.add(first, second);
                self.patch_levels[level].selected[shortest] = false;
                if improved {
                    return true;
                }
            }
        }
        false
    }

    // `left` includes the open component: closing after a visit reduces it by 1.
    fn patch_chain<F: FnMut() -> bool>(
        &mut self,
        ctx: &mut Context<'_, '_, F>,
        level: usize,
        first: usize,
        last: usize,
        depth: usize,
        left: usize,
    ) -> bool {
        if depth >= ctx.max_depth || ctx.patch_remaining == 0 || (ctx.stop)() {
            return false;
        }
        for c in 0..self.candidates[last].len() {
            let next = self.candidates[last][c];
            let component = self.patch_levels[level].component[next];
            if self.patch_levels[level].selected[component] || self.original_edge(last, next) {
                continue;
            }
            // This untouched component is still a closed cycle. Both orientations
            // of its retained edge are explored; inserted edges are never removed.
            for end in self.adjacency[next] {
                if !self.original_edge(next, end) {
                    continue;
                }
                if !Self::patch_branch(ctx) {
                    return false;
                }
                self.patch_levels[level].selected[component] = true;
                self.remove(next, end);
                self.add(last, next);
                let mut improved = false;
                if !self.original_edge(end, first) && !self.adjacency[end].contains(&first) {
                    self.add(end, first);
                    if left == 2 {
                        if self.closing_tour() {
                            improved = self.evaluate_tour(ctx);
                        }
                    } else if depth + left <= ctx.max_depth {
                        // Close this alternating cycle, then patch the remaining
                        // components with another one. Separate scratch preserves
                        // the labels of the outer search when backtracking.
                        improved = self.patch_components(ctx, depth + 1, level + 1);
                    }
                    self.remove(end, first);
                }
                if !improved && left > 2 && depth + left - 1 <= ctx.max_depth {
                    // Alternatively extend this one alternating cycle into another
                    // component; it may connect all components with fewer cuts.
                    improved = self.patch_chain(ctx, level, first, end, depth + 1, left - 1);
                }
                self.remove(last, next);
                self.add(next, end);
                self.patch_levels[level].selected[component] = false;
                if improved {
                    return true;
                }
            }
        }
        false
    }
}
