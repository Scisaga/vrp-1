//! Local OSM data preparation. This crate never contacts a map provider.
//!
//! Travel estimates are models, not observations. Missing paths are `None`,
//! allowing the application to apply its declared geometric fallback.
pub mod catalog;
mod geometry;
mod network;
mod pbf;

pub use geometry::{Bounds, GeoPoint, distance_m};
pub use network::{
    Config, Matrix, QueryGraph, RoadNetwork, Route, Snap, Source, TravelEstimate, Way, WayNode,
};
pub use pbf::{PbfInfo, inspect_pbf, validate_pbf};

use serde::{Deserialize, Serialize};
use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Error(pub String);
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self(e.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self(e.to_string())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Progress {
    pub phase: String,
    pub completed: u64,
    pub total: Option<u64>,
}

pub trait Observer {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn progress(&mut self, _progress: Progress) {}
}

pub struct NoopObserver;
impl Observer for NoopObserver {}

pub(crate) fn checkpoint(
    observer: &mut dyn Observer,
    phase: &str,
    completed: u64,
    total: Option<u64>,
) -> Result<()> {
    if observer.is_cancelled() {
        return Err(Error("cancelled".into()));
    }
    observer.progress(Progress {
        phase: phase.into(),
        completed,
        total,
    });
    Ok(())
}

pub(crate) struct TrackedIo<'a, T> {
    pub inner: T,
    observer: &'a mut dyn Observer,
    phase: &'a str,
    completed: u64,
    last_report: u64,
    calls: u64,
    total: Option<u64>,
}
impl<'a, T> TrackedIo<'a, T> {
    pub fn new(
        inner: T,
        observer: &'a mut dyn Observer,
        phase: &'a str,
        total: Option<u64>,
    ) -> Self {
        Self {
            inner,
            observer,
            phase,
            completed: 0,
            last_report: 0,
            calls: 0,
            total,
        }
    }
    fn tick(&mut self) -> std::io::Result<()> {
        self.calls += 1;
        if self.completed - self.last_report >= 256 * 1024 || self.calls.is_multiple_of(1024) {
            checkpoint(self.observer, self.phase, self.completed, self.total)
                .map_err(|e| std::io::Error::other(e.0))?;
            self.last_report = self.completed;
        } else if self.observer.is_cancelled() {
            return Err(std::io::Error::other("cancelled"));
        }
        Ok(())
    }
}
impl<T: std::io::Write> std::io::Write for TrackedIo<'_, T> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.tick()?;
        let count = self.inner.write(bytes)?;
        self.completed += count as u64;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.tick()?;
        self.inner.flush()
    }
}
impl<T: std::io::Read> std::io::Read for TrackedIo<'_, T> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.tick()?;
        let count = self.inner.read(bytes)?;
        self.completed += count as u64;
        Ok(count)
    }
}
