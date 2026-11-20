use vrp_geo::RoadNetwork;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: cache_check <road-graph.json>")?;
    let graph = RoadNetwork::load(path)?;
    println!(
        "validated {} ways, fingerprint {}",
        graph.ways.len(),
        graph.fingerprint
    );
    Ok(())
}
