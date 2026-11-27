//! RPC request types and dispatch.
//!
//! All operations (except admin endpoints) are dispatched through `POST /rpc`.
//! The request body contains an `"op"` field that determines which variant of
//! [`Request`] is deserialized; serde handles both dispatch and field extraction
//! in a single step.

use serde::Deserialize;
use uuid::Uuid;

use crate::error::Result;
use crate::{response, wire, AppState};

/// Wire-format scan direction; `"fwd"` or `"rev"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(crate) enum Dir {
    #[serde(rename = "fwd")]
    Forward,
    #[serde(rename = "rev")]
    Reverse,
}

impl From<Dir> for rumps_types::Dir {
    fn from(d: Dir) -> Self {
        match d {
            Dir::Forward => rumps_types::Dir::Forward,
            Dir::Reverse => rumps_types::Dir::Reverse,
        }
    }
}

/// An RPC request dispatched by the `"op"` field.
#[derive(Debug, Deserialize)]
#[serde(tag = "op")]
pub(crate) enum Request {
    // Session ops
    #[serde(rename = "session.start")]
    SessionStart,
    #[serde(rename = "session.end")]
    SessionEnd,

    // Transaction ops
    #[serde(rename = "txn.start")]
    TxnStart {
        #[serde(rename = "timeout-ms")]
        timeout_ms: Option<u64>,
        isolation: Option<String>,
    },
    #[serde(rename = "txn.commit")]
    TxnCommit { txn: Uuid },
    #[serde(rename = "txn.rollback")]
    TxnRollback { txn: Uuid },

    // Global ops
    #[serde(rename = "global.get")]
    GlobalGet {
        txn: Uuid,
        name: String,
        key: wire::Key,
    },
    #[serde(rename = "global.set")]
    GlobalSet {
        txn: Uuid,
        name: String,
        key: wire::Key,
        value: wire::Value,
    },
    #[serde(rename = "global.increment")]
    GlobalIncrement {
        txn: Uuid,
        name: String,
        key: wire::Key,
        amount: wire::Increment,
    },
    #[serde(rename = "global.kill")]
    GlobalKill {
        txn: Uuid,
        name: String,
        key: wire::Key,
    },
    #[serde(rename = "global.data")]
    GlobalData {
        txn: Uuid,
        name: String,
        key: wire::Key,
    },
    #[serde(rename = "global.order")]
    GlobalOrder {
        txn: Uuid,
        name: String,
        prefix: wire::Key,
        after: Option<wire::Subscript>,
    },
    #[serde(rename = "global.query")]
    GlobalQuery {
        txn: Uuid,
        name: String,
        after: Option<wire::Key>,
    },
    #[serde(rename = "global.collect")]
    GlobalCollect {
        txn: Uuid,
        name: String,
        start: Option<wire::Key>,
        end: Option<wire::Key>,
        #[serde(rename = "start-exclusive", default)]
        start_exclusive: bool,
        #[serde(rename = "end-inclusive", default)]
        end_inclusive: bool,
        dir: Option<Dir>,
        limit: Option<u64>,
    },
    #[serde(rename = "global.list")]
    GlobalList,

    // Local ops
    #[serde(rename = "local.get")]
    LocalGet { name: String, key: wire::Key },
    #[serde(rename = "local.set")]
    LocalSet {
        name: String,
        key: wire::Key,
        value: wire::Value,
    },
    #[serde(rename = "local.increment")]
    LocalIncrement {
        name: String,
        key: wire::Key,
        amount: wire::Increment,
    },
    #[serde(rename = "local.kill")]
    LocalKill { name: String, key: wire::Key },
    #[serde(rename = "local.data")]
    LocalData { name: String, key: wire::Key },
    #[serde(rename = "local.order")]
    LocalOrder {
        name: String,
        prefix: wire::Key,
        after: Option<wire::Subscript>,
    },
    #[serde(rename = "local.query")]
    LocalQuery {
        name: String,
        after: Option<wire::Key>,
    },
    #[serde(rename = "local.collect")]
    LocalCollect {
        name: String,
        start: Option<wire::Key>,
        end: Option<wire::Key>,
        #[serde(rename = "start-exclusive", default)]
        start_exclusive: bool,
        #[serde(rename = "end-inclusive", default)]
        end_inclusive: bool,
        dir: Option<Dir>,
        limit: Option<u64>,
    },
}

