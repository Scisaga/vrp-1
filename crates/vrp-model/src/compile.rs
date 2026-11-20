use std::collections::HashMap;

use vrp_core::problem::{MAX_BUFFER_PERMILLE, UNKNOWN, UNREACHABLE};
use vrp_core::{AgentSpec, Matrix, Problem, ProblemSpec, Solution, TicketKind, TicketSpec};
use vrp_proto::engine::v1::{self as pb, EdgeSource, TicketType};

use crate::fallback::Estimator;
use crate::{ModelError, ResolvedSolveOptions, resolve_options, weights_from_proto};

/// `estimate_buffer` 为空时的排班余量（千分比）。
pub const DEFAULT_ESTIMATE_BUFFER: pb::EstimateBuffer = pb::EstimateBuffer {
    model_permille: 100,
    fallback_permille: 200,
};

/// 编译结果。
#[derive(Debug)]
pub struct Compiled {
    pub problem: Problem,
    pub initial: Option<Solution>,
    pub options: ResolvedSolveOptions,
    /// 每张矩阵每条边的来源（[`EdgeSource`] 数值），补齐的边为 FALLBACK。
    pub sources: Vec<Vec<u8>>,
    pub stats: MatrixStats,
    /// 实际生效的估算余量。
    pub estimate_buffer: pb::EstimateBuffer,
}

/// 全部矩阵的边来源统计（不含对角线）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MatrixStats {
    pub entries: usize,
    pub exact: usize,
    pub cached: usize,
    pub model: usize,
    pub fallback: usize,
    pub unreachable: usize,
}

/// Persist the exact matrix produced by normal compilation. Durations remain
/// unbuffered: scheduling margins are applied only by the core at evaluation.
pub fn materialize_matrices(request: &mut pb::SolveRequest) -> Result<MatrixStats, ModelError> {
    let compiled = compile(request)?;
    let p = request.problem.as_mut().expect("validated problem");
    p.matrices = compiled
        .problem
        .matrices
        .iter()
        .enumerate()
        .map(|(i, m)| pb::TravelMatrix {
            profile: p
                .matrices
                .get(i)
                .map_or_else(|| "fallback".into(), |v| v.profile.clone()),
            durations: m.duration.iter().flat_map(|n| n.to_le_bytes()).collect(),
            distances: m.distance.iter().flat_map(|n| n.to_le_bytes()).collect(),
            sources: compiled.sources[i].clone(),
        })
        .collect();
    Ok(compiled.stats)
}

fn err<T>(msg: impl Into<String>) -> Result<T, ModelError> {
    Err(ModelError(msg.into()))
}

/// 按出现顺序给字符串编号，保证编译结果确定。
#[derive(Default)]
struct Dictionary(HashMap<String, u32>);

impl Dictionary {
    fn id(&mut self, key: &str) -> u32 {
        let next = self.0.len() as u32;
        *self.0.entry(key.to_string()).or_insert(next)
    }

    fn optional(&mut self, key: &str) -> Option<u32> {
        (!key.is_empty()).then(|| self.id(key))
    }
}

