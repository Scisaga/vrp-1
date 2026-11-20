//! Geofabrik catalog planning. No HTTP is performed here: callers supply HEAD
//! sizes and verified cache eligibility for their chosen freshness policy.
use crate::geometry::unwrap;
use crate::{Bounds, Error, NoopObserver, Observer, Result, checkpoint};
use geo::{Area, BooleanOps, Coord, Intersects, LineString, MultiPolygon, Point, Polygon};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub parent: Option<String>,
    pub name: String,
    pub pbf_url: String,
    pub coverage: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub entries: Vec<CatalogEntry>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlannedPackage {
    pub region_id: String,
    pub url: String,
    pub download_bytes: u64,
    pub reused: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SelectionPlan {
    pub packages: Vec<PlannedPackage>,
    pub download_bytes: u64,
    pub coverage_complete: bool,
    pub uncovered_fraction: f64,
}

/// Test source coverage, including polygon holes and antimeridian unwrapping.
/// Boundary points are included; a bounding rectangle is not substituted for
/// the actual source polygon.
pub fn coverage_contains_point(coverage: &Value, point: crate::GeoPoint) -> Result<bool> {
    point.validate()?;
    Ok(geometry(coverage, point.lng)?.intersects(&Point::new(point.lng, point.lat)))
}
impl CatalogEntry {
    pub fn covers_point(&self, point: crate::GeoPoint) -> Result<bool> {
        coverage_contains_point(&self.coverage, point)
    }
}

impl Catalog {
    pub fn covers_point(&self, point: crate::GeoPoint) -> Result<bool> {
        for entry in &self.entries {
            if entry.covers_point(point)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub fn parse(json: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(json)?;
        if value["type"] != "FeatureCollection" {
            return Err(Error("catalog must be a GeoJSON FeatureCollection".into()));
        }
        let features = value["features"]
            .as_array()
            .ok_or_else(|| Error("catalog features missing".into()))?;
        let mut entries = BTreeMap::new();
        for feature in features {
            let p = &feature["properties"];
            let Some(url) = p["urls"]["pbf"].as_str() else {
                continue;
            };
            if !url.starts_with("https://") && !url.starts_with("http://") {
                return Err(Error("catalog PBF URL must use HTTP(S)".into()));
            }
            let id = p["id"]
                .as_str()
                .ok_or_else(|| Error("catalog region ID missing".into()))?
                .to_owned();
            geometry(&feature["geometry"], 0.0)?;
            let entry = CatalogEntry {
                id: id.clone(),
                parent: p["parent"].as_str().map(str::to_owned),
                name: p["name"].as_str().unwrap_or(&id).to_owned(),
                pbf_url: url.to_owned(),
                coverage: feature["geometry"].clone(),
            };
            if entries.insert(id, entry).is_some() {
                return Err(Error("duplicate catalog region ID".into()));
            }
        }
        Ok(Self {
            entries: entries.into_values().collect(),
        })
    }
    pub fn candidates(&self, bounds: Bounds) -> Vec<&CatalogEntry> {
        if bounds.validate().is_err() {
            return vec![];
        }
        let required = required(bounds);
        let center = bounds.center_longitude();
        self.entries
            .iter()
            .filter(|entry| {
                geometry(&entry.coverage, center)
                    .is_ok_and(|g| g.intersection(&required).unsigned_area() > 0.0)
            })
            .collect()
    }
    /// Deterministic weighted greedy set cover with cheaper parent replacement
    /// and redundant-package removal. This is deliberately not an optimality claim.
    /// Unknown uncached byte sizes are ineligible until callers obtain HEAD data.
    pub fn plan(
        &self,
        bounds: Bounds,
        sizes: &BTreeMap<String, u64>,
        cached: &BTreeSet<String>,
    ) -> Result<SelectionPlan> {
        self.plan_with_budget(bounds, sizes, cached, u64::MAX)
    }
    pub fn plan_with_budget(
        &self,
        bounds: Bounds,
        sizes: &BTreeMap<String, u64>,
        cached: &BTreeSet<String>,
        budget: u64,
    ) -> Result<SelectionPlan> {
        self.plan_observed(bounds, sizes, cached, budget, &mut NoopObserver)
    }
    pub fn plan_observed(
        &self,
        bounds: Bounds,
        sizes: &BTreeMap<String, u64>,
        cached: &BTreeSet<String>,
        budget: u64,
        observer: &mut dyn Observer,
    ) -> Result<SelectionPlan> {
        bounds.validate()?;
        let required = required(bounds);
        let total = required.unsigned_area();
        let epsilon = (total * 1e-10).max(1e-14);
        let center = bounds.center_longitude();
        let mut candidates = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            if index.is_multiple_of(32) {
                checkpoint(
                    observer,
                    "catalog_candidates",
                    index as u64,
                    Some(self.entries.len() as u64),
                )?;
            }
            let cost = if cached.contains(&entry.id) {
                0
            } else if let Some(size) = sizes.get(&entry.id) {
                *size
            } else {
                continue;
            };
            // Keep the original polygon for subtraction. Clipping it to the
            // requested box first quantizes coincident box edges twice and can
            // manufacture microscopic uncovered slivers even for full cover.
            let shape = geometry(&entry.coverage, center)?;
            if shape.intersection(&required).unsigned_area() > epsilon {
                candidates.push((entry, cost, shape));
            }
        }
        let mut uncovered = required.clone();
        let mut selected = BTreeSet::new();
        let mut spent = 0_u64;
        loop {
            checkpoint(
                observer,
                "catalog_select",
                selected.len() as u64,
                Some(candidates.len() as u64),
            )?;
            if uncovered.unsigned_area() <= epsilon {
                break;
            }
            let mut best: Option<(usize, f64)> = None;
            for (i, (_, cost, shape)) in candidates.iter().enumerate() {
                if selected.contains(&i) || *cost > budget - spent {
                    continue;
                }
                let gain = shape.intersection(&uncovered).unsigned_area();
                if gain <= epsilon {
                    continue;
                }
                let wins = match best {
                    None => true,
                    Some((old, old_gain)) => {
                        let old_cost = candidates[old].1;
                        if *cost == 0 || old_cost == 0 {
                            (*cost == 0 && old_cost > 0)
                                || (*cost == old_cost
                                    && (gain > old_gain
                                        || (gain == old_gain
                                            && candidates[i].0.id < candidates[old].0.id)))
                        } else {
                            let score = gain / (*cost as f64);
                            let old_score = old_gain / (old_cost as f64);
                            score > old_score
                                || (score == old_score
                                    && (*cost, &candidates[i].0.id)
                                        < (old_cost, &candidates[old].0.id))
                        }
                    }
                };
                if wins {
                    best = Some((i, gain));
                }
            }
            let Some((index, _)) = best else {
                break;
            };
            selected.insert(index);
            spent += candidates[index].1;
            uncovered = uncovered.difference(&candidates[index].2);
        }
        // Actual geometric containment, rather than names or hierarchy alone,
        // also handles overlapping special extracts (e.g. capital regions).
        let mut changed = true;
        while changed {
            changed = false;
            for parent in 0..candidates.len() {
                if parent.is_multiple_of(32) {
                    checkpoint(
                        observer,
                        "catalog_replace",
                        parent as u64,
                        Some(candidates.len() as u64),
                    )?;
                }
                if selected.contains(&parent) {
                    continue;
                }
                let covered: Vec<_> = selected
                    .iter()
                    .copied()
                    .filter(|i| {
                        candidates[*i]
                            .2
                            .intersection(&required)
                            .difference(&candidates[parent].2)
                            .unsigned_area()
                            <= epsilon
                    })
                    .collect();
                let cost = covered
                    .iter()
                    .try_fold(0_u64, |n, i| n.checked_add(candidates[*i].1))
                    .ok_or_else(|| Error("catalog byte total overflow".into()))?;
                if !covered.is_empty() && candidates[parent].1 < cost {
                    for index in covered {
                        selected.remove(&index);
                    }
                    selected.insert(parent);
                    changed = true;
                    break;
                }
            }
        }
        // Remove redundant new downloads first; stable IDs make ties reproducible.
        let mut order: Vec<_> = selected.iter().copied().collect();
        order.sort_by(|a, b| {
            candidates[*b]
                .1
                .cmp(&candidates[*a].1)
                .then(candidates[*a].0.id.cmp(&candidates[*b].0.id))
        });
        for index in order {
            let mut others = MultiPolygon(vec![]);
            for i in &selected {
                if *i != index {
                    others = others.union(&candidates[*i].2);
                }
            }
            if candidates[index]
                .2
                .intersection(&required)
                .difference(&others)
                .unsigned_area()
                <= epsilon
            {
                selected.remove(&index);
            }
        }
        let mut missing = required;
        let mut packages = Vec::new();
        let mut download_bytes = 0_u64;
        for index in selected {
            let (entry, cost, shape) = &candidates[index];
            missing = missing.difference(shape);
            download_bytes = download_bytes
                .checked_add(*cost)
                .ok_or_else(|| Error("catalog byte total overflow".into()))?;
            packages.push(PlannedPackage {
                region_id: entry.id.clone(),
                url: entry.pbf_url.clone(),
                download_bytes: *cost,
                reused: cached.contains(&entry.id),
            });
        }
        packages.sort_by(|a, b| a.region_id.cmp(&b.region_id));
        let remaining = missing.unsigned_area();
        Ok(SelectionPlan {
            packages,
            download_bytes,
            coverage_complete: remaining <= epsilon,
            uncovered_fraction: (remaining / total).clamp(0.0, 1.0),
        })
    }
}

fn required(bounds: Bounds) -> MultiPolygon<f64> {
    let west = unwrap(bounds.west, bounds.center_longitude());
    let width = bounds.width().max(1e-7);
    let north = if bounds.north - bounds.south < 1e-7 {
        (bounds.south + 1e-7).min(90.0)
    } else {
        bounds.north
    };
    let south = if north == bounds.south {
        bounds.south - 1e-7
    } else {
        bounds.south
    };
    MultiPolygon(vec![Polygon::new(
        LineString::from(vec![
            (west, south),
            (west + width, south),
            (west + width, north),
            (west, north),
            (west, south),
        ]),
        vec![],
    )])
}
fn geometry(value: &Value, center: f64) -> Result<MultiPolygon<f64>> {
    let kind = value["type"]
        .as_str()
        .ok_or_else(|| Error("catalog geometry type missing".into()))?;
    let coordinates = value["coordinates"]
        .as_array()
        .ok_or_else(|| Error("catalog polygon coordinates missing".into()))?;
    match kind {
        "Polygon" => Ok(MultiPolygon(vec![polygon(coordinates, center)?])),
        "MultiPolygon" => coordinates
            .iter()
            .map(|p| {
                polygon(
                    p.as_array()
                        .ok_or_else(|| Error("invalid polygon".into()))?,
                    center,
                )
            })
            .collect::<Result<Vec<_>>>()
            .map(MultiPolygon),
        _ => Err(Error(
            "catalog coverage must be Polygon or MultiPolygon".into(),
        )),
    }
}
fn polygon(rings: &[Value], center: f64) -> Result<Polygon<f64>> {
    let mut parsed = Vec::new();
    for ring in rings {
        let coordinates = ring
            .as_array()
            .ok_or_else(|| Error("invalid polygon ring".into()))?;
        if coordinates.len() < 4 {
            return Err(Error("polygon ring has fewer than four coordinates".into()));
        }
        let mut points: Vec<Coord<f64>> = Vec::new();
        for coordinate in coordinates {
            let c = coordinate
                .as_array()
                .ok_or_else(|| Error("invalid polygon coordinate".into()))?;
            let lng = c
                .first()
                .and_then(Value::as_f64)
                .ok_or_else(|| Error("polygon longitude missing".into()))?;
            let lat = c
                .get(1)
                .and_then(Value::as_f64)
                .ok_or_else(|| Error("polygon latitude missing".into()))?;
            crate::GeoPoint { lng, lat }.validate()?;
            let reference = points.last().map_or(center, |c| c.x);
            points.push(Coord {
                x: unwrap(lng, reference),
                y: lat,
            });
        }
        let mean = points.iter().map(|c| c.x).sum::<f64>() / points.len() as f64;
        let shift = ((center - mean) / 360.0).round() * 360.0;
        for p in &mut points {
            p.x += shift;
        }
        parsed.push(LineString(points));
    }
    if parsed.is_empty() {
        return Err(Error("polygon has no rings".into()));
    }
    let outer = parsed.remove(0);
    Ok(Polygon::new(outer, parsed))
}