impl Request {
    pub(crate) async fn dispatch(
        self,
        _state: &AppState,
    ) -> Result<response::Response> {
        match self {
            Request::SessionStart => todo!(),
            Request::SessionEnd => todo!(),
            Request::TxnStart { .. } => todo!(),
            Request::TxnCommit { .. } => todo!(),
            Request::TxnRollback { .. } => todo!(),
            Request::GlobalGet { .. } => todo!(),
            Request::GlobalSet { .. } => todo!(),
            Request::GlobalIncrement { .. } => todo!(),
            Request::GlobalKill { .. } => todo!(),
            Request::GlobalData { .. } => todo!(),
            Request::GlobalOrder { .. } => todo!(),
            Request::GlobalQuery { .. } => todo!(),
            Request::GlobalCollect { .. } => todo!(),
            Request::GlobalList => todo!(),
            Request::LocalGet { .. } => todo!(),
            Request::LocalSet { .. } => todo!(),
            Request::LocalIncrement { .. } => todo!(),
            Request::LocalKill { .. } => todo!(),
            Request::LocalData { .. } => todo!(),
            Request::LocalOrder { .. } => todo!(),
            Request::LocalQuery { .. } => todo!(),
            Request::LocalCollect { .. } => todo!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn deser_session_start() {
        let req =
            serde_json::from_value::<Request>(json!({"op": "session.start"}))
                .unwrap();
        assert!(matches!(req, Request::SessionStart));
    }

    #[test]
    fn deser_session_end() {
        let req =
            serde_json::from_value::<Request>(json!({"op": "session.end"}))
                .unwrap();
        assert!(matches!(req, Request::SessionEnd));
    }

    #[test]
    fn deser_txn_start_minimal() {
        let req = serde_json::from_value::<Request>(json!({"op": "txn.start"}))
            .unwrap();
        assert!(matches!(
            req,
            Request::TxnStart {
                timeout_ms: None,
                isolation: None,
            }
        ));
    }

    #[test]
    fn deser_txn_start_with_fields() {
        let req = serde_json::from_value::<Request>(json!({
            "op": "txn.start",
            "timeout-ms": 5000,
            "isolation": "snapshot"
        }))
        .unwrap();
        assert!(matches!(
            req,
            Request::TxnStart {
                timeout_ms: Some(5000),
                isolation: Some(ref s),
            } if s == "snapshot"
        ));
    }

    #[test]
    fn deser_txn_commit() {
        let id = Uuid::new_v4();
        let req = serde_json::from_value::<Request>(json!({
            "op": "txn.commit",
            "txn": id.to_string()
        }))
        .unwrap();
        assert!(matches!(req, Request::TxnCommit { txn } if txn == id));
    }

    #[test]
    fn deser_txn_rollback() {
        let id = Uuid::new_v4();
        let req = serde_json::from_value::<Request>(json!({
            "op": "txn.rollback",
            "txn": id.to_string()
        }))
        .unwrap();
        assert!(matches!(req, Request::TxnRollback { txn } if txn == id));
    }

    #[test]
    fn deser_global_get() {
        let id = Uuid::new_v4();
        let req = serde_json::from_value::<Request>(json!({
            "op": "global.get",
            "txn": id.to_string(),
            "name": "PATIENT",
            "key": [123, "NAME"]
        }))
        .unwrap();
        assert!(matches!(
            req,
            Request::GlobalGet { txn, ref name, .. }
            if txn == id && name == "PATIENT"
        ));
    }

    #[test]
    fn deser_global_set() {
        let id = Uuid::new_v4();
        let req = serde_json::from_value::<Request>(json!({
            "op": "global.set",
            "txn": id.to_string(),
            "name": "PATIENT",
            "key": [123, "NAME"],
            "value": "Alice"
        }))
        .unwrap();
        assert!(matches!(
            req,
            Request::GlobalSet { txn, ref name, .. }
            if txn == id && name == "PATIENT"
        ));
    }

    #[test]
    fn deser_global_increment() {
        let id = Uuid::new_v4();
        let req = serde_json::from_value::<Request>(json!({
            "op": "global.increment",
            "txn": id.to_string(),
            "name": "COUNTER",
            "key": ["visits"],
            "amount": 1
        }))
        .unwrap();
        assert!(matches!(req, Request::GlobalIncrement { .. }));
    }

    #[test]
    fn deser_global_kill() {
        let id = Uuid::new_v4();
        let req = serde_json::from_value::<Request>(json!({
            "op": "global.kill",
            "txn": id.to_string(),
            "name": "PATIENT",
            "key": [123]
        }))
        .unwrap();
        assert!(matches!(req, Request::GlobalKill { .. }));
    }

    #[test]
    fn deser_global_data() {
        let id = Uuid::new_v4();
        let req = serde_json::from_value::<Request>(json!({
            "op": "global.data",
            "txn": id.to_string(),
            "name": "PATIENT",
            "key": [123]
        }))
        .unwrap();
        assert!(matches!(req, Request::GlobalData { .. }));
    }

    #[test]
    fn deser_global_order_no_after() {
        let id = Uuid::new_v4();
        let req = serde_json::from_value::<Request>(json!({
            "op": "global.order",
            "txn": id.to_string(),
            "name": "DATA",
            "prefix": [1]
        }))
        .unwrap();
        assert!(matches!(req, Request::GlobalOrder { after: None, .. }));
    }

    #[test]
    fn deser_global_order_with_after() {
        let id = Uuid::new_v4();
        let req = serde_json::from_value::<Request>(json!({
            "op": "global.order",
            "txn": id.to_string(),
            "name": "DATA",
            "prefix": [1],
            "after": "A"
        }))
        .unwrap();
        assert!(matches!(req, Request::GlobalOrder { after: Some(_), .. }));
    }

    #[test]
    fn deser_global_query_no_after() {
        let id = Uuid::new_v4();
        let req = serde_json::from_value::<Request>(json!({
            "op": "global.query",
            "txn": id.to_string(),
            "name": "PATIENT"
        }))
        .unwrap();
        assert!(matches!(req, Request::GlobalQuery { after: None, .. }));
    }

    #[test]
    fn deser_global_query_with_after() {
        let id = Uuid::new_v4();
        let req = serde_json::from_value::<Request>(json!({
            "op": "global.query",
            "txn": id.to_string(),
            "name": "PATIENT",
            "after": [123, "NAME"]
        }))
        .unwrap();
        assert!(matches!(req, Request::GlobalQuery { after: Some(_), .. }));
    }

    #[test]
    fn deser_global_collect_minimal() {
        let id = Uuid::new_v4();
        let req = serde_json::from_value::<Request>(json!({
            "op": "global.collect",
            "txn": id.to_string(),
            "name": "PATIENT"
        }))
        .unwrap();
        assert!(matches!(
            req,
            Request::GlobalCollect {
                start: None,
                end: None,
                start_exclusive: false,
                end_inclusive: false,
                dir: None,
                limit: None,
                ..
            }
        ));
    }

    #[test]
    fn deser_global_collect_full() {
        let id = Uuid::new_v4();
        let req = serde_json::from_value::<Request>(json!({
            "op": "global.collect",
            "txn": id.to_string(),
            "name": "PATIENT",
            "start": [100],
            "end": [200],
            "start-exclusive": true,
            "end-inclusive": true,
            "dir": "rev",
            "limit": 50
        }))
        .unwrap();
        assert!(matches!(
            req,
            Request::GlobalCollect {
                start: Some(_),
                end: Some(_),
                start_exclusive: true,
                end_inclusive: true,
                dir: Some(Dir::Reverse),
                limit: Some(50),
                ..
            }
        ));
    }

    #[test]
    fn deser_global_list() {
        let req =
            serde_json::from_value::<Request>(json!({"op": "global.list"}))
                .unwrap();
        assert!(matches!(req, Request::GlobalList));
    }

    #[test]
    fn deser_local_get() {
        let req = serde_json::from_value::<Request>(json!({
            "op": "local.get",
            "name": "TEMP",
            "key": ["x"]
        }))
        .unwrap();
        assert!(matches!(req, Request::LocalGet { .. }));
    }

    #[test]
    fn deser_local_set() {
        let req = serde_json::from_value::<Request>(json!({
            "op": "local.set",
            "name": "TEMP",
            "key": ["x"],
            "value": 42
        }))
        .unwrap();
        assert!(matches!(req, Request::LocalSet { .. }));
    }

    #[test]
    fn deser_local_increment() {
        let req = serde_json::from_value::<Request>(json!({
            "op": "local.increment",
            "name": "TEMP",
            "key": ["count"],
            "amount": 1
        }))
        .unwrap();
        assert!(matches!(req, Request::LocalIncrement { .. }));
    }

    #[test]
    fn deser_local_kill() {
        let req = serde_json::from_value::<Request>(json!({
            "op": "local.kill",
            "name": "TEMP",
            "key": ["x"]
        }))
        .unwrap();
        assert!(matches!(req, Request::LocalKill { .. }));
    }

    #[test]
    fn deser_local_data() {
        let req = serde_json::from_value::<Request>(json!({
            "op": "local.data",
            "name": "TEMP",
            "key": ["x"]
        }))
        .unwrap();
        assert!(matches!(req, Request::LocalData { .. }));
    }

    #[test]
    fn deser_local_order() {
        let req = serde_json::from_value::<Request>(json!({
            "op": "local.order",
            "name": "TEMP",
            "prefix": []
        }))
        .unwrap();
        assert!(matches!(req, Request::LocalOrder { .. }));
    }

    #[test]
    fn deser_local_query() {
        let req = serde_json::from_value::<Request>(json!({
            "op": "local.query",
            "name": "TEMP"
        }))
        .unwrap();
        assert!(matches!(req, Request::LocalQuery { .. }));
    }

    #[test]
    fn deser_local_collect_minimal() {
        let req = serde_json::from_value::<Request>(json!({
            "op": "local.collect",
            "name": "TEMP"
        }))
        .unwrap();
        assert!(matches!(
            req,
            Request::LocalCollect {
                start: None,
                end: None,
                dir: None,
                limit: None,
                ..
            }
        ));
    }

    #[test]
    fn deser_unknown_op_rejected() {
        assert!(
            serde_json::from_value::<Request>(json!({"op": "unknown.op"}))
                .is_err()
        );
    }

    #[test]
    fn deser_missing_op_rejected() {
        assert!(
            serde_json::from_value::<Request>(json!({"name": "PATIENT"}))
                .is_err()
        );
    }

    #[test]
    fn deser_global_get_missing_txn_rejected() {
        assert!(serde_json::from_value::<Request>(json!({
            "op": "global.get",
            "name": "PATIENT",
            "key": [123]
        }))
        .is_err());
    }

    #[test]
    fn deser_global_set_missing_value_rejected() {
        let id = Uuid::new_v4();
        assert!(serde_json::from_value::<Request>(json!({
            "op": "global.set",
            "txn": id.to_string(),
            "name": "PATIENT",
            "key": [123]
        }))
        .is_err());
    }

    #[test]
    fn deser_dir_forward() {
        let d = serde_json::from_value::<Dir>(json!("fwd")).unwrap();
        assert_eq!(d, Dir::Forward);
    }

    #[test]
    fn deser_dir_reverse() {
        let d = serde_json::from_value::<Dir>(json!("rev")).unwrap();
        assert_eq!(d, Dir::Reverse);
    }

    #[test]
    fn deser_dir_invalid_rejected() {
        assert!(serde_json::from_value::<Dir>(json!("backward")).is_err());
    }

    #[test]
    fn dir_into_rumps() {
        assert_eq!(rumps_types::Dir::Forward, Dir::Forward.into());
        assert_eq!(rumps_types::Dir::Reverse, Dir::Reverse.into());
    }
}
