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

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::{Bound, Deref};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{fmt, future};

use async_recursion::async_recursion;
use futures::stream::{self, BoxStream, StreamExt, TryStreamExt};
use rumps_types::{DataStatus, Key, Name, Result, Subscript, Value};
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedRwLockReadGuard, RwLock};

use crate::database::Database;
use crate::error::StorageError;
use crate::node::{NodeData, NodeId};

/// State for lazily merging buffered entries with a snapshot stream.
///
/// Uses O(n + m) merge iteration where n = snapshot entries, m = buffered writes.
/// Both sources are in sorted key order; we advance through both simultaneously
/// without per-entry lookups.
///
/// Buffered entries take precedence over snapshot entries with the same key.
struct MergeState<'a, T, F, P>
where
    F: Fn(&Key, &Option<Value>) -> Option<T>,
    P: Fn(&Key, &Option<Value>) -> bool,
{
    /// Owned guard for the writes BTreeMap - O(1) memory.
    writes_guard: OwnedRwLockReadGuard<BTreeMap<(Name, Key), WriteOp>>,
    /// Target name for filtering buffered entries.
    name: Name,
    /// Last buffered key visited (for range iteration). `None` = start.
    last_buffered_key: Option<Key>,
    /// Next buffered entry: `Some((key, Some(val)))` = yieldable Set,
    /// `Some((key, None))` = non-yieldable (Delete/Kill or failed pred).
    next_buffered: Option<(Key, Option<T>)>,
    /// Extract function to convert `Option<Value>` to `T`.
    extract: F,
    /// Predicate for filtering entries.
    pred: P,
    /// Start key for filtering.
    start: Option<Key>,
    /// The snapshot stream yielding `(Key, T)` pairs (lazy, unbounded).
    snapshot: BoxStream<'a, crate::error::Result<(Key, T)>>,
    /// Next snapshot entry to consider.
    next_snapshot: Option<(Key, T)>,
    /// Tracks if we've encountered an error in the snapshot stream.
    snapshot_error: Option<StorageError>,
}

impl<'a, T: Send, F, P> MergeState<'a, T, F, P>
where
    F: Fn(&Key, &Option<Value>) -> Option<T> + Clone + Send + Sync,
    P: Fn(&Key, &Option<Value>) -> bool + Send + Sync,
{
    /// Advances to the next buffered entry using O(log n) range lookup.
    ///
    /// Includes ALL write ops (Set, Delete, KillSubtree) so the merge logic
    /// can properly hide snapshot entries. Buffered Sets are always visible,
    /// even under killed subtrees (sets after kills take precedence).
    fn advance_buffered(&mut self) {
        let range_start = match &self.last_buffered_key {
            None => Bound::Included((self.name.clone(), Key::default())),
            Some(k) => Bound::Excluded((self.name.clone(), k.clone())),
        };

        // Find next entry in O(log n) - include ALL write ops
        self.next_buffered = self
            .writes_guard
            .range((range_start, Bound::Unbounded))
            .filter_map(|((n, k), op)| {
                // Stop if we've moved past our target name
                if n != &self.name {
                    None
                } else {
                    let after_start =
                        self.start.as_ref().map(|s| k >= s).unwrap_or(true);

                    if after_start {
                        match op {
                            WriteOp::Set(data) => {
                                // Buffered Sets are always visible; apply pred
                                let val = (self.pred)(k, &data.value)
                                    .then(|| (self.extract)(k, &data.value))
                                    .flatten();
                                Some((k.clone(), val))
                            }
                            WriteOp::Delete | WriteOp::KillSubtree => {
                                Some((k.clone(), None))
                            }
                        }
                    } else {
                        None
                    }
                }
            })
            .next();

        // Update position marker
        if let Some((k, _)) = &self.next_buffered {
            self.last_buffered_key = Some(k.clone());
        }
    }

    /// Advances the snapshot stream (no `is_buffered` check needed).
    #[async_recursion]
    async fn advance_snapshot(&mut self) {
        if self.next_snapshot.is_none() {
            match self.snapshot.next().await {
                None => {}
                Some(Err(e)) => {
                    self.snapshot_error = Some(e);
                }
                Some(Ok(entry)) => {
                    self.next_snapshot = Some(entry);
                }
            }
        }
    }

    /// Yields buffered value if present, skips snapshot if requested, recurses.
    #[async_recursion]
    async fn yield_buffered_or_skip(
        &mut self,
        skip_snapshot: bool,
    ) -> Option<crate::error::Result<T>> {
        let opt_val = self.next_buffered.take().and_then(|(_, v)| v);
        if skip_snapshot {
            self.next_snapshot = None;
        }
        self.advance_buffered();
        match opt_val {
            Some(val) => Some(Ok(val)),
            None => self.next_impl().await,
        }
    }

    /// Core merge logic with O(n + m) complexity.
    #[async_recursion]
    async fn next_impl(&mut self) -> Option<crate::error::Result<T>> {
        match self.snapshot_error.take() {
            Some(e) => Some(Err(e)),
            None => {
                // Initialize buffered on first call
                if self.next_buffered.is_none()
                    && self.last_buffered_key.is_none()
                {
                    self.advance_buffered();
                }

                self.advance_snapshot().await;

                // Clone keys for comparison to avoid borrow issues
                let buf_key =
                    self.next_buffered.as_ref().map(|(k, _)| k.clone());
                let snap_key =
                    self.next_snapshot.as_ref().map(|(k, _)| k.clone());

                match (buf_key, snap_key) {
                    (None, None) => None,
                    (Some(_), None) => self.yield_buffered_or_skip(false).await,
                    (None, Some(_)) => {
                        self.next_snapshot.take().map(|(_, val)| Ok(val))
                    }
                    (Some(bk), Some(sk)) => match bk.cmp(&sk) {
                        Ordering::Less => {
                            self.yield_buffered_or_skip(false).await
                        }
                        Ordering::Equal => {
                            self.yield_buffered_or_skip(true).await
                        }
                        Ordering::Greater => {
                            self.next_snapshot.take().map(|(_, val)| Ok(val))
                        }
                    },
                }
            }
        }
    }

    /// Gets the next item from the merged stream.
    async fn next(&mut self) -> Option<crate::error::Result<T>> {
        self.next_impl().await
    }
}

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
    /// The implicit transaction ID.
    ///
    /// Used for single-transaction WAL logging when full multi-transaction
    /// support is not required.
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
/// ```
/// use rumps_storage::IsolationLevel;
///
/// let level = IsolationLevel::default();
/// assert_eq!(level, IsolationLevel::SnapshotIsolation);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[derive(Default)]
pub enum IsolationLevel {
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
/// Includes:
/// - Write buffers for uncommitted changes
/// - Snapshot isolation metadata
/// - Conflict detection state
/// - Links to the WAL
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

/// Default maximum concurrent transactions.
const DEFAULT_MAX_CONCURRENT_TRANSACTIONS: usize = 1024;

/// A committed transaction's write set, used for conflict detection.
#[derive(Debug, Clone)]
struct CommittedWriteSet {
    /// Timestamp when this transaction committed.
    commit_ts: TransactionTimestamp,
    /// Keys that were written by this transaction.
    keys: HashSet<(Name, Key)>,
}

/// Manages concurrent transactions and their lifecycles.
///
/// The `TransactionManager` is responsible for:
/// - Allocating unique transaction IDs
/// - Tracking active transactions
/// - Providing timestamps for snapshot isolation
/// - Validating transactions for conflicts at commit time
///
/// # Thread Safety
///
/// Uses atomics for simple counters (`next_txn_id`, `next_timestamp`) and
/// `RwLock` for collections requiring consistent reads/writes.
#[derive(Debug)]
pub(crate) struct TransactionManager {
    /// Monotonically increasing transaction ID counter.
    ///
    /// Uses `AtomicU64` with `Relaxed` ordering; we only need uniqueness,
    /// not ordering guarantees. Transaction IDs are opaque identifiers.
    next_txn_id: AtomicU64,

