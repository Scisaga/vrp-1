use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use vrp_geo::{Bounds, catalog::Catalog};

fn entry(id: &str, parent: Option<&str>, west: f64, east: f64) -> Value {
    json!({"type":"Feature","properties":{"id":id,"parent":parent,"name":id,"urls":{"pbf":format!("https://example.org/{id}.osm.pbf")}},
        "geometry":{"type":"Polygon","coordinates":[[[west,0.0],[east,0.0],[east,1.0],[west,1.0],[west,0.0]]]}})
}
fn catalog(entries: Vec<Value>) -> Catalog {
    Catalog::parse(&json!({"type":"FeatureCollection","features":entries}).to_string()).unwrap()
}
fn bounds() -> Bounds {
    Bounds {
        south: 0.1,
        west: 0.1,
        north: 0.9,
        east: 1.9,
    }
}

#[test]
fn cached_parent_beats_downloads_and_children_can_beat_parent() {
    let c = catalog(vec![
        entry("parent", None, 0.0, 2.0),
        entry("left", Some("parent"), 0.0, 1.0),
        entry("right", Some("parent"), 1.0, 2.0),
    ]);
    let sizes = BTreeMap::from([
        ("parent".into(), 1000),
        ("left".into(), 100),
        ("right".into(), 100),
    ]);
    let plan = c.plan(bounds(), &sizes, &BTreeSet::new()).unwrap();
    assert!(plan.coverage_complete);
    assert_eq!(plan.download_bytes, 200);
    assert_eq!(plan.packages.len(), 2);
    let plan = c
        .plan(bounds(), &sizes, &BTreeSet::from(["parent".into()]))
        .unwrap();
    assert!(plan.coverage_complete);
    assert_eq!(plan.download_bytes, 0);
    assert_eq!(plan.packages.len(), 1);
    assert!(plan.packages[0].reused);
}

#[test]
fn polygon_holes_and_unknown_sizes_do_not_claim_coverage() {
    let mut polygon = entry("hole", None, 0.0, 2.0);
    polygon["geometry"]["coordinates"]
        .as_array_mut()
        .unwrap()
        .push(json!([
            [0.5, 0.2],
            [1.5, 0.2],
            [1.5, 0.8],
            [0.5, 0.8],
            [0.5, 0.2]
        ]));
    let c = catalog(vec![polygon]);
    let p = c
        .plan(
            bounds(),
            &BTreeMap::from([("hole".into(), 10)]),
            &BTreeSet::new(),
        )
        .unwrap();
    assert!(!p.coverage_complete);
    assert!(p.uncovered_fraction > 0.1);
    let p = c
        .plan(bounds(), &BTreeMap::new(), &BTreeSet::new())
        .unwrap();
    assert!(!p.coverage_complete);
    assert_eq!(p.download_bytes, 0);
    assert!(p.packages.is_empty());
}

#[test]
fn dateline_polygon_and_planning_are_deterministic() {
    let c = catalog(vec![entry("dateline", None, 179.0, -179.0)]);
    let b = Bounds {
        south: 0.1,
        west: 179.5,
        north: 0.9,
        east: -179.5,
    };
    let sizes = BTreeMap::from([("dateline".into(), 100)]);
    let p = c.plan(b, &sizes, &BTreeSet::new()).unwrap();
    assert!(p.coverage_complete);
    assert_eq!(p.download_bytes, 100);
    assert_eq!(c.candidates(b).len(), 1);
    assert_eq!(
        serde_json::to_string(&p).unwrap(),
        serde_json::to_string(&c.plan(b, &sizes, &BTreeSet::new()).unwrap()).unwrap()
    );
}

#[test]
fn cheaper_covering_extract_replaces_combination_without_double_download() {
    let c = catalog(vec![
        entry("parent", None, 0.0, 2.0),
        entry("left", Some("parent"), 0.0, 1.0),
        entry("right", Some("parent"), 1.0, 2.0),
    ]);
    let sizes = BTreeMap::from([
        ("parent".into(), 150),
        ("left".into(), 100),
        ("right".into(), 100),
    ]);
    let plan = c.plan(bounds(), &sizes, &BTreeSet::new()).unwrap();
    assert_eq!(plan.download_bytes, 150);
    assert_eq!(plan.packages.len(), 1);
    assert_eq!(plan.packages[0].region_id, "parent");
}

