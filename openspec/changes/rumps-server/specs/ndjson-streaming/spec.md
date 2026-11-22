## ADDED Requirements

### Requirement: NDJSON response format for collect
`global.collect` and `local.collect` SHALL return responses with `Content-Type: application/x-ndjson`. Each line SHALL be a complete JSON object representing one entry. The stream SHALL terminate when the scan is exhausted or the limit is reached.

#### Scenario: Streaming response
- **WHEN** a `global.collect` request is issued against a global with 3 entries
- **THEN** the response has `Content-Type: application/x-ndjson` and contains 3 newline-delimited JSON lines

#### Scenario: Empty result
- **WHEN** a `global.collect` request is issued against a global with no matching entries
- **THEN** the response has `Content-Type: application/x-ndjson` and an empty body (zero lines)

### Requirement: Collect entry format
Each NDJSON line SHALL contain `"key"` (the full key path as a JSON array) and `"value"` (the node's value using wire encoding, or `null` if the node has no value). The `extract` function used internally SHALL map each entry to `(Key, Option<Value>)`.

#### Scenario: Entry with value
- **WHEN** a node at key `[100, "NAME"]` has value `"Alice"`
- **THEN** the NDJSON line is `{"key": [100, "NAME"], "value": "Alice"}`

#### Scenario: Entry with no value (structural node)
- **WHEN** a node at key `[100]` has descendants but no value
- **THEN** the NDJSON line is `{"key": [100], "value": null}`

### Requirement: Collect scan parameters
`collect` ops SHALL accept optional fields to construct a `Scan`:
- `"start"`: key array; start bound (inclusive by default)
- `"end"`: key array; end bound (exclusive by default)
- `"start-exclusive"`: boolean; if `true`, start bound is exclusive (default `false`)
- `"end-inclusive"`: boolean; if `true`, end bound is inclusive (default `false`)
- `"dir"`: string `"fwd"` or `"rev"` (default `"fwd"`)
- `"limit"`: integer; maximum number of entries to stream

When no bounds are specified, the scan covers all keys (`Bounds::full()`). The `"dir"` field maps to `Dir::Forward` or `Dir::Reverse`.

#### Scenario: Full forward scan
- **WHEN** a request contains `{"op": "global.collect", "txn": "...", "name": "PATIENT"}`
- **THEN** the server uses `Scan::forward(..)` and streams all entries

#### Scenario: Bounded forward scan
- **WHEN** a request contains `{"op": "global.collect", "txn": "...", "name": "PATIENT", "start": [100], "end": [200]}`
- **THEN** the server uses `Scan::forward(key![100]..key![200])` (inclusive start, exclusive end)

#### Scenario: Reverse scan
- **WHEN** a request contains `{"op": "global.collect", "txn": "...", "name": "PATIENT", "dir": "rev"}`
- **THEN** the server uses `Scan::reverse(..)` and streams entries in reverse key order

#### Scenario: Limited scan
- **WHEN** a request contains `{"op": "global.collect", "txn": "...", "name": "PATIENT", "limit": 10}`
- **THEN** the server streams at most 10 entries

#### Scenario: Exclusive start bound
- **WHEN** a request contains `{"op": "global.collect", "txn": "...", "name": "DATA", "start": [5], "start-exclusive": true}`
- **THEN** the server constructs `Bounds::new(Bound::Excluded(key![5]), Bound::Unbounded)` and entries at key `[5]` are excluded

#### Scenario: Inclusive end bound
- **WHEN** a request contains `{"op": "global.collect", "txn": "...", "name": "DATA", "end": [10], "end-inclusive": true}`
- **THEN** the server constructs bounds with `Bound::Included(key![10])` and entries at key `[10]` are included

### Requirement: No pagination
The server SHALL NOT support cursor-based pagination for `collect`. Clients that need to resume a partial scan SHALL re-issue the `collect` request with updated `"start"` and `"start-exclusive": true` fields.

#### Scenario: Resume after disconnect
- **WHEN** a client receives entries up to key `[150, "NAME"]` before a disconnect
- **THEN** the client re-issues `collect` with `"start": [150, "NAME"], "start-exclusive": true` to resume

### Requirement: Local collect
`local.collect` SHALL behave identically to `global.collect` except it operates on the session's in-memory `Database` and does not require a `"txn"` field.

#### Scenario: Local collect streaming
- **WHEN** a request contains `{"op": "local.collect", "name": "TEMP"}` with a valid session
- **THEN** the server streams NDJSON from the session's local DB using the same scan and entry format
