## ADDED Requirements

### Requirement: Subscript JSON encoding with type inference
The system SHALL encode `Subscript` values in JSON using type inference from JSON primitives. JSON `true`/`false` SHALL map to `Subscript::Boolean`, JSON numbers with no fractional part SHALL map to `Subscript::Number` (as `f64`), JSON numbers with a fractional part SHALL map to `Subscript::Number`, and JSON strings SHALL map to `Subscript::String`. JSON objects and arrays MUST NOT be inferred as `Subscript::Json`; they MUST use the `$json` tag.

#### Scenario: Infer boolean subscript
- **WHEN** a JSON key array contains `true`
- **THEN** it deserializes as `Subscript::Boolean(true)`

#### Scenario: Infer integer subscript
- **WHEN** a JSON key array contains `123`
- **THEN** it deserializes as `Subscript::Number(OrderedFloat(123.0))`

#### Scenario: Infer float subscript
- **WHEN** a JSON key array contains `1.5`
- **THEN** it deserializes as `Subscript::Number(OrderedFloat(1.5))`

#### Scenario: Infer string subscript
- **WHEN** a JSON key array contains `"NAME"`
- **THEN** it deserializes as `Subscript::String("NAME")`

#### Scenario: Bare JSON object rejected as subscript
- **WHEN** a JSON key array contains `{"a": 1}` without a `$json` wrapper
- **THEN** the system SHALL attempt to parse it as a tagged type; if no recognized tag is found, it SHALL return a `400` error

### Requirement: Value JSON encoding with type inference
The system SHALL encode `Value` in JSON using type inference. JSON `true`/`false` SHALL map to `Value::Boolean`, JSON integers (no fractional part) SHALL map to `Value::Integer`, JSON numbers with a fractional part SHALL map to `Value::Double`, and JSON strings SHALL map to `Value::String`. JSON objects and arrays MUST NOT be inferred; they MUST use the `$json` tag.

#### Scenario: Infer boolean value
- **WHEN** a JSON value field contains `false`
- **THEN** it deserializes as `Value::Boolean(false)`

#### Scenario: Infer integer value
- **WHEN** a JSON value field contains `42`
- **THEN** it deserializes as `Value::Integer(42)`

#### Scenario: Infer float value
- **WHEN** a JSON value field contains `3.14`
- **THEN** it deserializes as `Value::Double(OrderedFloat(3.14))`

#### Scenario: Infer string value
- **WHEN** a JSON value field contains `"Alice"`
- **THEN** it deserializes as `Value::String("Alice")`

### Requirement: Explicit type tags with dollar-prefix
The system SHALL support `$`-prefixed type tags for explicit type control. Recognized tags: `$bool`, `$int`, `$float`, `$char`, `$str`, `$json`. A tagged form is a JSON object with exactly one key matching a recognized tag. When a tagged form is encountered, inference SHALL be bypassed and the tag SHALL determine the type.

#### Scenario: Explicit char subscript
- **WHEN** a JSON key array contains `{"$char": "A"}`
- **THEN** it deserializes as `Subscript::Char('A')`

#### Scenario: Explicit char value
- **WHEN** a JSON value field contains `{"$char": "X"}`
- **THEN** it deserializes as `Value::Char('X')`

#### Scenario: Force integer to double
- **WHEN** a JSON value field contains `{"$float": 42}`
- **THEN** it deserializes as `Value::Double(OrderedFloat(42.0))`

#### Scenario: Force float to integer
- **WHEN** a JSON value field contains `{"$int": 42}`
- **THEN** it deserializes as `Value::Integer(42)`

#### Scenario: Explicit JSON subscript
- **WHEN** a JSON key array contains `{"$json": {"a": 1}}`
- **THEN** it deserializes as `Subscript::Json(json!({"a": 1}))`

#### Scenario: Explicit JSON value
- **WHEN** a JSON value field contains `{"$json": [1, 2, 3]}`
- **THEN** it deserializes as `Value::Json(json!([1, 2, 3]))`

#### Scenario: Explicit string tag
- **WHEN** a JSON value field contains `{"$str": "hello"}`
- **THEN** it deserializes as `Value::String("hello")`

#### Scenario: Explicit bool tag
- **WHEN** a JSON value field contains `{"$bool": true}`
- **THEN** it deserializes as `Value::Boolean(true)`

### Requirement: Char tag validation
The system SHALL reject `$char` tags whose value is not a single-character string. Multi-character strings and non-string values MUST produce a `400` error.

#### Scenario: Multi-character char tag
- **WHEN** a field contains `{"$char": "AB"}`
- **THEN** the system SHALL return a `400` error indicating `$char` requires exactly one character

