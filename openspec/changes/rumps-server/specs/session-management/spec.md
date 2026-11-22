## ADDED Requirements

### Requirement: Session creation
`session.start` SHALL create a new session, allocate a UUID v4 external token, and assign a monotonic `u64` internal session ID. The external token SHALL be returned in the response. The internal ID is used as a key prefix for local variable isolation and is never exposed to the client.

#### Scenario: Create session
- **WHEN** a `session.start` request is received
- **THEN** a new session is created with a UUID v4 external token and a `u64` internal ID, and `{"id": "<uuid>"}` is returned

### Requirement: Session destruction
`session.end` SHALL destroy the session identified by the `X-Session-Id` header. This MUST roll back any active transactions owned by the session and clean up all local variables by calling `Database::kill` with `key![internal_id]` for each local name the session has used.

#### Scenario: Destroy session with active transaction
- **WHEN** `session.end` is called on a session with an uncommitted transaction
- **THEN** the transaction is rolled back before the session is destroyed

#### Scenario: Destroy session cleans up locals
- **WHEN** `session.end` is called on a session that has set local variables under names `"TEMP"` and `"SCRATCH"`
- **THEN** the server calls `kill(&local!("TEMP"), &key![internal_id])` and `kill(&local!("SCRATCH"), &key![internal_id])`, removing all of that session's local data

### Requirement: Session idle timeout and reaping
The server SHALL track the last activity time for each session. Sessions with no activity for longer than the configured idle timeout SHALL be reaped automatically. The default idle timeout SHALL be configurable (server config) with a default of `300` seconds (5 minutes). Reaping SHALL follow the same cleanup as `session.end` (rollback transactions, kill local prefixes).

#### Scenario: Session reaped after idle timeout
- **WHEN** a session has had no requests for longer than the idle timeout
- **THEN** the session is reaped: active transactions are rolled back, local variable subtrees are killed, and subsequent requests with that session token return `410`

#### Scenario: Activity resets timeout
- **WHEN** a session receives any valid request
- **THEN** the session's last activity timestamp is updated and the idle timeout resets

### Requirement: Session storage
Sessions SHALL be stored in a concurrent `HashMap` (or equivalent) keyed by external UUID. The server SHALL support concurrent access from multiple requests on the same session. The server SHALL maintain an `AtomicU64` counter for allocating internal session IDs.

#### Scenario: Concurrent requests on same session
- **WHEN** two requests arrive simultaneously for the same session
- **THEN** both are processed without data corruption; ordering between them is undefined

### Requirement: Shared in-memory database for locals
All sessions SHALL share a single `Database::in_memory()` instance for local variable storage. Local variables are stored under `Name::Local` with the session's internal `u64` ID prepended to the key. This provides session isolation through key prefixing rather than separate database instances.

#### Scenario: Local set with key prefix
- **WHEN** session with internal ID `7` calls `local.set` with `name: "TEMP", key: ["x"]`
- **THEN** the server calls `db.set(&local!("TEMP"), &key![7, "x"], val)` on the shared in-memory database

#### Scenario: Local get with key prefix
- **WHEN** session with internal ID `7` calls `local.get` with `name: "TEMP", key: ["x"]`
- **THEN** the server calls `db.get(&local!("TEMP"), &key![7, "x"])` and returns the result

### Requirement: Session-scoped local namespace isolation
Local variables SHALL be isolated between sessions through the internal ID key prefix. The internal session ID is prepended on the way in and stripped on the way out for all local operations. The client never sees the session ID subscript.

#### Scenario: Locals isolated between sessions
- **WHEN** session A (internal ID `5`) sets `local.set` with `name: "TEMP", key: ["x"], value: 1` and session B (internal ID `6`) does `local.get` with `name: "TEMP", key: ["x"]`
- **THEN** session B receives `{"value": null}` because the underlying keys are `[5, "x"]` and `[6, "x"]`

#### Scenario: Collect strips session prefix from keys
- **WHEN** session with internal ID `7` calls `local.collect` with `name: "TEMP"`
- **THEN** the server scans keys under prefix `[7]`, and each key in the NDJSON response has the leading `7` subscript removed

#### Scenario: Order uses session-scoped prefix
- **WHEN** session with internal ID `7` calls `local.order` with `name: "TEMP", prefix: []`
- **THEN** the server calls `order(&local!("TEMP"), &key![7], None)` and returns the result subscript without the session prefix

#### Scenario: Query strips session prefix from result
- **WHEN** session with internal ID `7` calls `local.query` with `name: "TEMP"`
- **THEN** the server calls `query` scoped under `[7]` and strips the leading `7` from the returned key

### Requirement: Local name tracking for cleanup
Each session SHALL maintain a set of local variable names it has written to (e.g. `HashSet<SmolStr>`). On `local.set`, `local.increment`, and `local.kill`, the local name SHALL be recorded. On session destruction, the server iterates this set and kills the session prefix under each name.

#### Scenario: Track used local names
- **WHEN** a session calls `local.set` with `name: "TEMP"` and later `local.set` with `name: "SCRATCH"`
- **THEN** the session's used-names set contains `{"TEMP", "SCRATCH"}`

#### Scenario: Cleanup kills only used names
- **WHEN** session with internal ID `7` is destroyed with used names `{"TEMP", "SCRATCH"}`
- **THEN** the server calls `kill(&local!("TEMP"), &key![7])` and `kill(&local!("SCRATCH"), &key![7])` but does not touch other local names
