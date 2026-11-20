use crate::{AppError, Result};
use vrp_proto::app::v1 as pb;

pub(crate) fn normalize(mut config: pb::MapConfig) -> Result<pb::MapConfig> {
    config.custom = config.custom || !config.style_url.is_empty() || !config.raster_url.is_empty();
    config.offline_basemaps = !config.custom && !config.disabled;
    if config.style_url.is_empty() && config.raster_url.is_empty() && !config.disabled {
        config.raster_url = "https://tile.openstreetmap.org/{z}/{x}/{y}.png".into();
        config.attribution = "© OpenStreetMap contributors".into();
    }
    if config.max_zoom == 0 {
        config.max_zoom = 19;
    }
    if config.max_zoom > 24 || config.attribution.len() > 2048 || config.resource_origins.len() > 32
    {
        return Err(AppError::invalid("invalid map configuration limits"));
    }
    for url in [&config.style_url, &config.raster_url] {
        if url.is_empty() || (url.starts_with('/') && !url.starts_with("//")) {
            continue;
        }
        let origin = origin(url)?;
        if !config.resource_origins.contains(&origin) {
            config.resource_origins.push(origin);
        }
    }
    for allowed in &config.resource_origins {
        if origin(allowed)? != *allowed {
            return Err(AppError::invalid(
                "map resource_origins must be HTTP(S) origins",
            ));
        }
    }
    config.resource_origins.sort();
    config.resource_origins.dedup();
    Ok(config)
}
fn origin(url: &str) -> Result<String> {
    let uri: axum::http::Uri = url
        .parse()
        .map_err(|_| AppError::invalid("invalid map URL"))?;
    let scheme = uri.scheme_str().unwrap_or_default();
    let authority = uri
        .authority()
        .ok_or_else(|| AppError::invalid("map URL needs an authority"))?
        .as_str();
    if !matches!(scheme, "https" | "http")
        || authority.contains('@')
        || authority.contains('*')
        || url.contains(['\'', '"', ';', '\n', '\r'])
    {
        return Err(AppError::invalid(
            "map URL must be HTTP(S), without userinfo or CSP syntax",
        ));
    }
    Ok(format!("{scheme}://{authority}"))
}
pub(crate) fn csp(config: &pb::MapConfig) -> String {
    let origins = config.resource_origins.join(" ");
    format!(
        "default-src 'self'; script-src 'self'; worker-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: {origins}; connect-src 'self' {origins}; object-src 'none'; frame-ancestors 'none'; base-uri 'none'"
    )
}

pub(crate) fn quality(input: &vrp_proto::engine::v1::SolveRequest) -> Result<pb::MatrixQuality> {
    let stats = vrp_model::compile(input)
        .map_err(|e| AppError::invalid(e.0))?
        .stats;
    Ok(pb::MatrixQuality {
        entries: stats.entries as u64,
        supplied_edges: (stats.exact + stats.cached) as u64,
        estimated_edges: (stats.model + stats.fallback) as u64,
        unreachable_edges: stats.unreachable as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_defaults_and_explicit_resources_produce_bounded_csp() {
        let default = normalize(pb::MapConfig::default()).unwrap();
        assert_eq!(default.resource_origins, ["https://tile.openstreetmap.org"]);
        assert_eq!(default.max_zoom, 19);
        let custom = normalize(pb::MapConfig {
            style_url: "https://maps.example.org/style.json".into(),
            resource_origins: vec!["https://archive.example.org".into()],
            ..Default::default()
        })
        .unwrap();
        assert_eq!(custom.resource_origins.len(), 2);
        let policy = csp(&custom);
        assert!(
            policy.contains(
                "connect-src 'self' https://archive.example.org https://maps.example.org"
            )
        );
        assert!(!policy.contains("unsafe-eval"));
        let disabled = normalize(pb::MapConfig {
            disabled: true,
            ..Default::default()
        })
        .unwrap();
        assert!(disabled.raster_url.is_empty());
    }

    #[test]
    fn invalid_map_origins_cannot_expand_csp() {
        for url in [
            "https://*.example.org",
            "https://user:password@example.org",
            "javascript:alert(1)",
            "https://example.org; script-src *",
            "https://example.org/path",
        ] {
            assert!(
                normalize(pb::MapConfig {
                    resource_origins: vec![url.into()],
                    ..Default::default()
                })
                .is_err(),
                "{url}"
            );
        }
    }
}
