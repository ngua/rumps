//! Unified error type for the RUMPS server.

#![allow(missing_docs)]

use std::path::PathBuf;
use std::{io, net};

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;

/// Unified error type for the RUMPS server.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read config file `{0}`: {1}")]
    ConfigIo(PathBuf, io::Error),
    #[error("failed to parse config: {0}")]
    ConfigParse(#[from] toml::de::Error),
    #[error("auth must be configured or `--no-auth` must be passed")]
    NoAuth,
    #[error("{0}")]
    BadRequest(String),
    #[error("session token required")]
    SessionRequired,
    #[error("session expired or not found")]
    SessionExpired,
    #[error("{0}")]
    Storage(#[from] rumps_storage::Error),
    #[error("invalid bind address: {0}")]
    AddrParse(#[from] net::AddrParseError),
    #[error("{0}")]
    Io(#[from] io::Error),
}

impl IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        let (status, msg) = match &self {
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            Self::SessionRequired => {
                (StatusCode::UNAUTHORIZED, self.to_string())
            }
            Self::SessionExpired => (StatusCode::GONE, self.to_string()),
            Self::Storage(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, self.to_string())
            }
            _ => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
        };

        (status, Json(serde_json::json!({"error": msg}))).into_response()
    }
}

/// Convenience alias for `Result<T, Error>`.
pub type Result<T> = std::result::Result<T, Error>;
