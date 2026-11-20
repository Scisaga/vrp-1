//! Anonymous synthetic acceptance workload; no external data or Python runtime.
use std::{collections::BTreeMap, time::Instant};
use vrp_geo::{Config, GeoPoint, NoopObserver, RoadNetwork, Way, WayNode};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    const SIDE: usize = 160;
    const LOCATIONS: usize = 600;
    let point = |x: usize, y: usize| GeoPoint {
        lng: 20.0 + x as f64 * 0.001,
        lat: 50.0 + y as f64 * 0.001,
    };
    let node = |x: usize, y: usize| WayNode {
        id: (y * SIDE + x + 1) as i64,
        point: point(x, y),
    };
    let start = Instant::now();
    let mut ways = Vec::new();
    for row in 0..SIDE {
        ways.push(Way {
            id: row as i64,
            nodes: (0..SIDE).map(|column| node(column, row)).collect(),
            tags: BTreeMap::new(),
        });
    }
    for column in 0..SIDE {
        ways.push(Way {
            id: (SIDE + column) as i64,
            nodes: (0..SIDE).map(|row| node(column, row)).collect(),
            tags: BTreeMap::new(),
        });
    }
    let network = RoadNetwork::from_ways(ways, Config::default())?;
    let locations: Vec<_> = (0..LOCATIONS)
        .map(|i| {
            let mut p = point(
                (i * 37) % (SIDE - 1),
                (i * 71 + i / (SIDE - 1)) % (SIDE - 1),
            );
            p.lng += 0.00025;
            p.lat += 0.00015;
            p
        })
        .collect();
    let graph = network.prepare(&locations, &mut NoopObserver)?;
    let prepared = start.elapsed().as_secs_f64();
    let matrix = graph.matrix(&mut NoopObserver)?;
    let matrix_seconds = start.elapsed().as_secs_f64() - prepared;
    assert!(matrix.entries.iter().all(Option::is_some));
    for i in 0..32 {
        let from = i * 17 % LOCATIONS;
        let to = (i * 41 + 1) % LOCATIONS;
        let route = graph.route(from, to, &mut NoopObserver)?.unwrap();
        let entry = matrix.entries[from * LOCATIONS + to].unwrap();
        assert_eq!(
            (route.duration_s, route.distance_m),
            (entry.duration_s, entry.distance_m)
        );
    }
    println!(
        "{}",
        serde_json::json!({"fixture":"synthetic-grid-v1","road_nodes":SIDE*SIDE,"ways":SIDE*2,"locations":LOCATIONS,"matrix_entries":LOCATIONS*LOCATIONS,"geometry_checks":32,"prepare_seconds":prepared,"matrix_seconds":matrix_seconds,"total_seconds":start.elapsed().as_secs_f64()})
    );
    Ok(())
}
