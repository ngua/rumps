//! Transaction-related types for the RUMPS database.
//!
//! # Overview
//!
//! RUMPS requires explicit transactions for all writes to persistent globals.
//! This module provides the fundamental types for managing transactions, including
//! unique identifiers, state tracking, and metadata for snapshot isolation.
//!
//! ## Transaction Model
//!
//! - **Explicit Transactions**: All writes to globals (`^NAME`) must occur within transactions
//! - **ACID Properties**: Atomicity, Consistency, Isolation, Durability
//! - **Snapshot Isolation**: Each transaction sees a consistent snapshot of the database
//! - **Auto-commit/rollback**: Transactions auto-commit on `Ok`, auto-rollback on `Err`
//! - **WAL Integration**: Transaction IDs are used in Write-Ahead Log records
//!
//! ## Key Types
//!
//! - [`TransactionId`]: Unique identifier for each transaction
//! - [`TransactionState`]: Lifecycle states (Active, Committed, Aborted)
//! - [`TransactionTimestamp`]: Logical timestamps for snapshot isolation
//! - [`IsolationLevel`]: Transaction isolation levels (currently only SnapshotIsolation)
//! - [`TransactionMetadata`]: Complete metadata for a transaction
//! - [`TransactionContext`]: Context passed to BTree operations
//!
//! ## Example Usage
//!
//! ```ignore
//! use rumps_storage::transaction::{TransactionId, TransactionState, TransactionMetadata, TransactionTimestamp};
//!
//! // Create a new transaction
//! let txn_id = TransactionId::from(1);
//! let start_ts = TransactionTimestamp::from(100);
//! let mut metadata = TransactionMetadata::new(txn_id, start_ts);
//!
//! // Transaction is initially active
//! assert!(metadata.state.is_active());
//!
//! // Commit the transaction
//! let commit_ts = TransactionTimestamp::from(101);
//! metadata = metadata.commit(commit_ts);
//! assert!(metadata.state.is_committed());
//! ```

use std::fmt;
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// A unique identifier for a database transaction.
///
/// Transaction IDs are monotonically increasing and are used throughout
/// the system for tracking operations in the Write-Ahead Log, managing
/// concurrent transactions, and ensuring consistency.
///
/// # Examples
///
/// ```ignore
/// use rumps_storage::transaction::TransactionId;
///
/// let txn1 = TransactionId::from(1);
/// let txn2 = TransactionId::from(2);
///
/// assert!(txn1 < txn2);
/// assert_eq!(u64::from(txn1), 1);
/// ```
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize
)]
pub(crate) struct TransactionId(u64);

impl fmt::Display for TransactionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Txn({})", self.0)
    }
}

impl From<u64> for TransactionId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

impl From<TransactionId> for u64 {
    fn from(id: TransactionId) -> Self {
        id.0
    }
}

/// The lifecycle state of a transaction.
///
/// Transactions progress through these states:
/// 1. **Active**: Transaction is in progress, can accept new operations
/// 2. **Committed**: Transaction has been successfully committed to the database
/// 3. **Aborted**: Transaction has been rolled back due to error or explicit abort
///
/// # Examples
///
/// ```ignore
/// use rumps_storage::transaction::TransactionState;
///
/// let state = TransactionState::Active;
/// assert!(state.is_active());
/// assert!(!state.is_terminal());
///
/// let state = TransactionState::Committed;
/// assert!(state.is_committed());
/// assert!(state.is_terminal());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum TransactionState {
    /// Transaction is currently active and accepting operations.
    Active,
    /// Transaction has been successfully committed.
    Committed,
    /// Transaction has been aborted/rolled back.
    Aborted,
}

