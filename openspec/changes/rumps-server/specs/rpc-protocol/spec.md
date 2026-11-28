## ADDED Requirements

### Requirement: Single RPC endpoint
All operations (except admin endpoints) SHALL be dispatched through `POST /rpc` with `Content-Type: application/json`. The request body MUST contain an `"op"` field identifying the operation. Unknown ops SHALL return `400`.

#### Scenario: Valid op dispatch
- **WHEN** a `POST /rpc` request contains `{"op": "global.get", "txn": "...", "name": "PATIENT", "key": [123]}`
- **THEN** the server dispatches to the `global.get` handler and returns a JSON response

#### Scenario: Unknown op
- **WHEN** a `POST /rpc` request contains `{"op": "unknown.op"}`
- **THEN** the server returns `400` with `{"error": "unknown operation: unknown.op"}`

#### Scenario: Missing op field
- **WHEN** a `POST /rpc` request contains `{"name": "PATIENT"}`
- **THEN** the server returns `400` with an error indicating the `op` field is required

### Requirement: Op namespace convention
Operations SHALL be namespaced with dot-separated prefixes: `session.*`, `txn.*`, `global.*`, `local.*`. The namespace determines validation rules (e.g. `global.*` requires `txn` field).

#### Scenario: Namespace determines txn requirement
- **WHEN** a request contains `{"op": "global.get", "name": "PATIENT", "key": [123]}` without a `"txn"` field
- **THEN** the server returns `400` with `{"error": "global operations require a transaction token"}`

### Requirement: Session operations
The system SHALL support `session.start` and `session.end` operations. `session.start` SHALL return a UUID session token. `session.end` SHALL require the `X-Session-Id` header and destroy the session, its locals, and any active transactions.

#### Scenario: Start session
- **WHEN** a request contains `{"op": "session.start"}`
- **THEN** the response contains `{"id": "<uuid>"}` and a session is created server-side

#### Scenario: End session
- **WHEN** a request contains `{"op": "session.end"}` with a valid `X-Session-Id` header
- **THEN** the session is destroyed, its in-memory locals are dropped, and any active transactions are rolled back

#### Scenario: End session with invalid token
- **WHEN** a request contains `{"op": "session.end"}` with an invalid or expired `X-Session-Id`
- **THEN** the server returns `410` with an error indicating the session was not found

### Requirement: Session token header
All operations except `session.start`, `GET /health`, and `GET /status` SHALL require a valid `X-Session-Id` header. The session gate is two-phase: an Axum extractor (`session::Gate`) attempts to resolve the header on every `POST /rpc` request, but always succeeds; it stores `None` when the header is absent, malformed, or references an unknown/expired session. Individual operations then call `session::Gate::require()` to enforce session presence, returning `401` when the session is `None`. This means `session.start` is never rejected by a stale or invalid `X-Session-Id` header; only operations that call `require()` enforce session presence.

#### Scenario: Missing session header
- **WHEN** a request to `POST /rpc` with `{"op": "global.get", ...}` lacks the `X-Session-Id` header
- **THEN** `session::Gate` resolves to `None`, `require()` rejects, and the server returns `401` with `{"error": "session token required"}`

#### Scenario: Expired session header
- **WHEN** a request to `POST /rpc` with `{"op": "global.get", ...}` uses an `X-Session-Id` for a session that has been reaped
- **THEN** `session::Gate` resolves to `None`, `require()` rejects, and the server returns `401` with `{"error": "session token required"}`

#### Scenario: Stale header on session.start
- **WHEN** a `session.start` request includes an `X-Session-Id` header referencing an expired or invalid session
- **THEN** `session::Gate` resolves to `None`, `session.start` does not call `require()`, and the request succeeds normally

### Requirement: Global operations
The system SHALL support the following `global.*` ops, each requiring a `"txn"` field with a valid transaction token and a `"name"` field with the global variable name:

| Op                 | Required Fields       | Response                    | Rust API                 |
|--------------------|-----------------------|-----------------------------|--------------------------|
| `global.get`       | `name`, `key`         | `{"value": ...}`            | `Transaction::get`       |
| `global.set`       | `name`, `key`, `value`| `{"ok": true}`              | `Transaction::set`       |
| `global.increment` | `name`, `key`, `amount`| `{"value": ...}`           | `Transaction::increment` |
| `global.kill`      | `name`, `key`         | `{"ok": true}`              | `Transaction::kill`      |
| `global.data`      | `name`, `key`         | `{"status": "..."}`         | `Transaction::data`      |
| `global.order`     | `name`, `prefix`      | `{"subscript": ...}`        | `Transaction::order`     |
| `global.query`     | `name`                | `{"key": [...]}`            | `Transaction::query`     |
| `global.collect`   | `name`                | NDJSON stream               | `Transaction::collect`   |
| `global.list`      | (none)                | `{"globals": [...]}`        | `Database::list_globals` |

`global.list` is the exception: it operates at the database level and SHALL NOT require a `txn` field. It still requires a valid session (i.e. `X-Session-Id` header).

#### Scenario: global.get
- **WHEN** a request contains `{"op": "global.get", "txn": "<token>", "name": "PATIENT", "key": [123, "NAME"]}`
- **THEN** the server calls `Transaction::get` and returns `{"value": "Alice"}` or `{"value": null}`

