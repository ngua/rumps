//! Integration tests for the derive macros.
//!
//! These tests are in an integration test file because the derive macros
//! generate code referencing `::rumps_storage`, which requires the crate
//! to be seen as an external dependency.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rumps_derive::{
    FromRumps, FromSubscript, FromValue, ToRumps, ToSubscript, ToValue,
};
use rumps_storage::orm::{RumpsRead, RumpsWrite};
use rumps_storage::Database;
use rumps_types::orm::{
    FromSubscript as _, FromValue as _, ToSubscript as _, ToValue as _,
};
use rumps_types::{global, key, Key, Subscript, Value};

/// Basic struct with derive macros
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "person")]
struct Person {
    #[rumps(key)]
    id: u64,
    name: String,
    email: String,
}

/// Struct with composite key
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "employee")]
struct Employee {
    #[rumps(key, order = 0)]
    dept: String,
    #[rumps(key, order = 1)]
    emp_id: u64,
    name: String,
    salary: u32,
}

/// Struct with optional fields
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "profile")]
struct Profile {
    #[rumps(key)]
    user_id: u64,
    bio: Option<String>,
    website: Option<String>,
}

/// Struct with default values
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "settings")]
struct Settings {
    #[rumps(key)]
    user_id: u64,
    #[rumps(default)]
    theme: String,
    #[rumps(default = 10)]
    page_size: u32,
}

/// Struct with renamed field
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "item")]
struct Item {
    #[rumps(key)]
    id: u64,
    #[rumps(rename = "desc")]
    description: String,
}

/// Struct with skipped field
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "cache_entry")]
struct CacheEntry {
    #[rumps(key)]
    key: String,
    value: String,
    #[rumps(skip)]
    cached_at: u64,
}

/// Nested struct (no global - used as embedded type)
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "_embedded")]
struct Address {
    #[rumps(key)]
    _dummy: u64, // key required but not used when embedded
    street: String,
    city: String,
}

/// Contact info for subtree tests
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "_embedded")]
struct ContactInfo {
    #[rumps(key)]
    _dummy: u64,
    phone: String,
    email: String,
}

/// Struct with flattened nested struct
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "customer")]
struct Customer {
    #[rumps(key)]
    id: u64,
    name: String,
    #[rumps(flatten)]
    addr: Address,
}

/// Struct with subtree nested struct
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "vendor")]
struct Vendor {
    #[rumps(key)]
    id: u64,
    name: String,
    #[rumps(subtree)]
    contact: ContactInfo,
}

/// Unit enum with ToValue/FromValue
#[derive(Debug, Clone, PartialEq, ToValue, FromValue)]
enum Status {
    Active,
    Inactive,
    Pending,
}

/// Unit enum with ToSubscript/FromSubscript
#[derive(Debug, Clone, PartialEq, ToSubscript, FromSubscript)]
enum Priority {
    Low,
    Medium,
    High,
}

/// Newtype wrapper for ToValue/FromValue
#[derive(Debug, Clone, PartialEq, ToValue, FromValue)]
struct UserId(u64);

/// Newtype wrapper for ToSubscript/FromSubscript
#[derive(Debug, Clone, PartialEq, ToSubscript, FromSubscript)]
struct Email(String);

/// Newtype wrapper for ToRumps/FromRumps with separate storage
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "wrapped_person")]
struct WrappedPerson(Person);

/// Newtype wrapper that shares storage with Person
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "person")]
struct PersonAlias(Person);

