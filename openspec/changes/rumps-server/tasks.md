## 1. Crate Scaffolding

- [x] 1.1 Create `crates/rumps-server/` with `Cargo.toml` depending on `rumps-storage`, `rumps-types`, `axum`, `tokio`, `serde`, `serde_json`, `uuid`, `tower`, `tower-http`, `toml`, `dashmap`, `tracing`, `tracing-subscriber`
- [x] 1.2 Add workspace dependencies for new crates (`axum`, `uuid`, `tower`, `tower-http`, `toml`, `dashmap`, `clap`) in root `Cargo.toml`
- [x] 1.3 Create `src/lib.rs` (server builder, public API) and `src/main.rs` (CLI binary with clap)
- [x] 1.4 Implement TOML config loading (`config::Config` struct with `config::Server`, `config::Database`, `config::Session`, `config::Transaction`, `config::Auth` sections)

## 2. Wire Encoding Types

- [x] 2.1 Implement `wire::Subscript` newtype with custom `Deserialize`: JSON primitive inference + `$`-prefixed tag parsing; `Serialize` with tags for `Char`/`Json` and bare JSON for others
- [x] 2.2 Implement `wire::Value` newtype with custom `Deserialize`: JSON primitive inference + `$`-prefixed tag parsing; `Serialize` with tags for `Char`/`Json` and bare JSON for others
- [x] 2.3 Implement `wire::Key` as `Vec<wire::Subscript>` with `From`/`Into` conversions to `Key`
- [x] 2.4 Implement `wire::Increment` with inference (JSON int -> `Increment::Integer`, JSON float -> `Increment::Double`) and tag support
- [x] 2.5 Implement `wire::DataStatus` serialization (numeric `$DATA` codes: `0`, `1`, `10`, `11`)
- [x] 2.6 Unit tests for all wire type round-trips: inference, explicit tags, edge cases (`$char` validation, `$json` nesting, tag type mismatches: `{"$bool": 42}`, `{"$str": 42}`, `{"$char": ""}`, negative float increment deser)

## 3. RPC Protocol and Dispatch

- [x] 3.1 Create `src/rpc.rs`; define `rpc::Request` as a `#[serde(tag = "op")]` enum where each variant carries its own typed fields (e.g. `GlobalGet { txn: Uuid, name: String, key: wire::Key }`). Serde handles both op dispatch and field extraction in one deserialization step; unknown ops fail at deserialization with a `400`.
- [x] 3.2 Implement `Request::dispatch` method: takes `self` (by value, so fields can be moved into handlers without cloning), takes shared state, returns `Result<response::Response>`, matches on `self`, and dispatches to per-op handlers (initially stubs returning `todo!()`; filled in as handlers land in sections 4-7)
- [x] 3.3 Create `src/response.rs`; define per-op response structs (`response::Value`, `response::Ok`, `response::Session`, `response::Txn`, `response::Increment`, `response::Data`, `response::Order`, `response::Query`, `response::ListGlobals`) with `Serialize`; unified `response::Response` enum with `IntoResponse` impl; `response::NdjsonStream` for NDJSON streaming; `Request::dispatch` returns `Result<response::Response>` (no Axum types in dispatch logic)
- [x] 3.4 Create `src/router.rs` and wire up Axum router: `POST /rpc` -> Axum handler that deserializes body into `rpc::Request` and calls `Request::dispatch`, `GET /health`, `GET /status`

## 4. Session Management

- [x] 4.1 Create `src/session.rs` and implement `Session` struct (external UUID, internal `u64` ID, last activity timestamp, owned transaction set, `HashSet<SmolStr>` of used local names)
- [x] 4.2 Implement `session::Manager` with `DashMap<Uuid, Session>`, `AtomicU64` counter, shared `Database::in_memory()` for locals, `create()`, `get()`, `destroy()` methods. `destroy()` rolls back owned transactions and calls `kill(&Name::local(name), &key![internal_id])` for each used local name.
- [ ] 4.3 Implement session reaper: background `tokio::spawn` task that periodically scans for idle sessions and destroys them via `session::Manager::destroy()`
- [ ] 4.4 Implement `Session` Axum extractor (`impl FromRequestParts`) that reads and validates the `X-Session-Id` header and injects the session into the handler. Only `session.start` does not require a session and omits this extractor from their handler signature. Every other op goes through the session gate, including `session.end`.
- [ ] 4.5 Implement `session.start` and `session.end` op handlers

