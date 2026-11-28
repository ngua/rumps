//! Transaction storage and lifecycle management.

use std::sync::Arc;
use std::time::Duration;

use dashmap::mapref::one::RefMut;
use dashmap::DashMap;
use futures::future;
use rumps_storage::Transaction;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::error::{Error, Result};
use crate::{response, session, Id};

/// A stored transaction entry with its owning session and last-use timestamp.
pub(crate) struct Entry {
    pub(crate) session: Id<response::Session>,
    pub(crate) txn: Transaction,
    pub(crate) last_use: Instant,
}

/// Concurrent store of active transactions keyed by their external token.
pub(crate) struct Store {
    txns: DashMap<Id<response::Txn>, Entry>,
}

impl Store {
    pub(crate) fn new() -> Self {
        Self {
            txns: DashMap::new(),
        }
    }

    /// Inserts a new transaction and returns its external token.
    pub(crate) fn insert(
        &self,
        session: Id<response::Session>,
        txn: Transaction,
    ) -> Id<response::Txn> {
        let id = Id::new();
        self.txns.insert(
            id,
            Entry {
                session,
                txn,
                last_use: Instant::now(),
            },
        );
        id
    }

    /// Looks up a transaction, verifying session ownership.
    ///
    /// Returns a mutable ref to the entry with its last-use timestamp updated.
    pub(crate) fn get(
        &self,
        id: &Id<response::Txn>,
        session: Id<response::Session>,
    ) -> Result<RefMut<'_, Id<response::Txn>, Entry>> {
        let mut entry = self.txns.get_mut(id).ok_or(Error::TxnNotFound)?;
        if entry.session == session {
            entry.last_use = Instant::now();
            Ok(entry)
        } else {
            Err(Error::TxnNotFound)
        }
    }

    /// Removes a transaction, verifying session ownership.
    ///
    /// Returns the owned [`Entry`] so the caller can commit/rollback without
    /// holding a `DashMap` ref across an await.
    pub(crate) fn remove(
        &self,
        id: &Id<response::Txn>,
        session: Id<response::Session>,
    ) -> Result<Entry> {
        self.txns
            .remove_if(id, |_, e| e.session == session)
            .map(|(_, entry)| entry)
            .ok_or(Error::TxnNotFound)
    }

    /// Removes a transaction unconditionally (for reaping).
    pub(crate) fn remove_unchecked(
        &self,
        id: &Id<response::Txn>,
    ) -> Option<Entry> {
        self.txns.remove(id).map(|(_, e)| e)
    }

    /// Returns an iterator over all entries for reaping.
    pub(crate) fn iter(
        &self,
    ) -> dashmap::iter::Iter<'_, Id<response::Txn>, Entry> {
        self.txns.iter()
    }

    /// Returns the number of active transactions.
    pub(crate) fn len(&self) -> usize {
        self.txns.len()
    }

    /// Spawns a background task that periodically reaps idle transactions.
    pub(crate) fn spawn_reaper(
        store: Arc<Self>,
        sessions: Arc<session::Manager>,
        timeout: Duration,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(timeout / 10);
            loop {
                tick.tick().await;

                let stale: Vec<(Id<response::Txn>, Id<response::Session>)> =
                    store
                        .iter()
                        .filter(|e| e.value().last_use.elapsed() > timeout)
                        .map(|e| (*e.key(), e.value().session))
                        .collect();

                future::join_all(stale.into_iter().map(|(tid, sid)| {
                    let store = &store;
                    let sessions = &sessions;
                    async move {
                        if let Some(entry) = store.remove_unchecked(&tid) {
                            if let Some(mut sess) = sessions.get(&sid) {
                                sess.txns.remove(&tid);
                            }
                            if let Err(e) = entry.txn.rollback().await {
                                tracing::warn!(
                                    txn = %tid,
                                    "rollback on txn reap failed: {e}"
                                );
                            }
                            tracing::info!(txn = %tid, "reaped idle transaction");
                        }
                    }
                }))
                .await;
            }
        })
    }
}
