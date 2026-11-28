#![warn(missing_docs)]
#![cfg_attr(
    test,
    allow(clippy::approx_constant, clippy::expect_used, clippy::unwrap_used)
)]

//! HTTP RPC server for remote RUMPS database access.

pub mod config;
pub mod error;
pub(crate) mod response;
pub(crate) mod router;
pub(crate) mod rpc;
pub(crate) mod session;
pub(crate) mod transaction;
pub(crate) mod wire;

use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use std::{fmt, result};

use config::Config;
use error::{Error, Result};
use rumps_storage::Database;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use uuid::Uuid;

/// Phantom-typed wrapper around [`Uuid`] for domain-specific token safety.
pub(crate) struct Id<T>(Uuid, PhantomData<T>);

impl<T> Id<T> {
    pub(crate) fn new() -> Self {
        Self(Uuid::new_v4(), PhantomData)
    }
}

impl<T> Clone for Id<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Id<T> {}

impl<T> PartialEq for Id<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<T> Eq for Id<T> {}

impl<T> Hash for Id<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl<T> fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id({})", self.0)
    }
}

impl<T> fmt::Display for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<T> Serialize for Id<T> {
    fn serialize<S: serde::Serializer>(
        &self,
        ser: S,
    ) -> result::Result<S::Ok, S::Error> {
        self.0.serialize(ser)
    }
}

impl<'de, T> Deserialize<'de> for Id<T> {
    fn deserialize<D: serde::Deserializer<'de>>(
        de: D,
    ) -> result::Result<Self, D::Error> {
        Uuid::deserialize(de).map(Self::from)
    }
}

impl<T> From<Uuid> for Id<T> {
    fn from(id: Uuid) -> Self {
        Self(id, PhantomData)
    }
}

impl<T> FromStr for Id<T> {
    type Err = uuid::Error;

    fn from_str(s: &str) -> result::Result<Self, Self::Err> {
        s.parse::<Uuid>().map(Self::from)
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: Database,
    pub(crate) config: Config,
    pub(crate) sessions: Arc<session::Manager>,
    pub(crate) txns: Arc<transaction::Store>,
}

impl AppState {
    fn into_router(self) -> axum::Router {
        router::Router::build(self)
    }
}

/// RUMPS database server.
pub struct Server {
    config: Config,
    no_auth: bool,
}

impl Server {
    /// Creates a new builder from the given config.
    pub fn new(config: Config) -> Self {
        Self {
            config,
            no_auth: false,
        }
    }

    /// Disables authentication checks.
    pub fn no_auth(mut self) -> Self {
        self.no_auth = true;
        self
    }

    /// Builds and starts the server, returning when it shuts down.
    pub async fn run(self) -> Result<()> {
        if !self.no_auth && self.config.auth.is_none() {
            Err(Error::NoAuth)?;
        }

        if self.no_auth {
            eprintln!("WARNING: running without authentication (--no-auth)");
        }

        let db = Database::open(&self.config.database.path).await?;
        let sessions = Arc::new(session::Manager::new()?);
        let txns = Arc::new(transaction::Store::new());

        let session_timeout =
            Duration::from_secs(self.config.session.idle_timeout_secs);
        let _session_reaper = session::Manager::spawn_reaper(
            Arc::clone(&sessions),
            Arc::clone(&txns),
            session_timeout,
        );

        let txn_timeout =
            Duration::from_secs(self.config.transaction.idle_timeout_secs);
        let _txn_reaper = transaction::Store::spawn_reaper(
            Arc::clone(&txns),
            Arc::clone(&sessions),
            txn_timeout,
        );

        let addr = SocketAddr::new(
            self.config.server.bind.parse()?,
            self.config.server.port,
        );

        let state = AppState {
            db,
            config: self.config,
            sessions,
            txns,
        };

        let app = state.into_router();
        let listener = TcpListener::bind(addr).await?;
        tracing::info!("listening on {}", addr);

        axum::serve(listener, app)
            .with_graceful_shutdown(Self::shutdown_signal())
            .await?;

        Ok(())
    }

    async fn shutdown_signal() {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("shutdown signal received");
    }
}
