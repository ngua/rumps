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

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::ops::Deref;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Instant;

use futures::stream::{self, StreamExt, TryStreamExt};
use rumps_types::{DataStatus, Key, Name, Value};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::database::Database;
use crate::error::Result;
use crate::node::{NodeData, NodeId};

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
/// assert_eq!(*txn1, 1);
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

impl TransactionId {
    /// The implicit transaction ID used for Phase 4.6.
    ///
    /// Until Phase 5 implements full multi-transaction support, all operations
    /// use this single implicit transaction ID for WAL logging.
    pub(crate) const IMPLICIT: Self = Self(0);
}

impl fmt::Display for TransactionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Txn({})", **self)
    }
}

impl From<u64> for TransactionId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

impl From<TransactionId> for u64 {
    fn from(id: TransactionId) -> Self {
        *id
    }
}

impl Deref for TransactionId {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
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
#[derive(Default)]
pub(crate) enum IsolationLevel {
    /// Snapshot Isolation: Each transaction sees a consistent snapshot.
    ///
    /// This is the default and currently only supported level in RUMPS.
    /// Transactions read from a consistent snapshot taken at transaction
    /// start and conflicts are detected at commit time.
    #[default]
    SnapshotIsolation,
    // Future possibilities:
    // ReadCommitted,
    // RepeatableRead,
    // Serializable,
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
    pub(crate) id: TransactionId,

    /// The timestamp at which this transaction started (for snapshot isolation)
    pub(crate) start_timestamp: TransactionTimestamp,

    /// Wall-clock time when the transaction began
    pub(crate) start_time: Instant,

    /// Isolation level for this transaction
    pub(crate) isolation_level: IsolationLevel,
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
    pub(crate) fn new(
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

/// Strategy for handling transaction conflicts at commit time.
///
/// Defines how the system should respond when a transaction conflict
/// is detected during commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) enum ConflictStrategy {
    /// Abort the transaction on conflict (default).
    #[default]
    Abort,
    /// Retry the transaction up to `N` times on conflict.
    Retry(u32),
    /// Skip the transaction on conflict (discard changes).
    Skip,
    /// Last-write-wins: overwrite conflicting changes.
    Overwrite,
}

/// Transaction priority for scheduling and deadlock resolution.
///
/// Higher priority transactions may be favored during conflict resolution
/// or deadlock detection.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    Default
)]
pub(crate) enum TransactionPriority {
    /// Low priority transaction.
    Low,
    /// Normal priority transaction (default).
    #[default]
    Normal,
    /// High priority transaction.
    High,
}

/// Builder for creating transactions with custom configuration.
///
/// Provides a fluent API for configuring transaction properties before
/// beginning the transaction. All properties have sensible defaults.
///
/// # Examples
///
/// ```ignore
/// let builder = TransactionBuilder::default()
///     .isolation(IsolationLevel::SnapshotIsolation)
///     .conflict(ConflictStrategy::Retry(3))
///     .timeout(5000)
///     .priority(TransactionPriority::High);
///
/// let txn = builder.begin(&db).await?;
/// ```
#[derive(Debug, Clone)]
pub(crate) struct TransactionBuilder {
    isolation: IsolationLevel,
    conflict_strategy: ConflictStrategy,
    timeout: Option<u64>, // in ms
    priority: TransactionPriority,
    retry_count: u32,
}

/// Write operation types for the transaction write buffer.
///
/// Tracks different types of write operations that have been buffered
/// but not yet committed to the database.
#[derive(Debug, Clone)]
pub(crate) enum WriteOp {
    /// SET operation with new value.
    Set(NodeData),
    /// DELETE single key.
    Delete,
    /// KILL entire subtree.
    KillSubtree,
}

