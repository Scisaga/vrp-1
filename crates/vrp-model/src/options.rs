//! 请求的求解档位与部署端线程额度。这里只解析参数，不启动搜索或工作线程。

use vrp_proto::engine::v1 as pb;

use crate::ModelError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedSolveOptions {
    pub preset: pb::SolvePreset,
    /// None 表示没有总时限。
    pub time_limit_ms: Option<u64>,
    /// None 表示没有无改进时限。
    pub unimproved_stop_ms: Option<u64>,
    pub threads: u32,
    pub seed: u64,
    pub search_strategy: pb::SearchStrategy,
    pub k_opt_depth: u32,
}

/// 参数缺省时使用 FAST；正值覆盖对应时限，0 沿用档位默认值。
pub fn resolve_options(
    options: Option<&pb::SolveOptions>,
) -> Result<ResolvedSolveOptions, ModelError> {
    let options = options.copied().unwrap_or_default();
    let preset = pb::SolvePreset::try_from(options.preset).map_err(|_| {
        ModelError(format!(
            "options.preset has unknown value {}",
            options.preset
        ))
    })?;
    let search_strategy = pb::SearchStrategy::try_from(options.search_strategy)
        .map_err(|_| ModelError("options.search_strategy is unknown".into()))?;
    let k_opt_depth = if options.k_opt_depth == 0 {
        5
    } else {
        options.k_opt_depth
    };
    if !(2..=12).contains(&k_opt_depth) {
        return Err(ModelError(
            "options.k_opt_depth must be 2..12 or 0 for default".into(),
        ));
    }
    let (time_limit_ms, unimproved_stop_ms) = match preset {
        pb::SolvePreset::Fast => (Some(30_000), None),
        pb::SolvePreset::Standard => (Some(120_000), None),
        pb::SolvePreset::Deep => (None, Some(300_000)),
    };
    Ok(ResolvedSolveOptions {
        preset,
        time_limit_ms: (options.time_limit_ms > 0)
            .then_some(options.time_limit_ms)
            .or(time_limit_ms),
        unimproved_stop_ms: (options.unimproved_stop_ms > 0)
            .then_some(options.unimproved_stop_ms)
            .or(unimproved_stop_ms),
        threads: options.threads.max(1),
        seed: options.seed,
        search_strategy,
        k_opt_depth,
    })
}

/// 机器/部署配额的线程预算。CPU 数由宿主提供，必须反映容器配额或 CPU 亲和性限制。
///
/// worker 仍需在申领和释放时管理剩余额度；本类型只负责预算计算和单任务准入校验。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadBudget {
    pub total_threads: u32,
}

impl ThreadBudget {
    pub fn from_cpu_count(available_cpus: u32, multiplier: f64) -> Result<Self, ModelError> {
        if available_cpus == 0 || !multiplier.is_finite() || multiplier <= 0.0 {
            return Err(ModelError(
                "thread budget requires positive CPU count and finite positive multiplier".into(),
            ));
        }
        let total = (f64::from(available_cpus) * multiplier).floor().max(1.0);
        if total > f64::from(u32::MAX) {
            return Err(ModelError("thread budget exceeds u32 range".into()));
        }
        Ok(Self {
            total_threads: total as u32,
        })
    }

    /// 拒绝无法在本预算内执行的任务；暂时没有剩余额度的任务由调度层排队。
    pub fn validate_request(self, options: &ResolvedSolveOptions) -> Result<(), ModelError> {
        if options.threads > self.total_threads {
            return Err(ModelError(format!(
                "options.threads ({}) exceeds total thread budget ({})",
                options.threads, self.total_threads,
            )));
        }
        Ok(())
    }
}
