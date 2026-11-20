use vrp_geo::{
    Bounds, Config, GeoPoint, NoopObserver, Observer, RoadNetwork, Way, WayNode, distance_m,
};

fn p(lng: f64, lat: f64) -> GeoPoint {
    GeoPoint { lng, lat }
}
fn way(id: i64, nodes: &[(i64, f64, f64)], tags: &[(&str, &str)]) -> Way {
    Way {
        id,
        nodes: nodes
            .iter()
            .map(|(id, lng, lat)| WayNode {
                id: *id,
                point: p(*lng, *lat),
            })
            .collect(),
        tags: tags
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect(),
    }
}
fn config() -> Config {
    let mut c = Config {
        connector_speed_kph: 18.0,
        ..Config::default()
    };
    c.speeds_kph.insert("residential".into(), 36.0);
    c
}

#[test]
fn interior_projection_connector_and_matrix_geometry_agree() {
    let network = RoadNetwork::from_ways(
        vec![way(1, &[(1, 0.0, 0.0), (2, 0.01, 0.0)], &[])],
        config(),
    )
    .unwrap();
    let points = [p(0.002, 0.001), p(0.008, 0.001), p(0.002, 0.001)];
    let graph = network.prepare(&points, &mut NoopObserver).unwrap();
    let matrix = graph.matrix(&mut NoopObserver).unwrap();
    let route = graph.route(0, 1, &mut NoopObserver).unwrap().unwrap();
    let road = distance_m(p(0.002, 0.0), p(0.008, 0.0));
    let connector = 2.0 * distance_m(p(0.002, 0.001), p(0.002, 0.0));
    assert!((f64::from(route.distance_m) - road - connector).abs() < 1.0);
    assert!((f64::from(route.duration_s) - (road / 10.0 + connector / 5.0)).abs() < 1.0);
    let estimate = matrix.entries[1].unwrap();
    assert_eq!(
        (estimate.distance_m, estimate.duration_s),
        (route.distance_m, route.duration_s)
    );
    assert_eq!(matrix.entries[2].unwrap().distance_m, 0);
    assert_eq!(route.points.first(), Some(&points[0]));
    assert_eq!(route.points.last(), Some(&points[1]));
    assert_eq!(route.road_start, 1);
    assert_eq!(route.road_end, 2);
    let drawn: f64 = route
        .points
        .windows(2)
        .map(|p| distance_m(p[0], p[1]))
        .sum();
    assert!((drawn - f64::from(route.distance_m)).abs() < 1.0);
}

#[test]
fn one_way_reverse_and_roundabout_are_directional() {
    for tags in [vec![("oneway", "yes")], vec![("junction", "roundabout")]] {
        let network = RoadNetwork::from_ways(
            vec![way(1, &[(1, 0.0, 0.0), (2, 0.01, 0.0)], &tags)],
            config(),
        )
        .unwrap();
        let graph = network
            .prepare(&[p(0.002, 0.0), p(0.008, 0.0)], &mut NoopObserver)
            .unwrap();
        assert!(graph.route(0, 1, &mut NoopObserver).unwrap().is_some());
        assert!(graph.route(1, 0, &mut NoopObserver).unwrap().is_none());
    }
    let network = RoadNetwork::from_ways(
        vec![way(
            1,
            &[(1, 0.0, 0.0), (2, 0.01, 0.0)],
            &[("oneway", "-1")],
        )],
        config(),
    )
    .unwrap();
    let graph = network
        .prepare(&[p(0.002, 0.0), p(0.008, 0.0)], &mut NoopObserver)
        .unwrap();
    assert!(graph.route(0, 1, &mut NoopObserver).unwrap().is_none());
    assert!(graph.route(1, 0, &mut NoopObserver).unwrap().is_some());
}

