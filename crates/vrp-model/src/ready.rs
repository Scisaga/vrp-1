//! Explicit matrix preparation and the strict boundary before freezing a solve.
//!
//! This does not change `compile`'s historical fallback semantics. New application
//! inputs can opt into `prepare_matrices`; legacy replay uses materialization.

use crate::{MatrixStats, ModelError, compile, fallback::Estimator, materialize_matrices};
use vrp_core::problem::{UNKNOWN, UNREACHABLE};
use vrp_proto::engine::v1::{self as pb, EdgeSource, fallback_estimator::Metric};

/// Require complete native matrices before a request is frozen for a new job.
/// Empty sources retain the protobuf contract's meaning (all EXACT).
pub fn validate_ready_matrices(request: &pb::SolveRequest) -> Result<(), ModelError> {
    let p = request
        .problem
        .as_ref()
        .ok_or_else(|| ModelError("problem is required".into()))?;
    let n = p.locations.len();
    let cells = n
        .checked_mul(n)
        .ok_or_else(|| ModelError("matrix dimensions overflow".into()))?;
    let bytes = cells
        .checked_mul(4)
        .ok_or_else(|| ModelError("matrix byte size overflows".into()))?;
    if p.matrices.is_empty() {
        return Err(ModelError(
            "matrices are not ready: prepare matrices before solving".into(),
        ));
    }
    for (i, agent) in p.agents.iter().enumerate() {
        if agent.matrix as usize >= p.matrices.len() {
            return Err(ModelError(format!("agents[{i}].matrix is out of range")));
        }
    }
    for (mi, matrix) in p.matrices.iter().enumerate() {
        if matrix.durations.len() != bytes || matrix.distances.len() != bytes {
            return Err(ModelError(format!(
                "matrices[{mi}] must contain {cells} duration/distance pairs"
            )));
        }
        if !matrix.sources.is_empty() && matrix.sources.len() != cells {
            return Err(ModelError(format!(
                "matrices[{mi}].sources must be empty or contain {cells} bytes"
            )));
        }
        for (k, (duration, distance)) in matrix
            .durations
            .as_chunks::<4>()
            .0
            .iter()
            .zip(matrix.distances.as_chunks::<4>().0)
            .enumerate()
        {
            let duration = u32::from_le_bytes(*duration);
            let distance = u32::from_le_bytes(*distance);
            if let Some(&source) = matrix.sources.get(k)
                && EdgeSource::try_from(i32::from(source)).is_err()
            {
                return Err(ModelError(format!(
                    "matrices[{mi}].sources[{k}] is invalid"
                )));
            }
            if duration == UNKNOWN || distance == UNKNOWN {
                return Err(ModelError(format!(
                    "matrices[{mi}] entry ({}, {}) is UNKNOWN; prepare matrices before solving",
                    k / n,
                    k % n
                )));
            }
            if (duration == UNREACHABLE) != (distance == UNREACHABLE) {
                return Err(ModelError(format!(
                    "matrices[{mi}] entry ({}, {}) disagrees on UNREACHABLE",
                    k / n,
                    k % n
                )));
            }
            if k / n == k % n && (duration != 0 || distance != 0) {
                return Err(ModelError(format!("matrices[{mi}] diagonal must be zero")));
            }
        }
    }
    Ok(())
}

