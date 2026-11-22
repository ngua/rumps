## Why

RUMPS currently only supports embedded access via the Rust API. Remote clients (other languages, network-distributed systems, tooling) have no way to interact with the database. An HTTP RPC server in a new `rumps-server` crate provides remote access while keeping `rumps-storage` free of server concerns.

## What Changes

- New `rumps-server` crate in the workspace with `axum`, `tokio`, `serde_json`, `uuid` dependencies
- Single RPC endpoint (`POST /rpc`) dispatching JSON-encoded operations; no REST resource mapping (RUMPS tree ops don't map naturally to REST)
- Type-inferred JSON wire encoding for `Subscript`, `Value`, and `Increment` with `$`-prefixed tags for explicit type control (`$int`, `$float`, `$char`, `$bool`, `$str`, `$json`)
- Session management: UUID-identified sessions with configurable idle timeout and reaping; each session owns an in-memory `Database` for locals
- Transaction management: `txn.start`/`txn.commit`/`txn.rollback` ops that map to `TransactionBuilder`/`Transaction`; transaction tokens (UUIDs) required on all `global.*` ops
- Full operation set mirroring the Rust API: `get`, `set`, `increment`, `kill`, `data`, `order`, `query`, `collect` for both `global.*` and `local.*` namespaces
- `collect` streams NDJSON (`application/x-ndjson`) with full `Scan` support (bounds, direction, limit); no pagination
- Admin endpoints: `GET /health`, `GET /status`
- Tower-layered authentication middleware; ships with bearer token auth, refuses to start without auth unless `--no-auth`
- Structured error responses mapping `StorageError` variants to HTTP status codes

## Capabilities

### New Capabilities
- `wire-encoding`: JSON wire format for RUMPS types (`Subscript`, `Value`, `Key`, `Increment`, `DataStatus`) with type inference from JSON primitives and `$`-prefixed tags for explicit typing; serde (de)serialization between wire JSON and core types
- `rpc-protocol`: RPC request/response envelope, operation dispatch, `POST /rpc` endpoint; defines the op string namespace (`session.*`, `txn.*`, `global.*`, `local.*`) and per-op request/response shapes
- `session-management`: Server-side session lifecycle (create, destroy, idle-reap); per-session in-memory `Database` for locals; session token validation middleware
- `transaction-management`: Server-side transaction lifecycle mapping to `TransactionBuilder`/`Transaction`; transaction token tracking, timeout/reap, binding to `global.*` ops
- `ndjson-streaming`: NDJSON response streaming for `collect` ops; `Scan`-based parameterization (bounds, direction, limit) over the wire
- `server-auth`: Tower-layered authentication middleware; built-in bearer token scheme; `--no-auth` escape hatch; designed for user extension (mTLS, OAuth, etc.)
- `server-errors`: Mapping from `rumps_types::Error`/`StorageError` to HTTP status codes and JSON error bodies

### Modified Capabilities

## Impact

- New crate: `crates/rumps-server/` with `Cargo.toml`, `src/lib.rs`, `src/main.rs`
- `Cargo.toml` (workspace root): add `rumps-server` to members, add `axum`, `uuid`, `tower`, `tower-http` to workspace dependencies
- `rumps-types`: may need `serde_json` `Serialize`/`Deserialize` impls for wire-format variants of `Subscript`, `Value`, `Key`, `Increment`, `DataStatus`, `Dir`, `Bounds` (or new wrapper types in `rumps-server` to avoid coupling)
- No changes to `rumps-storage` internals; server depends on its public API only
