use super::*;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use vrp_proto::maps::v1 as maps;

include!(concat!(env!("OUT_DIR"), "/basemap_assets.rs"));

pub(super) async fn list(State(s): State<HttpState>, r: Request) -> Result<Response> {
    let q = query::<maps::ListBasemapsRequest>(r.uri())?;
    blocking(s, accept(r.headers(), Wire::Json), move |v| {
        v.list_basemaps(q)
    })
    .await
}
pub(super) async fn get_package(
    State(s): State<HttpState>,
    Path(id): Path<String>,
    r: Request,
) -> Result<Response> {
    blocking(s, accept(r.headers(), Wire::Json), move |v| {
        v.get_basemap(&id)
    })
    .await
}
pub(super) async fn prepare(State(s): State<HttpState>, r: Request) -> Result<Response> {
    let out = accept(r.headers(), Wire::Json);
    let (w, b) = body(r, s.service.request_limit()).await?;
    let q = decode(w, &b)?;
    blocking(s, out, move |v| v.prepare_basemap(q)).await
}
pub(super) async fn estimate(State(s): State<HttpState>, r: Request) -> Result<Response> {
    let out = accept(r.headers(), Wire::Json);
    let (w, b) = body(r, s.service.request_limit()).await?;
    let q = decode(w, &b)?;
    blocking(s, out, move |v| v.estimate_basemap(q)).await
}
pub(super) async fn import(State(s): State<HttpState>, r: Request) -> Result<Response> {
    let out = accept(r.headers(), Wire::Json);
    let (w, b) = body(r, s.service.request_limit()).await?;
    let q = decode(w, &b)?;
    blocking(s, out, move |v| v.import_basemap(q)).await
}
pub(super) async fn enable(
    State(s): State<HttpState>,
    Path(id): Path<String>,
    r: Request,
) -> Result<Response> {
    let out = accept(r.headers(), Wire::Json);
    let (w, b) = body(r, s.service.request_limit()).await?;
    let mut q: maps::EnableBasemapRequest = decode(w, &b)?;
    q.package_id = id;
    blocking(s, out, move |v| v.enable_basemap(q)).await
}
pub(super) async fn update(
    State(s): State<HttpState>,
    Path(id): Path<String>,
    r: Request,
) -> Result<Response> {
    blocking(s, accept(r.headers(), Wire::Json), move |v| {
        v.update_basemap(&id)
    })
    .await
}
pub(super) async fn delete(
    State(s): State<HttpState>,
    Path(id): Path<String>,
    r: Request,
) -> Result<Response> {
    blocking(s, accept(r.headers(), Wire::Json), move |v| {
        v.delete_basemap(&id)
    })
    .await
}
pub(super) async fn selection(State(s): State<HttpState>, r: Request) -> Result<Response> {
    let q = query::<maps::BasemapContext>(r.uri())?;
    blocking(s, accept(r.headers(), Wire::Json), move |v| {
        v.select_basemap(q)
    })
    .await
}
pub(super) async fn set_selection(State(s): State<HttpState>, r: Request) -> Result<Response> {
    let out = accept(r.headers(), Wire::Json);
    let (w, b) = body(r, s.service.request_limit()).await?;
    let q = decode(w, &b)?;
    blocking(s, out, move |v| v.set_basemap_selection(q)).await
}

