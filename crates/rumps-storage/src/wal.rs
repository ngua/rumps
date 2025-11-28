//! Write-Ahead Log (WAL) for crash recovery and durability.
//!
//! The WAL ensures durability by logging all modifications before they are
//! applied to the main data file. On crash recovery, uncommitted transactions
//! can be rolled back and committed transactions can be replayed.

use rumps_types::{global, key, Key, Name, Value};
use serde::{Deserialize, Serialize};

use crate::node::NodeData;
use crate::transaction::TransactionId;

/// A record in the Write-Ahead Log.
///
/// Each record represents either a transaction lifecycle event (begin, commit,
/// abort) or a data modification operation (set, kill).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum WalRecord {
    /// Transaction has started.
    TxnBegin {
        /// The transaction's unique identifier.
        txn_id: TransactionId,
    },

    /// Transaction committed successfully.
    ///
    /// All operations from this transaction should be made durable.
    TxnCommit {
        /// The transaction's unique identifier.
        txn_id: TransactionId,
    },

    /// Transaction was aborted.
    ///
    /// All operations from this transaction should be discarded.
    TxnAbort {
        /// The transaction's unique identifier.
        txn_id: TransactionId,
    },

    /// A SET operation within a transaction.
    Set {
        /// The transaction performing this operation.
        txn_id: TransactionId,
        /// The variable name (global or local).
        name: Name,
        /// The key path.
        key: Key,
        /// The previous value (for undo on abort).
        old: Option<NodeData>,
        /// The new value being set.
        new: NodeData,
    },

    /// A KILL operation within a transaction.
    ///
    /// Kills remove an entire subtree rooted at the given key.
    Kill {
        /// The transaction performing this operation.
        txn_id: TransactionId,
        /// The variable name (global or local).
        name: Name,
        /// The key path of the subtree root.
        key: Key,
        /// Serialized subtree data for undo on abort.
        /// Contains all key-value pairs that were deleted.
        subtree: Vec<(Key, NodeData)>,
    },

    /// A checkpoint marker.
    ///
    /// Indicates that all data up to this point has been flushed to the main
    /// data file. WAL entries before this checkpoint can be discarded.
    Checkpoint {
        /// Monotonically increasing checkpoint sequence number.
        seq: u64,
    },
}

// NOTE on tests: We obviously never test `Name::Local`s here because they are
// ephemeral. I.e. we can't (intentionally) round-trip a local, so everything
// below is a global (which is what will really be written)
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txn_begin_roundtrip() {
        let rec = WalRecord::TxnBegin { txn_id: 42.into() };
        let bytes = bincode::serialize(&rec).expect("serialize");
        let decoded: WalRecord =
            bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(rec, decoded);
    }

    #[test]
    fn txn_commit_roundtrip() {
        let rec = WalRecord::TxnCommit { txn_id: 123.into() };
        let bytes = bincode::serialize(&rec).expect("serialize");
        let decoded: WalRecord =
            bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(rec, decoded);
    }

    #[test]
    fn txn_abort_roundtrip() {
        let rec = WalRecord::TxnAbort { txn_id: 999.into() };
        let bytes = bincode::serialize(&rec).expect("serialize");
        let decoded: WalRecord =
            bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(rec, decoded);
    }

    #[test]
    fn set_roundtrip() {
        let rec = WalRecord::Set {
            txn_id: 1.into(),
            name: global!("PATIENT"),
            key: key![123, "NAME"],
            old: None,
            new: NodeData::new(Some("John".into()), false),
        };
        let bytes = bincode::serialize(&rec).expect("serialize");
        let decoded: WalRecord =
            bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(rec, decoded);
    }

    #[test]
    fn set_with_old_value_roundtrip() {
        let rec = WalRecord::Set {
            txn_id: 2.into(),
            name: global!("PATIENT"),
            key: key![123, "NAME"],
            old: Some(NodeData::new(Some("Jane".into()), false)),
            new: NodeData::new(Some("John".into()), false),
        };
        let bytes = bincode::serialize(&rec).expect("serialize");
        let decoded: WalRecord =
            bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(rec, decoded);
    }

    #[test]
    fn kill_roundtrip() {
        let rec = WalRecord::Kill {
            txn_id: 3.into(),
            name: global!("PATIENT"),
            key: key![123],
            subtree: vec![
                (key![123, "NAME"], NodeData::new(Some("John".into()), false)),
                (
                    key![123, "DOB"],
                    NodeData::new(Some("1990-01-01".into()), false),
                ),
            ],
        };
        let bytes = bincode::serialize(&rec).expect("serialize");
        let decoded: WalRecord =
            bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(rec, decoded);
    }

    #[test]
    fn checkpoint_roundtrip() {
        let rec = WalRecord::Checkpoint { seq: 12345 };
        let bytes = bincode::serialize(&rec).expect("serialize");
        let decoded: WalRecord =
            bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(rec, decoded);
    }

    #[test]
    fn all_value_types_roundtrip() {
        let values: [Value; 4] =
            ["hello".into(), 42i64.into(), 3.14f64.into(), true.into()];

        values
            .into_iter()
            .enumerate()
            .try_for_each(|(i, val)| {
                let rec = WalRecord::Set {
                    txn_id: (i as u64).into(),
                    name: global!("TEST"),
                    key: key![i as i64],
                    old: None,
                    new: NodeData::new(Some(val), false),
                };
                let bytes = bincode::serialize(&rec)?;
                let decoded: WalRecord = bincode::deserialize(&bytes)?;
                assert_eq!(rec, decoded);
                Ok::<_, Box<bincode::ErrorKind>>(())
            })
            .expect("roundtrip");
    }
}
