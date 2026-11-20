//! Tiny, public synthetic OSM wire fixture. No external map service is queried.
use vrp_geo::{Bounds, Config, GeoPoint, NoopObserver, RoadNetwork, inspect_pbf, validate_pbf};

fn integer(mut n: u64) -> Vec<u8> {
    let mut out = vec![];
    while n >= 128 {
        out.push(n as u8 | 128);
        n >>= 7;
    }
    out.push(n as u8);
    out
}
fn value(field: u64, n: u64) -> Vec<u8> {
    let mut out = integer(field << 3);
    out.extend(integer(n));
    out
}
fn bytes(field: u64, data: &[u8]) -> Vec<u8> {
    let mut out = integer(field << 3 | 2);
    out.extend(integer(data.len() as u64));
    out.extend(data);
    out
}
fn sint(n: i64) -> u64 {
    ((n << 1) ^ (n >> 63)) as u64
}
fn blob(kind: &str, raw: &[u8]) -> Vec<u8> {
    let payload = bytes(1, raw);
    let mut header = bytes(1, kind.as_bytes());
    header.extend(value(3, payload.len() as u64));
    let mut out = (header.len() as u32).to_be_bytes().to_vec();
    out.extend(header);
    out.extend(payload);
    out
}
fn fixture() -> Vec<u8> {
    fixture_at(1000, 100000, false)
}
fn fixture_at(timestamp: u64, right_lng: i64, missing_right: bool) -> Vec<u8> {
    let mut header = bytes(4, b"OsmSchema-V0.6");
    header.extend(value(32, timestamp));
    let mut output = blob("OSMHeader", &header);
    let mut strings = vec![];
    for s in ["", "highway", "residential"] {
        strings.extend(bytes(1, s.as_bytes()));
    }
    let mut group = vec![];
    for (id, lng) in [(1, -100000_i64), (2, right_lng)] {
        if id == 2 && missing_right {
            continue;
        }
        let mut node = value(1, sint(id));
        node.extend(value(8, 0));
        node.extend(value(9, sint(lng)));
        group.extend(bytes(1, &node));
    }
    let mut way = value(1, 1);
    way.extend(bytes(2, &integer(1)));
    way.extend(bytes(3, &integer(2)));
    let mut refs = integer(sint(1));
    refs.extend(integer(sint(1)));
    way.extend(bytes(8, &refs));
    group.extend(bytes(3, &way));
    let mut block = bytes(1, &strings);
    block.extend(bytes(2, &group));
    output.extend(blob("OSMData", &block));
    output
}

#[test]
fn mixed_snapshot_merge_is_order_independent_and_conflicts_are_not_silently_hidden() {
    let directory = std::env::temp_dir().join(format!(
        "vrp-geo-merge-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let old = directory.join("old.pbf");
    let new = directory.join("new.pbf");
    std::fs::write(&old, fixture_at(1000, 100000, false)).unwrap();
    std::fs::write(&new, fixture_at(2000, 110000, false)).unwrap();
    let bounds = Bounds {
        south: -0.001,
        west: -0.001,
        north: 0.001,
        east: 0.001,
    };
    let a = RoadNetwork::from_pbf(
        &[old.clone(), new.clone()],
        bounds,
        Config::default(),
        &mut NoopObserver,
    )
    .unwrap();
    let b = RoadNetwork::from_pbf(
        &[new.clone(), old.clone()],
        bounds,
        Config::default(),
        &mut NoopObserver,
    )
    .unwrap();
    assert_eq!(a.fingerprint, b.fingerprint);
    assert!((a.ways[0].nodes[1].point.lng - 0.011).abs() < 1e-12);
    assert!(a.boundary_limited());
    std::fs::write(&new, fixture_at(1000, 110000, false)).unwrap();
    assert!(
        RoadNetwork::from_pbf(
            &[old.clone(), new.clone()],
            bounds,
            Config::default(),
            &mut NoopObserver
        )
        .is_err()
    );
    std::fs::write(&new, fixture_at(1000, 100000, true)).unwrap();
    validate_pbf(&new, &mut NoopObserver).unwrap();
    let error = RoadNetwork::from_pbf(
        std::slice::from_ref(&new),
        bounds,
        Config::default(),
        &mut NoopObserver,
    )
    .unwrap_err();
    assert!(error.0.contains("missing node"));
    std::fs::remove_file(old).unwrap();
    std::fs::remove_file(new).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn streaming_pbf_keeps_crossing_ways_and_complete_references() {
    let directory = std::env::temp_dir().join(format!(
        "vrp-geo-pbf-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("test.osm.pbf");
    std::fs::write(&file, fixture()).unwrap();
    let info = inspect_pbf(&file).unwrap();
    assert_eq!(info.timestamp_unix, Some(1000));
    assert!(info.bounds.is_none());
    let network = RoadNetwork::from_pbf(
        std::slice::from_ref(&file),
        Bounds {
            south: -0.001,
            west: -0.001,
            north: 0.001,
            east: 0.001,
        },
        Config::default(),
        &mut NoopObserver,
    )
    .unwrap();
    assert_eq!(network.ways.len(), 1);
    assert_eq!(network.ways[0].nodes.len(), 2);
    let graph = network
        .prepare(
            &[
                GeoPoint {
                    lng: -0.0005,
                    lat: 0.0,
                },
                GeoPoint {
                    lng: 0.0005,
                    lat: 0.0,
                },
            ],
            &mut NoopObserver,
        )
        .unwrap();
    assert!(graph.route(0, 1, &mut NoopObserver).unwrap().is_some());
    validate_pbf(&file, &mut NoopObserver).unwrap();
    let mut truncated = fixture();
    truncated.push(0);
    std::fs::write(&file, truncated).unwrap();
    assert!(validate_pbf(&file, &mut NoopObserver).is_err());
    let mut corrupted = fixture();
    corrupted.truncate(corrupted.len() - 4);
    std::fs::write(&file, corrupted).unwrap();
    assert!(validate_pbf(&file, &mut NoopObserver).is_err());
    std::fs::write(&file, b"invalid").unwrap();
    assert!(inspect_pbf(&file).is_err());
    std::fs::remove_file(file).unwrap();
    std::fs::remove_dir(directory).unwrap();
}