    /// Monotonically increasing timestamp for snapshot isolation.
    ///
    /// Uses `AtomicU64` with `Relaxed` ordering; relative ordering between
    /// transactions is established by `committed_writes` (which uses a lock).
    /// What matters is `commit_ts > start_ts` for the same transaction, which
    /// is guaranteed since both are fetched sequentially within the txn lifecycle.
    next_timestamp: AtomicU64,

    /// Currently active transactions (`txn_id` → metadata).
    active: RwLock<HashMap<TransactionId, TransactionMetadata>>,

    /// Write sets of recently committed transactions.
    ///
    /// Used for conflict detection: a transaction conflicts if it tries
    /// to write a key that was written by a transaction that committed
    /// after this transaction's start timestamp.
    committed_writes: RwLock<Vec<CommittedWriteSet>>,

    /// Maximum allowed concurrent transactions.
    max_concurrent: usize,
}

impl TransactionManager {
    /// Creates a new transaction manager with the specified limit.
    pub(crate) fn new(max_concurrent: usize) -> Self {
        Self {
            next_txn_id: AtomicU64::new(1), // 0 is reserved for IMPLICIT
            next_timestamp: AtomicU64::new(0),
            active: RwLock::new(HashMap::new()),
            committed_writes: RwLock::new(Vec::new()),
            max_concurrent,
        }
    }

    /// Allocates a new unique transaction ID (lock-free).
    pub(crate) fn allocate_txn_id(&self) -> TransactionId {
        TransactionId::from(
            self.next_txn_id.fetch_add(1, AtomicOrdering::Relaxed),
        )
    }

    /// Gets a new timestamp for snapshot isolation (lock-free).
    pub(crate) fn current_timestamp(&self) -> TransactionTimestamp {
        TransactionTimestamp::from(
            self.next_timestamp.fetch_add(1, AtomicOrdering::Relaxed),
        )
    }

    /// Registers a new transaction as active.
    ///
    /// # Errors
    ///
    /// Returns `TooManyConcurrentTransactions` if the limit is reached.
    pub(crate) async fn register(
        &self,
        metadata: TransactionMetadata,
    ) -> crate::error::Result<()> {
        let mut active = self.active.write().await;

        if active.len() >= self.max_concurrent {
            Err(StorageError::TooManyConcurrentTransactions {
                limit: self.max_concurrent,
            })
        } else {
            active.insert(metadata.id, metadata);
            Ok(())
        }
    }

    /// Marks a transaction as completed (committed).
    pub(crate) async fn complete(
        &self,
        txn_id: TransactionId,
    ) -> crate::error::Result<()> {
        let mut active = self.active.write().await;
        active.remove(&txn_id);
        Ok(())
    }

    /// Marks a transaction as aborted.
    pub(crate) async fn abort(
        &self,
        txn_id: TransactionId,
    ) -> crate::error::Result<()> {
        let mut active = self.active.write().await;
        active.remove(&txn_id);
        Ok(())
    }

    /// Checks if a transaction is currently active.
    pub(crate) async fn is_active(&self, txn_id: TransactionId) -> bool {
        let active = self.active.read().await;
        active.contains_key(&txn_id)
    }

    /// Validates that there are no write-write conflicts with other transactions.
    ///
    /// For snapshot isolation, a conflict occurs when this transaction tries
    /// to write a key that was written by another transaction that committed
    /// AFTER this transaction started.
    ///
    /// # First-Committer-Wins Rule
    ///
    /// If transaction A starts, then transaction B starts, both write to key X,
    /// and B commits first, then A will fail validation because B committed
    /// after A's start timestamp.
    pub(crate) async fn validate_no_conflicts(
        &self,
        txn_id: TransactionId,
        start_ts: TransactionTimestamp,
        _read_set: &HashSet<(Name, Key)>,
        write_set: &HashSet<(Name, Key)>,
    ) -> crate::error::Result<()> {
        let committed = self.committed_writes.read().await;

        // Check if any transaction that committed after our start timestamp
        // wrote to keys that we're trying to write (write-write conflict)
        committed
            .iter()
            .filter(|cws| cws.commit_ts > start_ts)
            .try_for_each(|cws| {
                let conflict =
                    write_set.iter().find(|key| cws.keys.contains(key));
                match conflict {
                    Some((name, key)) => Err(StorageError::WriteConflict {
                        txn_id: *txn_id,
                        name: name.clone(),
                        key: key.clone(),
                    }),
                    None => Ok(()),
                }
            })
    }

    /// Records a transaction's write set upon successful commit.
    ///
    /// This is called after validation passes but before the transaction
    /// is removed from the active set. Other transactions that started
    /// before this commit will check against this write set.
    pub(crate) async fn record_commit(
        &self,
        write_set: HashSet<(Name, Key)>,
    ) -> crate::error::Result<TransactionTimestamp> {
        let commit_ts = self.current_timestamp();

        // Only record if there were actual writes
        if !write_set.is_empty() {
            let mut committed = self.committed_writes.write().await;
            committed.push(CommittedWriteSet {
                commit_ts,
                keys: write_set,
            });
        }

        Ok(commit_ts)
    }

