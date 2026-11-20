//! Offline historical replay export. The running server never reads v1 JSON.
use super::*;
use std::f64::consts::PI;
use vrp_proto::app::v1 as app;

/// Export a native SaveScenarioRequest with explicit coordinate provenance.
pub fn export(
    value: &Value,
    id: &str,
    source_name: &str,
    sha: &str,
    crs: Option<&str>,
) -> Result<app::SaveScenarioRequest, ImportError> {
    let mut imported = import(value)?;
    let plan = find_plan(value).unwrap();
    let scenario = value.get("scenario").unwrap_or(value);
    let provider = scenario.get("map_provider").and_then(Value::as_str);
    let source_crs = crs.unwrap_or(if provider == Some("HERE") {
        "WGS84"
    } else {
        "GCJ-02"
    });
    if !matches!(source_crs, "WGS84" | "GCJ-02") {
        return Err(ImportError("source CRS must be WGS84 or GCJ-02".into()));
    }
    if imported
        .warnings
        .iter()
        .any(|w| w.contains("no usable coordinates") || w.contains("skipped"))
    {
        return Err(ImportError(
            "missing coordinates or duplicate/missing entity IDs; see historical import audit"
                .into(),
        ));
    }
    let p = imported.request.problem.as_mut().unwrap();
    if p.tickets.len() > 1000 || p.agents.len() > 200 {
        return Err(ImportError(
            "outside target: at most 1000 tickets and 200 agents".into(),
        ));
    }
    for location in &mut p.locations {
        if source_crs == "GCJ-02" {
            (location.lng, location.lat) = gcj_to_wgs(location.lng, location.lat);
        }
    }
    // v1 Long.MAX_VALUE remains unknown per the existing importer contract.
    // Preserve only explicitly declared unreachable cells in replay exports.
    if let Some(data) = plan
        .get("matrix")
        .and_then(|m| m.get("data"))
        .and_then(Value::as_object)
    {
        let n = p.locations.len();
        for (i, from) in p.locations.iter().enumerate() {
            for (j, to) in p.locations.iter().enumerate() {
                let cell = data.get(&from.id).and_then(|r| r.get(&to.id));
                if cell.is_some_and(|c| {
                    c.get("unreachable").and_then(Value::as_bool) == Some(true)
                        || (c.get("duration").and_then(Value::as_u64) == Some(u32::MAX as u64)
                            && c.get("distance").and_then(Value::as_u64) == Some(u32::MAX as u64))
                }) {
                    let k = (i * n + j) * 4;
                    p.matrices[0].durations[k..k + 4].copy_from_slice(&u32::MAX.to_le_bytes());
                    p.matrices[0].distances[k..k + 4].copy_from_slice(&u32::MAX.to_le_bytes());
                }
            }
        }
    }
    let mut display = app::ScenarioDisplay::default();
    for (key, destination) in [
        ("agents", &mut display.agents),
        ("tickets", &mut display.tickets),
        ("pois", &mut display.locations),
    ] {
        for item in array_of(plan, key).iter().filter_map(Value::as_object) {
            if let Some(id) = item.get("id").and_then(id_of) {
                destination.insert(
                    id.clone(),
                    app::ObjectLabel {
                        name: str_of(item, "name")
                            .or_else(|| str_of(item, "title"))
                            .unwrap_or(&id)
                            .into(),
                        address: str_of(item, "address").unwrap_or_default().into(),
                        note: str_of(item, "desc").unwrap_or_default().into(),
                    },
                );
            }
        }
    }
    // Normalize legacy solve-time settings to the application's default preset.
    imported.request.options = Some(pb::SolveOptions {
        threads: 1,
        ..Default::default()
    });
    vrp_model::materialize_matrices(&mut imported.request).map_err(|e| ImportError(e.0))?;
    let name = value
        .get("scenario_name")
        .and_then(Value::as_str)
        .or_else(|| scenario.get("name").and_then(Value::as_str))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(source_name);
    Ok(app::SaveScenarioRequest {
        scenario_id: id.into(),
        name: name.into(),
        expected_version: 0,
        request: Some(imported.request),
        display: Some(display),
        origin: Some(app::ScenarioOrigin {
            source_sha256: sha.into(),
            source_name: source_name.into(),
            source_crs: source_crs.into(),
            coordinate_note: if source_crs == "GCJ-02" {
                "按旧高德来源转换至 WGS84；保留原件，可按场景覆盖来源假设".into()
            } else {
                "来源声明 WGS84，未做偏移转换".into()
            },
            warnings: imported.warnings,
            historical_values_unverified: true,
        }),
    })
}

// Inverse of the conventional GCJ offset, iterated to sub-centimetre numeric
// convergence. This is coordinate alignment, not a road matching service.
pub fn gcj_to_wgs(lng: f64, lat: f64) -> (f64, f64) {
    if !(72.004..=137.8347).contains(&lng) || !(0.8293..=55.8271).contains(&lat) {
        return (lng, lat);
    }
    let (mut x, mut y) = (lng, lat);
    for _ in 0..12 {
        let (gx, gy) = wgs_to_gcj(x, y);
        let (dx, dy) = (gx - lng, gy - lat);
        x -= dx;
        y -= dy;
        if dx.abs().max(dy.abs()) < 1e-10 {
            break;
        }
    }
    (x, y)
}
fn wgs_to_gcj(lng: f64, lat: f64) -> (f64, f64) {
    let (x, y) = (lng - 105.0, lat - 35.0);
    let common = (20.0 * (6.0 * x * PI).sin() + 20.0 * (2.0 * x * PI).sin()) * 2.0 / 3.0;
    let mut dy =
        -100.0 + 2.0 * x + 3.0 * y + 0.2 * y * y + 0.1 * x * y + 0.2 * x.abs().sqrt() + common;
    dy += (20.0 * (y * PI).sin() + 40.0 * (y * PI / 3.0).sin()) * 2.0 / 3.0;
    dy += (160.0 * (y * PI / 12.0).sin() + 320.0 * (y * PI / 30.0).sin()) * 2.0 / 3.0;
    let mut dx = 300.0 + x + 2.0 * y + 0.1 * x * x + 0.1 * x * y + 0.1 * x.abs().sqrt() + common;
    dx += (20.0 * (x * PI).sin() + 40.0 * (x * PI / 3.0).sin()) * 2.0 / 3.0;
    dx += (150.0 * (x * PI / 12.0).sin() + 300.0 * (x * PI / 30.0).sin()) * 2.0 / 3.0;
    let rad = lat * PI / 180.0;
    let magic = 1.0 - 0.006_693_421_622_965_943 * rad.sin().powi(2);
    let root = magic.sqrt();
    dy = dy * 180.0 / ((6378245.0 * (1.0 - 0.006_693_421_622_965_943) / (magic * root)) * PI);
    dx = dx * 180.0 / (6378245.0 / root * rad.cos() * PI);
    (lng + dx, lat + dy)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn known_coordinate_and_overseas() {
        let (x, y) = gcj_to_wgs(116.41024449916938, 39.91640428150164);
        assert!((x - 116.404).abs() < 1e-7 && (y - 39.915).abs() < 1e-7);
        assert_eq!(gcj_to_wgs(174.76, -36.85), (174.76, -36.85));
    }
}