pub(super) fn resource_path(path: &str) -> bool {
    path.starts_with("/v1/basemap-assets/")
        || path.starts_with("/v1/basemaps/")
            && (path.ends_with("/archive.pmtiles")
                || path.ends_with("/style-light.json")
                || path.ends_with("/style-standard.json"))
}
fn etag_matches(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',').any(|s| {
                s.trim() == etag || s.trim() == "*" || s.trim().strip_prefix("W/") == Some(etag)
            })
        })
}
fn cached(status: StatusCode, body: Body, content_type: &str, etag: &str, len: u64) -> Response {
    let mut r = Response::new(body);
    *r.status_mut() = status;
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, content_type.parse().unwrap());
    h.insert(header::ETAG, etag.parse().unwrap());
    h.insert(
        header::CACHE_CONTROL,
        "private, max-age=86400, immutable".parse().unwrap(),
    );
    if status != StatusCode::NOT_MODIFIED {
        h.insert(header::CONTENT_LENGTH, len.into());
    }
    r
}
pub(super) async fn asset(
    Path((version, path)): Path<(String, String)>,
    r: Request,
) -> Result<Response> {
    if version != vrp_basemap::ASSETS_VERSION {
        return Err(AppError::new(
            "not_found",
            "basemap asset version not found",
        ));
    }
    let (_, bytes) = ASSETS
        .iter()
        .find(|(name, _)| *name == path)
        .ok_or_else(|| AppError::new("not_found", "basemap asset not found"))?;
    let etag = format!(
        "\"{version}-{}\"",
        super::super::runtime::tasks::basemaps::asset_digest(&path)
    );
    if etag_matches(r.headers(), &etag) {
        return Ok(cached(
            StatusCode::NOT_MODIFIED,
            Body::empty(),
            "application/octet-stream",
            &etag,
            0,
        ));
    }
    let mime = if path.ends_with(".json") {
        "application/json"
    } else if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".pbf") {
        "application/x-protobuf"
    } else {
        "text/plain; charset=utf-8"
    };
    let body = if r.method() == axum::http::Method::HEAD {
        Body::empty()
    } else {
        Body::from(*bytes)
    };
    let mut response = cached(StatusCode::OK, body, mime, &etag, bytes.len() as u64);
    if bytes.starts_with(&[0x1f, 0x8b]) {
        response
            .headers_mut()
            .insert(header::CONTENT_ENCODING, "gzip".parse().unwrap());
    }
    Ok(response)
}
pub(super) async fn resource(
    State(s): State<HttpState>,
    Path((id, file)): Path<(String, String)>,
    r: Request,
) -> Result<Response> {
    if !matches!(
        file.as_str(),
        "archive.pmtiles" | "style-light.json" | "style-standard.json"
    ) {
        return Err(AppError::new("not_found", "basemap resource not found"));
    }
    let service = s.service.clone();
    let (package, path) = offload(s.map_operations.clone(), move || {
        service.basemap_resource(&id)
    })
    .await?;
    if file != "archive.pmtiles" {
        let etag = format!(
            "\"{}-{}-{}\"",
            package.id,
            vrp_basemap::ASSETS_VERSION,
            file
        );
        if etag_matches(r.headers(), &etag) {
            return Ok(cached(
                StatusCode::NOT_MODIFIED,
                Body::empty(),
                "application/json",
                &etag,
                0,
            ));
        }
        let bytes = serde_json::to_vec(&style(&package, file == "style-light.json"))
            .map_err(|_| AppError::internal("style serialization failed"))?;
        let len = bytes.len() as u64;
        let body = if r.method() == axum::http::Method::HEAD {
            Body::empty()
        } else {
            Body::from(bytes)
        };
        return Ok(cached(StatusCode::OK, body, "application/json", &etag, len));
    }
    let etag = format!("\"{}\"", package.sha256);
    if etag_matches(r.headers(), &etag) {
        return Ok(cached(
            StatusCode::NOT_MODIFIED,
            Body::empty(),
            "application/vnd.pmtiles",
            &etag,
            0,
        ));
    }
    let mut f = tokio::fs::File::open(path).await?;
    let size = f.metadata().await?.len();
    if size != package.size_bytes {
        return Err(AppError::internal("basemap file length changed"));
    }
    let range = if r
        .headers()
        .get(header::IF_RANGE)
        .is_some_and(|v| v.to_str().ok() != Some(etag.as_str()))
    {
        None
    } else {
        r.headers().get(header::RANGE).and_then(|v| v.to_str().ok())
    };
    let (start, end, partial) = match byte_range(range, size) {
        Ok(v) => v,
        Err(()) => {
            let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                format!("bytes */{size}").parse().unwrap(),
            );
            return Ok(response);
        }
    };
    let len = end - start + 1;
    f.seek(std::io::SeekFrom::Start(start)).await?;
    let body = if r.method() == axum::http::Method::HEAD {
        Body::empty()
    } else {
        let mut f = f.take(len);
        let stream = async_stream::try_stream! {let mut buffer=vec![0;65536];loop{let n=f.read(&mut buffer).await?;if n==0{break;}yield axum::body::Bytes::copy_from_slice(&buffer[..n]);}};
        let stream: std::pin::Pin<
            Box<dyn futures_util::Stream<Item = std::io::Result<axum::body::Bytes>> + Send>,
        > = Box::pin(stream);
        Body::from_stream(stream)
    };
    let mut response = cached(
        if partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        },
        body,
        "application/vnd.pmtiles",
        &etag,
        len,
    );
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
    if partial {
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{size}").parse().unwrap(),
        );
    }
    Ok(response)
}
fn byte_range(range: Option<&str>, size: u64) -> std::result::Result<(u64, u64, bool), ()> {
    if size == 0 {
        return Err(());
    }
    let Some(range) = range else {
        return Ok((0, size - 1, false));
    };
    let (a, b) = range
        .strip_prefix("bytes=")
        .and_then(|v| v.split_once('-'))
        .ok_or(())?;
    if a.is_empty() {
        let count = b.parse::<u64>().map_err(|_| ())?;
        if count == 0 {
            return Err(());
        }
        return Ok((size.saturating_sub(count), size - 1, true));
    }
    let start = a.parse::<u64>().map_err(|_| ())?;
    let end = if b.is_empty() {
        size - 1
    } else {
        b.parse::<u64>().map_err(|_| ())?.min(size - 1)
    };
    if start >= size || start > end {
        return Err(());
    }
    Ok((start, end, true))
}
fn style(p: &maps::BasemapPackage, light: bool) -> serde_json::Value {
    use serde_json::json;
    let source = format!("pmtiles:///v1/basemaps/{}/archive.pmtiles", p.id);
    if p.format != "mvt" {
        return json!({"version":8,"sources":{"basemap":{"type":"raster","url":source,"tileSize":256,"attribution":p.attribution}},"layers":[{"id":"basemap","type":"raster","source":"basemap"}]});
    }
    let assets = format!("/v1/basemap-assets/{}", p.assets_version);
    let road = if light { "#ccd2d6" } else { "#ffffff" };
    let earth = if light { "#f4f5f3" } else { "#eeece5" };
    let text = if light { "#74828a" } else { "#52616d" };
    let name = json!([
        "coalesce",
        ["get", "name:zh-Hans"],
        ["get", "name:zh"],
        ["get", "name"],
        ""
    ]);
    json!({"version":8,"glyphs":format!("{assets}/fonts/{{fontstack}}/{{range}}.pbf"),"sprite":format!("{assets}/sprites/light"),"sources":{"basemap":{"type":"vector","url":source,"attribution":p.attribution}},"layers":[
        {"id":"base-background","type":"background","paint":{"background-color":earth}},
        {"id":"base-earth","type":"fill","source":"basemap","source-layer":"earth","paint":{"fill-color":earth}},
        {"id":"base-landuse","type":"fill","source":"basemap","source-layer":"landuse","paint":{"fill-color":"#e5ebdf","fill-opacity":0.6}},
        {"id":"base-water","type":"fill","source":"basemap","source-layer":"water","paint":{"fill-color":"#d6e6ec"}},
        {"id":"base-building","type":"fill","source":"basemap","source-layer":"buildings","minzoom":13,"paint":{"fill-color":"#dfe2e0","fill-outline-color":"#d4d9d7"}},
        {"id":"base-road-casing","type":"line","source":"basemap","source-layer":"roads","layout":{"line-cap":"round","line-join":"round"},"paint":{"line-color":"#c4cdcf","line-width":["interpolate",["linear"],["zoom"],7,0.6,14,3.5,19,10]}},
        {"id":"base-road","type":"line","source":"basemap","source-layer":"roads","layout":{"line-cap":"round","line-join":"round"},"paint":{"line-color":road,"line-width":["interpolate",["linear"],["zoom"],7,0.3,14,2.2,19,8]}},
        {"id":"base-road-label","type":"symbol","source":"basemap","source-layer":"roads","minzoom":12,"layout":{"symbol-placement":"line","text-field":name,"text-font":["VRP Sans"],"text-size":11},"paint":{"text-color":text,"text-halo-color":earth,"text-halo-width":1.5}},
        {"id":"base-poi","type":"symbol","source":"basemap","source-layer":"pois","minzoom":13,"layout":{"text-field":name,"text-font":["VRP Sans"],"text-size":11,"text-offset":[0,1],"icon-image":["coalesce",["image",["get","kind"]],["image","attraction"]],"icon-size":0.75},"paint":{"text-color":text,"text-halo-color":earth,"text-halo-width":1}},
        {"id":"base-place","type":"symbol","source":"basemap","source-layer":"places","layout":{"text-field":name,"text-font":["VRP Sans"],"text-size":["interpolate",["linear"],["zoom"],4,11,14,14]},"paint":{"text-color":text,"text-halo-color":earth,"text-halo-width":1.5}}
    ]})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn range_boundaries() {
        assert_eq!(byte_range(Some("bytes=0-126"), 200), Ok((0, 126, true)));
        assert_eq!(byte_range(Some("bytes=-20"), 200), Ok((180, 199, true)));
        assert_eq!(byte_range(Some("bytes=199-"), 200), Ok((199, 199, true)));
        for range in ["bytes=200-", "bytes=1-0", "bytes=0-1,3-5", "bytes=-0"] {
            assert!(byte_range(Some(range), 200).is_err());
        }
    }
}