pub fn compile(request: &pb::SolveRequest) -> Result<Compiled, ModelError> {
    let options = resolve_options(request.options.as_ref())?;
    let Some(p) = request.problem.as_ref() else {
        return err("problem is required");
    };
    let n = p.locations.len();
    let mut coords = Vec::with_capacity(n);
    for (i, l) in p.locations.iter().enumerate() {
        if !(l.lng.is_finite() && l.lat.is_finite() && l.lng.abs() <= 180.0 && l.lat.abs() <= 90.0)
        {
            return err(format!(
                "locations[{i}] has invalid coordinates ({}, {})",
                l.lng, l.lat
            ));
        }
        coords.push((l.lng, l.lat));
    }

    let estimator = Estimator::new(p.fallback.as_ref(), &coords)?;
    let estimate_buffer = p.estimate_buffer.unwrap_or(DEFAULT_ESTIMATE_BUFFER);
    for (field, value) in [
        ("model_permille", estimate_buffer.model_permille),
        ("fallback_permille", estimate_buffer.fallback_permille),
    ] {
        if value > MAX_BUFFER_PERMILLE as u32 {
            return err(format!(
                "estimate_buffer.{field} must not exceed {MAX_BUFFER_PERMILLE}"
            ));
        }
    }
    let unknown_matrix;
    let matrices: &[pb::TravelMatrix] = if p.matrices.is_empty() {
        let blank = UNKNOWN.to_le_bytes().repeat(n * n);
        unknown_matrix = [pb::TravelMatrix {
            profile: "fallback".into(),
            durations: blank.clone(),
            distances: blank,
            sources: Vec::new(),
        }];
        &unknown_matrix
    } else {
        &p.matrices
    };
    let mut compiled_matrices = Vec::with_capacity(matrices.len());
    let mut sources = Vec::with_capacity(matrices.len());
    let mut stats = MatrixStats::default();
    for (mi, m) in matrices.iter().enumerate() {
        let (mut matrix, source) = decode_matrix(mi, m, n, &coords, &estimator, &mut stats)?;
        matrix.buffer_permille = buffer_permille(&source, &estimate_buffer);
        compiled_matrices.push(matrix);
        sources.push(source);
    }

    let dims = p.capacity_dimensions.len();
    let mut dim_penalty_unit = Vec::with_capacity(dims);
    for (d, dim) in p.capacity_dimensions.iter().enumerate() {
        if dim.penalty_unit < 0 {
            return err(format!(
                "capacity_dimensions[{d}].penalty_unit must not be negative"
            ));
        }
        dim_penalty_unit.push(dim.penalty_unit.max(1));
    }

    let mut skills = Dictionary::default();
    let mut qualifications = Dictionary::default();
    let mut depots = Dictionary::default();

    let mut agents = Vec::with_capacity(p.agents.len());
    for a in &p.agents {
        agents.push(AgentSpec {
            id: a.id.clone(),
            day: a.day,
            start_location: a.start_location,
            end_location: a.end_location,
            start_time: a.start_time,
            end_time: a.end_time,
            capacity: a.capacity.clone(),
            tolerance_permille: a.capacity_tolerance_permille.clone(),
            max_tickets: a.max_tickets,
            fixed_cost: a.fixed_cost,
            restricted: a.restricted,
            skills: a.skills.iter().map(|s| skills.id(s)).collect(),
            qualifications: a
                .qualification_levels
                .iter()
                .map(|(k, v)| (qualifications.id(k), *v))
                .collect(),
            depot: depots.optional(&a.depot_id),
            matrix: a.matrix,
        });
    }

    let mut tickets = Vec::with_capacity(p.tickets.len());
    for (i, t) in p.tickets.iter().enumerate() {
        let kind = match TicketType::try_from(t.r#type) {
            Ok(TicketType::Service) => TicketKind::Service,
            Ok(TicketType::Delivery) => TicketKind::Delivery,
            Ok(TicketType::Pickup) => TicketKind::Pickup,
            Err(_) => return err(format!("tickets[{i}].type has unknown value {}", t.r#type)),
        };
        tickets.push(TicketSpec {
            id: t.id.clone(),
            location: t.location,
            kind,
            duration: t.duration,
            earliest_start: t.earliest_start,
            latest_start: t.latest_start,
            latest_end: t.latest_end,
            day: t.day,
            demand: t.demand.clone(),
            skills_required: t.skills_required.iter().map(|s| skills.id(s)).collect(),
            qualification_required: t
                .qualification_required
                .iter()
                .map(|(k, v)| (qualifications.id(k), *v))
                .collect(),
            depot: depots.optional(&t.depot_id),
            original_agent: t.original_agent,
            pinned: t.pinned,
        });
    }

    let problem = Problem::new(ProblemSpec {
        execution: p.execution.as_ref().map(execution_spec),
        dims,
        dim_penalty_unit,
        matrices: compiled_matrices,
        agents,
        tickets,
        related_pairs: p
            .related_pairs
            .iter()
            .map(|x| (x.first, x.second))
            .collect(),
        precedences: p.precedences.iter().map(|x| (x.first, x.second)).collect(),
        weights: weights_from_proto(p.weights.as_ref()),
        restricted_charge_per_hour: p.costs.as_ref().map_or(0, |c| c.restricted_charge_per_hour),
        day_starts: p
            .horizon
            .as_ref()
            .map_or_else(Vec::new, |h| h.day_starts.clone()),
    })?;

    let initial = match request.initial_solution.as_ref() {
        Some(s) => {
            let mut solution = Solution::empty(problem.agents.len());
            let mut seen = vec![false; problem.agents.len()];
            for r in &s.routes {
                let a = r.agent as usize;
                if a >= problem.agents.len() {
                    return err(format!("initial_solution: agent {a} out of range"));
                }
                if std::mem::replace(&mut seen[a], true) {
                    return err(format!(
                        "initial_solution: agent {a} has more than one route"
                    ));
                }
                solution.routes[a] = r.tickets.clone();
            }
            solution.validate(&problem)?;
            Some(solution)
        }
        _ => None,
    };

    Ok(Compiled {
        problem,
        initial,
        options,
        sources,
        stats,
        estimate_buffer,
    })
}

fn execution_spec(e: &pb::ExecutionContext) -> vrp_core::execution::ExecutionSpec {
    use vrp_core::execution as x;
    x::ExecutionSpec {
        routes: e
            .routes
            .iter()
            .map(|r| {
                (
                    r.agent,
                    x::ExecutedRoute {
                        initial_load: r.initial_load.as_ref().map(|v| v.values.clone()),
                        completed_tickets: r.completed_tickets,
                        worked_seconds: r.worked_seconds,
                        delivered: r.delivered.clone(),
                        fixed_cost_paid: r.fixed_cost_paid,
                        unavailable: r.unavailable,
                        must_return: r.must_return,
                        locked_prefix: r.locked_prefix.clone(),
                    },
                )
            })
            .collect(),
        tickets: e
            .tickets
            .iter()
            .map(|r| {
                (
                    r.ticket,
                    x::ExecutionTicket {
                        only_agent: r.only_agent,
                        blocked: r.blocked,
                        auxiliary: r.auxiliary,
                        load_before: r.load_before,
                        original_position: r.original_position,
                        original_start: r.original_start,
                        committed_earliest: r.committed_earliest,
                        committed_latest: r.committed_latest,
                    },
                )
            })
            .collect(),
        past_dependencies: e
            .past_dependencies
            .iter()
            .map(|r| x::PastDependency {
                ticket: r.ticket,
                agent: r.agent,
                finished_at: r.finished_at,
                same_agent: r.same_agent,
                after_finish: r.after_finish,
                related: r.related,
            })
            .collect(),
    }
}

/// 按边的来源给出排班余量；没有需要放大的边时返回空表。
fn buffer_permille(sources: &[u8], buffer: &pb::EstimateBuffer) -> Vec<u16> {
    let permille = |s: u8| match EdgeSource::try_from(s as i32) {
        Ok(EdgeSource::Model) => buffer.model_permille as u16,
        Ok(EdgeSource::Fallback) => buffer.fallback_permille as u16,
        _ => 0,
    };
    if sources.iter().all(|&s| permille(s) == 0) {
        return Vec::new();
    }
    sources.iter().map(|&s| permille(s)).collect()
}

fn decode_u32s(bytes: &[u8], expected: usize, what: &str) -> Result<Vec<u32>, ModelError> {
    if bytes.len() != expected * 4 {
        return err(format!(
            "{what} must hold {expected} little-endian u32 values, got {} bytes",
            bytes.len()
        ));
    }
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect())
}

fn decode_matrix(
    index: usize,
    m: &pb::TravelMatrix,
    n: usize,
    coords: &[(f64, f64)],
    estimator: &Estimator,
    stats: &mut MatrixStats,
) -> Result<(Matrix, Vec<u8>), ModelError> {
    let what = |field: &str| format!("matrices[{index}].{field}");
    let mut duration = decode_u32s(&m.durations, n * n, &what("durations"))?;
    let mut distance = decode_u32s(&m.distances, n * n, &what("distances"))?;
    let mut source = match m.sources.len() {
        0 => vec![EdgeSource::Exact as u8; n * n],
        len if len == n * n => m.sources.clone(),
        len => {
            return err(format!(
                "{} must be empty or hold {} bytes, got {len}",
                what("sources"),
                n * n
            ));
        }
    };
    for from in 0..n {
        for to in 0..n {
            let k = from * n + to;
            let (d, s) = (duration[k], distance[k]);
            if from == to {
                duration[k] = 0;
                distance[k] = 0;
                source[k] = EdgeSource::Exact as u8;
                continue;
            }
            stats.entries += 1;
            if (d == UNKNOWN) != (s == UNKNOWN) || (d == UNREACHABLE) != (s == UNREACHABLE) {
                return err(format!(
                    "{} entry ({from}, {to}): durations and distances disagree on reserved values",
                    what("")
                ));
            }
            if d == UNKNOWN {
                let (dur, dist) = estimator.estimate(coords[from], coords[to]);
                duration[k] = dur;
                distance[k] = dist;
                source[k] = EdgeSource::Fallback as u8;
                stats.fallback += 1;
                continue;
            }
            if d == UNREACHABLE {
                stats.unreachable += 1;
                continue;
            }
            match EdgeSource::try_from(source[k] as i32) {
                Ok(EdgeSource::Exact) => stats.exact += 1,
                Ok(EdgeSource::Cached) => stats.cached += 1,
                Ok(EdgeSource::Model) => stats.model += 1,
                Ok(EdgeSource::Fallback) => stats.fallback += 1,
                Err(_) => {
                    return err(format!(
                        "{} entry ({from}, {to}) has unknown source {}",
                        what("sources"),
                        source[k]
                    ));
                }
            }
        }
    }
    Ok((
        Matrix {
            n,
            duration,
            distance,
            buffer_permille: Vec::new(),
        },
        source,
    ))
}
