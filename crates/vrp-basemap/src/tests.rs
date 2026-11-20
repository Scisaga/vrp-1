use super::*;
use crate::extract::tile_ids;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::thread;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "vrp-basemap-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn area() -> Vec<pb::Bounds> {
    vec![pb::Bounds {
        west: 10.0,
        east: 10.00001,
        south: 10.0,
        north: 10.00001,
    }]
}
fn archive(f: &Fixture, duplicate: bool) -> Vec<u8> {
    let p = f.path("world.pmtiles");
    let metadata = serde_json::json!({"vector_layers":[{"id":"earth"},{"id":"water"},{"id":"roads"},{"id":"places"}],"name":"synthetic"});
    let mut writer = pmtiles::PmTilesWriter::new(pmtiles::TileType::Mvt)
        .precompressed(pmtiles::Compression::None)
        .bounds(-180., -85., 180., 85.)
        .metadata(metadata.to_string())
        .create(File::create(&p).unwrap())
        .unwrap();
    for id in tile_ids(&area(), 15).unwrap() {
        let coord: pmtiles::TileCoord = id.into();
        let bytes = vec![if duplicate { 1 } else { coord.z() }; 8192];
        writer.add_tile(coord, &bytes).unwrap();
    }
    writer.finalize().unwrap();
    fs::read(p).unwrap()
}
struct Source {
    url: String,
    mode: Arc<AtomicUsize>,
    ranges: Arc<Mutex<Vec<(u64, u64)>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Source {
    fn new(data: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mode = Arc::new(AtomicUsize::new(0));
        let m = mode.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let r = ranges.clone();
        let handle = thread::spawn(move || {
            for connection in listener.incoming() {
                if s.load(Ordering::Acquire) {
                    break;
                }
                let mut stream = connection.unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).unwrap_or(0) == 0 {
                    continue;
                }
                let mut range = None;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        let (a, b) = v.trim().split_once('-').unwrap();
                        range = Some((a.parse::<u64>().unwrap(), b.parse::<u64>().unwrap()));
                    }
                    assert!(
                        !line.to_ascii_lowercase().starts_with("authorization:"),
                        "service token leaked to upstream"
                    );
                }
                let mode = m.load(Ordering::Acquire);
                let etag = if mode == 4 {
                    "\"changed\""
                } else {
                    "\"source-v1\""
                };
                if first.starts_with("HEAD") {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: {etag}\r\nConnection: close\r\n\r\n",
                        data.len()
                    );
                    continue;
                }
                let (start, end) = range.unwrap();
                r.lock().unwrap().push((start, end));
                let status = if mode == 1 { 200 } else { 206 };
                let range_start = if mode == 2 { start + 1 } else { start };
                let len = (end - start + 1) as usize;
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} OK\r\nContent-Length: {len}\r\nETag: {etag}\r\nContent-Range: bytes {range_start}-{end}/{}\r\nConnection: close\r\n\r\n",
                    data.len()
                );
                let len = if mode == 3 { len / 2 } else { len };
                let _ = stream.write_all(&data[start as usize..start as usize + len]);
            }
        });
        Self {
            url: format!("http://{addr}/world.pmtiles"),
            mode,
            ranges,
            stop,
            thread: Some(handle),
        }
    }
    fn input(&self, max: u32) -> pb::BasemapTaskInput {
        pb::BasemapTaskInput {
            request: Some(pb::PrepareBasemapRequest {
                native_max_zoom: max,
                automatic: max == 14,
                ..Default::default()
            }),
            bounds: area(),
            source: Some(resolve_source(&self.url, "").unwrap().0),
            transfer_limit: 2 * 1024 * 1024,
            output_limit: 2 * 1024 * 1024,
            ..Default::default()
        }
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(
            self.url
                .trim_start_matches("http://")
                .split('/')
                .next()
                .unwrap(),
        );
        self.thread.take().unwrap().join().unwrap();
    }
}
fn prepared(input: &pb::BasemapTaskInput, path: &Path) -> pb::BasemapPackage {
    match prepare(input, path, &AtomicBool::new(false), |_, _, _| {}).unwrap() {
        Prepared::Package(p) => *p,
        _ => panic!("expected package"),
    }
}