#### Scenario: Non-string char tag
- **WHEN** a field contains `{"$char": 65}`
- **THEN** the system SHALL return a `400` error

### Requirement: Key wire format
A `Key` SHALL be represented as a JSON array of subscript values. An empty array `[]` represents `Key::new()`. Each element is decoded as a subscript using inference or explicit tags.

#### Scenario: Multi-subscript key
- **WHEN** the wire format contains `"key": [123, "NAME"]`
- **THEN** it deserializes as `Key::from(vec![Subscript::Number(123.0.into()), Subscript::String("NAME".into())])`

#### Scenario: Empty key
- **WHEN** the wire format contains `"key": []`
- **THEN** it deserializes as `Key::new()`

#### Scenario: Mixed-type key
- **WHEN** the wire format contains `"key": [true, 1, {"$char": "A"}, "foo", {"$json": null}]`
- **THEN** it deserializes as a `Key` with `Boolean(true)`, `Number(1.0)`, `Char('A')`, `String("foo")`, `Json(null)` subscripts

### Requirement: Increment wire format
The `amount` field for `increment` ops SHALL use type inference: JSON integers map to `Increment::Integer`, JSON floats map to `Increment::Double`. Explicit tags `$int` and `$float` SHALL also be supported.

#### Scenario: Integer increment
- **WHEN** the wire format contains `"amount": 5`
- **THEN** it deserializes as `Increment::Integer(5)`

#### Scenario: Float increment
- **WHEN** the wire format contains `"amount": 1.5`
- **THEN** it deserializes as `Increment::Double(OrderedFloat(1.5))`

#### Scenario: Explicit float increment
- **WHEN** the wire format contains `"amount": {"$float": 1}`
- **THEN** it deserializes as `Increment::Double(OrderedFloat(1.0))`

### Requirement: DataStatus wire format
`DataStatus` SHALL be serialized as a JSON string: `"no-data"`, `"has-value"`, `"has-descendants"`, `"both"`.

#### Scenario: DataStatus encoding
- **WHEN** a `data` op returns `DataStatus::Both`
- **THEN** the response contains `{"status": "both"}`

### Requirement: Dir wire format
`Dir` SHALL be serialized as a JSON string: `"fwd"` for `Dir::Forward`, `"rev"` for `Dir::Reverse`. On deserialization, `"fwd"` SHALL map to `Dir::Forward` and `"rev"` SHALL map to `Dir::Reverse`. Any other string SHALL return a `400` error.

#### Scenario: Forward direction encoding
- **WHEN** a `collect` request contains `"dir": "fwd"`
- **THEN** it deserializes as `Dir::Forward`

#### Scenario: Reverse direction encoding
- **WHEN** a `collect` request contains `"dir": "rev"`
- **THEN** it deserializes as `Dir::Reverse`

#### Scenario: Invalid direction
- **WHEN** a `collect` request contains `"dir": "backward"`
- **THEN** the server returns `400` with an error indicating the value must be `"fwd"` or `"rev"`

#### Scenario: Default direction
- **WHEN** a `collect` request omits the `"dir"` field
- **THEN** it defaults to `Dir::Forward`

### Requirement: Null value encoding
The system SHALL encode the absence of a value (Rust `None`) as JSON `null`. Operations that return `Option<Value>` (e.g. `get`) SHALL use `null` for `None` and the encoded value for `Some`.

#### Scenario: Get returns no value
- **WHEN** a `get` op returns `None`
- **THEN** the response contains `{"value": null}`

#### Scenario: Get returns a value
- **WHEN** a `get` op returns `Some(Value::String("Alice"))`
- **THEN** the response contains `{"value": "Alice"}`

### Requirement: Response value serialization
The system SHALL serialize `Value` in responses using the same inference rules in reverse: `Value::Boolean` as JSON bool, `Value::Integer` as JSON integer, `Value::Double` as JSON float, `Value::String` as JSON string, `Value::Char` as `{"$char": "X"}`, `Value::Json` as `{"$json": ...}`. `Char` and `Json` MUST always use tags in responses to enable lossless round-tripping.

#### Scenario: Char value in response
- **WHEN** a `get` returns `Value::Char('Z')`
- **THEN** the response contains `{"value": {"$char": "Z"}}`

#### Scenario: Json value in response
- **WHEN** a `get` returns `Value::Json(json!({"a": 1}))`
- **THEN** the response contains `{"value": {"$json": {"a": 1}}}`

#### Scenario: Integer value in response
- **WHEN** a `get` returns `Value::Integer(42)`
- **THEN** the response contains `{"value": 42}`

#### Scenario: Double value in response
- **WHEN** a `get` returns `Value::Double(OrderedFloat(3.14))`
- **THEN** the response contains `{"value": 3.14}`
