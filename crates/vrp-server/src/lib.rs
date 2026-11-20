//! Standalone application host. All I/O stays outside vrp-core/vrp-model.
mod error;
pub mod http;
mod maps;
mod operations;
mod runtime;
mod store;
mod trend;
pub use error::{AppError, Result};
pub use runtime::tasks::GeoConfig;
pub use runtime::tasks::basemaps::BasemapConfig;
pub use runtime::{Config, Runtime, Service};

pub fn serve_geo_child(limit: u32) -> std::io::Result<()> {
    vrp_worker::serve_process_child::<
        vrp_proto::worker::v1::GeoRunRequest,
        vrp_proto::worker::v1::GeoRunResponse,
    >(limit, runtime::tasks::workflow::execute_geo)
}
pub fn serve_basemap_child(limit: u32) -> std::io::Result<()> {
    vrp_worker::serve_process_child::<
        vrp_proto::worker::v1::BasemapRunRequest,
        vrp_proto::worker::v1::BasemapRunResponse,
    >(limit, runtime::tasks::basemaps::execute)
}

use std::time::{SystemTime, UNIX_EPOCH};
use vrp_proto::app::v1 as pb;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

pub fn terminal(job: &pb::Job) -> bool {
    matches!(
        job.state(),
        pb::JobState::Succeeded
            | pb::JobState::Cancelled
            | pb::JobState::Failed
            | pb::JobState::Interrupted
    )
}

fn job_id(value: &str) -> Result<i64> {
    let id = value
        .parse::<i64>()
        .map_err(|_| AppError::invalid("invalid job ID"))?;
    if id <= 0 || id.to_string() != value {
        return Err(AppError::invalid("invalid job ID"));
    }
    Ok(id)
}

fn scenario_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(AppError::invalid(
            "scenario ID must be 1..64 ASCII letters, digits, _ or -",
        ));
    }
    Ok(())
}

fn page_size(size: u32) -> Result<usize> {
    match size {
        0 => Ok(50),
        1..=100 => Ok(size as usize),
        _ => Err(AppError::invalid("page_size must be 1..100")),
    }
}