#[tokio::test]
async fn test_derive_basic_struct() {
    let db = Database::in_memory().unwrap();

    let person = Person {
        id: 42,
        name: "Alice".into(),
        email: "alice@example.com".into(),
    };

    // Insert
    db.transaction(|txn| {
        let p = person.clone();
        async move {
            txn.insert(&p).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Read back
    let fetched: Option<Person> = db.one(42u64).await.unwrap();
    assert_eq!(fetched, Some(person));
}

#[tokio::test]
async fn test_derive_composite_key() {
    let db = Database::in_memory().unwrap();

    let emp1 = Employee {
        dept: "engineering".into(),
        emp_id: 1,
        name: "Alice".into(),
        salary: 100000,
    };
    let emp2 = Employee {
        dept: "engineering".into(),
        emp_id: 2,
        name: "Bob".into(),
        salary: 95000,
    };
    let emp3 = Employee {
        dept: "sales".into(),
        emp_id: 1,
        name: "Charlie".into(),
        salary: 80000,
    };

    // Insert all
    db.transaction(|txn| {
        let e1 = emp1.clone();
        let e2 = emp2.clone();
        let e3 = emp3.clone();
        async move {
            txn.insert(&e1).await?;
            txn.insert(&e2).await?;
            txn.insert(&e3).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Get by composite key
    let fetched: Option<Employee> =
        db.one(("engineering", 1u64)).await.unwrap();
    assert_eq!(fetched, Some(emp1.clone()));

    // Query by prefix (all engineering employees)
    let eng_emps: Vec<Employee> = db.query(("engineering",)).await.unwrap();
    assert_eq!(eng_emps.len(), 2);

    // Get all employees
    let all_emps: Vec<Employee> = db.all().await.unwrap();
    assert_eq!(all_emps.len(), 3);
}

#[tokio::test]
async fn test_derive_optional_fields() {
    let db = Database::in_memory().unwrap();

    // Profile with all fields
    let full_profile = Profile {
        user_id: 1,
        bio: Some("Hello world".into()),
        website: Some("https://example.com".into()),
    };

    // Profile with some fields missing
    let partial_profile = Profile {
        user_id: 2,
        bio: Some("Just bio".into()),
        website: None,
    };

    // Profile with no optional fields
    let minimal_profile = Profile {
        user_id: 3,
        bio: None,
        website: None,
    };

    db.transaction(|txn| {
        let f = full_profile.clone();
        let p = partial_profile.clone();
        let m = minimal_profile.clone();
        async move {
            txn.insert(&f).await?;
            txn.insert(&p).await?;
            txn.insert(&m).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify round-trip
    let fetched1: Option<Profile> = db.one(1u64).await.unwrap();
    assert_eq!(fetched1, Some(full_profile));

    let fetched2: Option<Profile> = db.one(2u64).await.unwrap();
    assert_eq!(fetched2, Some(partial_profile));

    let fetched3: Option<Profile> = db.one(3u64).await.unwrap();
    assert_eq!(fetched3, Some(minimal_profile));
}

#[tokio::test]
async fn test_derive_renamed_field() {
    let db = Database::in_memory().unwrap();

    let item = Item {
        id: 1,
        description: "A fancy widget".into(),
    };

    db.transaction(|txn| {
        let i = item.clone();
        async move {
            txn.insert(&i).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify round-trip
    let fetched: Option<Item> = db.one(1u64).await.unwrap();
    assert_eq!(fetched, Some(item));

    // Verify the key uses "desc" not "description"
    // (Check via raw storage access)
    let desc_key = Key::from(vec![1i64.to_sub(), "desc".to_sub()]);
    let val = db.get(&global!("item"), &desc_key).await.unwrap();
    assert_eq!(val, Some(Value::String("A fancy widget".into())));
}

#[tokio::test]
async fn test_derive_skipped_field() {
    let db = Database::in_memory().unwrap();

    let entry = CacheEntry {
        key: "foo".into(),
        value: "bar".into(),
        cached_at: 12345, // This should not be persisted
    };

    db.transaction(|txn| {
        let e = entry.clone();
        async move {
            txn.insert(&e).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Read back - cached_at should be default (0)
    let fetched: Option<CacheEntry> = db.one("foo").await.unwrap();
    assert!(fetched.is_some());
    let f = fetched.unwrap();
    assert_eq!(f.key, "foo");
    assert_eq!(f.value, "bar");
    assert_eq!(f.cached_at, 0); // Default value, not 12345
}

#[test]
fn test_unit_enum_to_value() {
    // ToValue
    assert_eq!(Status::Active.to_val(), Value::String("Active".into()));
    assert_eq!(Status::Inactive.to_val(), Value::String("Inactive".into()));
    assert_eq!(Status::Pending.to_val(), Value::String("Pending".into()));

    // FromValue
    assert_eq!(
        Status::from_val(&Value::String("Active".into())),
        Ok(Status::Active)
    );
    assert_eq!(
        Status::from_val(&Value::String("Inactive".into())),
        Ok(Status::Inactive)
    );
    assert_eq!(
        Status::from_val(&Value::String("Pending".into())),
        Ok(Status::Pending)
    );

    // Invalid value
    assert!(Status::from_val(&Value::String("Unknown".into())).is_err());
    assert!(Status::from_val(&Value::Integer(42)).is_err());
}

#[test]
fn test_unit_enum_to_subscript() {
    // ToSubscript
    assert_eq!(Priority::Low.to_sub(), Subscript::String("Low".into()));
    assert_eq!(
        Priority::Medium.to_sub(),
        Subscript::String("Medium".into())
    );
    assert_eq!(Priority::High.to_sub(), Subscript::String("High".into()));

    // FromSubscript
    assert_eq!(
        Priority::from_sub(&Subscript::String("Low".into())),
        Ok(Priority::Low)
    );
    assert_eq!(
        Priority::from_sub(&Subscript::String("Medium".into())),
        Ok(Priority::Medium)
    );
    assert_eq!(
        Priority::from_sub(&Subscript::String("High".into())),
        Ok(Priority::High)
    );

    // Invalid subscript
    assert!(Priority::from_sub(&Subscript::String("Critical".into())).is_err());
    assert!(Priority::from_sub(&Subscript::from(42)).is_err());
}

#[tokio::test]
async fn test_derive_all_records() {
    let db = Database::in_memory().unwrap();

    // Insert multiple persons
    db.transaction(|txn| async move {
        txn.insert(&Person {
            id: 1,
            name: "Alice".into(),
            email: "alice@test.com".into(),
        })
        .await?;
        txn.insert(&Person {
            id: 2,
            name: "Bob".into(),
            email: "bob@test.com".into(),
        })
        .await?;
        txn.insert(&Person {
            id: 3,
            name: "Charlie".into(),
            email: "charlie@test.com".into(),
        })
        .await?;
        Ok(())
    })
    .await
    .unwrap();

    // Get all
    let all: Vec<Person> = db.all().await.unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].name, "Alice");
    assert_eq!(all[1].name, "Bob");
    assert_eq!(all[2].name, "Charlie");
}

#[tokio::test]
async fn test_derive_delete() {
    let db = Database::in_memory().unwrap();

    let person = Person {
        id: 1,
        name: "Alice".into(),
        email: "alice@test.com".into(),
    };

    // Insert
    db.transaction(|txn| {
        let p = person.clone();
        async move {
            txn.insert(&p).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify exists
    assert!(db.exists::<Person, _>(1u64).await.unwrap());

    // Delete
    db.transaction(|txn| async move {
        txn.delete::<Person, _>(1u64).await?;
        Ok(())
    })
    .await
    .unwrap();

    // Verify deleted
    assert!(!db.exists::<Person, _>(1u64).await.unwrap());
}

#[tokio::test]
async fn test_derive_default_values() {
    let db = Database::in_memory().unwrap();

    // Insert a Settings record with all fields
    db.transaction(|txn| async move {
        txn.insert(&Settings {
            user_id: 1,
            theme: "dark".into(),
            page_size: 25,
        })
        .await?;
        Ok(())
    })
    .await
    .unwrap();

    // Read it back - should match what we inserted
    let s: Option<Settings> = db.one(1u64).await.unwrap();
    assert_eq!(s.as_ref().map(|s| s.theme.as_str()), Some("dark"));
    assert_eq!(s.as_ref().map(|s| s.page_size), Some(25));

    // Now manually insert a record with missing fields to test defaults
    // We'll use raw set operations to skip some fields
    db.transaction(|txn| async move {
        // Only set the marker at prefix, no theme or page_size
        let prefix = Key::from(vec![2i64.to_sub()]);
        txn.set(&global!("settings"), &prefix, Value::String(String::new()))
            .await?;
        Ok(())
    })
    .await
    .unwrap();

    // Read the partial record - defaults should kick in
    let s2: Option<Settings> = db.one(2u64).await.unwrap();
    assert!(s2.is_some());
    let s2 = s2.unwrap();
    assert_eq!(s2.theme, ""); // Default::default() for String
    assert_eq!(s2.page_size, 10); // default = 10 from attribute
}

#[tokio::test]
async fn test_derive_flatten() {
    let db = Database::in_memory().unwrap();

    let customer = Customer {
        id: 1,
        name: "Acme Corp".into(),
        addr: Address {
            _dummy: 0,
            street: "123 Main St".into(),
            city: "Boston".into(),
        },
    };

    db.transaction(|txn| {
        let c = customer.clone();
        async move {
            txn.insert(&c).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify round-trip
    let fetched: Option<Customer> = db.one(1u64).await.unwrap();
    assert_eq!(fetched.as_ref().map(|c| c.name.as_str()), Some("Acme Corp"));
    assert_eq!(
        fetched.as_ref().map(|c| c.addr.street.as_str()),
        Some("123 Main St")
    );
    assert_eq!(
        fetched.as_ref().map(|c| c.addr.city.as_str()),
        Some("Boston")
    );

    // Verify storage layout: flattened fields are at same level
    // ^customer(1, "street") should exist (not ^customer(1, "addr", "street"))
    let street_key = Key::from(vec![1i64.to_sub(), "street".to_sub()]);
    let val = db.get(&global!("customer"), &street_key).await.unwrap();
    assert_eq!(val, Some(Value::String("123 Main St".into())));
}

#[tokio::test]
async fn test_derive_subtree() {
    let db = Database::in_memory().unwrap();

    let vendor = Vendor {
        id: 1,
        name: "Widget Inc".into(),
        contact: ContactInfo {
            _dummy: 0,
            phone: "555-1234".into(),
            email: "info@widget.com".into(),
        },
    };

    db.transaction(|txn| {
        let v = vendor.clone();
        async move {
            txn.insert(&v).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify round-trip
    let fetched: Option<Vendor> = db.one(1u64).await.unwrap();
    assert_eq!(
        fetched.as_ref().map(|v| v.name.as_str()),
        Some("Widget Inc")
    );
    assert_eq!(
        fetched.as_ref().map(|v| v.contact.phone.as_str()),
        Some("555-1234")
    );
    assert_eq!(
        fetched.as_ref().map(|v| v.contact.email.as_str()),
        Some("info@widget.com")
    );

    // Verify storage layout: subtree fields are nested under field name
    // ^vendor(1, "contact", "phone") should exist
    let phone_key =
        Key::from(vec![1i64.to_sub(), "contact".to_sub(), "phone".to_sub()]);
    let val = db.get(&global!("vendor"), &phone_key).await.unwrap();
    assert_eq!(val, Some(Value::String("555-1234".into())));
}

#[test]
fn test_newtype_to_value() {
    // ToValue - delegates to inner u64
    assert_eq!(UserId(42).to_val(), Value::Integer(42));
    assert_eq!(UserId(0).to_val(), Value::Integer(0));

    // FromValue - delegates to inner u64
    assert_eq!(UserId::from_val(&Value::Integer(42)), Ok(UserId(42)));
    assert_eq!(UserId::from_val(&Value::Integer(0)), Ok(UserId(0)));

    // Error cases
    assert!(UserId::from_val(&Value::String("not a number".into())).is_err());
}

#[test]
fn test_newtype_to_subscript() {
    // ToSubscript - delegates to inner String
    assert_eq!(
        Email("test@example.com".into()).to_sub(),
        Subscript::String("test@example.com".into())
    );

    // FromSubscript - delegates to inner String
    assert_eq!(
        Email::from_sub(&Subscript::String("test@example.com".into())),
        Ok(Email("test@example.com".into()))
    );

    // Error cases
    assert!(Email::from_sub(&Subscript::from(42)).is_err());
}

#[tokio::test]
async fn test_newtype_to_rumps() {
    let db = Database::in_memory().unwrap();

    // Test 1: Default behavior - newtype has separate storage
    let person = Person {
        id: 99,
        name: "Wrapped Alice".into(),
        email: "walice@example.com".into(),
    };
    let wrapped = WrappedPerson(person.clone());

    db.transaction(|txn| {
        let w = wrapped.clone();
        async move {
            txn.insert(&w).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Read back as WrappedPerson (stored in "WrappedPerson" global)
    let fetched: Option<WrappedPerson> = db.one(99u64).await.unwrap();
    assert_eq!(fetched, Some(wrapped));

    // Person global should be empty (separate storage)
    let fetched_person: Option<Person> = db.one(99u64).await.unwrap();
    assert_eq!(fetched_person, None);

    // Test 2: Explicit shared storage via #[rumps(global = "person")]
    let person2 = Person {
        id: 100,
        name: "Aliased Bob".into(),
        email: "bob@example.com".into(),
    };
    let alias = PersonAlias(person2.clone());

    db.transaction(|txn| {
        let a = alias.clone();
        async move {
            txn.insert(&a).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Both types can read the same data
    let fetched_alias: Option<PersonAlias> = db.one(100u64).await.unwrap();
    assert_eq!(fetched_alias, Some(alias));

    let fetched_person2: Option<Person> = db.one(100u64).await.unwrap();
    assert_eq!(fetched_person2, Some(person2));
}

// =============================================================================
// Tests for reading manually-written data (no ORM markers)
//
// These tests verify that the ORM can read data written directly via
// `txn.set()` without the empty-string marker that ORM `insert()` writes.
// This exercises the heuristic fallback path in `stream_and_parse`.
// =============================================================================

/// Struct for reading manually-written "raw_user" data
#[derive(Debug, Clone, PartialEq, FromRumps)]
#[rumps(global = "raw_user")]
struct RawUser {
    #[rumps(key)]
    id: u64,
    name: String,
    email: String,
}

/// Struct for reading manually-written composite key data
#[derive(Debug, Clone, PartialEq, FromRumps)]
#[rumps(global = "raw_order")]
struct RawOrder {
    #[rumps(key, order = 0)]
    customer_id: u64,
    #[rumps(key, order = 1)]
    order_id: u64,
    product: String,
    qty: u32,
}

/// Struct with optional fields for reading sparse manual data
#[derive(Debug, Clone, PartialEq, FromRumps)]
#[rumps(global = "raw_config")]
struct RawConfig {
    #[rumps(key)]
    name: String,
    value: Option<String>,
    #[rumps(default = 0)]
    version: u32,
}

#[tokio::test]
async fn test_manual_write_single_record_one() {
    let db = Database::in_memory().unwrap();

    // Write data manually WITHOUT the ORM marker
    db.transaction(|txn| async move {
        // ^raw_user(1, "name") = "Alice"
        // ^raw_user(1, "email") = "alice@test.com"
        // Note: No marker at ^raw_user(1) = ""
        txn.set(
            &global!("raw_user"),
            &key![1i64, "name"],
            Value::String("Alice".into()),
        )
        .await?;
        txn.set(
            &global!("raw_user"),
            &key![1i64, "email"],
            Value::String("alice@test.com".into()),
        )
        .await?;
        Ok(())
    })
    .await
    .unwrap();

    // Read back using ORM
    let user: Option<RawUser> = db.one(1u64).await.unwrap();
    assert!(user.is_some());
    let u = user.unwrap();
    assert_eq!(u.id, 1);
    assert_eq!(u.name, "Alice");
    assert_eq!(u.email, "alice@test.com");
}

#[tokio::test]
async fn test_manual_write_multiple_records_all() {
    let db = Database::in_memory().unwrap();

    // Write multiple records manually WITHOUT markers
    db.transaction(|txn| async move {
        // User 1
        txn.set(
            &global!("raw_user"),
            &key![1i64, "name"],
            Value::String("Alice".into()),
        )
        .await?;
        txn.set(
            &global!("raw_user"),
            &key![1i64, "email"],
            Value::String("alice@test.com".into()),
        )
        .await?;

        // User 2
        txn.set(
            &global!("raw_user"),
            &key![2i64, "name"],
            Value::String("Bob".into()),
        )
        .await?;
        txn.set(
            &global!("raw_user"),
            &key![2i64, "email"],
            Value::String("bob@test.com".into()),
        )
        .await?;

        // User 3
        txn.set(
            &global!("raw_user"),
            &key![3i64, "name"],
            Value::String("Charlie".into()),
        )
        .await?;
        txn.set(
            &global!("raw_user"),
            &key![3i64, "email"],
            Value::String("charlie@test.com".into()),
        )
        .await?;

        Ok(())
    })
    .await
    .unwrap();

    // Read all using ORM
    let users: Vec<RawUser> = db.all().await.unwrap();
    assert_eq!(users.len(), 3);
    assert_eq!(users[0].name, "Alice");
    assert_eq!(users[1].name, "Bob");
    assert_eq!(users[2].name, "Charlie");
}

#[tokio::test]
async fn test_manual_write_composite_key_query() {
    let db = Database::in_memory().unwrap();

    // Write orders with composite keys manually
    db.transaction(|txn| async move {
        // Customer 1, Order 1
        txn.set(
            &global!("raw_order"),
            &key![1i64, 1i64, "product"],
            Value::String("Widget".into()),
        )
        .await?;
        txn.set(
            &global!("raw_order"),
            &key![1i64, 1i64, "qty"],
            Value::Integer(5),
        )
        .await?;

        // Customer 1, Order 2
        txn.set(
            &global!("raw_order"),
            &key![1i64, 2i64, "product"],
            Value::String("Gadget".into()),
        )
        .await?;
        txn.set(
            &global!("raw_order"),
            &key![1i64, 2i64, "qty"],
            Value::Integer(3),
        )
        .await?;

        // Customer 2, Order 1
        txn.set(
            &global!("raw_order"),
            &key![2i64, 1i64, "product"],
            Value::String("Gizmo".into()),
        )
        .await?;
        txn.set(
            &global!("raw_order"),
            &key![2i64, 1i64, "qty"],
            Value::Integer(10),
        )
        .await?;

        Ok(())
    })
    .await
    .unwrap();

    // Get specific order by composite key
    let order: Option<RawOrder> = db.one((1u64, 2u64)).await.unwrap();
    assert!(order.is_some());
    let o = order.unwrap();
    assert_eq!(o.customer_id, 1);
    assert_eq!(o.order_id, 2);
    assert_eq!(o.product, "Gadget");
    assert_eq!(o.qty, 3);

    // Query all orders for customer 1
    let cust1_orders: Vec<RawOrder> = db.query((1u64,)).await.unwrap();
    assert_eq!(cust1_orders.len(), 2);
    assert_eq!(cust1_orders[0].product, "Widget");
    assert_eq!(cust1_orders[1].product, "Gadget");

    // Get all orders
    let all_orders: Vec<RawOrder> = db.all().await.unwrap();
    assert_eq!(all_orders.len(), 3);
}

#[tokio::test]
async fn test_manual_write_sparse_data_with_defaults() {
    let db = Database::in_memory().unwrap();

    // Write sparse config entries - some fields missing
    db.transaction(|txn| async move {
        // Config with all fields
        txn.set(
            &global!("raw_config"),
            &key!["full", "value"],
            Value::String("enabled".into()),
        )
        .await?;
        txn.set(
            &global!("raw_config"),
            &key!["full", "version"],
            Value::Integer(2),
        )
        .await?;

        // Config with only value (version uses default)
        txn.set(
            &global!("raw_config"),
            &key!["partial", "value"],
            Value::String("some_val".into()),
        )
        .await?;

        // Config with only version (value is Option, defaults to None)
        txn.set(
            &global!("raw_config"),
            &key!["minimal", "version"],
            Value::Integer(1),
        )
        .await?;

        Ok(())
    })
    .await
    .unwrap();

    // Read all configs
    let configs: Vec<RawConfig> = db.all().await.unwrap();
    assert_eq!(configs.len(), 3);

    // Full config
    let full = configs.iter().find(|c| c.name == "full").unwrap();
    assert_eq!(full.value, Some("enabled".into()));
    assert_eq!(full.version, 2);

    // Partial config (version defaults to 0)
    let partial = configs.iter().find(|c| c.name == "partial").unwrap();
    assert_eq!(partial.value, Some("some_val".into()));
    assert_eq!(partial.version, 0);

    // Minimal config (value defaults to None)
    let minimal = configs.iter().find(|c| c.name == "minimal").unwrap();
    assert_eq!(minimal.value, None);
    assert_eq!(minimal.version, 1);
}

#[tokio::test]
async fn test_manual_write_exists_check() {
    let db = Database::in_memory().unwrap();

    // Write one record manually
    db.transaction(|txn| async move {
        txn.set(
            &global!("raw_user"),
            &key![42i64, "name"],
            Value::String("Test".into()),
        )
        .await?;
        txn.set(
            &global!("raw_user"),
            &key![42i64, "email"],
            Value::String("test@test.com".into()),
        )
        .await?;
        Ok(())
    })
    .await
    .unwrap();

    // Check existence
    assert!(db.exists::<RawUser, _>(42u64).await.unwrap());
    assert!(!db.exists::<RawUser, _>(99u64).await.unwrap());
}

#[tokio::test]
async fn test_mixed_orm_and_manual_writes() {
    let db = Database::in_memory().unwrap();

    // Insert via ORM (will write marker)
    db.transaction(|txn| async move {
        txn.insert(&Person {
            id: 1,
            name: "ORM Alice".into(),
            email: "orm@test.com".into(),
        })
        .await?;
        Ok(())
    })
    .await
    .unwrap();

    // Insert via manual write (no marker)
    db.transaction(|txn| async move {
        txn.set(
            &global!("person"),
            &key![2i64, "name"],
            Value::String("Manual Bob".into()),
        )
        .await?;
        txn.set(
            &global!("person"),
            &key![2i64, "email"],
            Value::String("manual@test.com".into()),
        )
        .await?;
        Ok(())
    })
    .await
    .unwrap();

    // Read all - both should work
    let people: Vec<Person> = db.all().await.unwrap();
    assert_eq!(people.len(), 2);
    assert_eq!(people[0].name, "ORM Alice");
    assert_eq!(people[1].name, "Manual Bob");

    // Read individual records
    let p1: Option<Person> = db.one(1u64).await.unwrap();
    assert_eq!(p1.as_ref().map(|p| p.name.as_str()), Some("ORM Alice"));

    let p2: Option<Person> = db.one(2u64).await.unwrap();
    assert_eq!(p2.as_ref().map(|p| p.name.as_str()), Some("Manual Bob"));
}

// =============================================================================
// Tests for data-carrying enums (ToRumps/FromRumps)
// =============================================================================

/// Enum with unit and struct variants
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "status")]
enum EmploymentStatus {
    Active,
    OnLeave { reason: String },
    Terminated { date: u64, reason: String },
}

/// Enum with tuple variants
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "message")]
enum Message {
    Text(String),
    Binary(Vec<u8>),
    Coords(i32, i32),
}

/// Top-level enum with per-variant keys
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "entity")]
enum Entity {
    User {
        #[rumps(key)]
        id: u64,
        name: String,
        email: String,
    },
    Product {
        #[rumps(key)]
        sku: String,
        price: f64,
    },
}

/// Enum with renamed variants
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "event")]
enum Event {
    #[rumps(rename = "created")]
    Created { ts: u64 },
    #[rumps(rename = "updated")]
    Updated { ts: u64, by: String },
}

/// Struct with embedded enum field
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "worker")]
struct Worker {
    #[rumps(key)]
    id: u64,
    name: String,
    #[rumps(subtree)]
    status: EmploymentStatus,
}

#[tokio::test]
async fn test_enum_unit_variant_roundtrip() {
    let db = Database::in_memory().unwrap();

    let status = EmploymentStatus::Active;

    db.transaction(|txn| {
        let s = status.clone();
        async move {
            txn.insert(&s).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // The key is just ["Active"]
    let fetched: Option<EmploymentStatus> = db.one("Active").await.unwrap();
    assert_eq!(fetched, Some(EmploymentStatus::Active));
}

#[tokio::test]
async fn test_enum_struct_variant_roundtrip() {
    let db = Database::in_memory().unwrap();

    let status = EmploymentStatus::OnLeave {
        reason: "vacation".into(),
    };

    db.transaction(|txn| {
        let s = status.clone();
        async move {
            txn.insert(&s).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // The key is ["OnLeave"]
    let fetched: Option<EmploymentStatus> = db.one("OnLeave").await.unwrap();
    assert_eq!(fetched, Some(status));

    // Verify storage layout: ^status("OnLeave", "reason") = "vacation"
    let reason_key = key!["OnLeave", "reason"];
    let val = db.get(&global!("status"), &reason_key).await.unwrap();
    assert_eq!(val, Some(Value::String("vacation".into())));
}

#[tokio::test]
async fn test_enum_struct_variant_multiple_fields() {
    let db = Database::in_memory().unwrap();

    let status = EmploymentStatus::Terminated {
        date: 1234567890,
        reason: "layoff".into(),
    };

    db.transaction(|txn| {
        let s = status.clone();
        async move {
            txn.insert(&s).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    let fetched: Option<EmploymentStatus> = db.one("Terminated").await.unwrap();
    assert_eq!(fetched, Some(status));
}

#[tokio::test]
async fn test_enum_single_tuple_variant() {
    let db = Database::in_memory().unwrap();

    let msg = Message::Text("hello world".into());

    db.transaction(|txn| {
        let m = msg.clone();
        async move {
            txn.insert(&m).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    let fetched: Option<Message> = db.one("Text").await.unwrap();
    assert_eq!(fetched, Some(msg));

    // Verify storage: single-field tuple stores value directly
    // ^message("Text") = "hello world"
    let key = key!["Text"];
    let val = db.get(&global!("message"), &key).await.unwrap();
    assert_eq!(val, Some(Value::String("hello world".into())));
}

#[tokio::test]
async fn test_enum_multi_tuple_variant() {
    let db = Database::in_memory().unwrap();

    let msg = Message::Coords(10, 20);

    db.transaction(|txn| {
        let m = msg.clone();
        async move {
            txn.insert(&m).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    let fetched: Option<Message> = db.one("Coords").await.unwrap();
    assert_eq!(fetched, Some(msg));

    // Verify storage: multi-field tuple uses numeric indices
    // ^message("Coords", 0) = 10
    // ^message("Coords", 1) = 20
    let key0 = key!["Coords", 0i64];
    let val0 = db.get(&global!("message"), &key0).await.unwrap();
    assert_eq!(val0, Some(Value::Integer(10)));

    let key1 = key!["Coords", 1i64];
    let val1 = db.get(&global!("message"), &key1).await.unwrap();
    assert_eq!(val1, Some(Value::Integer(20)));
}

#[tokio::test]
async fn test_enum_with_per_variant_keys() {
    let db = Database::in_memory().unwrap();

    let user = Entity::User {
        id: 42,
        name: "Alice".into(),
        email: "alice@example.com".into(),
    };
    let product = Entity::Product {
        sku: "SKU-001".into(),
        price: 29.99,
    };

    db.transaction(|txn| {
        let u = user.clone();
        let p = product.clone();
        async move {
            txn.insert(&u).await?;
            txn.insert(&p).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // User key is ["User", 42]
    let fetched_user: Option<Entity> = db.one(("User", 42u64)).await.unwrap();
    assert_eq!(fetched_user, Some(user));

    // Product key is ["Product", "SKU-001"]
    let fetched_product: Option<Entity> =
        db.one(("Product", "SKU-001")).await.unwrap();
    assert_eq!(fetched_product, Some(product));

    // Query all Users
    let users: Vec<Entity> = db.query(("User",)).await.unwrap();
    assert_eq!(users.len(), 1);

    // Query all entities
    let all: Vec<Entity> = db.all().await.unwrap();
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn test_enum_variant_rename() {
    let db = Database::in_memory().unwrap();

    let event = Event::Created { ts: 1000 };

    db.transaction(|txn| {
        let e = event.clone();
        async move {
            txn.insert(&e).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Key uses renamed tag "created" not "Created"
    let fetched: Option<Event> = db.one("created").await.unwrap();
    assert_eq!(fetched, Some(event));

    // Verify storage uses renamed tag
    let key = key!["created", "ts"];
    let val = db.get(&global!("event"), &key).await.unwrap();
    assert_eq!(val, Some(Value::Integer(1000)));
}

#[tokio::test]
async fn test_struct_with_embedded_enum() {
    let db = Database::in_memory().unwrap();

    let worker = Worker {
        id: 1,
        name: "Bob".into(),
        status: EmploymentStatus::OnLeave {
            reason: "sick leave".into(),
        },
    };

    db.transaction(|txn| {
        let w = worker.clone();
        async move {
            txn.insert(&w).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    let fetched: Option<Worker> = db.one(1u64).await.unwrap();
    assert_eq!(fetched, Some(worker));

    // Verify storage layout:
    // ^worker(1, "name") = "Bob"
    // ^worker(1, "status", "OnLeave", "reason") = "sick leave"
    let reason_key = key![1i64, "status", "OnLeave", "reason"];
    let val = db.get(&global!("worker"), &reason_key).await.unwrap();
    assert_eq!(val, Some(Value::String("sick leave".into())));
}

#[tokio::test]
async fn test_enum_all_variants_in_same_global() {
    let db = Database::in_memory().unwrap();

    // Insert all variant types
    db.transaction(|txn| async move {
        txn.insert(&EmploymentStatus::Active).await?;
        txn.insert(&EmploymentStatus::OnLeave {
            reason: "vacation".into(),
        })
        .await?;
        txn.insert(&EmploymentStatus::Terminated {
            date: 1000,
            reason: "resignation".into(),
        })
        .await?;
        Ok(())
    })
    .await
    .unwrap();

    // Get all - should find all 3 variants
    let all: Vec<EmploymentStatus> = db.all().await.unwrap();
    assert_eq!(all.len(), 3);
}
