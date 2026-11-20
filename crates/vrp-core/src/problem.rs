//! 编译后的问题。由 vrp-model 从契约构造 [`ProblemSpec`]，再经 [`Problem::new`] 校验并预计算。

use std::fmt;

use crate::constraint::{ConstraintId, Weight, Weights};

/// 矩阵里"不可达"的保留值。编译后的矩阵不再含"未知"。
pub const UNREACHABLE: u32 = u32::MAX;
/// 契约里"未知"的保留值，必须在编译前补齐。
pub const UNKNOWN: u32 = u32::MAX - 1;
pub const SECONDS_PER_DAY: i64 = 86_400;
/// 估算余量千分比的上限（放大到 11 倍）。
pub const MAX_BUFFER_PERMILLE: u16 = 10_000;

/// 容量、需求的上限，保证容差乘法和逐站累加不溢出。
const MAX_QUANTITY: i64 = 1_000_000_000_000_000;
/// 时间、时长的上限（约 3 万年），保证时间推算不溢出。
const MAX_TIME: i64 = 1_000_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TicketKind {
    /// 上门服务 / 安装，不改变载荷。
    Service,
    /// 配送：出发时已装车，到站卸货。
    Delivery,
    /// 取件 / 返仓：到站装货，带回终点。
    Pickup,
}

/// 补齐后的 n×n 行主序矩阵。只可能含 [`UNREACHABLE`]，不含 [`UNKNOWN`]。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Matrix {
    pub n: usize,
    /// 秒。
    pub duration: Vec<u32>,
    /// 米。
    pub distance: Vec<u32>,
    /// 每条边的排班余量千分比（估算边才非 0，见 docs/constraints-v2.md 第 1 节）；为空表示全部为 0。
    pub buffer_permille: Vec<u16>,
}

impl Matrix {
    #[inline]
    pub fn index(&self, from: u32, to: u32) -> usize {
        from as usize * self.n + to as usize
    }

    #[inline]
    pub fn duration(&self, from: u32, to: u32) -> u32 {
        self.duration[self.index(from, to)]
    }

    #[inline]
    pub fn distance(&self, from: u32, to: u32) -> u32 {
        self.distance[self.index(from, to)]
    }
}

#[derive(Clone, Debug, Default)]
pub struct AgentSpec {
    pub id: String,
    pub day: i32,
    pub start_location: u32,
    pub end_location: u32,
    pub start_time: i64,
    /// 班次结束时刻；`None` 表示不限。
    pub end_time: Option<i64>,
    pub capacity: Vec<i64>,
    pub tolerance_permille: Vec<u32>,
    pub max_tickets: u32,
    pub fixed_cost: i64,
    pub restricted: bool,
    /// 技能字典下标。
    pub skills: Vec<u32>,
    /// （资质字典下标, 等级）。
    pub qualifications: Vec<(u32, f64)>,
    /// 网点字典下标。
    pub depot: Option<u32>,
    pub matrix: u32,
}

#[derive(Clone, Debug)]
pub struct TicketSpec {
    pub id: String,
    pub location: u32,
    pub kind: TicketKind,
    pub duration: i64,
    pub earliest_start: i64,
    pub latest_start: i64,
    pub latest_end: i64,
    pub day: i32,
    pub demand: Vec<i64>,
    pub skills_required: Vec<u32>,
    pub qualification_required: Vec<(u32, f64)>,
    pub depot: Option<u32>,
    pub original_agent: Option<u32>,
    pub pinned: bool,
}

