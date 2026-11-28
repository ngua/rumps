//! Per-op response types and unified `Response` enum.
//!
//! Each operation returns a typed struct. `Response` wraps all variants and
//! implements `IntoResponse` to handle both JSON bodies and NDJSON streams.

use std::pin::Pin;

use axum::body::Body;
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use bytes::Bytes;
use futures::Stream;
use serde::Serialize;

use crate::error::Result;
use crate::wire;

pub(crate) enum Response {
    Value(Value),
    Ok(Ok),
    Session(Session),
    Txn(Txn),
    Increment(Increment),
    Data(Data),
    Order(Order),
    Query(Query),
    ListGlobals(ListGlobals),
    Stream(NdjsonStream),
}

impl IntoResponse for Response {
    fn into_response(self) -> axum::response::Response {
        match self {
            Self::Value(v) => Json(v).into_response(),
            Self::Ok(v) => Json(v).into_response(),
            Self::Session(v) => Json(v).into_response(),
            Self::Txn(v) => Json(v).into_response(),
            Self::Increment(v) => Json(v).into_response(),
            Self::Data(v) => Json(v).into_response(),
            Self::Order(v) => Json(v).into_response(),
            Self::Query(v) => Json(v).into_response(),
            Self::ListGlobals(v) => Json(v).into_response(),
            Self::Stream(s) => (
                [(header::CONTENT_TYPE, "application/x-ndjson")],
                Body::from_stream(s.0),
            )
                .into_response(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct Value {
    pub(crate) value: Option<wire::Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Ok {}

#[derive(Debug, Serialize)]
pub(crate) struct Session {
    pub(crate) id: crate::Id<Session>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Txn {
    pub(crate) txn: crate::Id<Txn>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Increment {
    pub(crate) value: wire::Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct Data {
    pub(crate) status: wire::DataStatus,
}

#[derive(Debug, Serialize)]
pub(crate) struct Order {
    pub(crate) next: Option<wire::Subscript>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Query {
    pub(crate) next: Option<wire::Key>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ListGlobals {
    pub(crate) globals: Vec<String>,
}

/// Pre-serialized NDJSON byte stream.
pub(crate) struct NdjsonStream(
    pub(crate) Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>,
);