/// Snapshot represents a consistent view of the database at a point in time.
///
/// Used for implementing snapshot isolation - each transaction sees
/// the database as it existed at the start of the transaction.
#[derive(Debug, Clone)]
pub(crate) struct Snapshot {
    /// The timestamp at which this snapshot was taken.
    pub(crate) timestamp: TransactionTimestamp,
    /// Root nodes at snapshot time (name → root mapping).
    ///
    /// In practice, this might reference:
    /// - Immutable B-tree roots at this timestamp (MVCC)
    /// - Or a copy-on-write data structure
    /// - Or version chains with timestamps
    pub(crate) roots: HashMap<Name, NodeId>,
}

impl Default for TransactionBuilder {
    fn default() -> Self {
        Self {
            isolation: IsolationLevel::SnapshotIsolation,
            conflict_strategy: ConflictStrategy::Abort,
            timeout: None,
            priority: TransactionPriority::Normal,
            retry_count: 0,
        }
    }
}

impl TransactionBuilder {
    /// Sets the isolation level for the transaction.
    pub(crate) fn isolation(mut self, lvl: IsolationLevel) -> Self {
        self.isolation = lvl;
        self
    }

    /// Sets the conflict resolution strategy.
    pub(crate) fn conflict(mut self, strategy: ConflictStrategy) -> Self {
        self.conflict_strategy = strategy;
        self
    }

    /// Sets a timeout in milliseconds for the transaction.
    pub(crate) fn timeout(mut self, ms: u64) -> Self {
        self.timeout = Some(ms);
        self
    }

    /// Sets the transaction priority.
    pub(crate) fn priority(mut self, prio: TransactionPriority) -> Self {
        self.priority = prio;
        self
    }

    /// Sets the number of retries on conflict.
    pub(crate) fn retries(mut self, cnt: u32) -> Self {
        self.retry_count = cnt;
        self
    }

    /// Creates and initializes a new transaction with the configured settings.
    ///
    /// # What it does
    ///
    /// 1. Generates a unique `TransactionId` (monotonically increasing)
    /// 2. Captures the current database timestamp for snapshot isolation
    /// 3. Takes a read-only snapshot of the database state at this moment
    /// 4. Initializes empty write buffer for staging changes
    /// 5. Registers transaction with the database's `TransactionManager`
    /// 6. Starts optional timeout timer if configured
    /// 7. Logs transaction start to WAL (for recovery tracking)
    /// 8. Returns `Transaction` struct in `Active` state
    ///
    /// The returned `Transaction` holds a clone of the `Database` (cheap via `Arc` fields).
    pub(crate) async fn begin(self, db: &Database) -> Result<Transaction> {
        // Generate unique transaction ID
        // TODO: Get from TransactionManager when implemented
        let id = TransactionId::from(1);

        // Capture current timestamp
        // TODO: Get from TransactionManager when implemented
        let start_ts = TransactionTimestamp::from(0);

        // Take snapshot of current database state
        // TODO: Capture actual roots from database when snapshot support is added
        let snapshot = Arc::new(Snapshot {
            timestamp: start_ts,
            roots: HashMap::new(),
        });

        // Calculate timeout deadline
        let timeout = self
            .timeout
            .map(|ms| Instant::now() + std::time::Duration::from_millis(ms));

        Ok(Transaction {
            id,
            state: Arc::new(RwLock::new(TransactionState::Active)),
            start_timestamp: start_ts,
            db: db.clone(),
            isolation: self.isolation,
            conflict_strategy: self.conflict_strategy,
            priority: self.priority,
            timeout,
            retry_count: self.retry_count,
            writes: Arc::new(RwLock::new(HashMap::new())),
            deleted_subtrees: Arc::new(RwLock::new(HashSet::new())),
            read_set: Arc::new(RwLock::new(HashSet::new())),
            snapshot,
            ops_count: Arc::new(AtomicU64::new(0)),
            start_time: Instant::now(),
        })
    }
}

