//! Two streaming PBF passes: retain road references, then only referenced
//! coordinates. Buildings/relations and unrelated node objects are never kept.
use crate::network::{Config, Source, Way, WayNode, is_drivable};
use crate::{Bounds, Error, GeoPoint, Observer, Result, checkpoint};
use osmpbf::{BlobDecode, BlobReader, Element};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PbfInfo {
    pub bounds: Option<Bounds>,
    pub timestamp_unix: Option<i64>,
    pub writing_program: Option<String>,
}
pub fn inspect_pbf(path: impl AsRef<Path>) -> Result<PbfInfo> {
    let mut reader = BlobReader::from_path(path).map_err(|e| Error(e.to_string()))?;
    let blob = reader
        .next()
        .ok_or_else(|| Error("empty PBF".into()))?
        .map_err(|e| Error(e.to_string()))?;
    match blob.decode().map_err(|e| Error(e.to_string()))? {
        BlobDecode::OsmHeader(header) => {
            for feature in header.required_features() {
                if !matches!(
                    feature.as_str(),
                    "OsmSchema-V0.6" | "DenseNodes" | "LocationsOnWays" | "Sort.Type_then_ID"
                ) {
                    return Err(Error(format!(
                        "unsupported PBF required feature: {feature}"
                    )));
                }
            }
            let bounds = header.bbox().map(|b| Bounds {
                south: b.bottom,
                west: b.left,
                north: b.top,
                east: b.right,
            });
            if let Some(bounds) = bounds {
                bounds.validate()?;
            }
            Ok(PbfInfo {
                bounds,
                timestamp_unix: header.osmosis_replication_timestamp(),
                writing_program: header.writing_program().map(str::to_owned),
            })
        }
        _ => Err(Error("PBF does not start with an OSM header".into())),
    }
}

/// Validate every compressed/protobuf block without retaining OSM objects.
/// The strict final offset check also rejects a truncated 1--3 byte trailing
/// length prefix, which BlobReader itself treats as an ordinary EOF.
pub fn validate_pbf(path: impl AsRef<Path>, observer: &mut dyn Observer) -> Result<()> {
    let path = path.as_ref();
    inspect_pbf(path)?;
    let total = std::fs::metadata(path)?.len();
    let mut reader = BlobReader::seekable_from_path(path).map_err(|e| Error(e.to_string()))?;
    let mut end = 0_u64;
    let mut count = 0;
    while let Some(blob) = reader.next() {
        checkpoint(observer, "pbf_validate", end, Some(total))?;
        let blob = blob.map_err(|e| Error(e.to_string()))?;
        match blob.decode().map_err(|e| Error(e.to_string()))? {
            BlobDecode::OsmHeader(_) if count > 0 => {
                return Err(Error("unexpected repeated PBF header".into()));
            }
            BlobDecode::OsmHeader(_) | BlobDecode::OsmData(_) | BlobDecode::Unknown(_) => {}
        }
        end = reader
            .seek_raw(SeekFrom::Current(0))
            .map_err(|e| Error(e.to_string()))?;
        count += 1;
    }
    if end != total {
        return Err(Error("truncated PBF trailing block header".into()));
    }
    checkpoint(observer, "pbf_validate", total, Some(total))
}

#[derive(PartialEq)]
struct RawWay {
    id: i64,
    refs: Vec<i64>,
    tags: BTreeMap<String, String>,
    date: Option<i64>,
}

