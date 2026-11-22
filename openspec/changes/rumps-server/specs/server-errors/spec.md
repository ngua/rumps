## ADDED Requirements

### Requirement: Error response format
All error responses SHALL use the JSON body format `{"error": "<message>"}`. The HTTP status code SHALL reflect the error category.

#### Scenario: Error body structure
- **WHEN** any error occurs during request processing
- **THEN** the response body is `{"error": "<human-readable message>"}`

### Requirement: StorageError to HTTP status mapping
The server SHALL map `StorageError` variants to HTTP status codes:

| StorageError variant          | HTTP Status | Rationale                        |
|-------------------------------|-------------|----------------------------------|
| `WriteConflict`               | `409`       | Transaction isolation violation  |
| `TransactionNotActive`        | `410`       | Transaction expired or already ended |
| `TransactionTimeout`          | `408`       | Request timeout                  |
| `GlobalRequiresTransaction`   | `400`       | Missing required txn token       |
| `WrongValueType`              | `400`       | Type mismatch in operation       |
| `InvalidOperation`            | `400`       | Invalid request                  |
| `InvalidConfiguration`        | `400`       | Invalid parameters               |
| `IntegerOverflow`             | `400`       | Numeric overflow in increment    |
| `TooManyConcurrentTransactions` | `429`     | Server at capacity               |
| `KeyNotFound`                 | `500`       | Internal B-tree lookup fault     |
| All other variants            | `500`       | Internal server error            |

#### Scenario: Write conflict
- **WHEN** `txn.commit` encounters `StorageError::WriteConflict`
- **THEN** the server returns `409` with the conflict details in the error message

#### Scenario: Transaction not active
- **WHEN** a `global.set` is called with a transaction that has already been committed
- **THEN** the server returns `410` with `{"error": "Transaction ... is not active (state: ...)"}`

#### Scenario: Too many transactions
- **WHEN** `txn.start` encounters `StorageError::TooManyConcurrentTransactions`
- **THEN** the server returns `429` with the limit in the error message

#### Scenario: Internal storage error
- **WHEN** an I/O error or WAL error occurs
- **THEN** the server returns `500` with `{"error": "..."}`

### Requirement: Request validation errors
Malformed requests (missing fields, invalid JSON, unknown ops) SHALL return `400` with a descriptive error message. JSON parse errors SHALL also return `400`.

#### Scenario: Malformed JSON body
- **WHEN** the request body is not valid JSON
- **THEN** the server returns `400` with `{"error": "invalid JSON: ..."}`

#### Scenario: Missing required field
- **WHEN** a `global.set` request is missing the `"value"` field
- **THEN** the server returns `400` with `{"error": "missing required field: value"}`

#### Scenario: Invalid wire-encoded type
- **WHEN** a request contains `{"$char": "ABC"}` (multi-character)
- **THEN** the server returns `400` with an error describing the validation failure

### Requirement: Session and transaction lookup errors
Session token errors SHALL return `410` (Gone) for expired/reaped sessions and `401` for missing tokens. Transaction token errors SHALL return `410` for expired/not-found transactions and `403` for wrong-session access.

#### Scenario: Expired session
- **WHEN** a request uses an expired session token
- **THEN** the server returns `410`

#### Scenario: Wrong session transaction access
- **WHEN** a request tries to use a transaction owned by a different session
- **THEN** the server returns `403`