#[test]
fn budget_keeps_affordable_partial_coverage_instead_of_discarding_everything() {
    let c = catalog(vec![
        entry("parent", None, 0.0, 2.0),
        entry("left", Some("parent"), 0.0, 1.0),
        entry("right", Some("parent"), 1.0, 2.0),
    ]);
    let sizes = BTreeMap::from([
        ("parent".into(), 1000),
        ("left".into(), 100),
        ("right".into(), 100),
    ]);
    let p = c
        .plan_with_budget(bounds(), &sizes, &BTreeSet::new(), 100)
        .unwrap();
    assert_eq!(p.packages.len(), 1);
    assert_eq!(p.download_bytes, 100);
    assert!(!p.coverage_complete);
    let p = c
        .plan_with_budget(bounds(), &sizes, &BTreeSet::from(["left".into()]), 100)
        .unwrap();
    assert_eq!(p.packages.len(), 2);
    assert_eq!(p.download_bytes, 100);
    assert!(p.coverage_complete);
}

#[test]
fn workbench_synthetic_polygon_fully_covers_ten_and_twenty_kilometre_envelopes() {
    let c = catalog(vec![
        json!({"type":"Feature","properties":{"id":"synthetic","name":"synthetic","urls":{"pbf":"http://127.0.0.1/synthetic.osm.pbf"}},"geometry":{"type":"Polygon","coordinates":[[[121.0,30.8],[121.9,30.8],[121.9,31.8],[121.0,31.8],[121.0,30.8]]]}}),
    ]);
    let mut points = vec![vrp_geo::GeoPoint {
        lng: 121.47,
        lat: 31.23,
    }];
    for i in 0..12 {
        points.push(vrp_geo::GeoPoint {
            lng: 121.43 + (i % 4) as f64 * 0.025,
            lat: 31.2 + (i / 4) as f64 * 0.022,
        });
    }
    for buffer in [10.0, 20.0] {
        let bounds = Bounds::from_points(&points)
            .unwrap()
            .expanded_km(buffer)
            .unwrap();
        assert_eq!(c.candidates(bounds).len(), 1);
        for size in [0, 283] {
            let p = c
                .plan_with_budget(
                    bounds,
                    &BTreeMap::from([("synthetic".into(), size)]),
                    &BTreeSet::new(),
                    500 * 1024 * 1024,
                )
                .unwrap();
            assert!(p.coverage_complete, "{p:?}");
            assert_eq!(p.uncovered_fraction, 0.0);
        }
        let p = c
            .plan_with_budget(
                bounds,
                &BTreeMap::new(),
                &BTreeSet::from(["synthetic".into()]),
                0,
            )
            .unwrap();
        assert!(p.coverage_complete, "{p:?}");
    }
}

#[test]
fn point_coverage_respects_holes_boundary_and_antimeridian() {
    use vrp_geo::{GeoPoint, catalog::coverage_contains_point};
    let coverage = json!({"type":"Polygon","coordinates":[[[179.0,0.0],[-179.0,0.0],[-179.0,2.0],[179.0,2.0],[179.0,0.0]],[[179.5,0.5],[-179.5,0.5],[-179.5,1.5],[179.5,1.5],[179.5,0.5]]]});
    assert!(
        coverage_contains_point(
            &coverage,
            GeoPoint {
                lng: 179.2,
                lat: 1.0
            }
        )
        .unwrap()
    );
    assert!(
        coverage_contains_point(
            &coverage,
            GeoPoint {
                lng: -179.2,
                lat: 1.0
            }
        )
        .unwrap()
    );
    assert!(
        !coverage_contains_point(
            &coverage,
            GeoPoint {
                lng: 180.0,
                lat: 1.0
            }
        )
        .unwrap()
    );
    assert!(!coverage_contains_point(&coverage, GeoPoint { lng: 0.0, lat: 1.0 }).unwrap());
    assert!(
        coverage_contains_point(
            &coverage,
            GeoPoint {
                lng: 179.0,
                lat: 0.0
            }
        )
        .unwrap()
    );
}