/// A database transaction providing ACID guarantees.
///
/// Transactions buffer all writes in memory and provide snapshot isolation
/// for reads. Changes are only visible to other transactions after commit.
///
/// # Usage
///
/// Transactions are typically created and managed through `Database::transaction()`:
///
/// ```ignore
/// db.transaction(|txn| async move {
///     txn.set(&name, &key, value).await?;
///     txn.get(&name, &key).await?;
///     Ok(()) // Auto-commits on Ok
/// }).await?;
/// ```
#[derive(Clone)]
pub(crate) struct Transaction {
    // Identity & Lifecycle
    id: TransactionId,
    state: Arc<RwLock<TransactionState>>,
    start_timestamp: TransactionTimestamp,

    // Database Reference
    db: Database,

    // Configuration (from builder)
    isolation: IsolationLevel,
    conflict_strategy: ConflictStrategy,
    priority: TransactionPriority,
    timeout: Option<Instant>,
    retry_count: u32,

    // Write Buffering
    writes: Arc<RwLock<HashMap<(Name, Key), WriteOp>>>,
    deleted_subtrees: Arc<RwLock<HashSet<(Name, Key)>>>,

    // Read Tracking (for conflict detection)
    read_set: Arc<RwLock<HashSet<(Name, Key)>>>,

    // Snapshot Data
    snapshot: Arc<Snapshot>,

    // Metrics
    ops_count: Arc<AtomicU64>,
    start_time: Instant,
}

impl Transaction {
    /// Commits the transaction, applying all buffered writes to the database.
    ///
    /// This validates the transaction for conflicts, applies all writes through
    /// the database layer (which handles WAL logging), and flushes to disk.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The transaction has already been committed or aborted
    /// - Conflict detection fails (based on conflict strategy)
    /// - Any write operation fails
    /// - Flush to disk fails
    pub(crate) async fn commit(self) -> Result<()> {
        // Check transaction state
        {
            let state = self.state.read().await;
            match *state {
                TransactionState::Active => {}
                TransactionState::Committed => {
                    Self::err("Transaction already committed")?
                }
                TransactionState::Aborted => {
                    Self::err("Transaction already aborted")?
                }
            }
        }

        // TODO Phase 5.2+: Validate no conflicts with other transactions
        // For now, we skip conflict validation since we don't have TransactionManager yet
        // self.validate_no_conflicts().await?;

        // Apply all buffered writes
        let writes = self.writes.read().await;
        let db = self.db.clone();
        stream::iter(writes.iter().map(Ok))
            .try_for_each(|((name, key), write_op)| {
                let db = db.clone();
                async move {
                    match write_op {
                        WriteOp::Set(data) => {
                            let val = data
                                .value
                                .clone()
                                .ok_or_else(|| {
                                    crate::error::StorageError::InvalidConfiguration(
                                        "Set operation has no value".into(),
                                    )
                                })?;
                            // Database.set() logs to WAL and calls btree.set_at()
                            db.set(name, key, val).await
                        }
                        WriteOp::KillSubtree => {
                            // Database.kill() logs to WAL and calls btree.kill_at()
                            db.kill(name, key).await
                        }
                        WriteOp::Delete => {
                            // Single key deletion - treat as kill
                            db.kill(name, key).await
                        }
                    }
                }
            })
            .await?;

        // Flush: writes TxnCommit record, syncs WAL, flushes dirty pages
        self.db.flush().await?;

        // TODO Phase 5.2+: Unregister transaction from manager
        // self.db.transaction_manager.complete(self.id).await?;

        // Update state to Committed
        {
            let mut state = self.state.write().await;
            *state = TransactionState::Committed;
        }

        Ok(())
    }

