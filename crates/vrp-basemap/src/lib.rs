//! Offline display data. No VRP model, score or execution state is changed here.
mod archive;
mod extract;
#[cfg(test)]
mod tests;

pub use extract::{Prepared, prepare};
pub use pmtiles;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::time::Duration;
use vrp_proto::maps::v1 as pb;

pub const ASSETS_VERSION: &str = "protomaps-v4-1";
pub const MAX_LATITUDE: f64 = 85.051_128_779_8;

pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
pub(crate) fn client(url: &str) -> io::Result<reqwest::blocking::Client> {
    let mut builder = reqwest::blocking::Client::builder();
    // Local test sources and local deployments must not go through a system
    // HTTP proxy. Remote archives retain the administrator's proxy settings.
    if reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .is_some_and(|h| {
            h == "localhost"
                || h.trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
    {
        builder = builder.no_proxy();
    }
    builder
        .user_agent(concat!("vrp-1-basemap/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| invalid(e.to_string()))
}
pub fn file_hash(path: &Path) -> io::Result<(u64, String)> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut bytes = [0; 65536];
    let mut size = 0;
    loop {
        let n = f.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        h.update(&bytes[..n]);
        size += n as u64;
    }
    Ok((size, format!("{:x}", h.finalize())))
}
pub fn validate_bounds(bounds: &[pb::Bounds]) -> io::Result<()> {
    if bounds.is_empty() || bounds.len() > 2 {
        return Err(invalid("one or two bounds required"));
    }
    for b in bounds {
        if ![b.west, b.south, b.east, b.north]
            .iter()
            .all(|v| v.is_finite())
            || b.west < -180.0
            || b.east > 180.0
            || b.west > b.east
            || b.south < -MAX_LATITUDE
            || b.north > MAX_LATITUDE
            || b.south > b.north
        {
            return Err(invalid("bounds exceed Web Mercator coverage"));
        }
    }
    Ok(())
}
pub fn covers(have: &[pb::Bounds], need: &[pb::Bounds]) -> bool {
    !need.is_empty()
        && need.iter().all(|n| {
            have.iter().any(|h| {
                h.west <= n.west && h.east >= n.east && h.south <= n.south && h.north >= n.north
            })
        })
}
pub fn resolve_source(catalog: &str, archive_base: &str) -> io::Result<(pb::SourceSnapshot, u64)> {
    let client = client(catalog)?;
    let (mut source, used) = if catalog.ends_with(".pmtiles") {
        (
            pb::SourceSnapshot {
                url: catalog.into(),
                key: catalog.rsplit('/').next().unwrap_or_default().into(),
                version: "4".into(),
                ..Default::default()
            },
            0,
        )
    } else {
        let response = client
            .get(catalog)
            .timeout(Duration::from_secs(15))
            .send()
            .and_then(|r| r.error_for_status())
            .map_err(|e| invalid(e.to_string()))?;
        let mut bytes = Vec::new();
        response.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err(invalid("basemap catalog exceeds limit"));
        }
        let entries: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
        let item = entries
            .as_array()
            .ok_or_else(|| invalid("invalid build catalog"))?
            .iter()
            .filter(|v| v["version"].as_str().is_some_and(|v| v.starts_with("4.")))
            .filter(|v| {
                v["key"]
                    .as_str()
                    .is_some_and(|k| k.ends_with(".pmtiles") && !k.contains(['/', '\\']))
            })
            .max_by_key(|v| v["key"].as_str().unwrap_or_default())
            .ok_or_else(|| invalid("no compatible Protomaps v4 build"))?;
        let key = item["key"].as_str().unwrap();
        (
            pb::SourceSnapshot {
                url: format!("{}/{key}", archive_base.trim_end_matches('/')),
                key: key.into(),
                version: item["version"].as_str().unwrap_or_default().into(),
                published: item["uploaded"].as_str().unwrap_or_default().into(),
                ..Default::default()
            },
            bytes.len() as u64,
        )
    };
    let head = crate::client(&source.url)?
        .head(&source.url)
        .timeout(Duration::from_secs(15))
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| invalid(e.to_string()))?;
    source.size_bytes = head
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| invalid("archive length missing"))?;
    source.etag = head
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .into();
    if source.size_bytes < 127 || source.etag.is_empty() {
        return Err(invalid("archive requires length and ETag"));
    }
    Ok((source, used))
}
pub fn inspect(path: &Path) -> io::Result<pb::BasemapPackage> {
    let mut archive = archive::Archive::local(path)?;
    let metadata = archive.metadata()?;
    let format = archive.format(&metadata)?;
    archive.validate_all()?;
    // The independent library reader validates the native archive header/root.
    let reader = pmtiles::PmTilesReader::new_with_path(path).map_err(|e| invalid(e.to_string()))?;
    for entry in reader.entries() {
        let entry = entry.map_err(|e| invalid(e.to_string()))?;
        if let Some(id) = entry.iter_coords().next() {
            reader.get_tile(id).map_err(|e| invalid(e.to_string()))?;
        }
    }
    let (size, hash) = file_hash(path)?;
    let bounds = metadata
        .get("vrp_bounds")
        .and_then(|v| serde_json::from_value::<Vec<pb::Bounds>>(v.clone()).ok())
        .unwrap_or_else(|| {
            vec![pb::Bounds {
                west: reader.get_header().min_longitude,
                south: reader.get_header().min_latitude,
                east: reader.get_header().max_longitude,
                north: reader.get_header().max_latitude,
            }]
        });
    validate_bounds(&bounds)?;
    let source = metadata
        .get("vrp_source")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    Ok(pb::BasemapPackage {
        id: hash.clone(),
        sha256: hash,
        size_bytes: size,
        bounds,
        format,
        native_min_zoom: u32::from(reader.get_header().min_zoom),
        native_max_zoom: u32::from(reader.get_header().max_zoom),
        source,
        assets_version: ASSETS_VERSION.into(),
        version: 1,
        enabled: true,
        attribution: metadata
            .get("attribution")
            .and_then(|v| v.as_str())
            .unwrap_or("© OpenStreetMap contributors · Protomaps")
            .into(),
        ..Default::default()
    })
}
