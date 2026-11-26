//! TOML-based server configuration.

#![allow(missing_docs)]

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::Error;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: Server,
    #[serde(default)]
    pub database: Database,
    #[serde(default)]
    pub session: Session,
    #[serde(default)]
    pub transaction: Transaction,
    pub auth: Option<Auth>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Server {
    pub bind: String,
    pub port: u16,
    pub body_limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Database {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Session {
    pub idle_timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Transaction {
    pub idle_timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "method")]
pub enum Auth {
    #[serde(rename = "bearer")]
    Bearer { token: String },
}

impl Config {
    pub fn from_file(path: &Path) -> Result<Self, Error> {
        let contents = fs::read_to_string(path)
            .map_err(|e| Error::ConfigIo(path.to_path_buf(), e))?;
        toml::from_str(&contents).map_err(Error::ConfigParse)
    }
}

impl Default for Server {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0".into(),
            port: 7700,
            body_limit: None,
        }
    }
}

impl Default for Database {
    fn default() -> Self {
        Self {
            path: PathBuf::from("./data"),
        }
    }
}

impl Default for Session {
    fn default() -> Self {
        Self {
            idle_timeout_secs: 300,
        }
    }
}

impl Default for Transaction {
    fn default() -> Self {
        Self {
            idle_timeout_secs: 30,
        }
    }
}