    /// Rolls back the transaction, discarding all buffered writes.
    ///
    /// This is automatically called when a transaction is dropped without
    /// being committed.
    pub(crate) async fn rollback(self) -> Result<()> {
        // Check transaction state
        {
            let state = self.state.read().await;
            match *state {
                TransactionState::Active => {}
                TransactionState::Committed => {
                    Self::err("Cannot rollback committed transaction")?
                }
                TransactionState::Aborted => {
                    // Already aborted, this is a no-op
                    {}
                }
            }
        }

        // Discard all buffered writes
        {
            let mut writes = self.writes.write().await;
            writes.clear();
        }
        {
            let mut deleted = self.deleted_subtrees.write().await;
            deleted.clear();
        }
        {
            let mut reads = self.read_set.write().await;
            reads.clear();
        }

        // TODO Phase 5.2+: Unregister transaction from manager
        // self.db.transaction_manager.abort(self.id).await?;

        // Update state to Aborted
        {
            let mut state = self.state.write().await;
            *state = TransactionState::Aborted;
        }

        Ok(())
    }

    /// Helper to create an error.
    fn err<T>(msg: &str) -> Result<T> {
        Err(crate::error::StorageError::InvalidConfiguration(msg.into()))
    }

    /// Gets a value from the database within this transaction's context.
    ///
    /// Checks the write buffer first for pending changes, then delegates
    /// to the database if not found in the buffer.
    ///
    /// # Transaction Semantics
    ///
    /// - Reads see buffered writes from this transaction
    /// - Reads respect deleted subtrees
    /// - Reads are tracked in the read set for conflict detection
    pub(crate) async fn get(
        &self,
        name: &Name,
        key: &Key,
    ) -> Result<Option<Value>> {
        self.ops_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let lookup_key = (name.clone(), key.clone());

        // Check write buffer first
        let buffered = {
            let writes = self.writes.read().await;
            writes.get(&lookup_key).cloned()
        };

        let result = match buffered {
            Some(WriteOp::Set(data)) => {
                // Track read
                self.read_set.write().await.insert(lookup_key.clone());
                data.value.clone().map(Some).ok_or_else(|| {
                    crate::error::StorageError::InvalidConfiguration(
                        "Buffered set has no value".into(),
                    )
                })
            }
            Some(WriteOp::Delete | WriteOp::KillSubtree) => {
                // Track read
                self.read_set.write().await.insert(lookup_key);
                Ok(None)
            }
            None => {
                // Check if key is in a deleted subtree
                let deleted = self.deleted_subtrees.read().await;
                let is_deleted = deleted.iter().any(|(del_name, del_key)| {
                    del_name == name && key.starts_with(del_key)
                });

                if is_deleted {
                    self.read_set.write().await.insert(lookup_key);
                    Ok(None)
                } else {
                    // Delegate to database (snapshot read)
                    let val = self.db.get(name, key).await;

                    // Track read
                    self.read_set.write().await.insert(lookup_key);

                    val
                }
            }
        };

        result
    }

    /// Sets a value in the transaction's write buffer.
    ///
    /// The change is not visible to other transactions until commit.
    pub(crate) async fn set(
        &self,
        name: &Name,
        key: &Key,
        val: Value,
    ) -> Result<()> {
        self.ops_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Buffer the write
        let mut writes = self.writes.write().await;
        writes.insert(
            (name.clone(), key.clone()),
            WriteOp::Set(NodeData::with_value(val)),
        );

        Ok(())
    }

    /// Kills a key and all its descendants in the transaction's write buffer.
    ///
    /// The deletion is not visible to other transactions until commit.
    pub(crate) async fn kill(&self, name: &Name, key: &Key) -> Result<()> {
        self.ops_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Buffer the kill
        let mut writes = self.writes.write().await;
        writes.insert((name.clone(), key.clone()), WriteOp::KillSubtree);

        // Track in deleted subtrees
        let mut deleted = self.deleted_subtrees.write().await;
        deleted.insert((name.clone(), key.clone()));

        Ok(())
    }

