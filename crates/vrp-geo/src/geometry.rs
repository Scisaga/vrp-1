use crate::{Error, Result};
use serde::{Deserialize, Serialize};

pub(crate) const EARTH: f64 = 6_371_008.8;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GeoPoint {
    pub lng: f64,
    pub lat: f64,
}

impl GeoPoint {
    pub fn validate(self) -> Result<()> {
        if !self.lng.is_finite()
            || !self.lat.is_finite()
            || !(-180.0..=180.0).contains(&self.lng)
            || !(-90.0..=90.0).contains(&self.lat)
        {
            return Err(Error(
                "coordinates must be finite WGS84 longitude/latitude".into(),
            ));
        }
        Ok(())
    }
    pub(crate) fn xyz(self) -> [f64; 3] {
        let (lng, lat) = (self.lng.to_radians(), self.lat.to_radians());
        [
            EARTH * lat.cos() * lng.cos(),
            EARTH * lat.cos() * lng.sin(),
            EARTH * lat.sin(),
        ]
    }
    pub(crate) fn from_xyz(p: [f64; 3]) -> Self {
        Self {
            lng: p[1].atan2(p[0]).to_degrees(),
            lat: p[2].atan2(p[0].hypot(p[1])).to_degrees(),
        }
    }
}

/// Bounding longitude interval; `west > east` crosses the antimeridian.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
}

