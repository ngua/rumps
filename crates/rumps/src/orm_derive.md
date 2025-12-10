
# Quick Start

```
# tokio_test::block_on(async {
use rumps::orm::{ToRumps, FromRumps, RumpsRead, RumpsWrite};
use rumps::Database;

#[derive(Debug, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "user")]
struct User {
    #[rumps(key)]
    id: u64,
    name: String,
}

let db = Database::in_memory()?;
db.transaction(|txn| async move {
    User { id: 1, name: "Alice".into() }.insert(&txn).await?;
    Ok(())
}).await?;

let user = User::one(&db, 1u64).await?;
assert_eq!(user, Some(User { id: 1, name: "Alice".into() }));
# Ok::<(), rumps::Error>(())
# });
```

# Composite Keys

Use `order` to specify key field ordering:

```
# use rumps::orm::{ToRumps, FromRumps};
#[derive(ToRumps, FromRumps)]
#[rumps(global = "patient")]
struct Patient {
    #[rumps(key, order = 0)]
    dept: String,
    #[rumps(key, order = 1)]
    id: u64,
    name: String,
}
// Storage: ^patient("cardiology", 123, "name") = "Alice"
```

# Nested Structs

Use `flatten` to inline nested struct fields, or `subtree` to store under a subscript:

```text
#[derive(ToRumps, FromRumps)]
#[rumps(global = "patient")]
struct Patient {
    #[rumps(key)]
    id: u64,
    name: String,
    #[rumps(flatten)]   // Fields inlined at same level
    addr: Address,
    #[rumps(subtree)]   // Stored under "contact" subscript
    contact: Contact,
}

// flatten: ^patient(1, "city") = "NYC"
// subtree: ^patient(1, "contact", "phone") = "555-1234"
```

# Unit Enums

Unit enums convert to/from string values:

```
use rumps::orm::{ToValue, FromValue};
use rumps::Value;

#[derive(Debug, PartialEq, ToValue, FromValue)]
enum Priority {
    Low,
    Medium,
    High,
}

assert_eq!(Priority::High.to_val(), Value::from("High"));
assert_eq!(Priority::from_val(&Value::from("Low")), Ok(Priority::Low));
```

# Renaming with `rename_all`

```
use rumps::orm::{ToRumps, FromRumps, ToValue, FromValue};

// Snake case for struct fields
#[derive(ToRumps, FromRumps)]
#[rumps(global = "user", rename_all = "camel-case")]
struct User {
    #[rumps(key)]
    user_id: u64,
    first_name: String,  // stored as "firstName"
    last_name: String,   // stored as "lastName"
}

// Lowercase for enum variants
#[derive(ToValue, FromValue)]
#[rumps(rename_all = "lowercase")]
enum Priority {
    Low,   // stored as "low"
    High,  // stored as "high"
}
```

# Newtype Structs

Newtypes transparently delegate to their inner type. For `ToValue`/`FromValue`/`ToSubscript`/`FromSubscript`:

```
use rumps::orm::{ToValue, FromValue, ToSubscript, FromSubscript};

#[derive(ToValue, FromValue)]
struct UserId(u64);

#[derive(ToSubscript, FromSubscript)]
struct Email(String);
```

For `ToRumps`/`FromRumps` newtypes, an explicit `global` is required to prevent accidentally
overwriting the wrapped type's storage:

```
use rumps::orm::{ToRumps, FromRumps};

# #[derive(ToRumps, FromRumps)]
# #[rumps(global = "user")]
# struct User { #[rumps(key)] id: u64 }
#[derive(ToRumps, FromRumps)]
#[rumps(global = "admin_user")]  // Separate storage from User
struct AdminUser(User);
```

# Data-Carrying Enums

Enums with data derive `ToRumps`/`FromRumps` (not `ToValue`/`FromValue`):

```
# use rumps::orm::{ToRumps, FromRumps};
#[derive(ToRumps, FromRumps)]
#[rumps(global = "status")]
enum EmploymentStatus {
    Active,                              // Unit variant
    OnLeave { reason: String },          // Struct variant
    Terminated { date: String, reason: Option<String> },
}
// Unit: ^status("Active") = ""
// Struct: ^status("OnLeave") = "", ^status("OnLeave", "reason") = "vacation"
```

## Variant Types

```
# use rumps::orm::{ToRumps, FromRumps};
#[derive(ToRumps, FromRumps)]
#[rumps(global = "data")]
enum Data {
    // Unit variant: ^data("Empty") = ""
    Empty,

    // Single-field tuple: value stored directly
    // ^data("Error") = "something went wrong"
    Error(String),

    // Multi-field tuple: numeric indices
    // ^data("Point", 0) = 1.0, ^data("Point", 1) = 2.0
    Point(f64, f64),

    // Struct variant with key
    // ^data("Person", 123, "name") = "Alice"
    Person {
        #[rumps(key)]
        id: u64,
        name: String,
    },
}
```

## Embedded Enums (Subtrees)

```text
#[derive(ToRumps, FromRumps)]
#[rumps(global = "worker")]
struct Worker {
    #[rumps(key)]
    id: u64,
    name: String,
    #[rumps(subtree)]
    status: Status,
}

// ^worker(1, "name") = "Bob"
// ^worker(1, "status", "OnLeave") = ""
// ^worker(1, "status", "OnLeave", "reason") = "vacation"
```

## Untagged Enums

Use `#[rumps(untagged)]` when you don't want the variant name stored in the key.
Variants are tried in declaration order on deserialization:

```text
#[derive(ToRumps, FromRumps)]
#[rumps(global = "value", untagged)]
enum JsonValue {
    Null,                    // Matches empty data
    Number(f64),             // Matches single numeric value
    Text(String),            // Matches single string value
    Object { data: String }, // Matches struct with "data" field
}

// Number: ^value() = 42.0  (no variant tag)
// Text:   ^value() = "hello"
// Object: ^value("data") = "contents"
```

**Note**: For untagged enums, ensure variants have distinguishable structures.
The first matching variant (in declaration order) wins during deserialization.