    /// Atomically validates a transaction's write set and reserves a commit slot.
    ///
    /// This combines validation and recording under a single write lock to
    /// prevent race conditions where two transactions both pass validation
    /// before either records their commit.
    ///
    /// Returns the commit timestamp and an index into `committed_writes` where
    /// the commit was recorded. If the commit needs to be rolled back (e.g.,
    /// apply_writes fails), call `rollback_commit` with this index.
    ///
    /// # First-Committer-Wins Rule
    ///
    /// If transaction A starts, then transaction B starts, both write to key X,
    /// and B commits first, then A will fail validation because B's commit
    /// will be recorded before A can validate.
    pub(crate) async fn validate_and_record(
        &self,
        txn_id: TransactionId,
        start_ts: TransactionTimestamp,
        write_set: HashSet<(Name, Key)>,
        conflict_strategy: ConflictStrategy,
    ) -> crate::error::Result<TransactionTimestamp> {
        // Pre-allocate commit timestamp before taking the write lock
        // to avoid nested lock acquisition
        let commit_ts = self.current_timestamp();

        // Early exit if nothing to validate/record
        if write_set.is_empty() {
            Ok(commit_ts)
        } else {
            // Take exclusive lock for atomic validation + recording
            let mut committed = self.committed_writes.write().await;

            // Validate: check for write-write conflicts with transactions
            // that committed after our start timestamp.
            // Skip validation for Overwrite strategy (last-write-wins).
            if conflict_strategy != ConflictStrategy::Overwrite {
                committed
                    .iter()
                    .filter(|cws| cws.commit_ts > start_ts)
                    .try_for_each(|cws| {
                        write_set
                            .iter()
                            .find(|key| cws.keys.contains(key))
                            .map_or(Ok(()), |(name, key)| {
                                Err(StorageError::WriteConflict {
                                    txn_id: *txn_id,
                                    name: name.clone(),
                                    key: key.clone(),
                                })
                            })
                    })?;
            }

            // Record immediately (still holding lock)
            committed.push(CommittedWriteSet {
                commit_ts,
                keys: write_set,
            });

            Ok(commit_ts)
        }
    }

    /// Cleans up old committed write sets that are no longer needed.
    ///
    /// A committed write set can be removed once all transactions that
    /// started before the commit have finished.
    pub(crate) async fn cleanup_old_commits(&self) {
        let active = self.active.read().await;

        // Find the oldest active transaction's start timestamp
        let oldest_start = active.values().map(|m| m.start_timestamp).min();

        drop(active);

        // Remove committed write sets older than the oldest active transaction
        if let Some(oldest) = oldest_start {
            let mut committed = self.committed_writes.write().await;
            committed.retain(|cws| cws.commit_ts >= oldest);
        } else {
            // No active transactions, clear all committed write sets
            let mut committed = self.committed_writes.write().await;
            committed.clear();
        }
    }

    /// Returns the number of currently active transactions.
    pub(crate) async fn active_count(&self) -> usize {
        self.active.read().await.len()
    }
}

impl Default for TransactionManager {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_CONCURRENT_TRANSACTIONS)
    }
}

/// Strategy for handling transaction conflicts at commit time.
///
/// Defines how the system should respond when a transaction conflict
/// is detected during commit.
///
/// # Examples
///
/// ```
/// use rumps_storage::ConflictStrategy;
///
/// // Default is Abort
/// assert_eq!(ConflictStrategy::default(), ConflictStrategy::Abort);
///
/// // Overwrite bypasses conflict detection entirely
/// let overwrite = ConflictStrategy::Overwrite;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ConflictStrategy {
    /// Abort the transaction on conflict (default).
    #[default]
    Abort,
    /// Last-write-wins: bypass conflict detection entirely.
    Overwrite,
}

/// Builder for creating and configuring transactions.
///
/// Created via [`Database::build_transaction()`]. Provides a fluent API for
/// configuring transaction properties before execution.
///
/// # Retry Semantics
///
/// When `.retries(n)` is configured, the **commit phase** (not the entire
/// closure) is retried on retriable errors like `WriteConflict`. The closure
/// runs exactly once; only the final commit is retried.
///
/// For write-write conflicts with already-committed transactions, retries
/// will still fail (the conflict is permanent for that snapshot). For
/// transient errors, retries may help.
///
/// # Examples
///
/// ```
/// # tokio_test::block_on(async {
/// use rumps_storage::{Database, ConflictStrategy};
/// use rumps_types::{global, key, value};
///
/// let db = Database::in_memory()?;
///
/// // Simple transaction
/// db.build_transaction()
///     .conflict(ConflictStrategy::Overwrite)
///     .timeout(5000)
///     .begin(|txn| async move {
///         txn.set(&global!("DATA"), &key![1], value!("test")).await?;
///         Ok(())
///     })
///     .await?;
///
/// // With commit retries
/// db.build_transaction()
///     .retries(3)
///     .begin(|txn| async move {
///         txn.set(&global!("DATA"), &key![2], value!("retry")).await?;
///         Ok(())
///     })
///     .await?;
/// # Ok::<(), rumps_storage::Error>(())
/// # });
/// ```
///
/// [`Database::build_transaction()`]: crate::Database::build_transaction
#[derive(Clone)]
pub struct TransactionBuilder {
    db: Database,
    isolation: IsolationLevel,
    conflict_strategy: ConflictStrategy,
    timeout: Option<u64>,
    retry_count: u32,
}

impl fmt::Debug for TransactionBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransactionBuilder")
            .field("db", &"<Database>")
            .field("isolation", &self.isolation)
            .field("conflict_strategy", &self.conflict_strategy)
            .field("timeout", &self.timeout)
            .field("retry_count", &self.retry_count)
            .finish()
    }
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

impl TransactionBuilder {
    /// Creates a new builder for the given database.
    pub(crate) fn new(db: Database) -> Self {
        Self {
            db,
            isolation: IsolationLevel::SnapshotIsolation,
            conflict_strategy: ConflictStrategy::Abort,
            timeout: None,
            retry_count: 0,
        }
    }

    /// Sets the isolation level for the transaction.
    pub fn isolation(mut self, lvl: IsolationLevel) -> Self {
        self.isolation = lvl;
        self
    }

    /// Sets the conflict resolution strategy.
    pub fn conflict(mut self, strategy: ConflictStrategy) -> Self {
        self.conflict_strategy = strategy;
        self
    }

    /// Sets a timeout in milliseconds for the transaction.
    pub fn timeout(mut self, ms: u64) -> Self {
        self.timeout = Some(ms);
        self
    }

    /// Sets the number of times to retry the commit on retriable errors.
    ///
    /// When configured, only the **commit phase** is retried; the closure
    /// runs exactly once. See the [struct-level docs](Self) for details.
    pub fn retries(mut self, cnt: u32) -> Self {
        self.retry_count = cnt;
        self
    }