impl Bounds {
    pub fn validate(self) -> Result<()> {
        GeoPoint {
            lng: self.west,
            lat: self.south,
        }
        .validate()?;
        GeoPoint {
            lng: self.east,
            lat: self.north,
        }
        .validate()?;
        if self.south > self.north {
            return Err(Error("south exceeds north".into()));
        }
        Ok(())
    }
    pub fn from_points(points: &[GeoPoint]) -> Result<Self> {
        if points.is_empty() {
            return Err(Error("cannot bound an empty location set".into()));
        }
        for p in points {
            p.validate()?;
        }
        let south = points.iter().map(|p| p.lat).fold(90.0, f64::min);
        let north = points.iter().map(|p| p.lat).fold(-90.0, f64::max);
        let mut longitudes: Vec<_> = points.iter().map(|p| p.lng.rem_euclid(360.0)).collect();
        longitudes.sort_by(f64::total_cmp);
        let (index, _) = (0..longitudes.len())
            .map(|i| {
                let next = if i + 1 == longitudes.len() {
                    longitudes[0] + 360.0
                } else {
                    longitudes[i + 1]
                };
                (i, next - longitudes[i])
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        let west = wrap(longitudes[(index + 1) % longitudes.len()]);
        let east = wrap(longitudes[index]);
        Ok(Self {
            south,
            west,
            north,
            east,
        })
    }
    pub fn expanded_km(self, km: f64) -> Result<Self> {
        self.validate()?;
        if !km.is_finite() || km < 0.0 {
            return Err(Error(
                "buffer kilometres must be finite and nonnegative".into(),
            ));
        }
        let delta = km * 1000.0 / EARTH;
        let south = (self.south - delta.to_degrees()).max(-90.0);
        let north = (self.north + delta.to_degrees()).min(90.0);
        let extreme = self.south.abs().max(self.north.abs()).to_radians();
        if south <= -90.0
            || north >= 90.0
            || delta >= std::f64::consts::PI
            || self.width() + 2.0 * delta.to_degrees() >= 360.0
        {
            return Ok(Self {
                south,
                north,
                west: -180.0,
                east: 180.0,
            });
        }
        let longitude = (delta.sin() / extreme.cos())
            .clamp(-1.0, 1.0)
            .asin()
            .abs()
            .to_degrees();
        if self.width() + 2.0 * longitude >= 360.0 {
            return Ok(Self {
                south,
                north,
                west: -180.0,
                east: 180.0,
            });
        }
        Ok(Self {
            south,
            north,
            west: wrap(self.west - longitude),
            east: wrap(self.east + longitude),
        })
    }
    pub fn contains(self, point: GeoPoint) -> bool {
        point.lat >= self.south
            && point.lat <= self.north
            && if self.west <= self.east {
                point.lng >= self.west && point.lng <= self.east
            } else {
                point.lng >= self.west || point.lng <= self.east
            }
    }
    pub fn width(self) -> f64 {
        if self.west <= self.east {
            self.east - self.west
        } else {
            self.east + 360.0 - self.west
        }
    }
    pub fn center_longitude(self) -> f64 {
        wrap(self.west + self.width() / 2.0)
    }
    pub fn split(self) -> Vec<Self> {
        if self.west <= self.east {
            vec![self]
        } else {
            vec![
                Self {
                    east: 180.0,
                    ..self
                },
                Self {
                    west: -180.0,
                    ..self
                },
            ]
        }
    }
    pub(crate) fn intersects_segment(self, a: GeoPoint, b: GeoPoint) -> bool {
        let center = self.center_longitude();
        let ax = unwrap(a.lng, center);
        let bx = ax + wrap(b.lng - a.lng);
        let west = unwrap(self.west, center);
        let east = west + self.width();
        let minx = ax.min(bx);
        let maxx = ax.max(bx);
        if maxx < west
            || minx > east
            || a.lat.max(b.lat) < self.south
            || a.lat.min(b.lat) > self.north
        {
            return false;
        }
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        for (p, q) in [
            (-(bx - ax), ax - west),
            (bx - ax, east - ax),
            (-(b.lat - a.lat), a.lat - self.south),
            (b.lat - a.lat, self.north - a.lat),
        ] {
            if p == 0.0 {
                if q < 0.0 {
                    return false;
                }
            } else if p < 0.0 {
                lo = lo.max(q / p);
            } else {
                hi = hi.min(q / p);
            }
        }
        lo <= hi
    }
}

pub(crate) fn wrap(lng: f64) -> f64 {
    (lng + 180.0).rem_euclid(360.0) - 180.0
}
pub(crate) fn unwrap(lng: f64, center: f64) -> f64 {
    center + wrap(lng - center)
}

pub fn distance_m(a: GeoPoint, b: GeoPoint) -> f64 {
    let (a1, b1) = (a.lat.to_radians(), b.lat.to_radians());
    let h = ((b1 - a1) / 2.0).sin().powi(2)
        + a1.cos() * b1.cos() * (wrap(b.lng - a.lng).to_radians() / 2.0).sin().powi(2);
    2.0 * EARTH * h.sqrt().min(1.0).asin()
}

/// Closest point on the shorter great-circle segment. Works across 180 degrees
/// and at high latitude; no Web Mercator cutoff or degree-as-metre metric.
pub(crate) fn project_segment(point: GeoPoint, a: GeoPoint, b: GeoPoint) -> (GeoPoint, f64, f64) {
    let av = unit(a.xyz());
    let bv = unit(b.xyz());
    let pv = unit(point.xyz());
    let arc_angle = angle(av, bv);
    if arc_angle < 1e-12 || arc_angle > std::f64::consts::PI - 1e-8 {
        let da = distance_m(point, a);
        let db = distance_m(point, b);
        return if da <= db { (a, 0.0, da) } else { (b, 1.0, db) };
    }
    let normal = unit(cross(av, bv));
    let dot = dot(pv, normal);
    let projected = unit([
        pv[0] - dot * normal[0],
        pv[1] - dot * normal[1],
        pv[2] - dot * normal[2],
    ]);
    for candidate in [projected, [-projected[0], -projected[1], -projected[2]]] {
        let part = angle(av, candidate);
        if (part + angle(candidate, bv) - arc_angle).abs() < 1e-8 {
            let snap = GeoPoint::from_xyz(candidate);
            return (
                snap,
                (part / arc_angle).clamp(0.0, 1.0),
                distance_m(point, snap),
            );
        }
    }
    let da = distance_m(point, a);
    let db = distance_m(point, b);
    if da <= db { (a, 0.0, da) } else { (b, 1.0, db) }
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn unit(a: [f64; 3]) -> [f64; 3] {
    let n = dot(a, a).sqrt();
    if n < 1e-20 {
        [0.0, 0.0, 0.0]
    } else {
        [a[0] / n, a[1] / n, a[2] / n]
    }
}
fn angle(a: [f64; 3], b: [f64; 3]) -> f64 {
    dot(cross(a, b), cross(a, b)).sqrt().atan2(dot(a, b))
}