pub(crate) fn read(
    paths: &[PathBuf],
    bounds: Bounds,
    config: &Config,
    observer: &mut dyn Observer,
) -> Result<(Vec<Way>, Vec<Source>)> {
    if paths.is_empty() {
        return Err(Error("at least one PBF file is required".into()));
    }
    let mut inputs = Vec::new();
    for path in paths {
        let info = inspect_pbf(path)?;
        inputs.push((info.timestamp_unix, path, info));
    }
    inputs.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(b.1)));
    let mut raw = BTreeMap::<i64, RawWay>::new();
    let mut sources = Vec::new();
    const KEYS: [&str; 13] = [
        "highway",
        "access",
        "vehicle",
        "motor_vehicle",
        "motorcar",
        "oneway",
        "oneway:motor_vehicle",
        "oneway:motorcar",
        "junction",
        "maxspeed",
        "maxspeed:forward",
        "maxspeed:backward",
        "surface",
    ];
    for (file_number, (date, path, _)) in inputs.iter().enumerate() {
        let mut hasher = Sha256::new();
        let mut file = File::open(path)?;
        let mut buffer = vec![0_u8; 1024 * 1024];
        let mut read = 0;
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
            read += n as u64;
            checkpoint(observer, "fingerprint", read, Some(file.metadata()?.len()))?;
        }
        sources.push(Source {
            sha256: format!("{:x}", hasher.finalize()),
            bytes: read,
            data_timestamp_unix: *date,
        });
        let reader = BlobReader::from_path(path).map_err(|e| Error(e.to_string()))?;
        for (block_number, blob) in reader.enumerate() {
            checkpoint(observer, "pbf_roads", block_number as u64, None)?;
            if let BlobDecode::OsmData(block) = blob
                .map_err(|e| Error(e.to_string()))?
                .decode()
                .map_err(|e| Error(e.to_string()))?
            {
                for element in block.elements() {
                    if let Element::Way(way) = element {
                        let tags: BTreeMap<_, _> = way
                            .tags()
                            .filter(|(k, _)| KEYS.contains(k))
                            .map(|(k, v)| (k.into(), v.into()))
                            .collect();
                        // Actual OSM ways without highway are not roads; the default
                        // residential shortcut applies only to synthetic from_ways.
                        if !tags.contains_key("highway") || !is_drivable(&tags, config) {
                            raw.remove(&way.id());
                            continue;
                        }
                        let candidate = RawWay {
                            id: way.id(),
                            refs: way.refs().collect(),
                            tags,
                            date: *date,
                        };
                        if let Some(old) = raw.get(&candidate.id)
                            && old.date == candidate.date
                            && *old != candidate
                        {
                            return Err(Error(format!(
                                "conflicting same-snapshot way {}",
                                candidate.id
                            )));
                        }
                        raw.insert(candidate.id, candidate);
                    }
                }
            }
        }
        checkpoint(
            observer,
            "pbf_files",
            (file_number + 1) as u64,
            Some(inputs.len() as u64),
        )?;
    }
    let mut needed: Vec<_> = raw.values().flat_map(|w| w.refs.iter().copied()).collect();
    needed.sort_unstable();
    needed.dedup();
    let mut coordinates = vec![None; needed.len()];
    for (date, path, _) in &inputs {
        let reader = BlobReader::from_path(path).map_err(|e| Error(e.to_string()))?;
        for (block_number, blob) in reader.enumerate() {
            checkpoint(observer, "pbf_nodes", block_number as u64, None)?;
            if let BlobDecode::OsmData(block) = blob
                .map_err(|e| Error(e.to_string()))?
                .decode()
                .map_err(|e| Error(e.to_string()))?
            {
                for element in block.elements() {
                    let node = match element {
                        Element::Node(n) => Some((
                            n.id(),
                            GeoPoint {
                                lng: n.lon(),
                                lat: n.lat(),
                            },
                        )),
                        Element::DenseNode(n) => Some((
                            n.id(),
                            GeoPoint {
                                lng: n.lon(),
                                lat: n.lat(),
                            },
                        )),
                        _ => None,
                    };
                    if let Some((id, point)) = node
                        && let Ok(index) = needed.binary_search(&id)
                    {
                        point.validate()?;
                        if let Some((previous, previous_date)) = coordinates[index]
                            && previous_date == *date
                            && crate::distance_m(previous, point) > 0.02
                        {
                            return Err(Error(format!("conflicting same-snapshot node {id}")));
                        }
                        coordinates[index] = Some((point, *date));
                    }
                }
            }
        }
    }
    let mut result = Vec::new();
    for way in raw.into_values() {
        let nodes = way
            .refs
            .into_iter()
            .map(|id| {
                let index = needed.binary_search(&id).unwrap();
                let (point, _) = coordinates[index].ok_or_else(|| {
                    Error(format!("PBF road {} references missing node {id}", way.id))
                })?;
                Ok(WayNode { id, point })
            })
            .collect::<Result<Vec<_>>>()?;
        if nodes
            .windows(2)
            .any(|n| bounds.intersects_segment(n[0].point, n[1].point))
        {
            result.push(Way {
                id: way.id,
                nodes,
                tags: way.tags,
            });
        }
    }
    sources.sort_by(|a, b| a.sha256.cmp(&b.sha256));
    sources.dedup_by(|a, b| a.sha256 == b.sha256);
    checkpoint(
        observer,
        "pbf_complete",
        result.len() as u64,
        Some(result.len() as u64),
    )?;
    Ok((result, sources))
}