    /// Begins a transaction and executes the given function.
    ///
    /// The transaction auto-commits if the closure returns `Ok`, and
    /// auto-rollbacks if it returns `Err`.
    ///
    /// If a timeout was configured via [`timeout()`], the entire transaction
    /// (including the user closure) will be cancelled if it exceeds the limit.
    ///
    /// [`timeout()`]: Self::timeout
    pub async fn begin<F, Fut, R>(self, f: F) -> Result<R>
    where
        F: FnOnce(Transaction) -> Fut,
        Fut: future::Future<Output = Result<R>>,
    {
        let ms = self.timeout;
        let retries = self.retry_count;
        let txn = self.start().await?;
        let t = txn.clone();
        txn.timed(ms, async {
            match f(t.clone()).await {
                Ok(result) => t
                    .commit_with_retry(retries)
                    .await
                    .map(|()| result)
                    .map_err(rumps_types::Error::from),
                Err(e) => {
                    let _ = t.rollback().await;
                    Err(e)
                }
            }
        })
        .await
    }

    /// Creates and initializes a new transaction with the configured settings.
    ///
    /// This is the low-level method used internally by [`begin`]. It returns
    /// a `Transaction` that must be manually committed or rolled back.
    ///
    /// When using `start()` directly, commit retries are NOT automatic.
    /// Use [`Transaction::commit_with_retry`] if you need retry behavior.
    ///
    /// [`begin`]: Self::begin
    pub async fn start(self) -> Result<Transaction> {
        let db = &self.db;

        // Allocate unique transaction ID from manager
        let id = db.txn_manager.allocate_txn_id();

        // Get current timestamp for snapshot isolation
        let start_ts = db.txn_manager.current_timestamp();

        // Register transaction with manager
        let metadata = TransactionMetadata::new(id, start_ts);
        db.txn_manager.register(metadata).await?;

        // Take snapshot of current database state
        let snapshot = Arc::new(Snapshot {
            timestamp: start_ts,
            roots: HashMap::new(),
        });

        // Calculate timeout deadline
        let timeout = self
            .timeout
            .map(|ms| Instant::now() + Duration::from_millis(ms));

        Ok(Transaction {
            id,
            state: Arc::new(RwLock::new(TransactionState::Active)),
            start_timestamp: start_ts,
            db: self.db,
            isolation: self.isolation,
            conflict_strategy: self.conflict_strategy,
            timeout,
            retry_count: self.retry_count,
            writes: Arc::new(RwLock::new(BTreeMap::new())),
            deleted_subtrees: Arc::new(RwLock::new(HashSet::new())),
            read_set: Arc::new(RwLock::new(HashSet::new())),
            snapshot,
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
/// ```
/// # tokio_test::block_on(async {
/// use rumps_storage::Database;
/// use rumps_types::{global, key, Value};
///
/// let db = Database::in_memory()?;
///
/// db.transaction(|txn| async move {
///     txn.set(&global!("DATA"), &key![1], Value::from("hello")).await?;
///
///     // Reads within transaction see buffered writes
///     let val = txn.get(&global!("DATA"), &key![1]).await?;
///     assert_eq!(val, Some(Value::from("hello")));
///
///     Ok(()) // Auto-commits on Ok
/// }).await?;
///
/// // After commit, values are visible outside the transaction
/// let val = db.get(&global!("DATA"), &key![1]).await?;
/// assert_eq!(val, Some(Value::from("hello")));
/// # Ok::<(), rumps_storage::Error>(())
/// # });
/// ```
#[derive(Clone)]
pub struct Transaction {
    // Identity & Lifecycle
    id: TransactionId,
    state: Arc<RwLock<TransactionState>>,
    start_timestamp: TransactionTimestamp,

    // Database Reference
    db: Database,

    // Configuration (from builder)
    isolation: IsolationLevel,
    conflict_strategy: ConflictStrategy,
    timeout: Option<Instant>,
    retry_count: u32,

    // Write Buffering (BTreeMap for sorted iteration in `collects`)
    writes: Arc<RwLock<BTreeMap<(Name, Key), WriteOp>>>,
    deleted_subtrees: Arc<RwLock<HashSet<(Name, Key)>>>,

    /// Read set for future Serializable Snapshot Isolation (SSI).
    ///
    /// Currently tracked but unused. Standard Snapshot Isolation only requires
    /// write-write conflict detection (implemented in `validate_and_record`).
    /// SSI would additionally check read-write conflicts to prevent write skew.
    read_set: Arc<RwLock<HashSet<(Name, Key)>>>,

    // Snapshot Data
    snapshot: Arc<Snapshot>,

    start_time: Instant,
}

// Public API
impl Transaction {
    /// Returns the configured retry count for this transaction.
    pub fn retry_count(&self) -> u32 {
        self.retry_count
    }

    /// Wraps an async operation in a timeout.
    ///
    /// If `ms` is `Some`, the future is wrapped in `tokio::time::timeout`.
    /// If `ms` is `None`, the future runs without a timeout.
    ///
    /// This is primarily exposed for the query layer (`rumps-query`), which
    /// uses [`TransactionBuilder::start`] and needs manual timeout handling.
    /// Users of [`TransactionBuilder::begin`] get timeout handling automatically.
    pub async fn timed<F, T, E>(
        &self,
        ms: Option<u64>,
        f: F,
    ) -> std::result::Result<T, E>
    where
        F: future::Future<Output = std::result::Result<T, E>>,
        E: From<StorageError>,
    {
        match ms {
            Some(ms) => tokio::time::timeout(Duration::from_millis(ms), f)
                .await
                .unwrap_or_else(|_| {
                    Err(StorageError::TransactionTimeout { ms }.into())
                }),
            None => f.await,
        }
    }

    /// Gets a value from the database within this transaction's context.
    ///
    /// Checks the write buffer for pending changes, then delegates to the
    /// database if not found in the buffer.
    ///
    /// # Transaction Semantics
    ///
    /// - Buffered writes always take precedence (sets after kills are visible)
    /// - Keys under killed subtrees return `None` only if not in write buffer
    /// - Reads are tracked in the read set for conflict detection
    pub async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>> {
        let lookup_key = (name.clone(), key.clone());

        // Track read
        self.read_set.write().await.insert(lookup_key.clone());

        // Check write buffer first; buffered writes always take precedence
        let writes = self.writes.read().await;
        match writes.get(&lookup_key) {
            Some(WriteOp::Set(data)) => data
                .value
                .clone()
                .map(Some)
                .ok_or_else(|| {
                    StorageError::InvalidConfiguration(
                        "Buffered set has no value".into(),
                    )
                })
                .map_err(Into::into),
            Some(WriteOp::Delete | WriteOp::KillSubtree) => Ok(None),
            None => {
                drop(writes);
                // Check if key is under a killed subtree
                let deleted = self.deleted_subtrees.read().await;
                let is_killed = deleted
                    .iter()
                    .any(|(n, del)| n == name && key.starts_with(del));

                if is_killed {
                    Ok(None)
                } else {
                    drop(deleted);
                    self.db.get(name, key).await
                }
            }
        }
    }

    /// Sets a value in the transaction's write buffer.
    ///
    /// The change is not visible to other transactions until commit.
    pub async fn set(&self, name: &Name, key: &Key, val: Value) -> Result<()> {
        // Buffer the write
        let mut writes = self.writes.write().await;
        writes.insert(
            (name.clone(), key.clone()),
            WriteOp::Set(NodeData::with_value(val)),
        );

        Ok(())
    }

    /// Sets multiple values in the transaction's write buffer with a single lock.
    ///
    /// More efficient than calling `set` multiple times when inserting
    /// many key-value pairs, as it only acquires the write lock once.
    pub(crate) async fn set_many(
        &self,
        name: &Name,
        pairs: &[(Key, Value)],
    ) -> Result<()> {
        let mut writes = self.writes.write().await;
        pairs.iter().for_each(|(k, v)| {
            writes.insert(
                (name.clone(), k.clone()),
                WriteOp::Set(NodeData::with_value(v.clone())),
            );
        });
        Ok(())
    }

    /// Kills a key and all its descendants in the transaction's write buffer.
    ///
    /// The deletion is not visible to other transactions until commit.
    pub async fn kill(&self, name: &Name, key: &Key) -> Result<()> {
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
    ///
    /// # Semantics
    ///
    /// - Buffered `Set` operations always contribute (sets after kills visible)
    /// - Buffered `Kill`/`Delete` operations mask snapshot data
    /// - Killed subtrees only affect snapshot reads, not buffered writes
    /// - Final status combines buffered state with snapshot state
    pub async fn data(&self, name: &Name, key: &Key) -> Result<DataStatus> {
        let writes = self.writes.read().await;
        let deleted = self.deleted_subtrees.read().await;
        let lookup_key = (name.clone(), key.clone());

        // Check buffered state for this exact key
        let buffered_has_val = matches!(
            writes.get(&lookup_key),
            Some(WriteOp::Set(d)) if d.value.is_some()
        );

        let explicitly_deleted = matches!(
            writes.get(&lookup_key),
            Some(WriteOp::Delete | WriteOp::KillSubtree)
        );

        // Check for buffered descendants: Sets that are strict descendants.
        // Buffered Sets are always visible, even under killed subtrees.
        let has_buffered_desc = writes.iter().any(|((n, k), op)| {
            n == name
                && k.starts_with(key)
                && k.len() > key.len()
                && matches!(op, WriteOp::Set(_))
        });

        // A key is "killed" if it (or an ancestor) was killed in this txn.
        // Killed subtrees only mask snapshot data, not buffered writes.
        let is_killed = deleted
            .iter()
            .any(|(n, del)| n == name && key.starts_with(del));

        // Determine snapshot contribution (masked by kills and explicit deletes)
        let (snap_has_val, snap_has_desc) = if explicitly_deleted || is_killed {
            (false, false)
        } else {
            drop(writes);
            drop(deleted);
            let db_status = self.db.data(name, key).await?;
            (
                matches!(db_status, DataStatus::HasValue | DataStatus::Both),
                matches!(
                    db_status,
                    DataStatus::HasDescendants | DataStatus::Both
                ),
            )
        };

        let has_val = buffered_has_val || snap_has_val;
        let has_desc = has_buffered_desc || snap_has_desc;

        Ok(match (has_val, has_desc) {
            (true, true) => DataStatus::Both,
            (true, false) => DataStatus::HasValue,
            (false, true) => DataStatus::HasDescendants,
            (false, false) => DataStatus::NoData,
        })
    }

    /// Returns the next full key in lexicographic order (MUMPS `$QUERY`).
    ///
    /// Unlike `$ORDER` which returns the next subscript at a specific level,
    /// `$QUERY` returns the complete path to the next node with a value.
    ///
    /// Merges snapshot iteration with buffered writes; buffered sets may
    /// insert new keys, buffered kills may remove keys.
    pub async fn query(
        &self,
        name: &Name,
        after: Option<&Key>,
    ) -> Result<Option<Key>> {
        self.query_impl(name, after).await
    }

    /// Returns the next subscript at a specific level (MUMPS `$ORDER`).
    ///
    /// Unlike `$QUERY` which returns the full key path, `$ORDER` returns just
    /// the next subscript value at the level defined by `prefix`.
    ///
    /// Merges snapshot iteration with buffered writes; buffered sets may
    /// insert new subscripts, buffered kills may remove them.
    pub async fn order(
        &self,
        name: &Name,
        prefix: &Key,
        after: Option<&Subscript>,
    ) -> Result<Option<Subscript>> {
        self.order_impl(name, prefix, after).await
    }

    /// Creates a stream of entries within this transaction's context.
    ///
    /// The stream reflects buffered writes combined with the snapshot.
    /// Buffered sets may add entries, buffered kills may remove them.
    ///
    /// This implementation uses O(1) additional memory by:
    /// - Holding an owned lock guard (not collecting the write buffer)
    /// - Using key-based range iteration (O(log n) per access)
    /// - Checking buffered membership via BTreeMap lookup
    pub async fn collects<'a, P, F, T>(
        &'a self,
        name: &'a Name,
        start: Option<&'a Key>,
        pred: P,
        extract: F,
    ) -> Result<impl futures::stream::Stream<Item = Result<T>> + Send + 'a>
    where
        P: Fn(&Key, &Option<Value>) -> bool + Send + Sync + Clone + 'a,
        F: Fn(&Key, &Option<Value>) -> Option<T> + Send + Sync + Clone + 'a,
        T: Send + 'a,
    {
        // Get owned guard for writes - O(1) memory, no collection
        let writes_guard = Arc::clone(&self.writes).read_owned().await;

        // Build list of deleted subtrees for this name (for snapshot filtering)
        let deleted = self.deleted_subtrees.read().await;
        let deleted_list: Vec<Key> = deleted
            .iter()
            .filter_map(|(n, k)| if n == name { Some(k.clone()) } else { None })
            .collect();
        drop(deleted);

        // Create predicate that excludes killed keys from snapshot.
        // Buffered writes are NOT filtered by kills (handled in MergeState).
        let pred_clone = pred.clone();
        let pred_with_deletes = move |k: &Key, val: &Option<Value>| {
            let in_deleted =
                deleted_list.iter().any(|del_key| k.starts_with(del_key));

            if in_deleted {
                false
            } else {
                pred_clone(k, val)
            }
        };

        // Create extract_with_key to get (Key, T) pairs from snapshot
        let extract_with_key = {
            let extract = extract.clone();
            move |k: &Key, val: &Option<Value>| {
                extract(k, val).map(|t| (k.clone(), t))
            }
        };

        // Get snapshot stream yielding (Key, T) pairs (lazy, not collected!)
        // Map from public Error to internal StorageError
        let snapshot_stream = self
            .db
            .collects(name, start, pred_with_deletes, extract_with_key)
            .await?
            .map(|r| {
                r.map_err(|e| match e {
                    rumps_types::Error::Storage(se) => se,
                    other => rumps_types::StorageError::Serialization(
                        other.to_string(),
                    ),
                })
            });

        // Create O(1) memory merge state
        let state = MergeState {
            writes_guard,
            name: name.clone(),
            last_buffered_key: None,
            next_buffered: None,
            extract,
            pred,
            start: start.cloned(),
            snapshot: snapshot_stream.boxed(),
            next_snapshot: None,
            snapshot_error: None,
        };

        // Create a merge stream using unfold
        // Map from internal StorageError to public Error
        let merged = stream::unfold(state, |mut state| async move {
            state.next().await.map(|item| (item, state))
        })
        .map(|r| r.map_err(Into::into));

        Ok(merged)
    }

    /// Collects all matching entries into a `Vec`.
    ///
    /// This is a convenience wrapper around `collects()` that collects
    /// the stream into a vector. Useful when you need all results at once.
    ///
    /// The vector reflects buffered writes combined with the snapshot.
    /// Buffered sets may add entries, buffered kills may remove them.
    pub async fn collects_vec<P, F, T>(
        &self,
        name: &Name,
        start: Option<&Key>,
        pred: P,
        extract: F,
    ) -> Result<Vec<T>>
    where
        P: Fn(&Key, &Option<Value>) -> bool + Send + Sync + Clone,
        F: Fn(&Key, &Option<Value>) -> Option<T> + Send + Sync + Clone,
        T: Send,
    {
        self.collects(name, start, pred, extract)
            .await?
            .try_collect()
            .await
    }

    /// Collects all entries matching a key prefix into a `Vec`.
    ///
    /// This method is optimized for prefix-based queries. It uses the
    /// existing merge logic to combine buffered writes with the snapshot,
    /// filtering by the given prefix.
    ///
    /// # Parameters
    ///
    /// * `name` - The variable name (global or local)
    /// * `prefix` - The key prefix to match
    /// * `extract` - Extractor returning `Some(T)` to yield, `None` to skip
    pub(crate) async fn collects_prefix_vec<F, T>(
        &self,
        name: &Name,
        prefix: &Key,
        extract: F,
    ) -> Result<Vec<T>>
    where
        F: Fn(&Key, &Option<Value>) -> Option<T> + Send + Sync + Clone,
        T: Send,
    {
        let prefix_owned = prefix.clone();
        self.collects_vec(
            name,
            None,
            move |k, _| k.starts_with(&prefix_owned),
            extract,
        )
        .await
    }
}

// Internal methods
impl Transaction {
    /// Commits the transaction, applying all buffered writes to the database.
    ///
    /// This validates the transaction for conflicts, applies all writes through
    /// the database layer (which handles WAL logging), and flushes to disk.
    ///
    /// # Commit Protocol (Phase 4: Concurrent Transaction Commits)
    ///
    /// 1. Build write set from buffered writes
    /// 2. Atomically validate + record (brief serialization point)
    /// 3. Apply writes in parallel (grouped by Name)
    /// 4. Flush (group commit batches concurrent flushes)
    ///
    /// The atomic validate+record step prevents race conditions where two
    /// transactions both pass validation before either records their commit.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The transaction has already been committed or aborted
    /// - Conflict detection fails (based on conflict strategy)
    /// - Any write operation fails
    /// - Flush to disk fails
    pub async fn commit(self) -> crate::error::Result<()> {
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

        // Build write set for conflict detection
        let write_set: HashSet<(Name, Key)> = {
            let writes = self.writes.read().await;
            writes.keys().cloned().collect()
        };

        // Atomically validate and record (brief serialization point).
        // This prevents race conditions where two transactions both pass
        // validation before either records their commit.
        self.db
            .txn_manager
            .validate_and_record(
                self.id,
                self.start_timestamp,
                write_set,
                self.conflict_strategy,
            )
            .await?;

        // Apply all buffered writes using transaction-aware methods.
        // Group by Name to avoid concurrent modifications to the same B-tree.
        // Writes within each Name are applied sequentially; different Names run in parallel.
        //
        // Drain the write buffer to take ownership without cloning. This is safe
        // because commit is terminal; the buffer is never accessed after commit.
        let by_name: BTreeMap<Name, Vec<(Key, WriteOp)>> = {
            let mut writes = self.writes.write().await;
            let mut map: BTreeMap<Name, Vec<(Key, WriteOp)>> = BTreeMap::new();
            std::mem::take(&mut *writes).into_iter().for_each(
                |((name, key), op)| {
                    map.entry(name).or_default().push((key, op));
                },
            );
            map
        };

        let db = self.db.clone();
        let txn_id = self.id;
        let start_ts = self.start_timestamp;

        // Process each Name's writes sequentially, but run Names in parallel
        stream::iter(by_name.into_iter())
            .map(|(name, ops)| {
                let db = db.clone();
                async move {
                    // Sequential within this Name (same B-tree)
                    let name = Arc::new(name);
                    stream::iter(ops.into_iter().map(Ok))
                        .try_for_each(|(key, write_op)| {
                            let db = db.clone();
                            let name = Arc::clone(&name);
                            async move {
                                match write_op {
                                    WriteOp::Set(data) => {
                                        let val = data.value.ok_or_else(|| {
                                            StorageError::InvalidConfiguration(
                                                "Set operation has no value".into(),
                                            )
                                        })?;
                                        db.set_with_txn(&name, &key, val, txn_id, start_ts)
                                            .await
                                    }
                                    WriteOp::KillSubtree | WriteOp::Delete => {
                                        db.kill_with_txn(&name, &key, txn_id, start_ts).await
                                    }
                                }
                            }
                        })
                        .await
                }
            })
            .buffer_unordered(16)
            .try_for_each(|()| future::ready(Ok(())))
            .await?;

        // Flush with transaction ID (group commit batches concurrent flushes)
        self.db.flush_with_txn(self.id).await?;

        // Unregister transaction from manager and cleanup old commits
        self.db.txn_manager.complete(self.id).await?;
        self.db.txn_manager.cleanup_old_commits().await;

        // Update state to Committed
        {
            let mut state = self.state.write().await;
            *state = TransactionState::Committed;
        }

        Ok(())
    }

    /// Commits the transaction with retries on retriable errors.
    ///
    /// Like [`commit`](Self::commit), but retries the commit on retriable
    /// errors (e.g., `WriteConflict`, `TransactionTimeout`) up to the
    /// specified number of times.
    ///
    /// Since `Transaction` is `Clone` with shared `Arc` state, cloning for
    /// each retry attempt is cheap and shares the same underlying buffers.
    #[async_recursion]
    pub(crate) async fn commit_with_retry(
        self,
        retries: u32,
    ) -> crate::error::Result<()> {
        let t = self.clone();
        match self.commit().await {
            Ok(()) => Ok(()),
            Err(e) if e.is_retriable() && retries > 0 => {
                t.commit_with_retry(retries - 1).await
            }
            Err(e) => Err(e),
        }
    }

    /// Rolls back the transaction, discarding all buffered writes.
    ///
    /// This is automatically called when a transaction is dropped without
    /// being committed.
    pub async fn rollback(self) -> crate::error::Result<()> {
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

        // Unregister transaction from manager
        self.db.txn_manager.abort(self.id).await?;

        // Update state to Aborted
        {
            let mut state = self.state.write().await;
            *state = TransactionState::Aborted;
        }

        Ok(())
    }

    /// Finishes the transaction based on the result: commits on `Ok`, rolls
    /// back on `Err`.
    ///
    /// This is a convenience method for the common pattern of executing a body
    /// and then committing or rolling back based on whether it succeeded.
    pub async fn finish<T, E>(
        self,
        result: std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: From<crate::error::StorageError>,
    {
        match result {
            Ok(val) => {
                self.commit_with_retry(0).await?;
                Ok(val)
            }
            Err(e) => {
                let _ = self.rollback().await;
                Err(e)
            }
        }
    }

    /// Finishes a transaction with commit retries.
    ///
    /// Like [`finish`](Self::finish), but retries the commit on retriable
    /// errors up to the specified number of times.
    pub async fn finish_with_retry<T, E>(
        self,
        result: std::result::Result<T, E>,
        retries: u32,
    ) -> std::result::Result<T, E>
    where
        E: From<crate::error::StorageError>,
    {
        match result {
            Ok(val) => {
                self.commit_with_retry(retries).await?;
                Ok(val)
            }
            Err(e) => {
                let _ = self.rollback().await;
                Err(e)
            }
        }
    }

    /// Helper to create an error.
    fn err<T>(msg: &str) -> crate::error::Result<T> {
        Err(StorageError::InvalidConfiguration(msg.into()))
    }

    /// Internal recursive implementation of `query()`.
    ///
    /// Uses async recursion to avoid `loop` with `break`/`continue`.
    #[async_recursion]
    async fn query_impl(
        &self,
        name: &Name,
        after: Option<&Key>,
    ) -> Result<Option<Key>> {
        // Get snapshot view from database
        let snapshot_candidate = self.db.query(name, after).await?;

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
        let next =
            match (snapshot_candidate.as_ref(), buffered_candidate.as_ref()) {
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
                    self.query_impl(name, Some(&k)).await
                } else {
                    Ok(Some(k))
                }
            }
        }
    }

    /// Internal recursive implementation of `order()`.
    ///
    /// Merges snapshot subscripts with buffered writes at the specified level.
    #[async_recursion]
    async fn order_impl(
        &self,
        name: &Name,
        prefix: &Key,
        after: Option<&Subscript>,
    ) -> Result<Option<Subscript>> {
        // Get snapshot view from database
        let snapshot_sub = self.db.order(name, prefix, after).await?;

        // Collect buffered subscripts at this level
        let writes = self.writes.read().await;
        let deleted = self.deleted_subtrees.read().await;

        // Find minimum buffered subscript > `after` at this level
        let buffered_sub = writes
            .keys()
            .filter(|(n, _)| n == name)
            .map(|(_, k)| k)
            // Key must start with prefix
            .filter(|k| k.starts_with(prefix) && k.len() > prefix.len())
            // Extract subscript at prefix.len()
            .filter_map(|k| k.get(prefix.len()))
            // Must be > after
            .filter(|sub| match after {
                Some(a) => *sub > a,
                None => true,
            })
            // Must not be deleted
            .filter(|sub| {
                let mut full_key = prefix.clone();
                full_key.push((*sub).clone());
                let not_explicitly_deleted = !matches!(
                    writes.get(&(name.clone(), full_key.clone())),
                    Some(WriteOp::Delete | WriteOp::KillSubtree)
                );
                let not_in_deleted_subtree =
                    !deleted.iter().any(|(del_name, del_key)| {
                        del_name == name && full_key.starts_with(del_key)
                    });
                not_explicitly_deleted && not_in_deleted_subtree
            })
            .min()
            .cloned();

        // Choose the minimum between snapshot and buffered
        let next = match (snapshot_sub.as_ref(), buffered_sub.as_ref()) {
            (Some(snap), Some(buf)) => Some(snap.min(buf).clone()),
            (Some(snap), None) => Some(snap.clone()),
            (None, Some(buf)) => Some(buf.clone()),
            (None, None) => None,
        };

        // Check if candidate subscript's key is deleted; if so, recurse
        match next {
            None => Ok(None),
            Some(sub) => {
                let mut check_key = prefix.clone();
                check_key.push(sub.clone());

                let is_deleted =
                    matches!(
                        writes.get(&(name.clone(), check_key.clone())),
                        Some(WriteOp::Delete | WriteOp::KillSubtree)
                    ) || deleted.iter().any(|(del_name, del_key)| {
                        del_name == name && check_key.starts_with(del_key)
                    });

                if is_deleted {
                    // Recurse to find next valid subscript
                    self.order_impl(name, prefix, Some(&sub)).await
                } else {
                    Ok(Some(sub))
                }
            }
        }
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

    /// Tests for `Transaction::order` ($ORDER semantics).
    mod order_tests {
        use rumps_types::{global, key, value, Subscript};

        use crate::Database;

        #[tokio::test]
        async fn buffered_set_visible() {
            let db = Database::in_memory().unwrap();
            let name = global!("TEST");

            db.transaction(|txn| async move {
                // Buffer some writes (not yet committed)
                txn.set(&name, &key![1, "A"], value!(1)).await?;
                txn.set(&name, &key![1, "B"], value!(2)).await?;
                txn.set(&name, &key![2, "C"], value!(3)).await?;

                // order at root level should see buffered subscripts
                let sub = txn.order(&name, &key![], None).await?;
                assert_eq!(sub, Some(Subscript::from(1)));

                let sub = txn
                    .order(&name, &key![], Some(&Subscript::from(1)))
                    .await?;
                assert_eq!(sub, Some(Subscript::from(2)));

                let sub = txn
                    .order(&name, &key![], Some(&Subscript::from(2)))
                    .await?;
                assert!(sub.is_none());

                // order at nested level
                let sub = txn.order(&name, &key![1], None).await?;
                assert_eq!(sub, Some(Subscript::from("A")));

                let sub = txn
                    .order(&name, &key![1], Some(&Subscript::from("A")))
                    .await?;
                assert_eq!(sub, Some(Subscript::from("B")));

                Ok(())
            })
            .await
            .unwrap();
        }

        #[tokio::test]
        async fn buffered_kill_skipped() {
            let db = Database::in_memory().unwrap();
            let name = global!("TEST");

            // First commit some data
            db.transaction(|txn| {
                let n = name.clone();
                async move {
                    txn.set(&n, &key![1, "A"], value!(1)).await?;
                    txn.set(&n, &key![1, "B"], value!(2)).await?;
                    txn.set(&n, &key![1, "C"], value!(3)).await?;
                    Ok(())
                }
            })
            .await
            .unwrap();

            // Now kill "B" in a new transaction and verify order skips it
            db.transaction(|txn| {
                let n = name.clone();
                async move {
                    txn.kill(&n, &key![1, "B"]).await?;

                    // Should skip "B"
                    let sub = txn.order(&n, &key![1], None).await?;
                    assert_eq!(sub, Some(Subscript::from("A")));

                    let sub = txn
                        .order(&n, &key![1], Some(&Subscript::from("A")))
                        .await?;
                    assert_eq!(sub, Some(Subscript::from("C"))); // "B" skipped

                    let sub = txn
                        .order(&n, &key![1], Some(&Subscript::from("C")))
                        .await?;
                    assert!(sub.is_none());

                    Ok(())
                }
            })
            .await
            .unwrap();
        }

        #[tokio::test]
        async fn kill_subtree_excluded() {
            let db = Database::in_memory().unwrap();
            let name = global!("TEST");

            // Commit data under multiple subscripts
            db.transaction(|txn| {
                let n = name.clone();
                async move {
                    txn.set(&n, &key![1, "X"], value!(1)).await?;
                    txn.set(&n, &key![2, "Y"], value!(2)).await?;
                    txn.set(&n, &key![3, "Z"], value!(3)).await?;
                    Ok(())
                }
            })
            .await
            .unwrap();

            // Kill subscript 2's subtree
            db.transaction(|txn| {
                let n = name.clone();
                async move {
                    txn.kill(&n, &key![2]).await?;

                    // order should skip subscript 2
                    let sub = txn.order(&n, &key![], None).await?;
                    assert_eq!(sub, Some(Subscript::from(1)));

                    let sub = txn
                        .order(&n, &key![], Some(&Subscript::from(1)))
                        .await?;
                    assert_eq!(sub, Some(Subscript::from(3))); // 2 skipped

                    Ok(())
                }
            })
            .await
            .unwrap();
        }

        #[tokio::test]
        async fn merged_snapshot_and_buffer() {
            let db = Database::in_memory().unwrap();
            let name = global!("TEST");

            // Commit some data
            db.transaction(|txn| {
                let n = name.clone();
                async move {
                    txn.set(&n, &key![1], value!(1)).await?;
                    txn.set(&n, &key![3], value!(3)).await?;
                    txn.set(&n, &key![5], value!(5)).await?;
                    Ok(())
                }
            })
            .await
            .unwrap();

            // Buffer additional writes; verify merged iteration
            db.transaction(|txn| {
                let n = name.clone();
                async move {
                    // Add subscript 2 and 4 in buffer
                    txn.set(&n, &key![2], value!(2)).await?;
                    txn.set(&n, &key![4], value!(4)).await?;

                    // Should iterate 1, 2, 3, 4, 5 in order
                    let sub = txn.order(&n, &key![], None).await?;
                    assert_eq!(sub, Some(Subscript::from(1))); // snapshot

                    let sub = txn
                        .order(&n, &key![], Some(&Subscript::from(1)))
                        .await?;
                    assert_eq!(sub, Some(Subscript::from(2))); // buffer

                    let sub = txn
                        .order(&n, &key![], Some(&Subscript::from(2)))
                        .await?;
                    assert_eq!(sub, Some(Subscript::from(3))); // snapshot

                    let sub = txn
                        .order(&n, &key![], Some(&Subscript::from(3)))
                        .await?;
                    assert_eq!(sub, Some(Subscript::from(4))); // buffer

                    let sub = txn
                        .order(&n, &key![], Some(&Subscript::from(4)))
                        .await?;
                    assert_eq!(sub, Some(Subscript::from(5))); // snapshot

                    let sub = txn
                        .order(&n, &key![], Some(&Subscript::from(5)))
                        .await?;
                    assert!(sub.is_none());

                    Ok(())
                }
            })
            .await
            .unwrap();
        }

        #[tokio::test]
        async fn empty_returns_none() {
            let db = Database::in_memory().unwrap();
            let name = global!("TEST");

            db.transaction(|txn| async move {
                let sub = txn.order(&name, &key![], None).await?;
                assert!(sub.is_none());

                let sub = txn.order(&name, &key![1, 2, 3], None).await?;
                assert!(sub.is_none());

                Ok(())
            })
            .await
            .unwrap();
        }

        #[tokio::test]
        async fn buffer_overrides_snapshot() {
            let db = Database::in_memory().unwrap();
            let name = global!("TEST");

            // Commit [1, "A"]
            db.transaction(|txn| {
                let n = name.clone();
                async move {
                    txn.set(&n, &key![1, "A"], value!(1)).await?;
                    Ok(())
                }
            })
            .await
            .unwrap();

            // Kill [1, "A"] in buffer, add [1, "B"]
            db.transaction(|txn| {
                let n = name.clone();
                async move {
                    txn.kill(&n, &key![1, "A"]).await?;
                    txn.set(&n, &key![1, "B"], value!(2)).await?;

                    // First subscript should be "B", not "A"
                    let sub = txn.order(&n, &key![1], None).await?;
                    assert_eq!(sub, Some(Subscript::from("B")));

                    let sub = txn
                        .order(&n, &key![1], Some(&Subscript::from("B")))
                        .await?;
                    assert!(sub.is_none());

                    Ok(())
                }
            })
            .await
            .unwrap();
        }
    }
}
