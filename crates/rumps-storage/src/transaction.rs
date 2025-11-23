//! Transaction context for the RUMPS storage layer.
//!
//! This module provides the `TransactionContext` type which is used to pass
//! transaction information through BTree operations. This is currently a stub
//! implementation that will be expanded in Phase 5 when full transaction
//! support is added.

use std::time::Instant;

use rumps_types::{IsolationLevel, TransactionId, TransactionTimestamp};

/// A transaction context that tracks the current transaction state.
///
/// This is passed to BTree operations to enforce transaction semantics:
/// - Write operations (SET, KILL) require a transaction context
/// - Read operations (GET, DATA, ORDER) can optionally use a transaction context
///
/// # Phase 5 Note
///
/// This is currently a minimal stub. When full transaction support is implemented,
/// this will include:
/// - Write buffers for uncommitted changes
/// - Snapshot isolation metadata
/// - Conflict detection state
/// - Links to the WAL (Write-Ahead Log)
#[derive(Debug, Clone)]
pub struct TransactionContext {
    /// Unique transaction identifier
    pub id: TransactionId,

    /// The timestamp at which this transaction started (for snapshot isolation)
    pub start_timestamp: TransactionTimestamp,

    /// Wall-clock time when the transaction began
    pub start_time: Instant,

    /// Isolation level for this transaction
    pub isolation_level: IsolationLevel,
}

impl TransactionContext {
    /// Creates a new transaction context with the given ID and timestamp.
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_storage::TransactionContext;
    /// use rumps_types::{TransactionId, TransactionTimestamp};
    ///
    /// let txn = TransactionContext::new(
    ///     TransactionId::from(1),
    ///     TransactionTimestamp::from(100),
    /// );
    ///
    /// assert_eq!(rumps_types::TransactionId::from(1), txn.id);
    /// ```
    pub fn new(id: TransactionId, start_timestamp: TransactionTimestamp) -> Self {
        Self {
            id,
            start_timestamp,
            start_time: Instant::now(),
            isolation_level: IsolationLevel::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transaction_context_creation() {
        let id = TransactionId::from(1);
        let ts = TransactionTimestamp::from(100);
        let ctx = TransactionContext::new(id, ts);

        assert_eq!(ctx.id, id);
        assert_eq!(ctx.start_timestamp, ts);
        assert_eq!(ctx.isolation_level, IsolationLevel::SnapshotIsolation);
    }
}
