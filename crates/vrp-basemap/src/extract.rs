use crate::{
    archive::{Archive, Remote},
    covers, inspect, invalid, validate_bounds,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use vrp_proto::maps::v1 as pb;

pub enum Prepared {
    Package(Box<pb::BasemapPackage>),
    Estimate(pb::BasemapEstimate),
}
pub fn prepare(
    input: &pb::BasemapTaskInput,
    output: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&str, u64, u64),
) -> io::Result<Prepared> {
    let mut run = || -> io::Result<Prepared> {
        if !input.import_path.is_empty() {
            let origin = Path::new(&input.import_path);
            let origin = if origin.is_dir() {
                origin.join("archive.pmtiles")
            } else {
                origin.to_path_buf()
            };
            let size = origin.metadata()?.len();
            if size > input.output_limit {
                return Err(invalid("basemap disk budget exceeded"));
            }
            let mut from = File::open(origin)?;
            let mut to = File::create(output)?;
            let mut b = [0; 65536];
            let mut used = 0;
            loop {
                check(cancel)?;
                let n = from.read(&mut b)?;
                if n == 0 {
                    break;
                }
                if used + n as u64 > input.output_limit || used + n as u64 > size {
                    return Err(invalid("import file grew beyond reserved size"));
                }
                to.write_all(&b[..n])?;
                used += n as u64;
                progress("import", used, size);
            }
            to.sync_all()?;
            if used != size {
                return Err(invalid("import file changed during copy"));
            }
            drop(to);
            check(cancel)?;
            progress("verify", 0, 1);
            return inspect(output).map(Box::new).map(Prepared::Package);
        }
        validate_bounds(&input.bounds)?;
        let request = input
            .request
            .as_ref()
            .ok_or_else(|| invalid("basemap request required"))?;
        let max = if request.native_max_zoom == 0 {
            14
        } else {
            request.native_max_zoom
        };
        if max > 15 || request.automatic && max > 14 {
            return Err(invalid("automatic basemaps are limited to native zoom 14"));
        }
        let source = input
            .source
            .as_ref()
            .ok_or_else(|| invalid("frozen basemap source required"))?;
        let mut upstream = Archive::open(Box::new(Remote::new(
            source.clone(),
            input.transfer_limit,
            cancel,
        )?))?;
        let mut metadata = upstream.metadata()?;
        upstream.format(&metadata)?;
        let reuse = if !input.reuse_path.is_empty() {
            let p = inspect(Path::new(&input.reuse_path))?;
            if p.source.as_ref() == Some(source) {
                Some(Archive::local(Path::new(&input.reuse_path))?)
            } else {
                None
            }
        } else {
            None
        };
        let mut reuse = reuse;
        if reuse.as_ref().is_some_and(|r| {
            r.tile_type != upstream.tile_type || r.tile_compression != upstream.tile_compression
        }) {
            return Err(invalid("reuse format does not match frozen source"));
        }
        let ids = tile_ids(&input.bounds, max as u8)?;
        let mut groups: BTreeMap<(bool, u64, usize), Vec<pmtiles::TileId>> = BTreeMap::new();
        for (i, id) in ids.iter().enumerate() {
            check(cancel)?;
            let local = if let Some(r) = reuse.as_mut() {
                r.locate(id.value())?
            } else {
                None
            };
            let located = match local {
                Some(v) => Some((false, v)),
                None => upstream.locate(id.value())?.map(|v| (true, v)),
            };
            if let Some((remote, (offset, len))) = located {
                groups.entry((remote, offset, len)).or_default().push(*id);
            }
            if i % 128 == 0 {
                progress("index", i as u64, ids.len() as u64);
            }
        }
        // Account for the gaps included when nearby Range requests are merged.
        let mut remote_bytes = 0u64;
        let mut range: Option<(u64, u64)> = None;
        for (_, offset, len) in groups.keys().filter(|(remote, _, _)| *remote) {
            let end = *offset + *len as u64;
            if let Some((start, old_end)) = range {
                if *offset <= old_end + 4096 && end - start <= 2 * 1024 * 1024 {
                    range = Some((start, old_end.max(end)));
                } else {
                    remote_bytes += old_end - start;
                    range = Some((*offset, end));
                }
            } else {
                range = Some((*offset, end));
            }
        }
        if let Some((start, end)) = range {
            remote_bytes += end - start;
        }
        let remote_bytes = remote_bytes + upstream.io.transferred();
        let output_bytes: u64 = groups.keys().map(|(_, _, len)| *len as u64).sum::<u64>()
            + (ids.len() as u64) * 32
            + 32768;
        if remote_bytes > input.transfer_limit {
            return Err(invalid(
                "basemap transfer budget exceeded; reduce area or detail",
            ));
        }
        if output_bytes > input.output_limit {
            return Err(invalid("basemap disk budget exceeded"));
        }
        if input.estimate_only {
            return Ok(Prepared::Estimate(pb::BasemapEstimate {
                bounds: input.bounds.clone(),
                native_max_zoom: max,
                transfer_bytes: remote_bytes,
                package_bytes: output_bytes,
                disk_peak_bytes: output_bytes,
                source: Some(source.clone()),
            }));
        }
        metadata["minzoom"] = 0.into();
        metadata["maxzoom"] = max.into();
        metadata["vrp_bounds"] =
            serde_json::to_value(&input.bounds).map_err(|e| invalid(e.to_string()))?;
        metadata["vrp_source"] =
            serde_json::to_value(source).map_err(|e| invalid(e.to_string()))?;
        let west = input.bounds.iter().map(|b| b.west).fold(180.0, f64::min);
        let east = input.bounds.iter().map(|b| b.east).fold(-180.0, f64::max);
        let south = input.bounds.iter().map(|b| b.south).fold(90.0, f64::min);
        let north = input.bounds.iter().map(|b| b.north).fold(-90.0, f64::max);
        metadata["bounds"] = format!("{west},{south},{east},{north}").into();
        let mut writer = pmtiles::PmTilesWriter::new(upstream.tile_type)
            .precompressed(upstream.tile_compression)
            .bounds(west, south, east, north)
            .metadata(metadata.to_string())
            .create(File::create(output)?)
            .map_err(|e| invalid(e.to_string()))?;
        let entries: Vec<_> = groups.into_iter().collect();
        let mut index = 0;
        while index < entries.len() {
            check(cancel)?;
            let ((remote, start, length), _) = &entries[index];
            let mut end = *start + *length as u64;
            let mut last = index + 1;
            while last < entries.len() {
                let ((r, o, l), _) = &entries[last];
                if r != remote
                    || *o > end + 4096
                    || o + l.to_owned() as u64 - start > 2 * 1024 * 1024
                {
                    break;
                }
                end = end.max(*o + *l as u64);
                last += 1;
            }
            let bytes = if *remote {
                upstream.io.read(*start, (end - start) as usize)?
            } else {
                reuse
                    .as_mut()
                    .unwrap()
                    .io
                    .read(*start, (end - start) as usize)?
            };
            for ((_, offset, len), tiles) in &entries[index..last] {
                let at = (offset - start) as usize;
                for id in tiles {
                    writer
                        .add_tile((*id).into(), &bytes[at..at + len])
                        .map_err(|e| invalid(e.to_string()))?;
                }
            }
            progress("download", upstream.io.transferred(), remote_bytes);
            index = last;
        }
        writer.finalize().map_err(|e| invalid(e.to_string()))?;
        // Source precision and display precision are deliberately independent.
        let mut f = OpenOptions::new().write(true).open(output)?;
        f.seek(SeekFrom::Start(100))?;
        f.write_all(&[0, max as u8])?;
        f.sync_all()?;
        drop(f);
        check(cancel)?;
        progress("verify", 0, 1);
        let package = inspect(output)?;
        if !covers(&package.bounds, &input.bounds) {
            return Err(invalid("extracted coverage mismatch"));
        }
        if package.size_bytes > input.output_limit {
            return Err(invalid("basemap disk budget exceeded"));
        }
        Ok(Prepared::Package(Box::new(package)))
    };
    let result = run();
    if result.is_err() {
        let _ = fs::remove_file(output);
    }
    result
}
fn check(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(io::ErrorKind::Interrupted.into())
    } else {
        Ok(())
    }
}
pub(crate) fn tile_ids(bounds: &[pb::Bounds], max: u8) -> io::Result<BTreeSet<pmtiles::TileId>> {
    let mut ids = BTreeSet::new();
    for z in 0..=max {
        let side = 1u32 << z;
        for b in bounds {
            let x =
                |lon: f64| (((lon + 180.0) / 360.0 * f64::from(side)).floor() as u32).min(side - 1);
            let y = |lat: f64| {
                (((1.0 - lat.to_radians().tan().asinh() / std::f64::consts::PI) / 2.0
                    * f64::from(side))
                .floor() as u32)
                    .min(side - 1)
            };
            let (left, right, top, bottom) = (x(b.west), x(b.east), y(b.north), y(b.south));
            if (u64::from(right - left) + 1) * (u64::from(bottom - top) + 1) + ids.len() as u64
                > 1_000_000
            {
                return Err(invalid(
                    "region has too many tiles; reduce area or precision",
                ));
            }
            for ty in top..=bottom {
                for tx in left..=right {
                    ids.insert(
                        pmtiles::TileCoord::new(z, tx, ty)
                            .map_err(|e| invalid(e.to_string()))?
                            .into(),
                    );
                }
            }
        }
    }
    Ok(ids)
}