/// Complete missing edges for new application inputs. Existing values and
/// explicit fallback settings are preserved. Without explicit settings, pairs
/// at most 30 km apart use Manhattan distance; longer pairs use Haversine × 1.3.
/// Values remain unbuffered and generated edges are marked FALLBACK.
/// The request is changed only after preparation and validation succeed.
pub fn prepare_matrices(request: &mut pb::SolveRequest) -> Result<MatrixStats, ModelError> {
    let mut prepared = request.clone();
    let original = request
        .problem
        .as_ref()
        .ok_or_else(|| ModelError("problem is required".into()))?;
    let automatic = original.fallback.is_none();
    let missing: Vec<Vec<bool>> = original
        .matrices
        .iter()
        .map(|m| {
            m.durations
                .as_chunks::<4>()
                .0
                .iter()
                .map(|v| u32::from_le_bytes(*v) == UNKNOWN)
                .collect()
        })
        .collect();
    materialize_matrices(&mut prepared)?;
    if automatic {
        let p = prepared.problem.as_mut().expect("materialized problem");
        let coordinates: Vec<_> = p.locations.iter().map(|l| (l.lng, l.lat)).collect();
        let manhattan = Estimator::new(None, &coordinates)?;
        let long_distance = Estimator::new(
            Some(&pb::FallbackEstimator {
                metric: Metric::Haversine as i32,
                detour_permille: 1300,
                ..Default::default()
            }),
            &coordinates,
        )?;
        let n = coordinates.len();
        for (mi, matrix) in p.matrices.iter_mut().enumerate() {
            for from in 0..n {
                for to in 0..n {
                    let k = from * n + to;
                    if !missing.is_empty() && !missing[mi][k] {
                        continue;
                    }
                    let (duration, distance) = if from == to {
                        (0, 0)
                    } else {
                        let direct = crate::fallback::haversine(coordinates[from], coordinates[to]);
                        if direct <= 30_000.0 {
                            let start = coordinates[from];
                            let end = coordinates[to];
                            let longitude = if (end.0 - start.0).abs() > 180.0 {
                                start.0 + (end.0 - start.0 + 180.0).rem_euclid(360.0) - 180.0
                            } else {
                                end.0
                            };
                            manhattan.estimate_local(start, (longitude, end.1))
                        } else {
                            long_distance.estimate(coordinates[from], coordinates[to])
                        }
                    };
                    matrix.durations[k * 4..k * 4 + 4].copy_from_slice(&duration.to_le_bytes());
                    matrix.distances[k * 4..k * 4 + 4].copy_from_slice(&distance.to_le_bytes());
                    matrix.sources[k] = EdgeSource::Fallback as u8;
                }
            }
        }
    }
    validate_ready_matrices(&prepared)?;
    let stats = compile(&prepared)?.stats;
    *request = prepared;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> pb::SolveRequest {
        pb::SolveRequest {
            problem: Some(pb::Problem {
                locations: vec![
                    pb::Location {
                        id: "a".into(),
                        lng: 0.0,
                        lat: 0.0,
                    },
                    pb::Location {
                        id: "near".into(),
                        lng: 0.01,
                        lat: 0.01,
                    },
                    pb::Location {
                        id: "far".into(),
                        lng: 1.0,
                        lat: 1.0,
                    },
                ],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn value(bytes: &[u8], k: usize) -> u32 {
        u32::from_le_bytes(bytes[k * 4..k * 4 + 4].try_into().unwrap())
    }

    #[test]
    fn new_preparation_chooses_short_and_long_distance_models_without_changing_compile() {
        let mut input = request();
        let legacy = compile(&input).unwrap();
        let stats = prepare_matrices(&mut input).unwrap();
        validate_ready_matrices(&input).unwrap();
        let matrix = &input.problem.as_ref().unwrap().matrices[0];
        assert_eq!(stats.fallback, 6);
        assert_eq!(
            value(&matrix.distances, 1),
            legacy.problem.matrices[0].distance[1]
        );
        assert!((204_000..205_000).contains(&value(&matrix.distances, 2)));
        assert!(value(&matrix.distances, 2) < legacy.problem.matrices[0].distance[2]);
        assert!(
            matrix
                .sources
                .iter()
                .all(|&s| s == EdgeSource::Fallback as u8)
        );
        assert_eq!(
            compile(&request()).unwrap().problem.matrices[0].distance[2],
            legacy.problem.matrices[0].distance[2]
        );
    }

    #[test]
    fn automatic_short_distance_crosses_the_antimeridian_without_changing_legacy_replay() {
        let mut input = request();
        for (location, longitude) in input
            .problem
            .as_mut()
            .unwrap()
            .locations
            .iter_mut()
            .zip([179.99, -179.99, 179.98])
        {
            location.lng = longitude;
            location.lat = 0.0;
        }
        let legacy = compile(&input).unwrap().problem.matrices[0].distance[1];
        assert!(legacy > 39_000_000);
        prepare_matrices(&mut input).unwrap();
        let matrix = &input.problem.as_ref().unwrap().matrices[0];
        assert_eq!(value(&matrix.distances, 1), 2_224);
        assert_eq!(value(&matrix.distances, 3), 2_224);
        assert_eq!(matrix.sources[1], EdgeSource::Fallback as u8);
        let mut replay = input.clone();
        replay.problem.as_mut().unwrap().matrices.clear();
        assert_eq!(
            compile(&replay).unwrap().problem.matrices[0].distance[1],
            legacy
        );
    }
    #[test]
    fn automatic_polar_short_distance_is_independent_of_other_latitudes() {
        let mut input = request();
        let locations = &mut input.problem.as_mut().unwrap().locations;
        locations[0].lng = 0.0;
        locations[0].lat = 89.999;
        locations[1].lng = 90.0;
        locations[1].lat = 89.999;
        locations[2].lng = 0.0;
        locations[2].lat = -20.0;
        prepare_matrices(&mut input).unwrap();
        let matrix = &input.problem.as_ref().unwrap().matrices[0];
        assert_eq!(value(&matrix.distances, 1), 175);
        assert_eq!(value(&matrix.distances, 3), 175);
    }
    #[test]
    fn explicit_fallback_and_supplied_values_are_preserved() {
        let mut input = request();
        input.problem.as_mut().unwrap().fallback = Some(pb::FallbackEstimator {
            detour_permille: 2000,
            ..Default::default()
        });
        let expected = compile(&input).unwrap().problem.matrices[0]
            .distance
            .clone();
        prepare_matrices(&mut input).unwrap();
        assert_eq!(
            compile(&input).unwrap().problem.matrices[0].distance,
            expected
        );
        let p = input.problem.as_mut().unwrap();
        p.fallback = None;
        p.matrices[0].distances[4..8].copy_from_slice(&19u32.to_le_bytes());
        prepare_matrices(&mut input).unwrap();
        assert_eq!(
            value(&input.problem.as_ref().unwrap().matrices[0].distances, 1),
            19
        );
    }

    #[test]
    fn gate_rejects_unknown_bad_sources_nonzero_diagonals_and_unpaired_sentinels() {
        let mut input = request();
        assert!(validate_ready_matrices(&input).is_err());
        prepare_matrices(&mut input).unwrap();
        let valid = input.clone();
        for (field, value) in [(0, UNKNOWN), (1, UNREACHABLE), (2, 1)] {
            input = valid.clone();
            let m = &mut input.problem.as_mut().unwrap().matrices[0];
            let k = if field == 2 { 0 } else { 1 };
            m.durations[k * 4..k * 4 + 4].copy_from_slice(&value.to_le_bytes());
            assert!(validate_ready_matrices(&input).is_err());
        }
        input = valid;
        input.problem.as_mut().unwrap().matrices[0].sources[0] = 99;
        assert!(validate_ready_matrices(&input).is_err());
    }

    #[test]
    fn gate_checks_shape_agent_references_and_sources_even_on_unreachable_edges() {
        let mut input = request();
        prepare_matrices(&mut input).unwrap();
        let valid = input.clone();
        input.problem.as_mut().unwrap().matrices[0].sources.pop();
        assert!(validate_ready_matrices(&input).is_err());
        input = valid.clone();
        input.problem.as_mut().unwrap().matrices[0].durations.pop();
        assert!(validate_ready_matrices(&input).is_err());
        input = valid.clone();
        input.problem.as_mut().unwrap().agents.push(pb::Agent {
            matrix: 1,
            ..Default::default()
        });
        assert!(validate_ready_matrices(&input).is_err());
        input = valid;
        let matrix = &mut input.problem.as_mut().unwrap().matrices[0];
        matrix.durations[4..8].copy_from_slice(&UNREACHABLE.to_le_bytes());
        matrix.distances[4..8].copy_from_slice(&UNREACHABLE.to_le_bytes());
        validate_ready_matrices(&input).unwrap();
        input.problem.as_mut().unwrap().matrices[0].sources[1] = 99;
        assert!(validate_ready_matrices(&input).is_err());
    }

    #[test]
    fn failed_preparation_does_not_change_the_request() {
        let mut input = request();
        input.problem.as_mut().unwrap().fallback = Some(pb::FallbackEstimator {
            speed_bands: vec![pb::SpeedBand {
                speed_kmh: 0,
                ..Default::default()
            }],
            ..Default::default()
        });
        let original = input.clone();
        assert!(prepare_matrices(&mut input).is_err());
        assert_eq!(input, original);
    }

    #[test]
    fn preparation_fills_only_unknown_edges_and_preserves_confirmed_unreachable_pairs() {
        let mut input = request();
        prepare_matrices(&mut input).unwrap();
        let expected = input.problem.as_ref().unwrap().matrices[0].distances[8..12].to_vec();
        let matrix = &mut input.problem.as_mut().unwrap().matrices[0];
        matrix.durations[4..8].copy_from_slice(&UNKNOWN.to_le_bytes());
        matrix.distances[4..8].copy_from_slice(&UNKNOWN.to_le_bytes());
        matrix.durations[12..16].copy_from_slice(&UNREACHABLE.to_le_bytes());
        matrix.distances[12..16].copy_from_slice(&UNREACHABLE.to_le_bytes());
        let stats = prepare_matrices(&mut input).unwrap();
        let matrix = &input.problem.as_ref().unwrap().matrices[0];
        assert!(value(&matrix.durations, 1) < UNKNOWN);
        assert_eq!(value(&matrix.durations, 3), UNREACHABLE);
        assert_eq!(value(&matrix.distances, 3), UNREACHABLE);
        assert_eq!(&matrix.distances[8..12], expected);
        assert_eq!(stats.unreachable, 1);
        validate_ready_matrices(&input).unwrap();
    }
}