#[test]
fn crossings_connect_only_via_shared_osm_node() {
    let horizontal = way(1, &[(1, -0.01, 0.0), (2, 0.0, 0.0), (3, 0.01, 0.0)], &[]);
    let detached = way(2, &[(4, 0.0, -0.01), (5, 0.0, 0.0), (6, 0.0, 0.01)], &[]);
    let points = [p(-0.005, 0.0), p(0.0, 0.005)];
    let network =
        RoadNetwork::from_ways(vec![horizontal.clone(), detached.clone()], config()).unwrap();
    assert!(
        network
            .prepare(&points, &mut NoopObserver)
            .unwrap()
            .route(0, 1, &mut NoopObserver)
            .unwrap()
            .is_none()
    );
    let mut connected = detached;
    connected.nodes[1].id = 2;
    let network = RoadNetwork::from_ways(vec![horizontal, connected], config()).unwrap();
    assert!(
        network
            .prepare(&points, &mut NoopObserver)
            .unwrap()
            .route(0, 1, &mut NoopObserver)
            .unwrap()
            .is_some()
    );
}

#[test]
fn time_optimal_route_keeps_its_own_longer_distance() {
    let roads = vec![
        way(1, &[(1, 0.0, 0.0), (2, 0.01, 0.0)], &[("maxspeed", "5")]),
        way(
            2,
            &[
                (1, 0.0, 0.0),
                (3, 0.0, 0.01),
                (4, 0.01, 0.01),
                (2, 0.01, 0.0),
            ],
            &[],
        ),
    ];
    let network = RoadNetwork::from_ways(roads, config()).unwrap();
    let graph = network
        .prepare(&[p(0.0, 0.0), p(0.01, 0.0)], &mut NoopObserver)
        .unwrap();
    let route = graph.route(0, 1, &mut NoopObserver).unwrap().unwrap();
    assert!(route.distance_m > 3000);
    assert!(route.duration_s < 400);
    assert_eq!(
        route.distance_m,
        graph.matrix(&mut NoopObserver).unwrap().entries[1]
            .unwrap()
            .distance_m
    );
    let drawn: f64 = route
        .points
        .windows(2)
        .map(|p| distance_m(p[0], p[1]))
        .sum();
    assert!((drawn - f64::from(route.distance_m)).abs() < 1.0);
}

#[test]
fn sphere_handles_dateline_and_high_latitude() {
    let bounds = Bounds::from_points(&[p(179.9, 80.0), p(-179.9, 80.0)]).unwrap();
    assert!(bounds.west > bounds.east);
    assert!(bounds.width() < 1.0);
    let network = RoadNetwork::from_ways(
        vec![way(1, &[(1, 179.8, 80.0), (2, -179.8, 80.0)], &[])],
        config(),
    )
    .unwrap();
    let graph = network
        .prepare(&[p(179.9, 80.001), p(-179.9, 80.001)], &mut NoopObserver)
        .unwrap();
    let route = graph.route(0, 1, &mut NoopObserver).unwrap().unwrap();
    assert!(route.distance_m < 5000);
    assert!(route.distance_m > 3000);
    assert!(graph.snaps.iter().flatten().all(|s| s.distance_m < 120.0));
    let expanded = Bounds::from_points(&[p(0.0, 89.99)])
        .unwrap()
        .expanded_km(10.0)
        .unwrap();
    assert_eq!(expanded.west, -180.0);
    assert_eq!(expanded.east, 180.0);
}

#[test]
fn access_overrides_and_posted_mph_limits() {
    let mut config = config();
    config.speeds_kph.insert("residential".into(), 100.0);
    let private = way(
        1,
        &[(1, 0.0, 0.0), (2, 0.01, 0.0)],
        &[("access", "private")],
    );
    let network = RoadNetwork::from_ways(vec![private.clone()], config.clone()).unwrap();
    assert!(network.ways.is_empty());
    let mut allowed = private;
    allowed.tags.insert("motorcar".into(), "yes".into());
    allowed.tags.insert("maxspeed".into(), "10 mph".into());
    let network = RoadNetwork::from_ways(vec![allowed], config).unwrap();
    let graph = network
        .prepare(&[p(0.0, 0.0), p(0.01, 0.0)], &mut NoopObserver)
        .unwrap();
    let route = graph.route(0, 1, &mut NoopObserver).unwrap().unwrap();
    assert!((route.duration_s as i64 - 249).abs() < 2);
}

