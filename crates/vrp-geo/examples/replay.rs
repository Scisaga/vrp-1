//! Local, read-only replay against a native scenario snapshot; prints only
//! aggregate measurements, never location coordinates or customer labels.
use std::{fs::File, io::BufReader, path::PathBuf, time::Instant};
use vrp_geo::{Bounds, Config, GeoPoint, NoopObserver, RoadNetwork};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err(
            "usage: replay <region.osm.pbf> <native-snapshot.json> <scenario-index>".into(),
        );
    }
    let value: serde_json::Value = serde_json::from_reader(BufReader::new(File::open(&args[1])?))?;
    let index = args[2].parse::<usize>()?;
    let scenario = if value.is_array() {
        &value[index]["data"]
    } else {
        &value
    };
    let points: Vec<GeoPoint> =
        serde_json::from_value(scenario["request"]["problem"]["locations"].clone())?;
    let bounds = Bounds::from_points(&points)?.expanded_km(10.0)?;
    let start = Instant::now();
    let network = RoadNetwork::from_pbf(
        &[PathBuf::from(&args[0])],
        bounds,
        Config::default(),
        &mut NoopObserver,
    )?;
    let build_s = start.elapsed().as_secs_f64();
    let graph = network.prepare(&points, &mut NoopObserver)?;
    let matrix = graph.matrix(&mut NoopObserver)?;
    let mut checks = 0;
    for from in 0..points.len() {
        let to = (from + 1) % points.len();
        let route = graph.route(from, to, &mut NoopObserver)?;
        match (matrix.entries[from * points.len() + to], route) {
            (Some(estimate), Some(route)) => {
                assert_eq!(
                    (estimate.duration_s, estimate.distance_m),
                    (route.duration_s, route.distance_m)
                );
                let drawn: f64 = route
                    .points
                    .windows(2)
                    .map(|p| vrp_geo::distance_m(p[0], p[1]))
                    .sum();
                assert!((drawn - f64::from(route.distance_m)).abs() < 1.1);
                checks += 1;
            }
            (None, None) => {}
            _ => return Err("matrix/geometry reachability mismatch".into()),
        }
    }
    println!(
        "{}",
        serde_json::json!({"locations":points.len(),"selected_ways":network.ways.len(),"known_entries":matrix.entries.iter().filter(|e|e.is_some()).count(),"unknown_entries":matrix.entries.iter().filter(|e|e.is_none()).count(),"geometry_checks":checks,"pbf_build_seconds":build_s,"total_seconds":start.elapsed().as_secs_f64()})
    );
    Ok(())
}