## 5. Transaction Management

- [ ] 5.1 Create `src/transaction.rs`; implement `transaction::Store` with `DashMap<Uuid, (Uuid, Transaction, Instant)>` (txn token -> session id, transaction, last use)
- [ ] 5.2 Implement `txn.start` handler: build transaction from `Database`, optional isolation/timeout, store in `transaction::Store`
- [ ] 5.3 Implement `txn.commit` handler: remove the `Transaction` from the `DashMap` first (taking ownership), then call `.commit()` on the removed value. Removal must happen before the async commit call to avoid holding a `DashMap` ref across an await.
- [ ] 5.4 Implement `txn.rollback` handler: remove the `Transaction` from the `DashMap` first (taking ownership), then call `.rollback()` on the removed value. Same ownership constraint as commit.
- [ ] 5.5 Implement transaction token validation: lookup in store, verify session ownership, update last-use timestamp
- [ ] 5.6 Implement transaction reaper: background task that rolls back idle transactions

## 6. Shared Handler Logic and Operation Handlers

Global and local ops differ only in how the "database context" is obtained: global ops look up a `Transaction` by token, local ops use the session's in-memory `Database`. To avoid duplicating every handler, define a trait abstracting over both:

```rust
trait Context {
    async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>>;
    async fn set(&self, name: &Name, key: &Key, val: Value) -> Result<()>;
    async fn increment(&self, name: &Name, key: &Key, amt: Increment) -> Result<Value>;
    async fn kill(&self, name: &Name, key: &Key) -> Result<()>;
    async fn data(&self, name: &Name, key: &Key) -> Result<DataStatus>;
    async fn order(&self, name: &Name, key: &Key, after: Option<&Subscript>) -> Result<Option<Subscript>>;
    async fn query(&self, name: &Name, after: Option<&Key>) -> Result<Option<Key>>;
}
```

Implement for both `&Transaction` and session-scoped `&Database`. Each op handler is generic over `Context`; the `rpc::Request` match arm resolves which context to use and calls the shared handler.

- [ ] 6.1 Create `src/handler.rs`; define `handler::Context` trait with the methods above
- [ ] 6.2 Implement `handler::Context` for `Transaction` (delegates to `Transaction::get`, etc.)
- [ ] 6.3 Implement `handler::Context` for session-local context: wraps the shared in-memory `Database` and the session's internal `u64` ID. Prepends the session ID to keys on the way in, strips it on the way out. Tracks used local names in the session's `HashSet<SmolStr>` on write ops.
- [ ] 6.4 Implement shared `get` handler: deserialize `wire::Key`, call `Context::get`, serialize response
- [ ] 6.5 Implement shared `set` handler: deserialize `wire::Key` + `wire::Value`, call `Context::set`
- [ ] 6.6 Implement shared `increment` handler: deserialize `wire::Key` + `wire::Increment`, call `Context::increment`, serialize new value
- [ ] 6.7 Implement shared `kill` handler: deserialize `wire::Key`, call `Context::kill`
- [ ] 6.8 Implement shared `data` handler: deserialize `wire::Key`, call `Context::data`, serialize `wire::DataStatus`
- [ ] 6.9 Implement shared `order` handler: deserialize `wire::Key` (prefix) + optional `wire::Subscript` (after), call `Context::order`, serialize response
- [ ] 6.10 Implement shared `query` handler: deserialize optional `wire::Key` (after), call `Context::query`, serialize response
- [ ] 6.11 Implement `global.list`: call `Database::list_globals`, serialize response. This op is session-exempt and does not use `Context`.

## 7. Global/Local Dispatch Wiring

- [ ] 7.1 Wire `global.*` variants in the `rpc::Request` match to resolve the `Transaction` from the `txn` token and call the shared handler with the transaction as `handler::Context`
- [ ] 7.2 Wire `local.*` variants in the `rpc::Request` match to resolve the session's local `Database` and call the shared handler with it as `handler::Context`

## 8. NDJSON Streaming (collect)