impl TransactionState {
    /// Returns `true` if the transaction is active.
    #[inline]
    pub(crate) fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }

    /// Returns `true` if the transaction has been committed.
    #[inline]
    pub(crate) fn is_committed(&self) -> bool {
        matches!(self, Self::Committed)
    }

    /// Returns `true` if the transaction has been aborted.
    #[inline]
    pub(crate) fn is_aborted(&self) -> bool {
        matches!(self, Self::Aborted)
    }

    /// Returns `true` if the transaction is in a terminal state (Committed or Aborted).
    #[inline]
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Committed | Self::Aborted)
    }
}

impl fmt::Display for TransactionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Active => write!(f, "Active"),
            Self::Committed => write!(f, "Committed"),
            Self::Aborted => write!(f, "Aborted"),
        }
    }
}

/// A logical timestamp for snapshot isolation.
///
/// Timestamps are used to implement Multi-Version Concurrency Control (MVCC)
/// and snapshot isolation. Each transaction sees a consistent snapshot of
/// the database at its start timestamp.
///
/// # Examples
///
/// ```ignore
/// use rumps_storage::transaction::TransactionTimestamp;
///
/// let ts1 = TransactionTimestamp::from(100);
/// let ts2 = ts1.increment();
///
/// assert_eq!(u64::from(ts1), 100);
/// assert_eq!(u64::from(ts2), 101);
/// assert!(ts1 < ts2);
/// ```
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize
)]
pub(crate) struct TransactionTimestamp(u64);

impl TransactionTimestamp {
    /// Returns a new timestamp incremented by 1.
    #[inline]
    pub(crate) fn increment(&self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl fmt::Display for TransactionTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TS({})", self.0)
    }
}

impl From<u64> for TransactionTimestamp {
    fn from(ts: u64) -> Self {
        Self(ts)
    }
}

impl From<TransactionTimestamp> for u64 {
    fn from(ts: TransactionTimestamp) -> Self {
        ts.0
    }
}

/// Transaction isolation level.
///
/// Defines the level of isolation between concurrent transactions.
/// Currently, RUMPS only supports Snapshot Isolation, but the enum
/// is designed for future extensibility.
///
/// # Examples
///
/// ```ignore
/// use rumps_storage::transaction::IsolationLevel;
///
/// let level = IsolationLevel::default();
/// assert_eq!(level, IsolationLevel::SnapshotIsolation);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum IsolationLevel {
    /// Snapshot Isolation: Each transaction sees a consistent snapshot.
    ///
    /// This is the default and currently only supported level in RUMPS.
    /// Transactions read from a consistent snapshot taken at transaction
    /// start and conflicts are detected at commit time.
    SnapshotIsolation,
    // Future possibilities:
    // ReadCommitted,
    // RepeatableRead,
    // Serializable,
}

impl Default for IsolationLevel {
    fn default() -> Self {
        Self::SnapshotIsolation
    }
}

impl fmt::Display for IsolationLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SnapshotIsolation => write!(f, "SnapshotIsolation"),
        }
    }
}

/// Complete metadata for a transaction.
///
/// Contains all information about a transaction including its ID, state,
/// timestamps, and isolation level. This struct tracks the full lifecycle
/// of a transaction from creation through commit or abort.
///
/// # Examples
///
/// ```ignore
/// use rumps_storage::transaction::{TransactionId, TransactionMetadata, TransactionTimestamp};
///
/// // Create a new transaction
/// let id = TransactionId::from(1);
/// let start_ts = TransactionTimestamp::from(100);
/// let metadata = TransactionMetadata::new(id, start_ts);
///
/// // Initially active
/// assert!(metadata.state.is_active());
/// assert_eq!(metadata.commit_timestamp, None);
///
/// // Commit the transaction
/// let commit_ts = TransactionTimestamp::from(101);
/// let committed = metadata.commit(commit_ts);
/// assert!(committed.state.is_committed());
/// assert_eq!(committed.commit_timestamp, Some(commit_ts));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TransactionMetadata {
    /// Unique identifier for this transaction.
    pub(crate) id: TransactionId,
    /// Current state of the transaction.
    pub(crate) state: TransactionState,
    /// Timestamp when the transaction started (snapshot time).
    pub(crate) start_timestamp: TransactionTimestamp,
    /// Timestamp when the transaction was committed (if applicable).
    pub(crate) commit_timestamp: Option<TransactionTimestamp>,
    /// Isolation level for this transaction.
    pub(crate) isolation_level: IsolationLevel,
}

