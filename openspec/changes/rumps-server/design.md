## Context

RUMPS is an embedded database accessed exclusively through its Rust API. Remote clients, non-Rust languages, and network-distributed tooling have no access path. The `rumps-storage` crate is deliberately dependency-free from server concerns; networking belongs in a separate crate.

The existing API surface (`Database`, `Transaction`, `TransactionBuilder`) is well-defined with clear ownership semantics (transactions own write buffers, sessions own locals). The challenge is mapping these Rust-native ownership patterns to a stateless HTTP protocol.

## Goals / Non-Goals

**Goals:**
- HTTP RPC server in a new `rumps-server` crate that provides remote access to all core RUMPS operations
- Type-safe wire encoding that preserves the full RUMPS type system (all `Subscript` and `Value` variants) while keeping the common case (numbers, strings, bools) ergonomic
- Session and transaction lifecycle management with timeout-based reaping
- Streaming support for `collect` operations via NDJSON
- Pluggable auth via Axum extractor

**Non-Goals:**
- WebSocket support (future consideration)
- Rate limiting (defer to reverse proxy)
- TLS termination (defer to reverse proxy)
- Query language support (lives in `rumps-query`)
- Batch/pipeline requests (single op per request for v1)
- Client libraries (server-only; clients use raw HTTP)

## Decisions

### 1. Crate structure

`crates/rumps-server/` with both `lib.rs` (for embedding/testing) and `main.rs` (CLI binary). The library exposes the server builder; the binary handles CLI args, config loading, and startup.

**Why:** Separating lib from bin lets users embed the server in custom applications and makes integration testing straightforward.

### 2. Wire encoding: type inference with `$`-prefixed tags

JSON primitives are inferred to RUMPS types; exotic types use `$`-prefixed tags.

**Inference rules (request deserialization):**

| JSON value        | Subscript             | Value              |
|-------------------|-----------------------|--------------------|
| `true`/`false`    | `Boolean`             | `Boolean`          |
| integer (no `.`)  | `Number(n as f64)`    | `Integer(n)`       |
| float (has `.`)   | `Number(n)`           | `Double(n)`        |
| `"string"`        | `String`              | `String`           |
| object/array      | parse as tag; else error | parse as tag; else error |

Note: `Subscript::Number` is always `f64` regardless of whether the JSON was integer or float. `Value` distinguishes `Integer` from `Double` based on whether the JSON number has a fractional part.

**Explicit tags:** `{"$bool": true}`, `{"$int": 42}`, `{"$float": 42}`, `{"$char": "A"}`, `{"$str": "hello"}`, `{"$json": {...}}`

**Response serialization:** Always uses tags for `Char` and `Json` to enable lossless round-tripping. `Boolean`, `Integer`, `Double`, `String` use bare JSON.

**Implementation:** Custom `serde::Deserialize` for a `WireSubscript` / `WireValue` newtype that wraps the core types. The `Deserialize` impl uses `serde_json::Value` as an intermediate: check if it's a tagged object (single key starting with `$`), otherwise infer.

**Alternatives considered:**
- Full tagging always: rejected; makes the common case verbose (`{"$int": 123}` vs `123`)
- Infer everything including JSON: rejected; ambiguity between `{"a": 1}` as a tagged type vs a JSON subscript is irrecoverable

### 3. Session-scoped locals via shared in-memory DB with key prefix

All sessions share a single `Database::in_memory()` for local variables. Each session is assigned a monotonic `u64` internal ID (from an `AtomicU64` counter). Local ops prepend this ID as a `Subscript::Number` to the key: `local.set(name: "TEMP", key: ["x"])` becomes `db.set(&local!("TEMP"), &key![session_n, "x"], val)`.

Session destruction kills the prefix: for each local name the session wrote to (tracked in a `HashSet<SmolStr>`), call `db.kill(&Name::local(name), &key![session_n])`.

The external token (UUID v4, returned to client) maps to the internal `u64` ID. The client never sees the internal ID; keys are prefixed on the way in and stripped on the way out.

**Why:** One shared `Database::in_memory()` avoids per-session overhead (no extra `BTree`, `TransactionManager`, page allocator per session). Cleanup is a single `kill` per used local name; no database to drop. The key-prefix scheme reuses the existing tree semantics (`order`, `data`, `collect`) with zero special casing.

**Why `u64` not UUID for internal prefix:** A `Subscript::Number(n)` is 8 bytes; a UUID string subscript is 36 bytes. The counter is monotonic and collision-free within a server lifetime.

**Alternative considered:** Per-session `Database::in_memory()`. Rejected; each instance allocates a full `BTree`, `TransactionManager`, and root map. The shared-DB approach scales to thousands of sessions with constant overhead.

### 4. Transaction management: server-held `Transaction` objects

The server holds `Transaction` objects in a `DashMap<Uuid, (SessionId, Transaction)>`. On `txn.start`, a `TransactionBuilder` is configured and `.start().await` is called, yielding a `Transaction` that is stored. On `global.*` ops, the `Transaction` is looked up by token and borrowed for the operation. On `txn.commit`/`txn.rollback`, the `Transaction` is removed from the map and `.commit()`/`.rollback()` is called.

