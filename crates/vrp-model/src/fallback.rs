//! 矩阵未知边的兜底估算：确定性的几何距离 × 绕行系数，按距离分段车速换算时长。

use std::f64::consts::PI;

use vrp_proto::engine::v1::FallbackEstimator;
use vrp_proto::engine::v1::fallback_estimator::Metric;

use crate::ModelError;

/// 默认车速分段（米, km/h），按最小距离降序。
pub const DEFAULT_SPEED_BANDS: [(u32, u32); 5] = [
    (30_000, 60),
    (20_000, 45),
    (10_000, 30),
    (5_000, 20),
    (0, 15),
];

const EARTH_RADIUS_M: f64 = 6_371_000.0;
const METERS_PER_DEGREE: f64 = EARTH_RADIUS_M * PI / 180.0;
/// 估算值的上限，避开矩阵保留值。
const MAX_ESTIMATE: f64 = (u32::MAX - 2) as f64;

#[derive(Clone, Debug)]
pub struct Estimator {
    metric: Metric,
    detour: f64,
    /// (最小距离, km/h)，按最小距离降序。
    bands: Vec<(u32, u32)>,
    /// 曼哈顿距离的经度换算系数：cos(场景平均纬度)。整个问题只算一次，保证同一平台上可复现。
    lng_scale: f64,
}

impl Estimator {
    /// `locations` 为 (经度, 纬度)。
    pub fn new(
        config: Option<&FallbackEstimator>,
        locations: &[(f64, f64)],
    ) -> Result<Estimator, ModelError> {
        let metric = config.map_or(Metric::Manhattan, |c| c.metric());
        let detour_permille = config.map_or(0, |c| c.detour_permille);
        let mut bands: Vec<(u32, u32)> = match config {
            Some(c) if !c.speed_bands.is_empty() => c
                .speed_bands
                .iter()
                .map(|b| (b.min_distance_m, b.speed_kmh))
                .collect(),
            _ => DEFAULT_SPEED_BANDS.to_vec(),
        };
        if bands.iter().any(|b| b.1 == 0) {
            return Err(ModelError(
                "fallback.speed_bands: speed_kmh must be positive".into(),
            ));
        }
        bands.sort_by_key(|b| std::cmp::Reverse(b.0));
        let mean_lat = if locations.is_empty() {
            0.0
        } else {
            locations.iter().map(|l| l.1).sum::<f64>() / locations.len() as f64
        };
        Ok(Estimator {
            metric,
            detour: if detour_permille == 0 {
                1.0
            } else {
                detour_permille as f64 / 1000.0
            },
            bands,
            lng_scale: (mean_lat * PI / 180.0).cos(),
        })
    }

    /// 估算 from → to 的（时长秒, 距离米）。
    pub fn estimate(&self, from: (f64, f64), to: (f64, f64)) -> (u32, u32) {
        self.estimate_with_scale(from, to, self.lng_scale)
    }

    /// New automatic preparation uses a local latitude scale; legacy replay
    /// continues to use the scene-wide scale through `estimate`.
    pub(crate) fn estimate_local(&self, from: (f64, f64), to: (f64, f64)) -> (u32, u32) {
        self.estimate_with_scale(from, to, ((from.1 + to.1) * PI / 360.0).cos())
    }

    fn estimate_with_scale(&self, from: (f64, f64), to: (f64, f64), lng_scale: f64) -> (u32, u32) {
        let base = match self.metric {
            Metric::Manhattan => {
                let dx = (from.0 - to.0).abs() * METERS_PER_DEGREE * lng_scale;
                let dy = (from.1 - to.1).abs() * METERS_PER_DEGREE;
                dx + dy
            }
            Metric::Haversine => haversine(from, to),
        };
        let meters = (base * self.detour).min(MAX_ESTIMATE);
        let speed = self
            .bands
            .iter()
            .find(|b| meters >= b.0 as f64)
            .or(self.bands.last())
            .map_or(15, |b| b.1);
        let seconds = (meters * 3.6 / speed as f64).min(MAX_ESTIMATE);
        (seconds.round() as u32, meters.round() as u32)
    }
}

pub(crate) fn haversine(from: (f64, f64), to: (f64, f64)) -> f64 {
    let (lat1, lat2) = (from.1.to_radians(), to.1.to_radians());
    let dlat = lat2 - lat1;
    let dlng = (to.0 - from.0).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlng / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().min(1.0).asin()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vrp_proto::engine::v1::SpeedBand;

    #[test]
    fn manhattan_scales_longitude_by_latitude() {
        // 北京附近：东西向 0.1 度约 8.5 公里，南北向 0.1 度约 11.1 公里。
        let est = Estimator::new(None, &[(116.4, 39.9)]).unwrap();
        let (_, east) = est.estimate((116.4, 39.9), (116.5, 39.9));
        let (_, north) = est.estimate((116.4, 39.9), (116.4, 40.0));
        assert!((8_400..8_600).contains(&east), "east-west {east}");
        assert!((11_000..11_200).contains(&north), "north-south {north}");
    }

    #[test]
    fn speed_bands_pick_the_largest_matching_minimum() {
        let est = Estimator::new(None, &[(0.0, 0.0)]).unwrap();
        // 约 3.3 公里：15 km/h → 约 800 秒。
        let (seconds, meters) = est.estimate((0.0, 0.0), (0.03, 0.0));
        assert_eq!(meters, 3_336);
        assert_eq!(seconds, 801);
        let config = FallbackEstimator {
            metric: Metric::Haversine as i32,
            detour_permille: 1300,
            speed_bands: vec![SpeedBand {
                min_distance_m: 0,
                speed_kmh: 36,
            }],
        };
        let est = Estimator::new(Some(&config), &[(0.0, 0.0)]).unwrap();
        let (seconds, meters) = est.estimate((0.0, 0.0), (0.0, 0.01));
        assert_eq!(meters, 1_446);
        assert_eq!(seconds, 145);
    }

    #[test]
    fn zero_speed_is_rejected() {
        let config = FallbackEstimator {
            speed_bands: vec![SpeedBand {
                min_distance_m: 0,
                speed_kmh: 0,
            }],
            ..FallbackEstimator::default()
        };
        assert!(Estimator::new(Some(&config), &[]).is_err());
    }
}