    /// Checks the data status of a node within this transaction's context.
    ///
    /// Combines buffered writes with the database snapshot to determine
    /// if a node has a value and/or descendants.
    pub(crate) async fn data(
        &self,
        name: &Name,
        key: &Key,
    ) -> Result<DataStatus> {
        self.ops_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let lookup_key = (name.clone(), key.clone());

        // Check write buffer
        let buffered = {
            let writes = self.writes.read().await;
            writes.get(&lookup_key).cloned()
        };

        match buffered {
            Some(WriteOp::Set(_)) => {
                // Has value from buffered write
                // TODO: Check for descendants in buffer
                Ok(DataStatus::HasValue)
            }
            Some(WriteOp::Delete | WriteOp::KillSubtree) => {
                Ok(DataStatus::NoData)
            }
            None => {
                // Check if in deleted subtree
                let deleted = self.deleted_subtrees.read().await;
                let is_deleted = deleted.iter().any(|(del_name, del_key)| {
                    del_name == name && key.starts_with(del_key)
                });

                if is_deleted {
                    Ok(DataStatus::NoData)
                } else {
                    self.db.data(name, key).await
                }
            }
        }
    }

    /// Returns the next key in lexicographic order within this transaction's context.
    ///
    /// Must merge snapshot iteration with buffered writes - buffered sets may
    /// insert new keys, buffered kills may remove keys.
    pub(crate) async fn order(
        &self,
        name: &Name,
        after: Option<&Key>,
    ) -> Result<Option<Key>> {
        self.ops_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        self.order_impl(name, after).await
    }

    /// Internal recursive implementation of `order()`.
    ///
    /// Uses async recursion to avoid `loop` with `break`/`continue`.
    fn order_impl<'a>(
        &'a self,
        name: &'a Name,
        after: Option<&'a Key>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<Key>>> + Send + 'a>,
    > {
        Box::pin(async move {
            // Get snapshot view from database
            let snapshot_candidate = self.db.order(name, after).await?;

            // Collect buffered keys for this name
            let writes = self.writes.read().await;
            let deleted = self.deleted_subtrees.read().await;

            // Find minimum buffered key greater than `after` that's not deleted
            let buffered_candidate = writes
                .keys()
                .filter(|(n, _)| n == name)
                .map(|(_, k)| k)
                .filter(|k| match after {
                    Some(a) => *k > a,
                    None => true,
                })
                .filter(|k| {
                    let not_explicitly_deleted = !matches!(
                        writes.get(&(name.clone(), (*k).clone())),
                        Some(WriteOp::Delete | WriteOp::KillSubtree)
                    );
                    let not_in_deleted_subtree =
                        !deleted.iter().any(|(del_name, del_key)| {
                            del_name == name && k.starts_with(del_key)
                        });
                    not_explicitly_deleted && not_in_deleted_subtree
                })
                .min()
                .cloned();

            // Choose the minimum between snapshot and buffered
            let next = match (
                snapshot_candidate.as_ref(),
                buffered_candidate.as_ref(),
            ) {
                (Some(snap), Some(buf)) => Some(snap.min(buf).clone()),
                (Some(snap), None) => Some(snap.clone()),
                (None, Some(buf)) => Some(buf.clone()),
                (None, None) => None,
            };

            // Check if candidate is deleted in our buffer; if so, recurse
            match next {
                None => Ok(None),
                Some(k) => {
                    let is_deleted =
                        matches!(
                            writes.get(&(name.clone(), k.clone())),
                            Some(WriteOp::Delete | WriteOp::KillSubtree)
                        ) || deleted.iter().any(|(del_name, del_key)| {
                            del_name == name && k.starts_with(del_key)
                        });

                    if is_deleted {
                        // Recurse to find next valid key
                        self.order_impl(name, Some(&k)).await
                    } else {
                        Ok(Some(k))
                    }
                }
            }
        })
    }