#[test]
fn invalid_inputs_and_cancellation_are_explicit() {
    let mut c = config();
    c.connector_speed_kph = 0.0;
    assert!(RoadNetwork::from_ways(vec![], c).is_err());
    let network = RoadNetwork::from_ways(vec![], config()).unwrap();
    assert!(
        network
            .prepare(&[p(f64::NAN, 0.0)], &mut NoopObserver)
            .is_err()
    );
    struct Cancel;
    impl Observer for Cancel {
        fn is_cancelled(&self) -> bool {
            true
        }
    }
    assert!(network.prepare(&[p(0.0, 0.0)], &mut Cancel).is_err());
    let m = network
        .matrix(&[p(0.0, 0.0), p(1.0, 0.0)], &mut NoopObserver)
        .unwrap();
    assert_eq!(m.entries[0].unwrap().distance_m, 0);
    assert!(m.entries[1].is_none());
}

#[test]
fn cache_roundtrip_and_changed_configuration_have_distinct_hashes() {
    let road = way(1, &[(1, 0.0, 0.0), (2, 0.01, 0.0)], &[]);
    let network = RoadNetwork::from_ways(vec![road.clone()], config()).unwrap();
    let dir = std::env::temp_dir().join(format!(
        "vrp-geo-cache-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = dir.join("graph.json");
    network.save(&path).unwrap();
    let loaded = RoadNetwork::load(&path).unwrap();
    assert_eq!(network.fingerprint, loaded.fingerprint);
    let points = [p(0.002, 0.001), p(0.008, 0.001)];
    assert_eq!(
        network.matrix(&points, &mut NoopObserver).unwrap().entries,
        loaded.matrix(&points, &mut NoopObserver).unwrap().entries
    );
    let mut changed = config();
    changed.connector_speed_kph = 10.0;
    let changed = RoadNetwork::from_ways(vec![road], changed).unwrap();
    assert_ne!(network.fingerprint, changed.fingerprint);
    assert!(changed.save(&path).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(&dir).unwrap();
}

#[test]
fn degenerate_ways_and_zero_length_segments_never_leave_dangling_snap_keys() {
    let network =
        RoadNetwork::from_ways(vec![way(1, &[(1, 0.0, 0.0), (1, 0.0, 0.0)], &[])], config())
            .unwrap();
    assert!(network.ways.is_empty());
    let points = [p(0.0, 0.001), p(0.0, 0.002)];
    assert!(network.matrix(&points, &mut NoopObserver).unwrap().entries[1].is_none());
    let network =
        RoadNetwork::from_ways(vec![way(1, &[(1, 0.0, 0.0), (2, 0.0, 0.0)], &[])], config())
            .unwrap();
    let graph = network.prepare(&points, &mut NoopObserver).unwrap();
    assert!(graph.route(0, 1, &mut NoopObserver).unwrap().is_some());
}

#[test]
fn cancelling_serialization_returns_without_publishing_a_cache() {
    use std::cell::Cell;
    struct CancelAfter(Cell<usize>);
    impl Observer for CancelAfter {
        fn is_cancelled(&self) -> bool {
            let n = self.0.get();
            self.0.set(n + 1);
            n >= 20
        }
    }
    let network = RoadNetwork::from_ways(
        vec![way(1, &[(1, 0.0, 0.0), (2, 0.01, 0.0)], &[])],
        config(),
    )
    .unwrap();
    let path = std::env::temp_dir().join(format!(
        "vrp-geo-cancel-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    assert!(
        network
            .save_observed(&path, &mut CancelAfter(Cell::new(0)))
            .is_err()
    );
    assert!(!path.exists());
}

#[test]
fn cache_preserves_non_decimal_pbf_coordinates_exactly() {
    let network = RoadNetwork::from_ways(
        vec![way(
            1,
            &[
                (1, 116.40000000000002, 39.900000000000006),
                (2, 116.41000000000001, 39.910000000000004),
            ],
            &[],
        )],
        config(),
    )
    .unwrap();
    let path = std::env::temp_dir().join(format!(
        "vrp-geo-float-cache-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    network.save(&path).unwrap();
    let loaded = RoadNetwork::load(&path).unwrap();
    assert_eq!(loaded.fingerprint, network.fingerprint);
    assert_eq!(loaded.ways, network.ways);
    std::fs::remove_file(path).unwrap();
}
