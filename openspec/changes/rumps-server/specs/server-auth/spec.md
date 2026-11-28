## ADDED Requirements

### Requirement: Extractor-based authentication
Authentication SHALL be implemented as an Axum extractor (`impl FromRequestParts`) that produces a typed `Auth` value. Handlers that require authentication declare `Auth` in their parameter list; handlers that do not (e.g. `health`, `status`) simply omit it. No Tower middleware layer is used for auth.

#### Scenario: Auth-required handler
- **WHEN** a `POST /rpc` request arrives with a `global.get` op and no valid authentication
- **THEN** the `Auth` extractor rejects the request with `401` before the handler body executes

#### Scenario: Health endpoint bypasses auth
- **WHEN** a `GET /health` request arrives without authentication
- **THEN** the request succeeds with `200` because the handler does not extract `Auth`

#### Scenario: Status endpoint bypasses auth
- **WHEN** a `GET /status` request arrives without authentication
- **THEN** the request succeeds with `200` because the handler does not extract `Auth`

### Requirement: Built-in bearer token auth
The server SHALL ship with a built-in authentication scheme: bearer token auth. The `Auth` extractor reads the `Authorization: Bearer <token>` header and compares against the configured token. The token is configured in the server config file.

#### Scenario: Valid bearer token
- **WHEN** a request includes `Authorization: Bearer <correct-token>`
- **THEN** the `Auth` extractor succeeds and the handler receives a typed `Auth` value

#### Scenario: Invalid bearer token
- **WHEN** a request includes `Authorization: Bearer <wrong-token>`
- **THEN** the extractor returns `401` with `{"error": "invalid authentication token"}`

#### Scenario: Missing authorization header
- **WHEN** a request to a protected endpoint lacks the `Authorization` header
- **THEN** the extractor returns `401` with `{"error": "authentication required"}`

### Requirement: Auth configuration
Auth configuration SHALL live in the server config file (TOML). The config SHALL specify the auth method and its parameters (e.g. bearer token value).

#### Scenario: Bearer token in config
- **WHEN** the server config contains `[auth]\nmethod = "bearer"\ntoken = "secret123"`
- **THEN** the server uses bearer token authentication with that token

### Requirement: No-auth mode
The server SHALL accept a `--no-auth` CLI flag that disables authentication. When started with `--no-auth`, the `Auth` extractor always succeeds with a default context rather than checking any header. The server SHALL print a loud warning to stderr on startup.

#### Scenario: Start with no-auth
- **WHEN** the server is started with `--no-auth`
- **THEN** all requests pass the `Auth` extractor regardless of headers, and a warning is printed to stderr

#### Scenario: Refuse to start without auth config
- **WHEN** the server is started without `--no-auth` and no auth configuration is present
- **THEN** the server refuses to start and prints an error explaining that auth must be configured or `--no-auth` must be passed

### Requirement: Extensible auth for library users
Users who embed the server via the library crate SHALL be able to provide their own `Auth` extractor implementation. The server builder SHALL accept a custom extractor factory, allowing arbitrary auth schemes without modifying server internals.

#### Scenario: Custom auth extractor
- **WHEN** a library user provides a custom `Auth` implementation that validates JWTs
- **THEN** the server uses that implementation for all auth-required handlers