impl TransactionMetadata {
    /// Creates new metadata for an active transaction.
    pub(crate) fn new(
        id: TransactionId,
        start_timestamp: TransactionTimestamp,
    ) -> Self {
        Self {
            id,
            state: TransactionState::Active,
            start_timestamp,
            commit_timestamp: None,
            isolation_level: IsolationLevel::default(),
        }
    }

    /// Creates a new instance with the transaction committed.
    ///
    /// Returns a new `TransactionMetadata` with state set to `Committed`
    /// and the commit timestamp recorded.
    pub(crate) fn commit(mut self, timestamp: TransactionTimestamp) -> Self {
        self.state = TransactionState::Committed;
        self.commit_timestamp = Some(timestamp);
        self
    }

    /// Creates a new instance with the transaction aborted.
    ///
    /// Returns a new `TransactionMetadata` with state set to `Aborted`.
    pub(crate) fn abort(mut self) -> Self {
        self.state = TransactionState::Aborted;
        self.commit_timestamp = None;
        self
    }

    /// Returns the duration of the transaction if it has been committed.
    ///
    /// Returns `None` if the transaction is still active or was aborted.
    pub(crate) fn duration(&self) -> Option<u64> {
        self.commit_timestamp.map(|commit_ts| {
            u64::from(commit_ts).saturating_sub(u64::from(self.start_timestamp))
        })
    }
}