impl Default for TicketSpec {
    fn default() -> Self {
        TicketSpec {
            id: String::new(),
            location: 0,
            kind: TicketKind::Service,
            duration: 0,
            earliest_start: 0,
            latest_start: MAX_TIME,
            latest_end: MAX_TIME,
            day: 0,
            demand: Vec::new(),
            skills_required: Vec::new(),
            qualification_required: Vec::new(),
            depot: None,
            original_agent: None,
            pinned: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProblemSpec {
    pub execution: Option<crate::execution::ExecutionSpec>,
    pub dims: usize,
    /// 每个容量维度的超载计数单位；缺省或 <= 0 视为 1。
    pub dim_penalty_unit: Vec<i64>,
    pub matrices: Vec<Matrix>,
    pub agents: Vec<AgentSpec>,
    pub tickets: Vec<TicketSpec>,
    pub related_pairs: Vec<(u32, u32)>,
    pub precedences: Vec<(u32, u32)>,
    pub weights: Weights,
    pub restricted_charge_per_hour: i64,
    pub day_starts: Vec<i64>,
}

#[derive(Clone, Copy, Debug)]
pub struct Agent {
    pub day: i32,
    pub start_location: u32,
    pub end_location: u32,
    pub start_time: i64,
    pub end_time: Option<i64>,
    pub max_tickets: u32,
    pub fixed_cost: i64,
    pub restricted: bool,
    pub matrix: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Ticket {
    pub location: u32,
    pub kind: TicketKind,
    pub duration: i64,
    pub earliest_start: i64,
    pub latest_start: i64,
    pub latest_end: i64,
    pub day: i32,
    pub original_agent: Option<u32>,
    pub pinned: bool,
}

/// 工单 × agent 的静态标记，也用作剪枝原因。
pub mod flag {
    pub const SKILLS: u8 = 1;
    pub const QUALIFICATION: u8 = 1 << 1;
    pub const DEPOT: u8 = 1 << 2;
    /// agent 所在日与工单计划日不同（只用于剪枝；评分按实际开始服务的日期判断）。
    pub const DAY: u8 = 1 << 3;
    /// 因 minimize_ticket_changing 为硬约束，只保留原 agent。
    pub const ORIGINAL: u8 = 1 << 4;
    /// 工单固定在原 agent 上。
    pub const PINNED: u8 = 1 << 5;
}

/// 工单涉及的一个关联对或依赖对。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairRef {
    Related(u32),
    Precedence(u32),
}

#[derive(Clone, Debug)]
pub struct Problem {
    pub execution: crate::execution::Execution,
    pub agent_ids: Vec<String>,
    pub ticket_ids: Vec<String>,
    pub agents: Vec<Agent>,
    pub tickets: Vec<Ticket>,
    pub matrices: Vec<Matrix>,
    pub dims: usize,
    pub dim_penalty_unit: Vec<i64>,
    /// `[agent * dims + d]`
    pub agent_capacity: Vec<i64>,
    /// 计入容差后的上限，`[agent * dims + d]`。
    pub agent_limit: Vec<i64>,
    /// `[ticket * dims + d]`
    pub ticket_demand: Vec<i64>,
    /// `[ticket * agents.len() + agent]`，取值见 [`flag`]。
    pub static_flags: Vec<u8>,
    /// 无序对，first < second，已去重。
    pub related_pairs: Vec<(u32, u32)>,
    /// (前置, 后续)，已去重。
    pub precedences: Vec<(u32, u32)>,
    /// 每张工单涉及的对。
    pub ticket_pairs: Vec<Vec<PairRef>>,
    pub weights: Weights,
    pub restricted_charge_per_hour: i64,
    pub day_starts: Vec<i64>,
    /// 每张工单的候选 agent（剪枝后），升序。
    pub candidates: Vec<Vec<u32>>,
    /// 每张工单被剪掉 agent 的原因（[`flag`] 的并集）。
    pub prune_reasons: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProblemError(pub String);

impl fmt::Display for ProblemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProblemError {}

macro_rules! ensure {
    ($cond:expr, $($arg:tt)*) => {
        if !$cond {
            return Err(ProblemError(format!($($arg)*)));
        }
    };
}

impl Problem {
    pub fn new(spec: ProblemSpec) -> Result<Problem, ProblemError> {
        let ProblemSpec {
            execution,
            dims,
            dim_penalty_unit,
            matrices,
            agents,
            tickets,
            related_pairs,
            precedences,
            weights,
            restricted_charge_per_hour,
            day_starts,
        } = spec;

        ensure!(!matrices.is_empty(), "at least one matrix is required");
        let n = matrices[0].n;
        for (i, m) in matrices.iter().enumerate() {
            ensure!(
                m.n == n,
                "matrix {i} has size {} but matrix 0 has size {n}",
                m.n
            );
            ensure!(
                m.duration.len() == n * n && m.distance.len() == n * n,
                "matrix {i} must hold {n}x{n} entries"
            );
            for k in 0..n * n {
                let (d, s) = (m.duration[k], m.distance[k]);
                ensure!(
                    d != UNKNOWN && s != UNKNOWN,
                    "matrix {i} still has unknown entries"
                );
                ensure!(
                    (d == UNREACHABLE) == (s == UNREACHABLE),
                    "matrix {i} entry {k}: duration and distance disagree on reachability"
                );
            }
            ensure!(
                m.buffer_permille.is_empty() || m.buffer_permille.len() == n * n,
                "matrix {i} buffer must be empty or hold {n}x{n} entries"
            );
            ensure!(
                m.buffer_permille.iter().all(|&p| p <= MAX_BUFFER_PERMILLE),
                "matrix {i} buffer exceeds {MAX_BUFFER_PERMILLE} permille"
            );
        }
        for (k, w) in ConstraintId::ALL.iter().map(|&c| (c, weights.get(c))) {
            ensure!(
                valid_weight(w),
                "weight of {} out of range: {w:?}",
                k.name()
            );
        }
        ensure!(
            (0..=MAX_QUANTITY).contains(&restricted_charge_per_hour),
            "restricted_charge_per_hour out of range"
        );
        ensure!(
            day_starts.is_empty()
                || (day_starts[0] == 0 && day_starts.windows(2).all(|w| w[0] < w[1])),
            "day_starts must start at 0 and be strictly increasing"
        );

        let mut penalty_unit: Vec<i64> = (0..dims)
            .map(|d| dim_penalty_unit.get(d).copied().unwrap_or(1))
            .collect();
        for u in &mut penalty_unit {
            if *u <= 0 {
                *u = 1;
            }
        }

        let n_agents = agents.len();
        let n_tickets = tickets.len();
        let mut agent_ids = Vec::with_capacity(n_agents);
        let mut compiled_agents = Vec::with_capacity(n_agents);
        let mut agent_capacity = vec![0; n_agents * dims];
        let mut agent_limit = vec![0; n_agents * dims];
        let mut agent_skill_bits = Vec::with_capacity(n_agents);
        let mut agent_qualifications = Vec::with_capacity(n_agents);
        let mut agent_depots = Vec::with_capacity(n_agents);
        let skill_words = skill_word_count(&agents, &tickets);

        for (a, spec) in agents.into_iter().enumerate() {
            ensure!(
                (spec.start_location as usize) < n,
                "agent {} start_location out of range",
                spec.id
            );
            ensure!(
                (spec.end_location as usize) < n,
                "agent {} end_location out of range",
                spec.id
            );
            ensure!(
                (spec.matrix as usize) < matrices.len(),
                "agent {} matrix out of range",
                spec.id
            );
            ensure!(
                spec.capacity.len() <= dims,
                "agent {} has more capacity values than dimensions",
                spec.id
            );
            ensure!(
                spec.tolerance_permille.len() <= dims,
                "agent {} has more tolerance values than dimensions",
                spec.id
            );
            ensure!(
                (-MAX_TIME..=MAX_TIME).contains(&spec.start_time),
                "agent {} start_time out of range",
                spec.id
            );
            if let Some(end) = spec.end_time {
                ensure!(
                    (-MAX_TIME..=MAX_TIME).contains(&end),
                    "agent {} end_time out of range",
                    spec.id
                );
                ensure!(
                    execution.is_some() || end >= spec.start_time,
                    "agent {} end_time is before start_time",
                    spec.id
                );
            }
            ensure!(
                (0..=MAX_QUANTITY).contains(&spec.fixed_cost),
                "agent {} fixed_cost out of range",
                spec.id
            );
            for d in 0..dims {
                let cap = spec.capacity.get(d).copied().unwrap_or(0);
                ensure!(
                    (0..=MAX_QUANTITY).contains(&cap),
                    "agent {} capacity out of range",
                    spec.id
                );
                let tol = spec.tolerance_permille.get(d).copied().unwrap_or(0) as i128;
                agent_capacity[a * dims + d] = cap;
                let limit = cap as i128 * (1000 + tol) / 1000;
                ensure!(
                    limit <= i64::MAX as i128 / 4,
                    "agent {} capacity tolerance out of range",
                    spec.id
                );
                agent_limit[a * dims + d] = limit as i64;
            }
            agent_skill_bits.push(skill_bits(&spec.skills, skill_words));
            let mut quals = spec.qualifications.clone();
            ensure!(
                quals.iter().all(|q| q.1.is_finite()),
                "agent {} has a non-finite qualification level",
                spec.id
            );
            quals.sort_by_key(|q| q.0);
            agent_qualifications.push(quals);
            agent_depots.push(spec.depot);
            compiled_agents.push(Agent {
                day: spec.day,
                start_location: spec.start_location,
                end_location: spec.end_location,
                start_time: spec.start_time,
                end_time: spec.end_time,
                max_tickets: spec.max_tickets,
                fixed_cost: spec.fixed_cost,
                restricted: spec.restricted,
                matrix: spec.matrix,
            });
            agent_ids.push(spec.id);
        }

        let mut ticket_ids = Vec::with_capacity(n_tickets);
        let mut compiled_tickets = Vec::with_capacity(n_tickets);
        let mut ticket_demand = vec![0; n_tickets * dims];
        let mut static_flags = vec![0u8; n_tickets * n_agents];
        for (t, spec) in tickets.into_iter().enumerate() {
            ensure!(
                (spec.location as usize) < n,
                "ticket {} location out of range",
                spec.id
            );
            ensure!(
                (0..=MAX_TIME).contains(&spec.duration),
                "ticket {} duration out of range",
                spec.id
            );
            for v in [spec.earliest_start, spec.latest_start, spec.latest_end] {
                ensure!(
                    (-MAX_TIME..=MAX_TIME).contains(&v),
                    "ticket {} time out of range",
                    spec.id
                );
            }
            ensure!(
                spec.demand.len() <= dims,
                "ticket {} has more demand values than dimensions",
                spec.id
            );
            for (d, &q) in spec.demand.iter().enumerate() {
                ensure!(
                    (0..=MAX_QUANTITY).contains(&q),
                    "ticket {} demand out of range",
                    spec.id
                );
                ticket_demand[t * dims + d] = q;
            }
            if let Some(o) = spec.original_agent {
                ensure!(
                    (o as usize) < n_agents,
                    "ticket {} original_agent out of range",
                    spec.id
                );
            }
            ensure!(
                !spec.pinned || spec.original_agent.is_some(),
                "ticket {} is pinned but has no original_agent",
                spec.id
            );
            ensure!(
                spec.qualification_required.iter().all(|q| q.1.is_finite()),
                "ticket {} has a non-finite qualification level",
                spec.id
            );

            let required = skill_bits(&spec.skills_required, skill_words);
            for a in 0..n_agents {
                let mut f = 0u8;
                if !spec.skills_required.is_empty()
                    && required
                        .iter()
                        .zip(&agent_skill_bits[a])
                        .any(|(r, have)| r & !have != 0)
                {
                    f |= flag::SKILLS;
                }
                if !qualified(&agent_qualifications[a], &spec.qualification_required) {
                    f |= flag::QUALIFICATION;
                }
                if let (Some(td), Some(ad)) = (spec.depot, agent_depots[a])
                    && td != ad
                {
                    f |= flag::DEPOT;
                }
                if compiled_agents[a].day != spec.day {
                    f |= flag::DAY;
                }
                static_flags[t * n_agents + a] = f;
            }
            compiled_tickets.push(Ticket {
                location: spec.location,
                kind: spec.kind,
                duration: spec.duration,
                earliest_start: spec.earliest_start,
                latest_start: spec.latest_start,
                latest_end: spec.latest_end,
                day: spec.day,
                original_agent: spec.original_agent,
                pinned: spec.pinned,
            });
            ticket_ids.push(spec.id);
        }

        let mut related: Vec<(u32, u32)> = Vec::with_capacity(related_pairs.len());
        for (a, b) in related_pairs {
            ensure!(
                (a as usize) < n_tickets && (b as usize) < n_tickets,
                "related pair ({a}, {b}) out of range"
            );
            ensure!(a != b, "related pair ({a}, {b}) refers to the same ticket");
            related.push((a.min(b), a.max(b)));
        }
        related.sort_unstable();
        related.dedup();
        let mut ordered: Vec<(u32, u32)> = Vec::with_capacity(precedences.len());
        for (a, b) in precedences {
            ensure!(
                (a as usize) < n_tickets && (b as usize) < n_tickets,
                "precedence ({a}, {b}) out of range"
            );
            ensure!(a != b, "precedence ({a}, {b}) refers to the same ticket");
            ordered.push((a, b));
        }
        ordered.sort_unstable();
        ordered.dedup();

        let mut successors = vec![Vec::new(); n_tickets];
        let mut incoming = vec![0usize; n_tickets];
        for &(first, second) in &ordered {
            successors[first as usize].push(second as usize);
            incoming[second as usize] += 1;
        }
        let mut ready: Vec<usize> = (0..n_tickets).filter(|&t| incoming[t] == 0).collect();
        let mut cursor = 0;
        while cursor < ready.len() {
            let t = ready[cursor];
            cursor += 1;
            for &next in &successors[t] {
                incoming[next] -= 1;
                if incoming[next] == 0 {
                    ready.push(next);
                }
            }
        }
        ensure!(
            ready.len() == n_tickets,
            "precedences contain a dependency cycle"
        );

        let mut ticket_pairs = vec![Vec::new(); n_tickets];
        for (i, &(a, b)) in related.iter().enumerate() {
            ticket_pairs[a as usize].push(PairRef::Related(i as u32));
            ticket_pairs[b as usize].push(PairRef::Related(i as u32));
        }
        for (i, &(a, b)) in ordered.iter().enumerate() {
            ticket_pairs[a as usize].push(PairRef::Precedence(i as u32));
            ticket_pairs[b as usize].push(PairRef::Precedence(i as u32));
        }

        let (candidates, prune_reasons) = candidates(&compiled_tickets, n_agents);

        let mut problem = Problem {
            execution: crate::execution::Execution::default(),
            agent_ids,
            ticket_ids,
            agents: compiled_agents,
            tickets: compiled_tickets,
            matrices,
            dims,
            dim_penalty_unit: penalty_unit,
            agent_capacity,
            agent_limit,
            ticket_demand,
            static_flags,
            related_pairs: related,
            precedences: ordered,
            ticket_pairs,
            weights,
            restricted_charge_per_hour,
            day_starts,
            candidates,
            prune_reasons,
        };
        problem.execution = crate::execution::Execution::compile(execution, &problem)?;
        for (t, candidates) in problem.candidates.iter_mut().enumerate() {
            candidates.retain(|&a| problem.execution.permits_ticket(a, t as u32));
            ensure!(
                !problem.tickets[t].pinned || !candidates.is_empty(),
                "pinned ticket {t} conflicts with execution restrictions"
            );
        }
        crate::numeric::validate(&problem)?;
        Ok(problem)
    }

    #[inline]
    pub fn static_flags(&self, ticket: u32, agent: u32) -> u8 {
        self.static_flags[ticket as usize * self.agents.len() + agent as usize]
    }

    #[inline]
    pub fn demand(&self, ticket: u32) -> &[i64] {
        let d = self.dims;
        &self.ticket_demand[ticket as usize * d..(ticket as usize + 1) * d]
    }

    #[inline]
    pub fn capacity(&self, agent: u32) -> &[i64] {
        let d = self.dims;
        &self.agent_capacity[agent as usize * d..(agent as usize + 1) * d]
    }

    #[inline]
    pub fn limit(&self, agent: u32) -> &[i64] {
        let d = self.dims;
        &self.agent_limit[agent as usize * d..(agent as usize + 1) * d]
    }

    /// 某时刻所在的日序号（可为负，表示计划起点之前）。
    pub fn day_of(&self, t: i64) -> i32 {
        if self.day_starts.is_empty() || t < 0 {
            return t.div_euclid(SECONDS_PER_DAY) as i32;
        }
        let i = self.day_starts.partition_point(|&s| s <= t) - 1;
        if i + 1 < self.day_starts.len() {
            i as i32
        } else {
            // 超出给定日期表的部分按每天 86400 秒顺延。
            (i as i64 + (t - self.day_starts[i]) / SECONDS_PER_DAY) as i32
        }
    }
}

fn valid_weight(w: Weight) -> bool {
    let range = 0..=Weight::MAX_COMPONENT;
    range.contains(&w.hard) && range.contains(&w.medium) && range.contains(&w.soft)
}

fn skill_word_count(agents: &[AgentSpec], tickets: &[TicketSpec]) -> usize {
    let max = agents
        .iter()
        .flat_map(|a| a.skills.iter())
        .chain(tickets.iter().flat_map(|t| t.skills_required.iter()))
        .copied()
        .max();
    max.map_or(0, |m| m as usize / 64 + 1)
}

fn skill_bits(skills: &[u32], words: usize) -> Vec<u64> {
    let mut bits = vec![0u64; words];
    for &s in skills {
        bits[s as usize / 64] |= 1 << (s % 64);
    }
    bits
}

fn qualified(agent: &[(u32, f64)], required: &[(u32, f64)]) -> bool {
    required.iter().all(|&(key, level)| {
        agent
            .binary_search_by_key(&key, |q| q.0)
            .is_ok_and(|i| agent[i].1 >= level)
    })
}

/// 按 constraints-v2 第 3 节计算候选 agent 和剪枝原因。
fn candidates(tickets: &[Ticket], n_agents: usize) -> (Vec<Vec<u32>>, Vec<u8>) {
    let mut all = Vec::with_capacity(tickets.len());
    let mut reasons = Vec::with_capacity(tickets.len());
    for ticket in tickets {
        let mut list = Vec::new();
        let mut why = 0u8;
        let only = match ticket.original_agent {
            Some(o) if ticket.pinned => Some((o, flag::PINNED)),
            _ => None,
        };
        for a in 0..n_agents {
            if let Some((o, reason)) = only
                && a as u32 != o
            {
                why |= reason;
                continue;
            }
            list.push(a as u32);
        }
        all.push(list);
        reasons.push(why);
    }
    (all, reasons)
}
