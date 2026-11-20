use std::fmt;
use vrp_proto::app::v1 as pb;

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Clone, Debug)]
pub struct AppError {
    pub code: &'static str,
    pub message: String,
}
impl AppError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new("conflict", message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("internal", message)
    }
    pub fn envelope(&self) -> pb::Error {
        pb::Error {
            code: self.code.into(),
            message: self.message.clone(),
        }
    }
}
impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for AppError {}
impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        eprintln!("SQLite error: {e}");
        Self::internal("task storage unavailable")
    }
}
impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self::internal(format!("local I/O: {e}"))
    }
}
impl From<prost::DecodeError> for AppError {
    fn from(_: prost::DecodeError) -> Self {
        Self::internal("stored protobuf is invalid")
    }
}