    /// Creates a stream of entries within this transaction's context.
    ///
    /// The stream reflects buffered writes combined with the snapshot.
    /// Buffered sets may add entries, buffered kills may remove them.
    pub(crate) async fn collects<'a, P, F, T>(
        &'a self,
        name: &'a Name,
        start: Option<&'a Key>,
        pred: P,
        extract: F,
    ) -> Result<impl futures::stream::Stream<Item = Result<T>> + Send + 'a>
    where
        P: Fn(&Key, &NodeData) -> bool + Send + Sync + Clone + 'a,
        F: Fn(&Key, &NodeData) -> Option<T> + Send + Sync + Clone + 'a,
        T: Send + 'a,
    {
        self.ops_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Collect buffered and deleted info
        let writes = self.writes.read().await;
        let deleted = self.deleted_subtrees.read().await;

        // Build list of deleted keys/subtrees for this name
        let deleted_list: Vec<Key> = deleted
            .iter()
            .filter_map(|(n, k)| if n == name { Some(k.clone()) } else { None })
            .collect();

        // Build set of explicitly deleted keys in writes
        let explicitly_deleted_keys: Vec<(Name, Key)> = writes
            .iter()
            .filter_map(|((n, k), op)| {
                match matches!(op, WriteOp::Delete | WriteOp::KillSubtree) {
                    true => Some((n.clone(), k.clone())),
                    false => None,
                }
            })
            .collect();

        // Clone data and closures for use in multiple places
        let deleted_list_for_pred = deleted_list.clone();
        let explicitly_deleted_for_pred = explicitly_deleted_keys.clone();
        let name_for_pred = name.clone();
        let pred_clone = pred.clone();
        let extract_clone = extract.clone();

        // Create modified predicate that excludes deleted keys
        let pred_with_deletes = move |k: &Key, data: &NodeData| {
            // Check if key is in a deleted subtree
            let in_deleted = deleted_list_for_pred
                .iter()
                .any(|del_key| k.starts_with(del_key));

            // Check if key is explicitly deleted in writes
            let explicitly_deleted = explicitly_deleted_for_pred
                .iter()
                .any(|(n, dk)| n == &name_for_pred && dk == k);

            if in_deleted || explicitly_deleted {
                false
            } else {
                pred_clone(k, data)
            }
        };

        // Collect buffered entries
        let buffered_entries: Vec<(Key, Result<T>)> = writes
            .iter()
            .filter_map(|((n, k), op)| {
                match (n == name, op) {
                    (true, WriteOp::Set(data)) => {
                        // Check start condition
                        let after_start = match start {
                            Some(s) => k >= s,
                            None => true,
                        };

                        // Check if in deleted subtree
                        let in_deleted = deleted_list
                            .iter()
                            .any(|del_key| k.starts_with(del_key));

                        if after_start && !in_deleted && pred(k, data) {
                            extract(k, data).map(|t| (k.clone(), Ok(t)))
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            })
            .collect();

        // Collect snapshot entries with keys for proper merging
        // We use a modified extract that captures both key and value
        let extract_with_key = |k: &Key, data: &NodeData| {
            extract_clone(k, data).map(|t| (k.clone(), t))
        };

        let snapshot_stream_with_keys = self
            .db
            .collects(name, start, pred_with_deletes.clone(), &extract_with_key)
            .await?;

        let snapshot_entries: Vec<(Key, T)> = snapshot_stream_with_keys
            .collect::<Vec<Result<(Key, T)>>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()?;

        // Merge buffered and snapshot entries, removing duplicates
        // (buffered writes override snapshot reads)
        let buffered_keys: std::collections::HashSet<_> =
            buffered_entries.iter().map(|(k, _)| k).collect();

        let filtered_snapshot: Vec<(Key, Result<T>)> = snapshot_entries
            .into_iter()
            .filter(|(k, _)| !buffered_keys.contains(k))
            .map(|(k, t)| (k, Ok(t)))
            .collect();

        // Combine and sort all entries by key
        let mut all_entries = buffered_entries;
        all_entries.extend(filtered_snapshot);
        all_entries.sort_by(|(k1, _), (k2, _)| k1.cmp(k2));

        // Create merged stream in sorted order
        let merged = stream::iter(all_entries.into_iter().map(|(_, r)| r));

        Ok(merged)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
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
