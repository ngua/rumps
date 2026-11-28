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
pub(crate) mod wire;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use config::Config;
use error::{Error, Result};
use rumps_storage::Database;
use tokio::net::TcpListener;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: Database,
    pub(crate) config: Config,
    pub(crate) sessions: Arc<session::Manager>,
}

impl AppState {
    fn into_router(self) -> axum::Router {
        router::Router::build(self)
    }
}

/// Builder for constructing and launching a RUMPS server.
pub struct ServerBuilder {
    config: Config,
    no_auth: bool,
}

impl ServerBuilder {
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

        let idle_timeout =
            Duration::from_secs(self.config.session.idle_timeout_secs);
        let _reaper =
            session::Manager::spawn_reaper(Arc::clone(&sessions), idle_timeout);

        let addr = SocketAddr::new(
            self.config.server.bind.parse()?,
            self.config.server.port,
        );

        let state = AppState {
            db,
            config: self.config,
            sessions,
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
