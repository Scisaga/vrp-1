use serde_json::json;
use vrp_core::{Solution, oracle};
use vrp_proto::engine::v1 as e;

fn sample() -> serde_json::Value {
    json!({"name":"Replay","planning_date":"2025-06-21","plan":{
        "pois":[{"id":"b","location":"116.42,39.92"},{"id":"a","location":"116.41,39.91"}],
        "agents":[{"id":"v","date":"2025-06-21","start_loc":"a","tickets":["t"]}],
        "tickets":[{"id":"t","loc":"b","type":"Inst","duration":"PT10M","min_start_time":"2025-06-21 00:00:00","max_end_time":"2025-06-21 23:00:00"}],
        "matrix":{"data":{"a":{"b":{"duration":91,"distance":1234}}}}
    }})
}

#[test]
fn historical_values_are_not_recomputed_and_missing_direction_is_materialized() {
    let raw = sample();
    let scene = vrp_import_v1::scene::export(&raw, "case", "synthetic.json", "hash", Some("WGS84"))
        .unwrap();
    assert!(scene.origin.as_ref().unwrap().historical_values_unverified);
    let p = scene.request.as_ref().unwrap().problem.as_ref().unwrap();
    assert_eq!(p.locations[0].id, "a"); // JSON POI order differs from matrix order.
    let m = &p.matrices[0];
    assert_eq!(
        u32::from_le_bytes(m.durations[4..8].try_into().unwrap()),
        91
    );
    assert_eq!(
        u32::from_le_bytes(m.distances[4..8].try_into().unwrap()),
        1234
    );
    assert_eq!(m.sources, vec![0, 0, 3, 0]);
    let before = vrp_model::compile(&vrp_import_v1::import(&raw).unwrap().request).unwrap();
    let after = vrp_model::compile(scene.request.as_ref().unwrap()).unwrap();
    assert_eq!(
        before.problem.matrices[0].duration,
        after.problem.matrices[0].duration
    );
    assert_eq!(
        before.problem.matrices[0].distance,
        after.problem.matrices[0].distance
    );
    assert_eq!(before.sources, after.sources);
    let solution = Solution {
        routes: vec![vec![0]],
    };
    assert_eq!(
        oracle::evaluate(&before.problem, &solution).score,
        oracle::evaluate(&after.problem, &solution).score
    );
}

#[test]
fn empty_matrix_and_explicit_unreachable_remain_distinct() {
    let mut raw = sample();
    raw["plan"].as_object_mut().unwrap().remove("matrix");
    let scene = vrp_import_v1::scene::export(&raw, "case", "test", "hash", Some("WGS84")).unwrap();
    let c = vrp_model::compile(scene.request.as_ref().unwrap()).unwrap();
    assert_eq!(c.stats.fallback, 2);
    raw["plan"]["matrix"] = json!({"data":{"a":{"b":{"unreachable":true}}}});
    let scene = vrp_import_v1::scene::export(&raw, "case", "test", "hash", Some("WGS84")).unwrap();
    let c = vrp_model::compile(scene.request.as_ref().unwrap()).unwrap();
    assert_eq!(c.problem.matrices[0].duration[1], u32::MAX);
    assert_eq!(c.stats.unreachable, 1);
    assert_eq!(c.stats.fallback, 1);
}

#[test]
fn default_coordinates_are_gcj_but_here_and_overrides_are_wgs() {
    let mut raw = sample();
    let converted = vrp_import_v1::scene::export(&raw, "case", "test", "hash", None).unwrap();
    assert_eq!(converted.origin.as_ref().unwrap().source_crs, "GCJ-02");
    assert!((converted.request.unwrap().problem.unwrap().locations[0].lng - 116.41).abs() > 0.001);
    raw["map_provider"] = json!("HERE");
    let here = vrp_import_v1::scene::export(&raw, "case", "test", "hash", None).unwrap();
    assert_eq!(
        here.request.unwrap().problem.unwrap().locations[0].lng,
        116.41
    );
}

#[test]
fn native_materialization_preserves_nondefault_buffer_and_sources() {
    let mut request = vrp_import_v1::import(&sample()).unwrap().request;
    request.problem.as_mut().unwrap().estimate_buffer = Some(e::EstimateBuffer {
        model_permille: 75,
        fallback_permille: 350,
    });
    let before = vrp_model::compile(&request).unwrap();
    vrp_model::materialize_matrices(&mut request).unwrap();
    let after = vrp_model::compile(&request).unwrap();
    assert_eq!(
        before.problem.matrices[0].buffer_permille,
        after.problem.matrices[0].buffer_permille
    );
    assert_eq!(
        before.problem.matrices[0].duration,
        after.problem.matrices[0].duration
    );
}