impl fmt::Display for TransactionMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Transaction {{ id: {}, state: {}, start: {}, commit: {:?}, isolation: {} }}",
            self.id, self.state, self.start_timestamp, self.commit_timestamp, self.isolation_level
        )
    }
}

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
    /// ```ignore
    /// use rumps_storage::TransactionContext;
    /// use rumps_storage::transaction::{TransactionId, TransactionTimestamp};
    ///
    /// let txn = TransactionContext::new(
    ///     TransactionId::from(1),
    ///     TransactionTimestamp::from(100),
    /// );
    ///
    /// assert_eq!(TransactionId::from(1), txn.id);
    /// ```
    pub fn new(
        id: TransactionId,
        start_timestamp: TransactionTimestamp,
    ) -> Self {
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

    // Tests from rumps-types/src/transaction.rs

    #[test]
    fn test_transaction_id_creation() {
        let id1 = TransactionId::from(1);
        let id2 = TransactionId::from(2);

        assert_eq!(u64::from(id1), 1);
        assert_eq!(u64::from(id2), 2);
        assert!(id1 < id2);
    }

    #[test]
    fn test_transaction_id_from_u64() {
        let id: TransactionId = 42u64.into();
        assert_eq!(u64::from(id), 42);
    }

    #[test]
    fn test_transaction_id_display() {
        let id = TransactionId::from(123);
        assert_eq!(id.to_string(), "Txn(123)");
    }

    #[test]
    fn test_transaction_id_ordering() {
        let ids: Vec<TransactionId> = vec![5, 1, 3, 2, 4]
            .into_iter()
            .map(TransactionId::from)
            .collect();

        let mut sorted = ids.clone();
        sorted.sort();

        let expected: Vec<TransactionId> =
            (1..=5).map(TransactionId::from).collect();
        assert_eq!(sorted, expected);
    }

    #[test]
    fn test_transaction_state_checks() {
        let active = TransactionState::Active;
        assert!(active.is_active());
        assert!(!active.is_committed());
        assert!(!active.is_aborted());
        assert!(!active.is_terminal());

        let committed = TransactionState::Committed;
        assert!(!committed.is_active());
        assert!(committed.is_committed());
        assert!(!committed.is_aborted());
        assert!(committed.is_terminal());

        let aborted = TransactionState::Aborted;
        assert!(!aborted.is_active());
        assert!(!aborted.is_committed());
        assert!(aborted.is_aborted());
        assert!(aborted.is_terminal());
    }

    #[test]
    fn test_transaction_state_display() {
        assert_eq!(TransactionState::Active.to_string(), "Active");
        assert_eq!(TransactionState::Committed.to_string(), "Committed");
        assert_eq!(TransactionState::Aborted.to_string(), "Aborted");
    }

    #[test]
    fn test_transaction_timestamp_creation() {
        let ts1 = TransactionTimestamp::from(100);
        let ts2 = TransactionTimestamp::from(200);

        assert_eq!(u64::from(ts1), 100);
        assert_eq!(u64::from(ts2), 200);
        assert!(ts1 < ts2);
    }

    #[test]
    fn test_transaction_timestamp_increment() {
        let ts1 = TransactionTimestamp::from(100);
        let ts2 = ts1.increment();
        let ts3 = ts2.increment();

        assert_eq!(u64::from(ts1), 100);
        assert_eq!(u64::from(ts2), 101);
        assert_eq!(u64::from(ts3), 102);
    }

    #[test]
    fn test_transaction_timestamp_increment_overflow() {
        let ts = TransactionTimestamp::from(u64::MAX);
        let next = ts.increment();
        // Should saturate at max value
        assert_eq!(u64::from(next), u64::MAX);
    }

    #[test]
    fn test_transaction_timestamp_display() {
        let ts = TransactionTimestamp::from(999);
        assert_eq!(ts.to_string(), "TS(999)");
    }

    #[test]
    fn test_transaction_timestamp_ordering() {
        let timestamps: Vec<TransactionTimestamp> = vec![50, 10, 30, 20, 40]
            .into_iter()
            .map(TransactionTimestamp::from)
            .collect();

        let mut sorted = timestamps.clone();
        sorted.sort();

        let expected: Vec<TransactionTimestamp> = vec![10, 20, 30, 40, 50]
            .into_iter()
            .map(TransactionTimestamp::from)
            .collect();
        assert_eq!(sorted, expected);
    }

    #[test]
    fn test_isolation_level_default() {
        let level = IsolationLevel::default();
        assert_eq!(level, IsolationLevel::SnapshotIsolation);
    }

    #[test]
    fn test_isolation_level_display() {
        let level = IsolationLevel::SnapshotIsolation;
        assert_eq!(level.to_string(), "SnapshotIsolation");
    }

    #[test]
    fn test_transaction_metadata_creation() {
        let id = TransactionId::from(1);
        let start_ts = TransactionTimestamp::from(100);
        let metadata = TransactionMetadata::new(id, start_ts);

        assert_eq!(metadata.id, id);
        assert_eq!(metadata.state, TransactionState::Active);
        assert_eq!(metadata.start_timestamp, start_ts);
        assert_eq!(metadata.commit_timestamp, None);
        assert_eq!(metadata.isolation_level, IsolationLevel::SnapshotIsolation);
    }

    #[test]
    fn test_transaction_metadata_commit() {
        let id = TransactionId::from(1);
        let start_ts = TransactionTimestamp::from(100);
        let commit_ts = TransactionTimestamp::from(150);

        let metadata = TransactionMetadata::new(id, start_ts);
        let committed = metadata.commit(commit_ts);

        assert_eq!(committed.state, TransactionState::Committed);
        assert_eq!(committed.commit_timestamp, Some(commit_ts));
        assert!(committed.state.is_terminal());
    }

    #[test]
    fn test_transaction_metadata_abort() {
        let id = TransactionId::from(1);
        let start_ts = TransactionTimestamp::from(100);

        let metadata = TransactionMetadata::new(id, start_ts);
        let aborted = metadata.abort();

        assert_eq!(aborted.state, TransactionState::Aborted);
        assert_eq!(aborted.commit_timestamp, None);
        assert!(aborted.state.is_terminal());
    }

    #[test]
    fn test_transaction_metadata_duration() {
        let id = TransactionId::from(1);
        let start_ts = TransactionTimestamp::from(100);
        let commit_ts = TransactionTimestamp::from(150);

        let metadata = TransactionMetadata::new(id, start_ts);

        // No duration for active transaction
        assert_eq!(metadata.duration(), None);

        // Duration available after commit
        let committed = metadata.clone().commit(commit_ts);
        assert_eq!(committed.duration(), Some(50));

        // No duration for aborted transaction
        let aborted = metadata.abort();
        assert_eq!(aborted.duration(), None);
    }

    #[test]
    fn test_transaction_metadata_display() {
        let id = TransactionId::from(1);
        let start_ts = TransactionTimestamp::from(100);
        let metadata = TransactionMetadata::new(id, start_ts);

        let display = metadata.to_string();
        assert!(display.contains("id: Txn(1)"));
        assert!(display.contains("state: Active"));
        assert!(display.contains("start: TS(100)"));
        assert!(display.contains("commit: None"));
        assert!(display.contains("isolation: SnapshotIsolation"));
    }

    #[test]
    fn test_serialization_roundtrip() {
        // Test TransactionId
        let id = TransactionId::from(42);
        let serialized = bincode::serialize(&id).unwrap();
        let deserialized: TransactionId =
            bincode::deserialize(&serialized).unwrap();
        assert_eq!(id, deserialized);

        // Test TransactionState
        let state = TransactionState::Committed;
        let serialized = bincode::serialize(&state).unwrap();
        let deserialized: TransactionState =
            bincode::deserialize(&serialized).unwrap();
        assert_eq!(state, deserialized);

        // Test TransactionTimestamp
        let ts = TransactionTimestamp::from(999);
        let serialized = bincode::serialize(&ts).unwrap();
        let deserialized: TransactionTimestamp =
            bincode::deserialize(&serialized).unwrap();
        assert_eq!(ts, deserialized);

        // Test IsolationLevel
        let level = IsolationLevel::SnapshotIsolation;
        let serialized = bincode::serialize(&level).unwrap();
        let deserialized: IsolationLevel =
            bincode::deserialize(&serialized).unwrap();
        assert_eq!(level, deserialized);

        // Test TransactionMetadata
        let metadata = TransactionMetadata::new(
            TransactionId::from(1),
            TransactionTimestamp::from(100),
        );
        let serialized = bincode::serialize(&metadata).unwrap();
        let deserialized: TransactionMetadata =
            bincode::deserialize(&serialized).unwrap();
        assert_eq!(metadata, deserialized);
    }

    #[test]
    fn test_edge_cases() {
        // Test with zero values
        let id_zero = TransactionId::from(0);
        assert_eq!(u64::from(id_zero), 0);

        let ts_zero = TransactionTimestamp::from(0);
        assert_eq!(u64::from(ts_zero), 0);

        // Test with max values
        let id_max = TransactionId::from(u64::MAX);
        assert_eq!(u64::from(id_max), u64::MAX);

        let ts_max = TransactionTimestamp::from(u64::MAX);
        assert_eq!(u64::from(ts_max), u64::MAX);

        // Test increment at max doesn't panic (uses saturating_add)
        let ts_overflow = ts_max.increment();
        assert_eq!(u64::from(ts_overflow), u64::MAX);
    }

    #[test]
    fn test_transaction_id_hash() {
        use std::collections::HashMap;

        let id1 = TransactionId::from(1);
        let id2 = TransactionId::from(2);
        let id1_copy = TransactionId::from(1);

        let mut map = HashMap::new();
        map.insert(id1, "txn1");
        map.insert(id2, "txn2");

        assert_eq!(map.get(&id1_copy), Some(&"txn1"));
        assert_eq!(map.get(&id2), Some(&"txn2"));
    }

    // Tests from rumps-storage/src/transaction.rs

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
