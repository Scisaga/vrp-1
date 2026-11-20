use vrp_proto::{app::v1 as app, engine::v1 as engine};

pub(crate) const SAMPLE_INTERVAL_MS: u64 = 1_000;
pub(crate) const MAX_SAMPLES: usize = 2_048;

pub(crate) fn point(job: &app::Job) -> Option<engine::Progress> {
    if job.has_result
        && let (Some(score), Some(stats)) = (job.score, job.stats.as_ref())
    {
        return Some(engine::Progress {
            elapsed_ms: stats.elapsed_ms,
            best_score: Some(score),
            evaluated_moves: stats.evaluated_moves,
            best_found_ms: stats.best_found_ms,
        });
    }
    job.progress
        .as_ref()
        .and_then(|progress| progress.search)
        .filter(|sample| sample.best_score.is_some())
}

pub(crate) fn append(trend: &mut app::JobTrend, sample: engine::Progress, force: bool) -> bool {
    trend.sample_interval_ms = trend.sample_interval_ms.max(SAMPLE_INTERVAL_MS);
    if let Some(last) = trend.samples.last_mut() {
        if sample.elapsed_ms < last.elapsed_ms {
            return false;
        }
        if sample.elapsed_ms == last.elapsed_ms {
            if *last == sample {
                return false;
            }
            *last = sample;
            return true;
        }
        if !force && sample.elapsed_ms - last.elapsed_ms < trend.sample_interval_ms {
            return false;
        }
    }
    trend.samples.push(sample);
    if trend.samples.len() > MAX_SAMPLES {
        let last = *trend.samples.last().expect("sample was appended");
        trend.samples = trend.samples.iter().step_by(2).copied().collect();
        if trend.samples.last().map(|point| point.elapsed_ms) != Some(last.elapsed_ms) {
            trend.samples.push(last);
        }
        trend.sample_interval_ms = trend.sample_interval_ms.saturating_mul(2);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(elapsed_ms: u64, soft: i64) -> engine::Progress {
        engine::Progress {
            elapsed_ms,
            best_score: Some(engine::Score {
                soft,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn sampling_keeps_real_first_final_and_lower_priority_regressions() {
        let mut trend = app::JobTrend::default();
        assert!(append(&mut trend, sample(25, -100), false));
        assert!(!append(&mut trend, sample(100, -90), false));
        assert!(append(&mut trend, sample(1_025, -200), false));
        assert!(append(&mut trend, sample(1_100, -50), true));
        assert_eq!(
            trend
                .samples
                .iter()
                .map(|point| point.elapsed_ms)
                .collect::<Vec<_>>(),
            [25, 1_025, 1_100]
        );
        assert_eq!(trend.samples[1].best_score.unwrap().soft, -200);
        assert!(append(&mut trend, sample(1_100, -40), true));
        assert_eq!(trend.samples.len(), 3);
        assert!(!append(&mut trend, sample(900, -30), true));
    }

    #[test]
    fn long_history_is_bounded_and_retains_both_endpoints() {
        let mut trend = app::JobTrend::default();
        for index in 0..5_000 {
            append(
                &mut trend,
                sample(index * 1_000 + 13, -(index as i64)),
                false,
            );
        }
        append(&mut trend, sample(5_000_020, -5_000), true);
        assert!(trend.samples.len() <= MAX_SAMPLES);
        assert_eq!(trend.samples[0].elapsed_ms, 13);
        assert_eq!(trend.samples.last().unwrap().elapsed_ms, 5_000_020);
        assert!(trend.sample_interval_ms > SAMPLE_INTERVAL_MS);
        assert!(
            trend
                .samples
                .windows(2)
                .all(|pair| pair[0].elapsed_ms < pair[1].elapsed_ms)
        );
    }
}