- [ ] 8.1 Implement `Scan` construction from wire fields using `rumps-types` types: `start`/`end` -> `Bounds::new(...)` with `Bound::Included`/`Bound::Excluded`/`Bound::Unbounded`; `dir` -> `Dir::Forward`/`Dir::Reverse` (see wire-encoding `Dir` spec); combine into `Scan::new(bounds, dir)`. Default to `Bounds::full()` and `Dir::Forward`.
- [ ] 8.2 Implement `global.collect`: build `Scan`, call `Transaction::collect` with `pred: |_, _| true` and `extract: |k: &Key, v: &Option<Value>| -> Option<(wire::Key, Option<wire::Value>)> { Some((wire::Key::from(k.clone()), v.as_ref().map(wire::Value::from))) }`. Note: `collect` takes `pred: Fn(&Key, &Option<Value>) -> bool` and `extract: Fn(&Key, &Option<Value>) -> Option<T>`. Map the resulting stream items to JSON lines and return via `Body::from_stream()`.
- [ ] 8.3 Implement `local.collect`: same as above but on the shared in-memory `Database`. Prepend session internal ID to scan bounds, strip the session ID prefix from keys in the NDJSON output.
- [ ] 8.4 Implement `limit` support: wrap stream in `.take(limit)` when present
- [ ] 8.5 Integration test: stream multiple entries, verify NDJSON format and ordering

## 9. Authentication

- [ ] 9.1 Implement `Auth` Axum extractor (`impl FromRequestParts`): reads `Authorization: Bearer <token>` from the request, compares to the configured token, and produces a typed `Auth` value on success or returns `401` on failure. In `--no-auth` mode, the extractor always succeeds with a default `Auth` context.
- [ ] 9.2 Add `Auth` extractor to the `rpc` signature. Handlers for health, status, and `global.list` omit it, naturally bypassing auth with no special logic.
- [ ] 9.3 Refuse to start without auth config and without `--no-auth`; print loud warning to stderr when `--no-auth`

## 10. Error Handling

- [ ] 10.1 Implement `StorageError` -> HTTP status code mapping (per server-errors spec) in the unified `error::Error` type
- [ ] 10.2 Implement `IntoResponse` for `error::Error`, producing `{"error": "..."}` JSON bodies with appropriate HTTP status codes
- [ ] 10.3 Wire up error handling in all op handlers: convert `rumps_types::Error` to `error::Error`. All fallible functions use the crate's `Result` alias (`use error::Result`); use `result::Result` when `std`'s `Result` is needed.
- [ ] 10.4 Handle request validation errors (missing fields, bad JSON, unknown ops) as `400`

## 11. Admin Endpoints

- [ ] 11.1 Implement `GET /health` returning `{"ok": true}`
- [ ] 11.2 Implement `GET /status` returning JSON with: `session-count`, `transaction-count`, and `DatabaseStats` fields: `root-count`, `active-txns`, `cache` (hit/miss/eviction), `storage` (page count, file size), `in-memory`

## 12. Integration Tests

- [ ] 12.1 End-to-end test: session lifecycle (start, use, end)
- [ ] 12.2 End-to-end test: transaction lifecycle (start, global ops, commit/rollback)
- [ ] 12.3 End-to-end test: local ops across sessions (isolation check)
- [ ] 12.4 End-to-end test: wire encoding round-trips (all type variants including tags)
- [ ] 12.5 End-to-end test: NDJSON streaming with bounded scans
- [ ] 12.6 End-to-end test: error responses (missing txn, expired session, write conflict)
- [ ] 12.7 End-to-end test: auth (valid token, invalid token, missing token, `--no-auth`)
- [ ] 12.8 End-to-end test: NDJSON streaming disconnection/partial read (client disconnects mid-stream; verify server does not leak resources)

## 13. Observability

- [ ] 13.1 Add `tracing` dependency; set up `tracing-subscriber` in `main.rs` with structured JSON logging
- [ ] 13.2 Instrument `rpc` with a `tracing::Span` per request containing: op name, session ID, transaction ID (when present), and a generated request ID
- [ ] 13.3 Add `tracing` spans to session and transaction lifecycle events (create, destroy, reap, commit, rollback)

## 14. Graceful Shutdown

- [ ] 14.1 Implement graceful shutdown on `SIGTERM`/`SIGINT`: stop accepting new connections, drain in-flight requests using `axum::serve`'s `with_graceful_shutdown`
- [ ] 14.2 On shutdown, roll back all active transactions and destroy all sessions
- [ ] 14.3 Ensure the WAL is flushed and the database is closed cleanly before process exit

## 15. Request Safety

- [ ] 15.1 Add `tower-http` `RequestBodyLimit` layer to cap request body size (default 1 MB, configurable). This prevents unbounded memory allocation from oversized request bodies.
