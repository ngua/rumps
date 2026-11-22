## ADDED Requirements

### Requirement: Transaction start
`txn.start` SHALL create a new transaction via `Database::build_transaction()` and return a UUID v4 token. The transaction SHALL be associated with the current session. Optional fields: `"isolation"` (string, default `"snapshot"`), `"timeout-ms"` (integer, optional).

#### Scenario: Start transaction with defaults
- **WHEN** a request contains `{"op": "txn.start"}`
- **THEN** the server calls `Database::build_transaction().start().await`, stores the resulting `Transaction` keyed by a UUID, and returns `{"id": "<uuid>"}`

#### Scenario: Start transaction with timeout
- **WHEN** a request contains `{"op": "txn.start", "timeout-ms": 5000}`
- **THEN** the server calls `build_transaction().timeout(5000).start().await` and returns the token

#### Scenario: Start transaction with isolation
- **WHEN** a request contains `{"op": "txn.start", "isolation": "snapshot"}`
- **THEN** the server uses `IsolationLevel::SnapshotIsolation` (currently the only supported level)

### Requirement: Transaction commit
`txn.commit` SHALL commit the transaction identified by the `"txn"` field. On success, the response SHALL be `{"ok": true}`. On conflict or other commit failure, the appropriate error response SHALL be returned.

#### Scenario: Successful commit
- **WHEN** a request contains `{"op": "txn.commit", "txn": "<token>"}` and the transaction has no conflicts
- **THEN** the server calls `Transaction::commit().await` and returns `{"ok": true}`

#### Scenario: Commit with write conflict
- **WHEN** `txn.commit` is called and the transaction has a write conflict
- **THEN** the server returns `409` with the conflict error details

#### Scenario: Commit already committed transaction
- **WHEN** `txn.commit` is called with a token for an already-committed transaction
- **THEN** the server returns `410` with an error indicating the transaction is not active

### Requirement: Transaction rollback
`txn.rollback` SHALL roll back the transaction identified by the `"txn"` field. On success, the response SHALL be `{"ok": true}`.

#### Scenario: Successful rollback
- **WHEN** a request contains `{"op": "txn.rollback", "txn": "<token>"}`
- **THEN** the server calls `Transaction::rollback().await` and returns `{"ok": true}`

### Requirement: Transaction token binding
All `global.*` ops (except `global.list`) SHALL require a `"txn"` field containing a valid transaction UUID. The server SHALL look up the `Transaction` by this token and use it for the operation. An invalid or expired token SHALL return `410`.

#### Scenario: Valid transaction token
- **WHEN** a `global.get` request includes a valid `"txn"` token
- **THEN** the server uses the corresponding `Transaction` to execute the operation

#### Scenario: Invalid transaction token
- **WHEN** a `global.get` request includes a `"txn"` token that does not match any active transaction
- **THEN** the server returns `410` with `{"error": "transaction not found or expired"}`

#### Scenario: Transaction token from wrong session
- **WHEN** session A creates a transaction and session B tries to use its token
- **THEN** the server returns `403` with an error indicating the transaction belongs to another session

### Requirement: Transaction session ownership
Transactions SHALL be scoped to the session that created them. Only the owning session SHALL be allowed to use, commit, or rollback a transaction.

#### Scenario: Transaction owned by session
- **WHEN** session A starts a transaction and session A issues `global.set` with that transaction's token
- **THEN** the operation succeeds

### Requirement: Transaction idle timeout and reaping
Transactions that have had no operations for longer than a configurable timeout SHALL be automatically rolled back and removed. The default transaction idle timeout SHALL be 30 seconds.

#### Scenario: Transaction reaped after idle
- **WHEN** a transaction has had no operations for longer than the transaction idle timeout
- **THEN** the transaction is rolled back and subsequent use of its token returns `410`

### Requirement: Transaction storage
Active transactions SHALL be stored in a concurrent map keyed by UUID, associated with their owning session ID. The server SHALL support concurrent requests using different transactions.

#### Scenario: Multiple transactions on one session
- **WHEN** a session starts two transactions
- **THEN** both are tracked independently with their own tokens and can be used concurrently