#### Scenario: global.set
- **WHEN** a request contains `{"op": "global.set", "txn": "<token>", "name": "PATIENT", "key": [123, "NAME"], "value": "Alice"}`
- **THEN** the server calls `Transaction::set` and returns `{"ok": true}`

#### Scenario: global.increment
- **WHEN** a request contains `{"op": "global.increment", "txn": "<token>", "name": "COUNTER", "key": ["visits"], "amount": 1}`
- **THEN** the server calls `Transaction::increment` and returns `{"value": 42}` (the new value)

#### Scenario: global.kill
- **WHEN** a request contains `{"op": "global.kill", "txn": "<token>", "name": "PATIENT", "key": [123]}`
- **THEN** the server calls `Transaction::kill` and returns `{"ok": true}`

#### Scenario: global.data
- **WHEN** a request contains `{"op": "global.data", "txn": "<token>", "name": "PATIENT", "key": [123]}`
- **THEN** the server calls `Transaction::data` and returns `{"status": "both"}`

#### Scenario: global.order
- **WHEN** a request contains `{"op": "global.order", "txn": "<token>", "name": "PATIENT", "prefix": [], "after": 123}`
- **THEN** the server calls `Transaction::order` and returns `{"subscript": 124}` or `{"subscript": null}`

#### Scenario: global.query
- **WHEN** a request contains `{"op": "global.query", "txn": "<token>", "name": "PATIENT", "after": [123, "NAME"]}`
- **THEN** the server calls `Transaction::query` and returns `{"key": [124, "AGE"]}` or `{"key": null}`

#### Scenario: global.list
- **WHEN** a request contains `{"op": "global.list"}` with a valid `X-Session-Id` header
- **THEN** the server calls `Database::list_globals` and returns `{"globals": ["PATIENT", "ORDER"]}`

### Requirement: Global order op fields
`global.order` SHALL accept `"prefix"` (a key array; required) and `"after"` (an optional subscript). The `"after"` field, when present, SHALL use the same wire encoding as a single subscript element. When absent, the server passes `None` to `Transaction::order` to get the first subscript at that level.

#### Scenario: Order with no after
- **WHEN** a request contains `{"op": "global.order", "txn": "...", "name": "DATA", "prefix": [1]}`
- **THEN** the server calls `order(name, key![1], None)` and returns the first subscript under key `[1]`

#### Scenario: Order with after
- **WHEN** a request contains `{"op": "global.order", "txn": "...", "name": "DATA", "prefix": [1], "after": "A"}`
- **THEN** the server calls `order(name, key![1], Some(&Subscript::String("A")))` and returns the next subscript after `"A"`

### Requirement: Global query op fields
`global.query` SHALL accept an optional `"after"` field (a key array). When absent, the server passes `None` to `Transaction::query`. The response SHALL contain `"key"` with the full key path or `null` if no more valued nodes exist.

#### Scenario: Query from start
- **WHEN** a request contains `{"op": "global.query", "txn": "...", "name": "PATIENT"}`
- **THEN** the server calls `query(name, None)` and returns the first key with a value

#### Scenario: Query with cursor
- **WHEN** a request contains `{"op": "global.query", "txn": "...", "name": "PATIENT", "after": [123, "NAME"]}`
- **THEN** the server calls `query(name, Some(&key))` and returns the next key after `[123, "NAME"]`

### Requirement: Local operations
The system SHALL support the same operation set for locals (`local.*`) without a `txn` field. Local ops use a shared in-memory `Database` with the session's internal ID prepended to keys for isolation. The op names and field shapes mirror `global.*` with these differences:
- No `txn` field
- `local.list` is not supported (locals are not enumerable at the database level)
- The session's internal `u64` ID is transparently prepended to keys on input and stripped on output; the client never sees it

#### Scenario: local.set
- **WHEN** a request contains `{"op": "local.set", "name": "TEMP", "key": ["x"], "value": 42}` with a valid session (internal ID `7`)
- **THEN** the server calls `db.set(&local!("TEMP"), &key![7, "x"], val)` on the shared in-memory DB and returns `{"ok": true}`

#### Scenario: local.get
- **WHEN** a request contains `{"op": "local.get", "name": "TEMP", "key": ["x"]}` with a valid session (internal ID `7`)
- **THEN** the server calls `db.get(&local!("TEMP"), &key![7, "x"])` and returns the value

#### Scenario: local.collect
- **WHEN** a request contains `{"op": "local.collect", "name": "TEMP"}` with a valid session (internal ID `7`)
- **THEN** the server streams NDJSON from the shared in-memory DB, scoped to prefix `[7]`, with the leading `7` subscript stripped from each key in the output

### Requirement: Admin endpoints
The system SHALL expose `GET /health` and `GET /status` as plain HTTP endpoints (not RPC). These SHALL NOT require session or auth headers.

#### Scenario: Health check
- **WHEN** a `GET /health` request is received
- **THEN** the server returns `200` with `{"ok": true}`

#### Scenario: Status
- **WHEN** a `GET /status` request is received
- **THEN** the server returns `200` with a JSON object containing database stats, open transaction count, and active session count
