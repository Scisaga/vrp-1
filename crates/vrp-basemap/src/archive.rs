use crate::{client, invalid};
use flate2::read::GzDecoder;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use vrp_proto::maps::v1 as pb;

pub(crate) trait ReadAt {
    fn read(&mut self, offset: u64, len: usize) -> io::Result<Vec<u8>>;
    fn size(&self) -> u64;
    fn transferred(&self) -> u64 {
        0
    }
}
struct Local {
    file: File,
    size: u64,
}
impl ReadAt for Local {
    fn read(&mut self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        if offset
            .checked_add(len as u64)
            .is_none_or(|end| end > self.size)
        {
            return Err(invalid("archive range out of bounds"));
        }
        let mut bytes = vec![0; len];
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    fn size(&self) -> u64 {
        self.size
    }
}
pub(crate) struct Remote<'a> {
    client: reqwest::blocking::Client,
    source: pb::SourceSnapshot,
    used: u64,
    limit: u64,
    cancel: &'a AtomicBool,
}
impl<'a> Remote<'a> {
    pub fn new(source: pb::SourceSnapshot, limit: u64, cancel: &'a AtomicBool) -> io::Result<Self> {
        Ok(Self {
            client: client(&source.url)?,
            source,
            used: 0,
            limit,
            cancel,
        })
    }
}
impl ReadAt for Remote<'_> {
    fn read(&mut self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if len == 0
            || len > 16 * 1024 * 1024
            || offset
                .checked_add(len as u64)
                .is_none_or(|end| end > self.source.size_bytes)
        {
            return Err(invalid("invalid archive range"));
        }
        if self.used.saturating_add(len as u64) > self.limit {
            return Err(invalid("basemap transfer budget exceeded"));
        }
        let end = offset + len as u64 - 1;
        let r = self
            .client
            .get(&self.source.url)
            .header("Range", format!("bytes={offset}-{end}"))
            .header("If-Match", &self.source.etag)
            .header("Accept-Encoding", "identity")
            .send()
            .map_err(|e| invalid(e.to_string()))?;
        if r.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(invalid(
                "upstream must honor Range (206), whole-planet fallback is forbidden",
            ));
        }
        let expected = format!("bytes {offset}-{end}/{}", self.source.size_bytes);
        if r.headers()
            .get("Content-Range")
            .and_then(|v| v.to_str().ok())
            != Some(expected.as_str())
            || r.headers().get("ETag").and_then(|v| v.to_str().ok())
                != Some(self.source.etag.as_str())
        {
            return Err(invalid(
                "source changed or returned an invalid Content-Range",
            ));
        }
        let mut bytes = Vec::with_capacity(len);
        r.take(len as u64 + 1).read_to_end(&mut bytes)?;
        self.used += bytes.len() as u64;
        if bytes.len() != len {
            return Err(invalid("truncated or oversized range response"));
        }
        Ok(bytes)
    }
    fn size(&self) -> u64 {
        self.source.size_bytes
    }
    fn transferred(&self) -> u64 {
        self.used
    }
}
#[derive(Clone, Copy, Debug)]
struct Entry {
    id: u64,
    run: u64,
    len: usize,
    offset: u64,
}
pub(crate) struct Archive<'a> {
    pub io: Box<dyn ReadAt + 'a>,
    root: (u64, usize),
    leaf: (u64, u64),
    data: (u64, u64),
    meta: (u64, usize),
    compression: u8,
    pub tile_compression: pmtiles::Compression,
    pub tile_type: pmtiles::TileType,
    cache: BTreeMap<u64, Vec<Entry>>,
    cache_bytes: usize,
    native_min: u8,
    native_max: u8,
}
impl Archive<'static> {
    pub fn local(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        Self::open(Box::new(Local { file, size }))
    }
}
impl<'a> Archive<'a> {
    pub fn open(mut io: Box<dyn ReadAt + 'a>) -> io::Result<Self> {
        let h = io.read(0, 127)?;
        if &h[..8] != b"PMTiles\x03" {
            return Err(invalid("PMTiles v3 required"));
        }
        let u = |i| u64::from_le_bytes(h[i..i + 8].try_into().unwrap());
        for (off, len) in [
            (u(8), u(16)),
            (u(24), u(32)),
            (u(40), u(48)),
            (u(56), u(64)),
        ] {
            if off.checked_add(len).is_none_or(|end| end > io.size()) {
                return Err(invalid("PMTiles section exceeds file"));
            }
        }
        let tile_type = match h[99] {
            1 => pmtiles::TileType::Mvt,
            2 => pmtiles::TileType::Png,
            3 => pmtiles::TileType::Jpeg,
            4 => pmtiles::TileType::Webp,
            _ => return Err(invalid("unsupported PMTiles tile type")),
        };
        let tile_compression = match h[98] {
            1 => pmtiles::Compression::None,
            2 => pmtiles::Compression::Gzip,
            _ => return Err(invalid("unsupported tile compression")),
        };
        if !matches!(h[97], 1 | 2) || h[100] > h[101] || h[101] > 24 {
            return Err(invalid("invalid PMTiles compression or zoom"));
        }
        Ok(Self {
            io,
            root: (
                u(8),
                u(16)
                    .try_into()
                    .map_err(|_| invalid("directory too large"))?,
            ),
            leaf: (u(40), u(48)),
            data: (u(56), u(64)),
            meta: (
                u(24),
                u(32)
                    .try_into()
                    .map_err(|_| invalid("metadata too large"))?,
            ),
            compression: h[97],
            tile_compression,
            tile_type,
            cache: BTreeMap::new(),
            cache_bytes: 0,
            native_min: h[100],
            native_max: h[101],
        })
    }
    pub fn validate_all(&mut self) -> io::Result<()> {
        let mut stack = vec![(self.root, 0)];
        let mut visited = std::collections::BTreeSet::new();
        let first = ((1u64 << (2 * self.native_min)) - 1) / 3;
        let last = ((1u64 << (2 * (self.native_max + 1))) - 1) / 3 - 1;
        while let Some((at, depth)) = stack.pop() {
            if depth >= 6 || !visited.insert(at.0) {
                return Err(invalid("cyclic or excessively deep directory"));
            }
            let entries = directory(&self.decoded(at.0, at.1)?)?;
            for e in entries {
                if e.run == 0 {
                    if e.offset
                        .checked_add(e.len as u64)
                        .is_none_or(|end| end > self.leaf.1)
                    {
                        return Err(invalid("leaf exceeds archive section"));
                    }
                    stack.push(((self.leaf.0 + e.offset, e.len), depth + 1));
                } else {
                    if e.id < first || e.id.checked_add(e.run - 1).is_none_or(|end| end > last) {
                        return Err(invalid("tile data exceeds declared native zoom levels"));
                    }
                    if e.offset
                        .checked_add(e.len as u64)
                        .is_none_or(|end| end > self.data.1)
                    {
                        return Err(invalid("tile payload exceeds archive section"));
                    }
                }
            }
        }
        Ok(())
    }
    fn decoded(&mut self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        if len > 4 * 1024 * 1024 {
            return Err(invalid("compressed metadata/directory exceeds limit"));
        }
        let bytes = self.io.read(offset, len)?;
        let decoded = if self.compression == 2 {
            let mut out = Vec::new();
            GzDecoder::new(bytes.as_slice())
                .take(32 * 1024 * 1024 + 1)
                .read_to_end(&mut out)?;
            out
        } else {
            bytes
        };
        if decoded.len() > 32 * 1024 * 1024 {
            return Err(invalid("expanded metadata/directory exceeds limit"));
        }
        Ok(decoded)
    }
    pub fn metadata(&mut self) -> io::Result<serde_json::Value> {
        let b = self.decoded(self.meta.0, self.meta.1)?;
        serde_json::from_slice(&b).map_err(|e| invalid(e.to_string()))
    }
    pub fn format(&self, metadata: &serde_json::Value) -> io::Result<String> {
        Ok(match self.tile_type {
            pmtiles::TileType::Png => "png",
            pmtiles::TileType::Jpeg => "jpeg",
            pmtiles::TileType::Webp => "webp",
            pmtiles::TileType::Mvt => {
                let layers = metadata["vector_layers"]
                    .as_array()
                    .ok_or_else(|| invalid("vector_layers metadata required"))?;
                if !["earth", "water", "roads", "places"]
                    .iter()
                    .all(|name| layers.iter().any(|v| v["id"].as_str() == Some(name)))
                {
                    return Err(invalid(
                        "only Protomaps v4 compatible vector layers are supported",
                    ));
                }
                "mvt"
            }
            _ => return Err(invalid("unsupported tile type")),
        }
        .into())
    }
    pub fn locate(&mut self, id: u64) -> io::Result<Option<(u64, usize)>> {
        let mut at = self.root;
        for _ in 0..6 {
            if !self.cache.contains_key(&at.0) {
                let raw = self.decoded(at.0, at.1)?;
                let entries = directory(&raw)?;
                self.cache_bytes += entries.len() * std::mem::size_of::<Entry>();
                if self.cache_bytes > 64 * 1024 * 1024 {
                    return Err(invalid("directory cache exceeds limit"));
                }
                self.cache.insert(at.0, entries);
            }
            let entries = &self.cache[&at.0];
            let n = entries.partition_point(|e| e.id <= id);
            if n == 0 {
                return Ok(None);
            }
            let e = entries[n - 1];
            if e.run == 0 {
                if e.offset
                    .checked_add(e.len as u64)
                    .is_none_or(|v| v > self.leaf.1)
                {
                    return Err(invalid("leaf exceeds section"));
                }
                at = (self.leaf.0 + e.offset, e.len);
            } else {
                if id - e.id >= e.run {
                    return Ok(None);
                }
                if e.offset
                    .checked_add(e.len as u64)
                    .is_none_or(|v| v > self.data.1)
                {
                    return Err(invalid("tile exceeds section"));
                }
                return Ok(Some((self.data.0 + e.offset, e.len)));
            }
        }
        Err(invalid("directory depth exceeds limit"))
    }
}
fn varint(input: &mut &[u8]) -> io::Result<u64> {
    let mut v = 0u64;
    for i in 0..10 {
        let (&b, tail) = input
            .split_first()
            .ok_or_else(|| invalid("truncated directory varint"))?;
        *input = tail;
        if i == 9 && b > 1 {
            return Err(invalid("directory varint overflow"));
        }
        v |= u64::from(b & 127) << (i * 7);
        if b < 128 {
            return Ok(v);
        }
    }
    Err(invalid("directory varint overflow"))
}
fn directory(mut b: &[u8]) -> io::Result<Vec<Entry>> {
    let n = usize::try_from(varint(&mut b)?).map_err(|_| invalid("directory too large"))?;
    if n > b.len() / 4 {
        return Err(invalid("invalid directory length"));
    }
    let mut out = vec![
        Entry {
            id: 0,
            run: 0,
            len: 0,
            offset: 0
        };
        n
    ];
    let mut sum = 0u64;
    for e in &mut out {
        sum = sum
            .checked_add(varint(&mut b)?)
            .ok_or_else(|| invalid("tile ID overflow"))?;
        e.id = sum;
    }
    for e in &mut out {
        e.run = varint(&mut b)?;
    }
    for e in &mut out {
        e.len = usize::try_from(varint(&mut b)?).map_err(|_| invalid("tile too large"))?;
        if e.len == 0 || e.len > 16 * 1024 * 1024 {
            return Err(invalid("invalid tile/directory size"));
        }
    }
    let mut end = 0;
    for (i, e) in out.iter_mut().enumerate() {
        let off = varint(&mut b)?;
        if off == 0 && i == 0 {
            return Err(invalid("invalid first offset"));
        }
        e.offset = if off == 0 { end } else { off - 1 };
        end = e
            .offset
            .checked_add(e.len as u64)
            .ok_or_else(|| invalid("offset overflow"))?;
    }
    if !b.is_empty() || out.windows(2).any(|w| w[0].id >= w[1].id) {
        return Err(invalid("invalid directory order"));
    }
    Ok(out)
}
