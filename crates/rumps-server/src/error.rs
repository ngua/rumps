//! Unified error type for the RUMPS server.

#![allow(missing_docs)]

use std::path::PathBuf;
use std::{io, net};

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
    Storage(#[from] rumps_storage::Error),
    #[error("invalid bind address: {0}")]
    AddrParse(#[from] net::AddrParseError),
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// Convenience alias for `Result<T, Error>`.
pub type Result<T> = std::result::Result<T, Error>;