#[test]
fn default_14_and_explicit_15_reuse_the_frozen_lower_pyramid() {
    let f = Fixture::new();
    let source = Source::new(archive(&f, false));
    let mut input = source.input(14);
    let low = f.path("low.pmtiles");
    let p = prepared(&input, &low);
    assert_eq!(p.native_max_zoom, 14);
    let ids = tile_ids(&area(), 15).unwrap();
    let reader = pmtiles::PmTilesReader::new_with_path(&low).unwrap();
    for id in &ids {
        let coord: pmtiles::TileCoord = (*id).into();
        assert_eq!(reader.get_tile(coord).unwrap().is_some(), coord.z() <= 14);
    }
    source.ranges.lock().unwrap().clear();
    input.request.as_mut().unwrap().native_max_zoom = 15;
    input.request.as_mut().unwrap().automatic = false;
    input.reuse_path = low.to_string_lossy().into();
    let high = prepared(&input, &f.path("high.pmtiles"));
    assert_eq!(high.native_max_zoom, 15);
    assert!(low.exists());
    let bytes: u64 = source
        .ranges
        .lock()
        .unwrap()
        .iter()
        .map(|(a, b)| b - a + 1)
        .sum();
    assert!(
        bytes < 32768,
        "low native levels were downloaded again: {bytes}"
    );
    input.request.as_mut().unwrap().automatic = true;
    assert!(
        prepare(
            &input,
            &f.path("invalid"),
            &AtomicBool::new(false),
            |_, _, _| {}
        )
        .is_err()
    );
}
#[test]
fn range_failures_never_publish_an_output() {
    let f = Fixture::new();
    let source = Source::new(archive(&f, false));
    let input = source.input(14);
    for mode in 1..=4 {
        source.mode.store(mode, Ordering::Release);
        let output = f.path("bad.pmtiles");
        assert!(
            prepare(&input, &output, &AtomicBool::new(false), |_, _, _| {}).is_err(),
            "mode {mode}"
        );
        assert!(!output.exists());
    }
}
#[test]
fn estimate_reads_only_index_and_budget_or_cancel_leaves_no_file() {
    let f = Fixture::new();
    let bytes = archive(&f, false);
    let data_start = u64::from_le_bytes(bytes[56..64].try_into().unwrap());
    let source = Source::new(bytes);
    let mut input = source.input(14);
    input.estimate_only = true;
    let output = f.path("estimate.pmtiles");
    let Prepared::Estimate(e) =
        prepare(&input, &output, &AtomicBool::new(false), |_, _, _| {}).unwrap()
    else {
        panic!()
    };
    assert!(e.transfer_bytes > 8192);
    assert!(!output.exists());
    assert!(
        source
            .ranges
            .lock()
            .unwrap()
            .iter()
            .all(|(_, end)| *end < data_start)
    );
    input.estimate_only = false;
    input.transfer_limit = 128;
    assert!(prepare(&input, &output, &AtomicBool::new(false), |_, _, _| {}).is_err());
    assert!(!output.exists());
    input.transfer_limit = 2 * 1024 * 1024;
    assert!(prepare(&input, &output, &AtomicBool::new(true), |_, _, _| {}).is_err());
    assert!(!output.exists());
    input.output_limit = 1;
    assert!(prepare(&input, &output, &AtomicBool::new(false), |_, _, _| {}).is_err());
}
#[test]
fn repeated_payload_and_import_are_content_stable() {
    let f = Fixture::new();
    let source = Source::new(archive(&f, true));
    let input = source.input(14);
    let output = f.path("package.pmtiles");
    let p = prepared(&input, &output);
    assert!(p.size_bytes < 32768);
    let imported = prepared(
        &pb::BasemapTaskInput {
            import_path: output.to_string_lossy().into(),
            output_limit: 2 * 1024 * 1024,
            ..Default::default()
        },
        &f.path("imported.pmtiles"),
    );
    assert_eq!(p, imported);
}
#[test]
fn antimeridian_is_split_and_no_global_high_zoom_pyramid_is_enumerated() {
    let split = vec![
        pb::Bounds {
            west: 179.99,
            east: 180.,
            south: 0.,
            north: 0.01,
        },
        pb::Bounds {
            west: -180.,
            east: -179.99,
            south: 0.,
            north: 0.01,
        },
    ];
    validate_bounds(&split).unwrap();
    let ids = tile_ids(&split, 14).unwrap();
    assert!(ids.len() < 100);
    let high: Vec<pmtiles::TileCoord> = ids
        .into_iter()
        .map(Into::into)
        .filter(|c: &pmtiles::TileCoord| c.z() == 14)
        .collect();
    assert!(high.iter().any(|c| c.x() == 0));
    assert!(high.iter().any(|c| c.x() == 16383));
    let mut bad = split;
    bad[0].north = 90.;
    assert!(validate_bounds(&bad).is_err());
    assert!(
        tile_ids(
            &[pb::Bounds {
                west: -180.,
                east: 180.,
                south: -85.,
                north: 85.
            }],
            14
        )
        .is_err()
    );
}

#[test]
fn imports_reject_tiles_above_the_declared_native_max_and_truncated_sections() {
    let f = Fixture::new();
    let mut bytes = archive(&f, false);
    bytes[101] = 14;
    let path = f.path("invalid.pmtiles");
    fs::write(&path, &bytes).unwrap();
    assert!(
        inspect(&path)
            .unwrap_err()
            .to_string()
            .contains("native zoom")
    );
    bytes[101] = 15;
    bytes.truncate(bytes.len() - 1);
    fs::write(&path, &bytes).unwrap();
    assert!(inspect(&path).is_err());
}
