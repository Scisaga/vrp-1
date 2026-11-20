use crate::geometry::{EARTH, project_segment};
use crate::{
    Bounds, Error, GeoPoint, NoopObserver, Observer, Result, TrackedIo, checkpoint, distance_m,
};
use rstar::{AABB, PointDistance, RTree, RTreeObject};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

const FORMAT: u32 = 1;
const MAX_VALUE: u64 = u32::MAX as u64 - 2;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub connector_speed_kph: f64,
    pub speeds_kph: BTreeMap<String, f64>,
    pub include_private: bool,
    pub include_tracks: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            connector_speed_kph: 15.0,
            include_private: false,
            include_tracks: false,
            speeds_kph: [
                ("motorway", 80.0),
                ("trunk", 60.0),
                ("primary", 45.0),
                ("secondary", 35.0),
                ("tertiary", 30.0),
                ("unclassified", 25.0),
                ("residential", 20.0),
                ("living_street", 10.0),
                ("service", 15.0),
                ("road", 20.0),
                ("motorway_link", 35.0),
                ("trunk_link", 30.0),
                ("primary_link", 25.0),
                ("secondary_link", 20.0),
                ("tertiary_link", 20.0),
                ("track", 10.0),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect(),
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        if !self.connector_speed_kph.is_finite()
            || self.connector_speed_kph <= 0.0
            || self
                .speeds_kph
                .values()
                .any(|v| !v.is_finite() || *v <= 0.0)
        {
            return Err(Error("speeds must be finite and positive".into()));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WayNode {
    pub id: i64,
    pub point: GeoPoint,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Way {
    pub id: i64,
    pub nodes: Vec<WayNode>,
    pub tags: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Source {
    pub sha256: String,
    pub bytes: u64,
    pub data_timestamp_unix: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoadNetwork {
    pub format_version: u32,
    pub fingerprint: String,
    pub config: Config,
    pub bounds: Option<Bounds>,
    pub sources: Vec<Source>,
    pub ways: Vec<Way>,
}

impl RoadNetwork {
    pub fn from_pbf(
        paths: &[PathBuf],
        bounds: Bounds,
        config: Config,
        observer: &mut dyn Observer,
    ) -> Result<Self> {
        bounds.validate()?;
        config.validate()?;
        let (ways, sources) = crate::pbf::read(paths, bounds, &config, observer)?;
        let mut network = Self::from_ways_observed(ways, config, observer)?;
        network.bounds = Some(bounds);
        network.sources = sources;
        network.fingerprint = network.calculate_fingerprint_observed(observer)?;
        Ok(network)
    }
    pub fn from_ways(ways: Vec<Way>, config: Config) -> Result<Self> {
        Self::from_ways_observed(ways, config, &mut NoopObserver)
    }
    pub fn from_ways_observed(
        ways: Vec<Way>,
        config: Config,
        observer: &mut dyn Observer,
    ) -> Result<Self> {
        config.validate()?;
        let mut unique = BTreeMap::new();
        let mut nodes: HashMap<i64, GeoPoint> = HashMap::new();
        let total = ways.len();
        for (index, mut way) in ways.into_iter().enumerate() {
            if index.is_multiple_of(1024) {
                checkpoint(observer, "graph_validate", index as u64, Some(total as u64))?;
            }
            for node in &way.nodes {
                node.point.validate()?;
                if let Some(previous) = nodes.insert(node.id, node.point)
                    && distance_m(previous, node.point) > 0.02
                {
                    return Err(Error(format!(
                        "conflicting coordinates for OSM node {}",
                        node.id
                    )));
                }
            }
            way.nodes.dedup_by_key(|n| n.id);
            if way.nodes.len() < 2 || !is_drivable(&way.tags, &config) {
                continue;
            }
            if let Some(previous) = unique.insert(way.id, way.clone())
                && previous != way
            {
                return Err(Error(format!("conflicting OSM way {}", way.id)));
            }
        }
        let mut network = Self {
            format_version: FORMAT,
            fingerprint: String::new(),
            config,
            bounds: None,
            sources: vec![],
            ways: unique.into_values().collect(),
        };
        network.fingerprint = network.calculate_fingerprint_observed(observer)?;
        Ok(network)
    }
    fn calculate_fingerprint_observed(&self, observer: &mut dyn Observer) -> Result<String> {
        struct HashWriter(Sha256);
        impl Write for HashWriter {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.update(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = TrackedIo::new(HashWriter(Sha256::new()), observer, "graph_hash", None);
        serde_json::to_writer(
            &mut writer,
            &(
                self.format_version,
                &self.config,
                &self.bounds,
                &self.sources,
                &self.ways,
            ),
        )?;
        Ok(format!("{:x}", writer.inner.0.finalize()))
    }
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        self.save_observed(path, &mut NoopObserver)
    }
    pub fn save_observed(&self, path: impl AsRef<Path>, observer: &mut dyn Observer) -> Result<()> {
        checkpoint(observer, "graph_save", 0, None)?;
        if self.calculate_fingerprint_observed(observer)? != self.fingerprint {
            return Err(Error("road graph was changed after fingerprinting".into()));
        }
        let path = path.as_ref();
        if path.exists() {
            if Self::load_observed(path, observer)?.fingerprint == self.fingerprint {
                return Ok(());
            }
            return Err(Error(
                "refusing to overwrite a different immutable road graph".into(),
            ));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| Error(e.to_string()))?
            .as_nanos();
        let temporary = path.with_extension(format!("{}.{}.tmp", std::process::id(), unique));
        let result = (|| {
            let writer = BufWriter::new(
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)?,
            );
            let mut writer = TrackedIo::new(writer, observer, "graph_save", None);
            serde_json::to_writer(&mut writer, self)?;
            writer.flush()?;
            writer.inner.get_ref().sync_all()?;
            std::fs::rename(&temporary, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        Self::load_observed(path, &mut NoopObserver)
    }
    pub fn load_observed(path: impl AsRef<Path>, observer: &mut dyn Observer) -> Result<Self> {
        checkpoint(observer, "graph_load", 0, None)?;
        let file = File::open(path)?;
        let total = file.metadata()?.len();
        let reader = TrackedIo::new(BufReader::new(file), observer, "graph_load", Some(total));
        let result: Self = serde_json::from_reader(reader)?;
        if result.format_version != FORMAT {
            return Err(Error("unsupported road graph cache format".into()));
        }
        result.config.validate()?;
        if let Some(bounds) = result.bounds {
            bounds.validate()?;
        }
        for (index, way) in result.ways.iter().enumerate() {
            if index.is_multiple_of(1024) {
                checkpoint(
                    observer,
                    "graph_validate",
                    index as u64,
                    Some(result.ways.len() as u64),
                )?;
            }
            for node in &way.nodes {
                node.point.validate()?;
            }
        }
        if result.calculate_fingerprint_observed(observer)? != result.fingerprint {
            return Err(Error("road graph cache checksum mismatch".into()));
        }
        Ok(result)
    }
    pub fn prepare(&self, points: &[GeoPoint], observer: &mut dyn Observer) -> Result<QueryGraph> {
        QueryGraph::build(self, points, observer)
    }
    pub fn matrix(&self, points: &[GeoPoint], observer: &mut dyn Observer) -> Result<Matrix> {
        self.prepare(points, observer)?.matrix(observer)
    }
    /// This graph was spatially extracted. A missing path may leave its indexed
    /// bounds, so callers may retry a wider area before geometric fallback.
    /// This is a coverage hint, never a claim that a missing path is impossible.
    pub fn boundary_limited(&self) -> bool {
        self.bounds.is_some()
    }
}

pub(crate) fn is_drivable(tags: &BTreeMap<String, String>, config: &Config) -> bool {
    let highway = tags
        .get("highway")
        .map(String::as_str)
        .unwrap_or("residential");
    if !config.speeds_kph.contains_key(highway) || (highway == "track" && !config.include_tracks) {
        return false;
    }
    let access = ["motorcar", "motor_vehicle", "vehicle", "access"]
        .iter()
        .find_map(|k| tags.get(*k))
        .map(String::as_str)
        .unwrap_or("");
    matches!(
        access,
        "" | "yes" | "designated" | "permissive" | "destination" | "customers" | "delivery"
    ) || (access == "private" && config.include_private)
}
fn posted_speed(value: &str) -> Option<f64> {
    value
        .split(';')
        .filter_map(|v| {
            let v = v.trim().to_ascii_lowercase();
            let mph = v.ends_with("mph");
            let n = v
                .trim_end_matches("mph")
                .trim_end_matches("km/h")
                .trim()
                .parse::<f64>()
                .ok()?;
            (n.is_finite() && n > 0.0).then_some(if mph { n * 1.609344 } else { n })
        })
        .min_by(f64::total_cmp)
}
fn way_speeds(way: &Way, config: &Config) -> (Option<f64>, Option<f64>) {
    let highway = way
        .tags
        .get("highway")
        .map(String::as_str)
        .unwrap_or("residential");
    let speed = config.speeds_kph[highway];
    let general = way
        .tags
        .get("maxspeed")
        .and_then(|v| posted_speed(v))
        .unwrap_or(speed);
    let forward = way
        .tags
        .get("maxspeed:forward")
        .and_then(|v| posted_speed(v))
        .unwrap_or(general)
        .min(speed);
    let backward = way
        .tags
        .get("maxspeed:backward")
        .and_then(|v| posted_speed(v))
        .unwrap_or(general)
        .min(speed);
    let oneway = ["oneway:motorcar", "oneway:motor_vehicle", "oneway"]
        .iter()
        .find_map(|k| way.tags.get(*k))
        .map(String::as_str);
    match oneway {
        Some("-1" | "reverse") => (None, Some(backward)),
        Some("yes" | "1" | "true") => (Some(forward), None),
        None if highway == "motorway"
            || way.tags.get("junction").is_some_and(|v| v == "roundabout") =>
        {
            (Some(forward), None)
        }
        _ => (Some(forward), Some(backward)),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snap {
    pub original: GeoPoint,
    pub point: GeoPoint,
    pub distance_m: f64,
    pub way_id: i64,
    pub segment_index: usize,
    pub fraction: f64,
    pub graph_node: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TravelEstimate {
    pub duration_s: u32,
    pub distance_m: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Matrix {
    pub size: usize,
    pub entries: Vec<Option<TravelEstimate>>,
    pub snaps: Vec<Option<Snap>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Route {
    pub points: Vec<GeoPoint>,
    pub road_start: usize,
    pub road_end: usize,
    pub duration_s: u32,
    pub distance_m: u32,
    pub connector_distance_m: f64,
}

#[derive(Clone)]
struct Segment {
    way: usize,
    index: usize,
    a: GeoPoint,
    b: GeoPoint,
}
impl RTreeObject for Segment {
    type Envelope = AABB<[f64; 3]>;
    fn envelope(&self) -> Self::Envelope {
        let a = self.a.xyz();
        let b = self.b.xyz();
        let sagitta = EARTH * (1.0 - (distance_m(self.a, self.b) / EARTH / 2.0).cos()) + 1e-6;
        AABB::from_corners(
            std::array::from_fn(|i| a[i].min(b[i]) - sagitta),
            std::array::from_fn(|i| a[i].max(b[i]) + sagitta),
        )
    }
}
impl PointDistance for Segment {
    fn distance_2(&self, p: &[f64; 3]) -> f64 {
        let (point, _, _) = project_segment(GeoPoint::from_xyz(*p), self.a, self.b);
        let xyz = point.xyz();
        (0..3).map(|i| (xyz[i] - p[i]).powi(2)).sum()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Key {
    Osm(i64),
    Virtual(usize, usize, u64),
}
#[derive(Clone)]
struct Break {
    measure: f64,
    key: Key,
    point: GeoPoint,
    distance: f64,
}
#[derive(Clone)]
struct Edge {
    points: Vec<GeoPoint>,
}
#[derive(Clone, Copy)]
struct Arc {
    to: usize,
    edge: usize,
    reverse: bool,
    time: u64,
    length: u64,
}

pub struct QueryGraph {
    points: Vec<GeoPoint>,
    pub snaps: Vec<Option<Snap>>,
    connector_speed: f64,
    offsets: Vec<usize>,
    arcs: Vec<Arc>,
    edges: Vec<Edge>,
    node_count: usize,
}
impl QueryGraph {
    fn build(
        network: &RoadNetwork,
        points: &[GeoPoint],
        observer: &mut dyn Observer,
    ) -> Result<Self> {
        for p in points {
            p.validate()?;
        }
        checkpoint(
            observer,
            "spatial_index",
            0,
            Some(network.ways.len() as u64),
        )?;
        let mut counts: HashMap<i64, u32> = HashMap::new();
        let mut segments = Vec::new();
        for (wi, way) in network.ways.iter().enumerate() {
            for node in &way.nodes {
                *counts.entry(node.id).or_default() += 1;
            }
            for (i, nodes) in way.nodes.windows(2).enumerate() {
                segments.push(Segment {
                    way: wi,
                    index: i,
                    a: nodes[0].point,
                    b: nodes[1].point,
                });
            }
            if wi % 1024 == 0 {
                checkpoint(
                    observer,
                    "spatial_index",
                    wi as u64,
                    Some(network.ways.len() as u64),
                )?;
            }
        }
        let tree = RTree::bulk_load(segments);
        let mut snaps = Vec::with_capacity(points.len());
        let mut additions: HashMap<usize, Vec<Break>> = HashMap::new();
        let mut snap_keys = Vec::with_capacity(points.len());
        for (pi, point) in points.iter().copied().enumerate() {
            checkpoint(observer, "snapping", pi as u64, Some(points.len() as u64))?;
            let mut nearest = tree.nearest_neighbor_iter_with_distance_2(point.xyz());
            let Some((first, distance2)) = nearest.next() else {
                snaps.push(None);
                snap_keys.push(None);
                continue;
            };
            let mut best = first;
            for (candidate, d2) in nearest {
                if d2 > distance2 + 1e-7 {
                    break;
                }
                if (network.ways[candidate.way].id, candidate.index)
                    < (network.ways[best.way].id, best.index)
                {
                    best = candidate;
                }
            }
            let (projected, mut fraction, connection) = project_segment(point, best.a, best.b);
            if fraction < 1e-10 {
                fraction = 0.0;
            } else if fraction > 1.0 - 1e-10 {
                fraction = 1.0;
            }
            let way = &network.ways[best.way];
            let key = if fraction == 0.0 {
                Key::Osm(way.nodes[best.index].id)
            } else if fraction == 1.0 {
                Key::Osm(way.nodes[best.index + 1].id)
            } else {
                Key::Virtual(best.way, best.index, (fraction * 1e12).round() as u64)
            };
            additions.entry(best.way).or_default().push(Break {
                measure: best.index as f64 + fraction,
                key,
                point: projected,
                distance: 0.0,
            });
            snap_keys.push(Some(key));
            snaps.push(Some(Snap {
                original: point,
                point: projected,
                distance_m: connection,
                way_id: way.id,
                segment_index: best.index,
                fraction,
                graph_node: 0,
            }));
        }
        let snapped_keys: HashSet<_> = snap_keys.iter().flatten().copied().collect();
        let mut node_keys = BTreeMap::new();
        let mut edges = Vec::new();
        let mut raw_arcs = Vec::new();
        for (wi, way) in network.ways.iter().enumerate() {
            if wi % 1024 == 0 {
                checkpoint(
                    observer,
                    "query_graph",
                    wi as u64,
                    Some(network.ways.len() as u64),
                )?;
            }
            let mut prefix = vec![0.0];
            for pair in way.nodes.windows(2) {
                prefix.push(prefix.last().unwrap() + distance_m(pair[0].point, pair[1].point));
            }
            let mut breaks = additions.remove(&wi).unwrap_or_default();
            for br in &mut breaks {
                let i = (br.measure.floor() as usize).min(way.nodes.len() - 2);
                br.distance = prefix[i] + (br.measure - i as f64) * (prefix[i + 1] - prefix[i]);
            }
            for (i, node) in way.nodes.iter().enumerate() {
                if i == 0
                    || i + 1 == way.nodes.len()
                    || counts[&node.id] > 1
                    || snapped_keys.contains(&Key::Osm(node.id))
                {
                    breaks.push(Break {
                        measure: i as f64,
                        key: Key::Osm(node.id),
                        point: node.point,
                        distance: prefix[i],
                    });
                }
            }
            breaks.sort_by(|a, b| a.measure.total_cmp(&b.measure).then(a.key.cmp(&b.key)));
            breaks.dedup_by(|a, b| a.key == b.key && (a.measure - b.measure).abs() < 1e-9);
            let (forward, backward) = way_speeds(way, &network.config);
            for pair in breaks.windows(2) {
                if pair[1].measure - pair[0].measure < 1e-11 {
                    continue;
                }
                node_keys.entry(pair[0].key).or_insert(0);
                node_keys.entry(pair[1].key).or_insert(0);
                let mut shape = vec![pair[0].point];
                let from = pair[0].measure.floor() as usize + 1;
                let until = pair[1].measure.ceil() as usize;
                for node in &way.nodes[from..until] {
                    push_point(&mut shape, node.point);
                }
                push_point(&mut shape, pair[1].point);
                let edge = edges.len();
                edges.push(Edge { points: shape });
                let length = fixed(pair[1].distance, 1000.0)?
                    .saturating_sub(fixed(pair[0].distance, 1000.0)?);
                for (speed, reverse) in [(forward, false), (backward, true)] {
                    if let Some(speed) = speed {
                        let time = fixed(pair[1].distance * 3.6 / speed, 1e6)?
                            .saturating_sub(fixed(pair[0].distance * 3.6 / speed, 1e6)?);
                        let (a, b) = if reverse {
                            (pair[1].key, pair[0].key)
                        } else {
                            (pair[0].key, pair[1].key)
                        };
                        raw_arcs.push((
                            a,
                            b,
                            Arc {
                                to: 0,
                                edge,
                                reverse,
                                time,
                                length,
                            },
                        ));
                    }
                }
            }
        }
        for (i, v) in node_keys.values_mut().enumerate() {
            *v = i;
        }
        for (snap, key) in snaps.iter_mut().zip(snap_keys) {
            if let (Some(snap), Some(key)) = (snap, key) {
                snap.graph_node = node_keys[&key];
            }
        }
        raw_arcs.sort_by_key(|(a, b, arc)| (*a, *b, arc.time, arc.length, arc.edge, arc.reverse));
        let mut offsets = vec![0; node_keys.len() + 1];
        let mut arcs = Vec::with_capacity(raw_arcs.len());
        for (a, b, mut arc) in raw_arcs {
            offsets[node_keys[&a] + 1] += 1;
            arc.to = node_keys[&b];
            arcs.push(arc);
        }
        for i in 1..offsets.len() {
            offsets[i] += offsets[i - 1];
        }
        Ok(Self {
            points: points.to_vec(),
            snaps,
            connector_speed: network.config.connector_speed_kph,
            offsets,
            arcs,
            edges,
            node_count: node_keys.len(),
        })
    }
    pub fn matrix(&self, observer: &mut dyn Observer) -> Result<Matrix> {
        checkpoint(observer, "matrix", 0, Some(self.points.len() as u64))?;
        let n = self.points.len();
        let entries_count = n
            .checked_mul(n)
            .ok_or_else(|| Error("matrix size overflow".into()))?;
        let mut entries = vec![None; entries_count];
        for from in 0..n {
            checkpoint(observer, "matrix", from as u64, Some(n as u64))?;
            let search = if let Some(snap) = &self.snaps[from] {
                Some(self.search(snap.graph_node, None, observer)?)
            } else {
                None
            };
            for to in 0..n {
                if from == to || self.points[from] == self.points[to] {
                    entries[from * n + to] = Some(TravelEstimate {
                        duration_s: 0,
                        distance_m: 0,
                    });
                    continue;
                }
                if let (Some(search), Some(a), Some(b)) =
                    (&search, &self.snaps[from], &self.snaps[to])
                    && search.costs[b.graph_node].0 != u64::MAX
                {
                    entries[from * n + to] = Some(
                        self.estimate(search.costs[b.graph_node], a.distance_m + b.distance_m)?,
                    );
                }
            }
        }
        checkpoint(observer, "matrix", n as u64, Some(n as u64))?;
        Ok(Matrix {
            size: n,
            entries,
            snaps: self.snaps.clone(),
        })
    }
    fn estimate(&self, cost: (u64, u64), connection: f64) -> Result<TravelEstimate> {
        let time = cost
            .0
            .checked_add(fixed(connection * 3.6 / self.connector_speed, 1e6)?)
            .ok_or_else(|| Error("travel time overflow".into()))?;
        let distance = cost
            .1
            .checked_add(fixed(connection, 1000.0)?)
            .ok_or_else(|| Error("travel distance overflow".into()))?;
        let seconds = time
            .checked_add(500_000)
            .ok_or_else(|| Error("travel time rounding overflow".into()))?
            / 1_000_000;
        let metres = distance
            .checked_add(500)
            .ok_or_else(|| Error("travel distance rounding overflow".into()))?
            / 1000;
        if seconds > MAX_VALUE || metres > MAX_VALUE {
            return Err(Error(
                "travel estimate exceeds matrix u32 sentinel range".into(),
            ));
        }
        Ok(TravelEstimate {
            duration_s: seconds as u32,
            distance_m: metres as u32,
        })
    }
    pub fn route(
        &self,
        from: usize,
        to: usize,
        observer: &mut dyn Observer,
    ) -> Result<Option<Route>> {
        checkpoint(observer, "route", 0, Some(1))?;
        if from >= self.points.len() || to >= self.points.len() {
            return Err(Error("location index out of range".into()));
        }
        if self.points[from] == self.points[to] {
            return Ok(Some(Route {
                points: vec![self.points[from], self.points[to]],
                road_start: 0,
                road_end: 0,
                duration_s: 0,
                distance_m: 0,
                connector_distance_m: 0.0,
            }));
        }
        let (Some(a), Some(b)) = (&self.snaps[from], &self.snaps[to]) else {
            return Ok(None);
        };
        let search = self.search(a.graph_node, Some(b.graph_node), observer)?;
        let cost = search.costs[b.graph_node];
        if cost.0 == u64::MAX {
            return Ok(None);
        }
        let mut path = Vec::new();
        let mut current = b.graph_node;
        while current != a.graph_node {
            let Some((previous, arc)) = search.previous[current] else {
                return Err(Error("broken shortest-path predecessor".into()));
            };
            path.push(arc);
            current = previous;
            if path.len() > self.node_count {
                return Err(Error("shortest-path predecessor cycle".into()));
            }
        }
        path.reverse();
        let mut points = vec![self.points[from]];
        push_point(&mut points, a.point);
        let road_start = points.len() - 1;
        for arc in path {
            let arc = self.arcs[arc];
            let shape = &self.edges[arc.edge].points;
            if arc.reverse {
                for p in shape.iter().rev() {
                    push_point(&mut points, *p);
                }
            } else {
                for p in shape {
                    push_point(&mut points, *p);
                }
            }
        }
        push_point(&mut points, b.point);
        let road_end = points.len() - 1;
        push_point(&mut points, self.points[to]);
        if points.len() == 1 {
            points.push(self.points[to]);
        }
        let connection = a.distance_m + b.distance_m;
        let estimate = self.estimate(cost, connection)?;
        Ok(Some(Route {
            points,
            road_start,
            road_end,
            duration_s: estimate.duration_s,
            distance_m: estimate.distance_m,
            connector_distance_m: connection,
        }))
    }
    fn search(
        &self,
        start: usize,
        target: Option<usize>,
        observer: &mut dyn Observer,
    ) -> Result<Search> {
        let mut costs = vec![(u64::MAX, u64::MAX); self.node_count];
        let mut previous = vec![None; self.node_count];
        let mut queue = BinaryHeap::new();
        costs[start] = (0, 0);
        queue.push(Reverse((0_u64, 0_u64, start)));
        let mut popped = 0;
        while let Some(Reverse((time, length, node))) = queue.pop() {
            if (time, length) != costs[node] {
                continue;
            }
            if target == Some(node) {
                break;
            }
            popped += 1;
            if popped % 4096 == 0 && observer.is_cancelled() {
                return Err(Error("cancelled".into()));
            }
            for index in self.offsets[node]..self.offsets[node + 1] {
                let arc = self.arcs[index];
                let next = (
                    time.checked_add(arc.time)
                        .ok_or_else(|| Error("path time overflow".into()))?,
                    length
                        .checked_add(arc.length)
                        .ok_or_else(|| Error("path length overflow".into()))?,
                );
                if next < costs[arc.to] {
                    costs[arc.to] = next;
                    previous[arc.to] = Some((node, index));
                    queue.push(Reverse((next.0, next.1, arc.to)));
                }
            }
        }
        Ok(Search { costs, previous })
    }
}
struct Search {
    costs: Vec<(u64, u64)>,
    previous: Vec<Option<(usize, usize)>>,
}
fn fixed(value: f64, scale: f64) -> Result<u64> {
    let v = value * scale;
    if !v.is_finite() || v < 0.0 || v > u64::MAX as f64 - 1.0 {
        return Err(Error("nonfinite or overflowing travel value".into()));
    }
    Ok(v.round() as u64)
}
fn push_point(points: &mut Vec<GeoPoint>, point: GeoPoint) {
    if points.last().is_none_or(|p| distance_m(*p, point) > 1e-5) {
        points.push(point);
    }
}
