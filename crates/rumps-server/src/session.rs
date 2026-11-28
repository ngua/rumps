//! Session state and lifecycle management.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use dashmap::mapref::one::RefMut;
use dashmap::DashMap;
use futures::future;
use rumps_storage::Database;
use rumps_types::{key, Name};
use smol_str::SmolStr;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::error::{Error, Result};
use crate::{response, AppState, Id};

/// A single client session.
pub(crate) struct Session {
    /// Client-facing session token.
    pub(crate) id: Id<response::Session>,
    /// Monotonic internal ID used as a key prefix for local variable isolation.
    pub(crate) internal_id: u64,
    /// Last time this session was used; reset on every request.
    pub(crate) last_activity: Instant,
    /// Transaction tokens owned by this session.
    pub(crate) txns: HashSet<Id<response::Txn>>,
    /// Local variable names this session has written to.
    pub(crate) locals: HashSet<SmolStr>,
}

impl Session {
    pub(crate) fn new(id: Id<response::Session>, internal_id: u64) -> Self {
        Self {
            id,
            internal_id,
            last_activity: Instant::now(),
            txns: HashSet::new(),
            locals: HashSet::new(),
        }
    }

    /// Updates the last activity timestamp to now.
    pub(crate) fn touch(&mut self) {
        self.last_activity = Instant::now();
    }
}

/// Validated session extracted from the `X-Session-Id` header.
pub(crate) struct ActiveSession {
    pub(crate) id: Id<response::Session>,
    pub(crate) internal_id: u64,
}

/// Axum extractor that optionally resolves a session from the `X-Session-Id`
/// header. Always succeeds; stores `None` when the header is absent or the
/// session is not found. Only `require()` enforces session presence.
pub(crate) struct Gate(pub(crate) Option<ActiveSession>);

impl Gate {
    /// Requires that a valid session was resolved. Returns `SessionRequired`
    /// (`401`) if not.
    pub(crate) fn require(self) -> Result<ActiveSession> {
        match self.0 {
            Some(s) => Ok(s),
            None => Err(Error::SessionRequired),
        }
    }
}

impl FromRequestParts<AppState> for Gate {
    type Rejection = Error;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self> {
        let active = parts
            .headers
            .get("x-session-id")
            .and_then(|hdr| hdr.to_str().ok())
            .and_then(|raw| raw.parse::<Id<response::Session>>().ok())
            .and_then(|id| {
                let mut sess = state.sessions.get(&id)?;
                sess.touch();
                let a = ActiveSession {
                    id,
                    internal_id: sess.internal_id,
                };
                drop(sess);
                Some(a)
            });
        Ok(Self(active))
    }
}

/// Manages active sessions and owns the shared in-memory database for locals.
pub(crate) struct Manager {
    sessions: DashMap<Id<response::Session>, Session>,
    counter: AtomicU64,
    /// Shared in-memory database backing all sessions' local variables.
    pub(crate) locals_db: Database,
}

impl Manager {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            sessions: DashMap::new(),
            counter: AtomicU64::new(0),
            locals_db: Database::in_memory()?,
        })
    }

    /// Creates a new session and returns its external token.
    pub(crate) fn create(&self) -> Id<response::Session> {
        let id = Id::new();
        let internal = self.counter.fetch_add(1, Ordering::Relaxed);
        self.sessions.insert(id, Session::new(id, internal));
        id
    }

    /// Looks up a session by its external token.
    pub(crate) fn get(
        &self,
        id: &Id<response::Session>,
    ) -> Option<RefMut<'_, Id<response::Session>, Session>> {
        self.sessions.get_mut(id)
    }

    /// Destroys a session, killing all its local variable subtrees.
    ///
    /// Returns the set of transaction tokens owned by the session; the
    /// caller is responsible for rolling them back.
    pub(crate) async fn destroy(
        &self,
        id: &Id<response::Session>,
    ) -> HashSet<Id<response::Txn>> {
        match self.sessions.remove(id) {
            Some((_, session)) => {
                let Session {
                    internal_id,
                    locals,
                    txns,
                    ..
                } = session;

                future::join_all(locals.into_iter().map(|name| {
                    let db = &self.locals_db;
                    async move {
                        if let Err(e) = db
                            .kill(
                                &Name::local(&name),
                                // `Subscript::Number` is `f64`; precision is
                                // exact up to `2^53`, which is unreachable for
                                // a session counter. That would be ~9 quadrillion
                                // sessions, not a realistic concern
                                &key![internal_id as i64],
                            )
                            .await
                        {
                            tracing::warn!(
                                local = %name,
                                "failed to kill local: {e}"
                            );
                        }
                    }
                }))
                .await;

                txns
            }
            None => HashSet::new(),
        }
    }

    /// Spawns a background task that periodically reaps idle sessions.
    pub(crate) fn spawn_reaper(
        mgr: Arc<Self>,
        timeout: Duration,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(timeout / 10);
            loop {
                tick.tick().await;

                let stale: Vec<Id<response::Session>> = mgr
                    .sessions
                    .iter()
                    .filter(|e| e.value().last_activity.elapsed() > timeout)
                    .map(|e| *e.key())
                    .collect();

                future::join_all(stale.into_iter().map(|id| {
                    let mgr = &mgr;
                    async move {
                        let txns = mgr.destroy(&id).await;
                        if !txns.is_empty() {
                            // TODO(5.1, 5.6): roll back via `transaction::Store`;
                            // these are orphaned and will leak locks until then
                            todo!("roll back orphaned transactions");
                        }
                        tracing::info!(session = %id, "reaped idle session");
                    }
                }))
                .await;
            }
        })
    }
}