**Why `Transaction` is `Clone`:** The existing `Transaction` type uses `Arc`-wrapped internal state, so cloning is cheap and multiple `global.*` ops can reference the same transaction concurrently.

**Reaping:** A background `tokio` task runs periodically (e.g. every 5s), iterating the transaction map and rolling back any transaction whose last-use timestamp exceeds the idle timeout.

### 5. NDJSON streaming with no pagination

`collect` ops stream `application/x-ndjson` using `axum::body::Body::from_stream()`. The stream is produced by `Transaction::collect()` (or `Database::collect()` for locals), mapped to JSON lines. The `limit` parameter, if present, wraps the stream in `.take(limit)`.

**Why no pagination:** Pagination requires either server-side cursor state (memory pressure, reaping complexity) or client-side re-seeking (expensive for B-tree prefix scans). NDJSON streaming is universally supported and avoids both problems. Clients that disconnect can resume by re-issuing with an updated `start` bound.

### 6. Axum router structure

```
Router::new()
    .route("/health", get(health))
    .route("/status", get(status))
    .route("/rpc", post(rpc))
    .with_state(AppState { db, config, sessions, transactions })
```

Auth and session validation are handled via Axum extractors in handler signatures, not middleware layers. See sections 7 and the session-management spec.

**Dispatch pattern:** `RpcRequest` is a `#[serde(tag = "op")]` enum. Each variant carries its own typed fields. Invalid or unknown ops fail at deserialization. The `rpc` deserializes the request body into `RpcRequest` and matches on the variant:

```rust
#[serde(tag = "op")]
enum RpcRequest {
    #[serde(rename = "session.start")]
    SessionStart,
    #[serde(rename = "session.end")]
    SessionEnd,
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
    #[serde(rename = "global.get")]
    GlobalGet { txn: Uuid, name: String, key: WireKey },
    #[serde(rename = "global.set")]
    GlobalSet { txn: Uuid, name: String, key: WireKey, value: WireValue },
    // ... remaining variants follow the same pattern
}
```

No string dispatch, no dynamic dispatch, no trait objects, no registry. The op set is small and fixed; serde handles both dispatch and field extraction in one step.

### 7. Auth as Axum extractor

Auth is an Axum extractor (`impl FromRequestParts`) that produces a typed `Auth` value. Handlers that require authentication declare `Auth(ctx)` in their parameter list. Handlers that do not need auth (health, status) simply omit the extractor; no bypass logic or path-matching is needed.

The built-in implementation reads the `Authorization: Bearer <token>` header and compares against the configured token. In `--no-auth` mode, the extractor always succeeds with a default context.

**Why not Tower middleware:** A Tower layer requires conditional application (changing the `Router` type), explicit bypass for unprotected routes, and a no-op passthrough variant. The extractor approach makes auth a typed value in the handler signature: present means required, absent means open. Different auth schemes become different extractor types. This is more type-driven and simpler.

**Extensibility:** Users who embed the server via the library can provide their own `FromRequestParts` implementation for `Auth`. The server builder accepts a custom extractor factory.

### 8. Server config

TOML config file for server settings: bind address, port, database path, session idle timeout, transaction idle timeout, auth method and parameters.

```toml
[server]
bind = "0.0.0.0"
port = 7700

[database]
path = "./data"

[session]
idle_timeout_secs = 300

[transaction]
idle_timeout_secs = 30

[auth]
method = "bearer"
token = "my-secret-token"
```

CLI args override config file values. `--no-auth` overrides `[auth]` section.

## Risks / Trade-offs

**[Transaction holding across HTTP requests]** Transactions are held in server memory between requests. If a client starts a transaction and disappears, the transaction holds resources until reaped. Mitigation: Aggressive default timeout (30s), background reaper task.

**[NDJSON streaming and backpressure]** If a client reads slowly, the server buffers NDJSON lines. `axum`/`hyper` handle TCP backpressure at the transport level, but the stream producer (`collect`) may still allocate B-tree pages ahead of consumption. Mitigation: The existing `collect` implementation is lazy (cursor-based); it only fetches pages as the stream is consumed.

**[Type inference ambiguity at JSON boundaries]** `serde_json` represents both `42` and `42.0` as `Number`. The `is_i64()` / `is_f64()` methods on `serde_json::Number` are used to distinguish them, but some JSON producers may not preserve this distinction. Mitigation: Explicit tags (`$int`, `$float`) are always available as a fallback.

**[Single-threaded dispatch per request]** Each RPC request is handled in a single Tokio task. For `collect` streams this means one task per stream. This is fine for expected concurrency levels but could be a bottleneck under extreme load. Mitigation: Tokio's work-stealing scheduler distributes tasks across cores.

## Resolved Questions

- **`global.get` on missing key:** Returns `200` with `{"value": null}`. The Rust API returns `Ok(None)`, not `Err(KeyNotFound)`. `KeyNotFound` is an internal B-tree error mapped to `500`.
- **`local.list`:** Not supported. The Rust `Database::list_globals` only lists globals; locals are not enumerable at the database level.
- **`GET /status` response shape:** Returns `session-count`, `transaction-count`, and the subset of `DatabaseStats` fields useful for monitoring: `root-count`, `active-txns`, `cache` (hit/miss/eviction counts), `storage` (page count, file size), and `in-memory` (whether the DB is memory-only).
