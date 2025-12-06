//! Integration tests for the derive macros.
//!
//! These tests are in an integration test file because the derive macros
//! generate code referencing `::rumps_storage`, which requires the crate
//! to be seen as an external dependency.

#![cfg(feature = "derive")]
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

// =============================================================================
// Enum field attribute tests
// =============================================================================

/// Enum with default field values
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "task")]
enum Task {
    Pending,
    InProgress {
        assignee: String,
        #[rumps(default)]
        priority: u32,
        #[rumps(default = 100)]
        timeout: u32,
    },
}

/// Enum with skip and rename on fields
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "record")]
enum Record {
    Simple {
        val: String,
    },
    Complex {
        #[rumps(rename = "v")]
        value: String,
        #[rumps(skip)]
        cached: u32,
    },
}

#[tokio::test]
async fn test_enum_field_default_attr() {
    let db = Database::in_memory().unwrap();

    // Insert a task with all fields
    let task = Task::InProgress {
        assignee: "Alice".into(),
        priority: 5,
        timeout: 300,
    };

    db.transaction(|txn| {
        let t = task.clone();
        async move {
            txn.insert(&t).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify roundtrip with all fields
    let fetched: Option<Task> = db.one("InProgress").await.unwrap();
    assert_eq!(fetched, Some(task));

    // Now manually delete the priority and timeout fields
    db.transaction(|txn| async move {
        txn.kill(&global!("task"), &key!["InProgress", "priority"])
            .await?;
        txn.kill(&global!("task"), &key!["InProgress", "timeout"])
            .await?;
        Ok(())
    })
    .await
    .unwrap();

    // Fetch again - should get defaults
    let fetched: Option<Task> = db.one("InProgress").await.unwrap();
    assert_eq!(
        fetched,
        Some(Task::InProgress {
            assignee: "Alice".into(),
            priority: 0,  // Default::default()
            timeout: 100, // default = 100
        })
    );
}

#[tokio::test]
async fn test_enum_field_rename_attr() {
    let db = Database::in_memory().unwrap();

    let rec = Record::Complex {
        value: "hello".into(),
        cached: 42,
    };

    db.transaction(|txn| {
        let r = rec.clone();
        async move {
            txn.insert(&r).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify the field is stored with renamed key "v" not "value"
    let renamed_key = key!["Complex", "v"];
    let val = db.get(&global!("record"), &renamed_key).await.unwrap();
    assert_eq!(val, Some(Value::String("hello".into())));

    // Original name should NOT exist
    let orig_key = key!["Complex", "value"];
    let val = db.get(&global!("record"), &orig_key).await.unwrap();
    assert_eq!(val, None);
}

#[tokio::test]
async fn test_enum_field_skip_attr() {
    let db = Database::in_memory().unwrap();

    let rec = Record::Complex {
        value: "test".into(),
        cached: 999,
    };

    db.transaction(|txn| {
        let r = rec.clone();
        async move {
            txn.insert(&r).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // The "cached" field should NOT be stored
    let cached_key = key!["Complex", "cached"];
    let val = db.get(&global!("record"), &cached_key).await.unwrap();
    assert_eq!(val, None);

    // Roundtrip should restore cached to Default::default() (0)
    let fetched: Option<Record> = db.one("Complex").await.unwrap();
    assert_eq!(
        fetched,
        Some(Record::Complex {
            value: "test".into(),
            cached: 0, // Default, not 999
        })
    );
}

// =============================================================================
// Tests for rename_all attribute
// =============================================================================

/// Struct with snake_case field renaming
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "snake_item", rename_all = "snake-case")]
struct SnakeCaseItem {
    #[rumps(key)]
    item_id: u64,
    item_name: String,         // -> "item_name"
    is_active: bool,           // -> "is_active"
    created_at_timestamp: u64, // -> "created_at_timestamp"
}

/// Struct with camelCase field renaming
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "camel_item", rename_all = "camel-case")]
struct CamelCaseItem {
    #[rumps(key)]
    item_id: u64,
    item_name: String,         // -> "itemName"
    is_active: bool,           // -> "isActive"
    created_at_timestamp: u64, // -> "createdAtTimestamp"
}

/// Struct with train-case (kebab-case) field renaming
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "kebab_item", rename_all = "train-case")]
struct TrainCaseItem {
    #[rumps(key)]
    item_id: u64,
    item_name: String, // -> "item-name"
    is_active: bool,   // -> "is-active"
    created_at: u64,   // -> "created-at"
}

/// Struct with uppercase field renaming
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "upper_item", rename_all = "uppercase")]
struct UppercaseItem {
    #[rumps(key)]
    item_id: u64,
    name: String, // -> "NAME"
    active: bool, // -> "ACTIVE"
}

/// Struct with lowercase field renaming (keeps underscores)
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "lower_item", rename_all = "lowercase")]
struct LowercaseItem {
    #[rumps(key)]
    item_id: u64,
    item_name: String, // -> "item_name"
    is_active: bool,   // -> "is_active"
}

/// Enum with lowercase variant renaming
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "lower_status", rename_all = "lowercase")]
enum LowercaseStatus {
    IsActive,                         // -> "isactive"
    IsPending,                        // -> "ispending"
    WasTerminated { reason: String }, // -> "wasterminated"
}

/// Struct with SCREAMING_SNAKE_CASE field renaming
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "scream_item", rename_all = "screaming-snake-case")]
struct ScreamingSnakeItem {
    #[rumps(key)]
    item_id: u64,
    item_name: String, // -> "ITEM_NAME"
    is_active: bool,   // -> "IS_ACTIVE"
}

/// Enum with snake_case variant renaming
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "snake_status", rename_all = "snake-case")]
enum SnakeCaseStatus {
    IsActive,                         // -> "is_active"
    IsPending,                        // -> "is_pending"
    WasTerminated { reason: String }, // -> "was_terminated"
}

/// Unit enum with rename_all for ToValue/FromValue
#[derive(Debug, Clone, PartialEq, ToValue, FromValue)]
#[rumps(rename_all = "snake-case")]
enum SnakeCaseUnitEnum {
    FirstOption,  // -> "first_option"
    SecondOption, // -> "second_option"
    ThirdOption,  // -> "third_option"
}

/// Unit enum with rename_all for ToSubscript/FromSubscript
#[derive(Debug, Clone, PartialEq, ToSubscript, FromSubscript)]
#[rumps(rename_all = "camel-case")]
enum CamelCaseSubscriptEnum {
    LowPriority,  // -> "lowPriority"
    HighPriority, // -> "highPriority"
}

/// Enum with variant-level rename_all override
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "mixed_case", rename_all = "snake-case")]
enum MixedCaseEnum {
    SimpleVariant, // -> "simple_variant"
    #[rumps(rename_all = "camel-case")]
    ComplexVariant {
        field_one: String, // -> "fieldOne" (variant override)
        field_two: u32,    // -> "fieldTwo"
    },
    AnotherVariant {
        some_field: String, // -> "some_field" (container default)
    },
}

/// Enum with explicit rename combined with rename_all
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "rename_combo", rename_all = "snake-case")]
enum RenameCombined {
    #[rumps(rename = "custom_name")]
    OriginalName, // -> "custom_name" (explicit overrides rename_all)
    AnotherName, // -> "another_name" (rename_all applies)
}

/// Struct with rename on field overriding rename_all
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "field_override", rename_all = "camel-case")]
struct FieldOverride {
    #[rumps(key)]
    id: u64,
    normal_field: String, // -> "normalField" (rename_all)
    #[rumps(rename = "CUSTOM")]
    override_field: String, // -> "CUSTOM" (explicit rename)
}

#[tokio::test]
async fn test_snake_case_struct_fields() {
    let db = Database::in_memory().unwrap();

    let item = SnakeCaseItem {
        item_id: 1,
        item_name: "Widget".into(),
        is_active: true,
        created_at_timestamp: 12345,
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

    // Verify roundtrip
    let fetched: Option<SnakeCaseItem> = db.one(1u64).await.unwrap();
    assert_eq!(fetched, Some(item));

    // Verify keys use snake_case (fields already snake_case stay same)
    let key = key![1i64, "item_name"];
    let val = db.get(&global!("snake_item"), &key).await.unwrap();
    assert_eq!(val, Some(Value::String("Widget".into())));
}

#[tokio::test]
async fn test_camel_case_struct_fields() {
    let db = Database::in_memory().unwrap();

    let item = CamelCaseItem {
        item_id: 1,
        item_name: "Gadget".into(),
        is_active: false,
        created_at_timestamp: 67890,
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

    // Verify roundtrip
    let fetched: Option<CamelCaseItem> = db.one(1u64).await.unwrap();
    assert_eq!(fetched, Some(item));

    // Verify keys use camelCase
    let key = key![1i64, "itemName"];
    let val = db.get(&global!("camel_item"), &key).await.unwrap();
    assert_eq!(val, Some(Value::String("Gadget".into())));

    let key2 = key![1i64, "isActive"];
    let val2 = db.get(&global!("camel_item"), &key2).await.unwrap();
    assert_eq!(val2, Some(Value::Boolean(false)));
}

#[tokio::test]
async fn test_train_case_struct_fields() {
    let db = Database::in_memory().unwrap();

    let item = TrainCaseItem {
        item_id: 1,
        item_name: "Gizmo".into(),
        is_active: true,
        created_at: 99999,
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

    // Verify roundtrip
    let fetched: Option<TrainCaseItem> = db.one(1u64).await.unwrap();
    assert_eq!(fetched, Some(item));

    // Verify keys use train-case (kebab-case)
    let key = key![1i64, "item-name"];
    let val = db.get(&global!("kebab_item"), &key).await.unwrap();
    assert_eq!(val, Some(Value::String("Gizmo".into())));
}

#[tokio::test]
async fn test_uppercase_struct_fields() {
    let db = Database::in_memory().unwrap();

    let item = UppercaseItem {
        item_id: 1,
        name: "THING".into(),
        active: true,
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

    let fetched: Option<UppercaseItem> = db.one(1u64).await.unwrap();
    assert_eq!(fetched, Some(item));

    // Verify keys use UPPERCASE
    let key = key![1i64, "NAME"];
    let val = db.get(&global!("upper_item"), &key).await.unwrap();
    assert_eq!(val, Some(Value::String("THING".into())));
}

#[tokio::test]
async fn test_lowercase_struct_fields() {
    let db = Database::in_memory().unwrap();

    let item = LowercaseItem {
        item_id: 1,
        item_name: "thing".into(),
        is_active: false,
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

    let fetched: Option<LowercaseItem> = db.one(1u64).await.unwrap();
    assert_eq!(fetched, Some(item));

    // Verify keys use lowercase (keeps underscores)
    let key = key![1i64, "item_name"];
    let val = db.get(&global!("lower_item"), &key).await.unwrap();
    assert_eq!(val, Some(Value::String("thing".into())));
}

#[tokio::test]
async fn test_lowercase_enum_variants() {
    let db = Database::in_memory().unwrap();

    let status = LowercaseStatus::IsActive;

    db.transaction(|txn| {
        let s = status.clone();
        async move {
            txn.insert(&s).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Key uses lowercase variant name: "IsActive" -> "isactive"
    let fetched: Option<LowercaseStatus> = db.one("isactive").await.unwrap();
    assert_eq!(fetched, Some(LowercaseStatus::IsActive));

    // Test struct variant
    let term = LowercaseStatus::WasTerminated {
        reason: "timeout".into(),
    };

    db.transaction(|txn| {
        let t = term.clone();
        async move {
            txn.insert(&t).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // "WasTerminated" -> "wasterminated"
    let fetched: Option<LowercaseStatus> =
        db.one("wasterminated").await.unwrap();
    assert_eq!(fetched, Some(term));
}

#[tokio::test]
async fn test_screaming_snake_case_struct_fields() {
    let db = Database::in_memory().unwrap();

    let item = ScreamingSnakeItem {
        item_id: 1,
        item_name: "SCREAMER".into(),
        is_active: true,
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

    let fetched: Option<ScreamingSnakeItem> = db.one(1u64).await.unwrap();
    assert_eq!(fetched, Some(item));

    // Verify keys use SCREAMING_SNAKE_CASE
    let key = key![1i64, "ITEM_NAME"];
    let val = db.get(&global!("scream_item"), &key).await.unwrap();
    assert_eq!(val, Some(Value::String("SCREAMER".into())));

    let key2 = key![1i64, "IS_ACTIVE"];
    let val2 = db.get(&global!("scream_item"), &key2).await.unwrap();
    assert_eq!(val2, Some(Value::Boolean(true)));
}

#[tokio::test]
async fn test_snake_case_enum_variants() {
    let db = Database::in_memory().unwrap();

    let status = SnakeCaseStatus::IsActive;

    db.transaction(|txn| {
        let s = status.clone();
        async move {
            txn.insert(&s).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Key uses snake_case variant name
    let fetched: Option<SnakeCaseStatus> = db.one("is_active").await.unwrap();
    assert_eq!(fetched, Some(SnakeCaseStatus::IsActive));

    // Test struct variant
    let term = SnakeCaseStatus::WasTerminated {
        reason: "layoff".into(),
    };

    db.transaction(|txn| {
        let t = term.clone();
        async move {
            txn.insert(&t).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    let fetched: Option<SnakeCaseStatus> =
        db.one("was_terminated").await.unwrap();
    assert_eq!(fetched, Some(term));
}

#[test]
fn test_unit_enum_rename_all_to_value() {
    // ToValue
    assert_eq!(
        SnakeCaseUnitEnum::FirstOption.to_val(),
        Value::String("first_option".into())
    );
    assert_eq!(
        SnakeCaseUnitEnum::SecondOption.to_val(),
        Value::String("second_option".into())
    );
    assert_eq!(
        SnakeCaseUnitEnum::ThirdOption.to_val(),
        Value::String("third_option".into())
    );

    // FromValue
    assert_eq!(
        SnakeCaseUnitEnum::from_val(&Value::String("first_option".into())),
        Ok(SnakeCaseUnitEnum::FirstOption)
    );
    assert_eq!(
        SnakeCaseUnitEnum::from_val(&Value::String("second_option".into())),
        Ok(SnakeCaseUnitEnum::SecondOption)
    );

    // Invalid - original name shouldn't work
    assert!(
        SnakeCaseUnitEnum::from_val(&Value::String("FirstOption".into()))
            .is_err()
    );
}

#[test]
fn test_unit_enum_rename_all_to_subscript() {
    // ToSubscript
    assert_eq!(
        CamelCaseSubscriptEnum::LowPriority.to_sub(),
        Subscript::String("lowPriority".into())
    );
    assert_eq!(
        CamelCaseSubscriptEnum::HighPriority.to_sub(),
        Subscript::String("highPriority".into())
    );

    // FromSubscript
    assert_eq!(
        CamelCaseSubscriptEnum::from_sub(&Subscript::String(
            "lowPriority".into()
        )),
        Ok(CamelCaseSubscriptEnum::LowPriority)
    );
    assert_eq!(
        CamelCaseSubscriptEnum::from_sub(&Subscript::String(
            "highPriority".into()
        )),
        Ok(CamelCaseSubscriptEnum::HighPriority)
    );

    // Invalid - original name shouldn't work
    assert!(CamelCaseSubscriptEnum::from_sub(&Subscript::String(
        "LowPriority".into()
    ))
    .is_err());
}

#[tokio::test]
async fn test_variant_rename_all_override() {
    let db = Database::in_memory().unwrap();

    // Test variant with different rename_all than container
    let complex = MixedCaseEnum::ComplexVariant {
        field_one: "value1".into(),
        field_two: 42,
    };

    db.transaction(|txn| {
        let c = complex.clone();
        async move {
            txn.insert(&c).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Variant name uses container's rename_all (snake_case)
    let fetched: Option<MixedCaseEnum> =
        db.one("complex_variant").await.unwrap();
    assert_eq!(fetched, Some(complex));

    // Field names use variant's rename_all (camelCase)
    let key = key!["complex_variant", "fieldOne"];
    let val = db.get(&global!("mixed_case"), &key).await.unwrap();
    assert_eq!(val, Some(Value::String("value1".into())));

    // Test variant without override (uses container default)
    let another = MixedCaseEnum::AnotherVariant {
        some_field: "test".into(),
    };

    db.transaction(|txn| {
        let a = another.clone();
        async move {
            txn.insert(&a).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Field names use container's rename_all (snake_case)
    let key2 = key!["another_variant", "some_field"];
    let val2 = db.get(&global!("mixed_case"), &key2).await.unwrap();
    assert_eq!(val2, Some(Value::String("test".into())));
}

#[tokio::test]
async fn test_explicit_rename_overrides_rename_all() {
    let db = Database::in_memory().unwrap();

    // Test enum variant with explicit rename
    db.transaction(|txn| async move {
        txn.insert(&RenameCombined::OriginalName).await?;
        txn.insert(&RenameCombined::AnotherName).await?;
        Ok(())
    })
    .await
    .unwrap();

    // Explicit rename takes precedence
    let fetched1: Option<RenameCombined> = db.one("custom_name").await.unwrap();
    assert_eq!(fetched1, Some(RenameCombined::OriginalName));

    // rename_all applies when no explicit rename
    let fetched2: Option<RenameCombined> =
        db.one("another_name").await.unwrap();
    assert_eq!(fetched2, Some(RenameCombined::AnotherName));
}

#[tokio::test]
async fn test_field_rename_overrides_rename_all() {
    let db = Database::in_memory().unwrap();

    let item = FieldOverride {
        id: 1,
        normal_field: "normal".into(),
        override_field: "override".into(),
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

    let fetched: Option<FieldOverride> = db.one(1u64).await.unwrap();
    assert_eq!(fetched, Some(item));

    // Normal field uses rename_all (camelCase)
    let key1 = key![1i64, "normalField"];
    let val1 = db.get(&global!("field_override"), &key1).await.unwrap();
    assert_eq!(val1, Some(Value::String("normal".into())));

    // Override field uses explicit rename
    let key2 = key![1i64, "CUSTOM"];
    let val2 = db.get(&global!("field_override"), &key2).await.unwrap();
    assert_eq!(val2, Some(Value::String("override".into())));
}

// =============================================================================
// Tests for field-level rename with case transformation
// =============================================================================

/// Struct with per-field case transformations
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "mixed_field")]
struct MixedFieldRename {
    #[rumps(key)]
    id: u64,
    // Apply camelCase to just this field
    #[rumps(rename = "camel-case")]
    some_field_name: String, // -> "someFieldName"
    // Apply UPPERCASE to just this field (keeps underscores)
    #[rumps(rename = "uppercase")]
    another_field: String, // -> "ANOTHER_FIELD"
    // Literal rename (not a case keyword)
    #[rumps(rename = "custom_literal")]
    third_field: String, // -> "custom_literal"
    // No rename - uses field name as-is
    plain_field: String, // -> "plain_field"
}

/// Enum with per-variant case transformations
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "mixed_variant")]
enum MixedVariantRename {
    // Apply snake_case to just this variant
    #[rumps(rename = "snake-case")]
    SomeVariantName, // -> "some_variant_name"
    // Apply camelCase to just this variant
    #[rumps(rename = "camel-case")]
    AnotherVariantHere {
        val: u32,
    }, // -> "anotherVariantHere"
    // Literal rename
    #[rumps(rename = "custom_tag")]
    ThirdVariant, // -> "custom_tag"
    // No rename - uses variant name as-is
    PlainVariant, // -> "PlainVariant"
}

/// Unit enum with per-variant case transformations
#[derive(Debug, Clone, PartialEq, ToValue, FromValue)]
enum MixedUnitRename {
    #[rumps(rename = "snake-case")]
    FirstOption, // -> "first_option"
    #[rumps(rename = "uppercase")]
    SecondOption, // -> "SECONDOPTION"
    #[rumps(rename = "literal_name")]
    ThirdOption, // -> "literal_name"
    FourthOption, // -> "FourthOption"
}

/// Unit enum with container-level `rename_all` for ToValue/FromValue
#[derive(Debug, Clone, PartialEq, ToValue, FromValue)]
#[rumps(rename_all = "lowercase")]
enum PriorityValue {
    Low,    // -> "low"
    Medium, // -> "medium"
    High,   // -> "high"
    #[rumps(rename = "CRITICAL")]
    Urgent, // -> "CRITICAL" (override)
}

/// Unit enum with container-level `rename_all` for ToSubscript/FromSubscript
#[derive(Debug, Clone, PartialEq, ToSubscript, FromSubscript)]
#[rumps(rename_all = "snake-case")]
enum StatusSubscript {
    InProgress, // -> "in_progress"
    OnHold,     // -> "on_hold"
    Completed,  // -> "completed"
    #[rumps(rename = "CANCELLED")]
    WasCancelled, // -> "CANCELLED" (override)
}

#[tokio::test]
async fn test_field_level_case_transformation() {
    let db = Database::in_memory().unwrap();

    let item = MixedFieldRename {
        id: 1,
        some_field_name: "camel".into(),
        another_field: "upper".into(),
        third_field: "literal".into(),
        plain_field: "plain".into(),
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

    // Verify roundtrip
    let fetched: Option<MixedFieldRename> = db.one(1u64).await.unwrap();
    assert_eq!(fetched, Some(item));

    // Verify camelCase transformation on some_field_name
    let key1 = key![1i64, "someFieldName"];
    let val1 = db.get(&global!("mixed_field"), &key1).await.unwrap();
    assert_eq!(val1, Some(Value::String("camel".into())));

    // Verify UPPERCASE transformation on another_field (keeps underscores)
    let key2 = key![1i64, "ANOTHER_FIELD"];
    let val2 = db.get(&global!("mixed_field"), &key2).await.unwrap();
    assert_eq!(val2, Some(Value::String("upper".into())));

    // Verify literal rename on third_field
    let key3 = key![1i64, "custom_literal"];
    let val3 = db.get(&global!("mixed_field"), &key3).await.unwrap();
    assert_eq!(val3, Some(Value::String("literal".into())));

    // Verify no rename on plain_field
    let key4 = key![1i64, "plain_field"];
    let val4 = db.get(&global!("mixed_field"), &key4).await.unwrap();
    assert_eq!(val4, Some(Value::String("plain".into())));
}

#[tokio::test]
async fn test_variant_level_case_transformation() {
    let db = Database::in_memory().unwrap();

    // Test snake_case variant
    db.transaction(|txn| async move {
        txn.insert(&MixedVariantRename::SomeVariantName).await?;
        Ok(())
    })
    .await
    .unwrap();

    let fetched: Option<MixedVariantRename> =
        db.one("some_variant_name").await.unwrap();
    assert_eq!(fetched, Some(MixedVariantRename::SomeVariantName));

    // Test camelCase variant
    db.transaction(|txn| async move {
        txn.insert(&MixedVariantRename::AnotherVariantHere { val: 42 })
            .await?;
        Ok(())
    })
    .await
    .unwrap();

    let fetched: Option<MixedVariantRename> =
        db.one("anotherVariantHere").await.unwrap();
    assert_eq!(
        fetched,
        Some(MixedVariantRename::AnotherVariantHere { val: 42 })
    );

    // Test literal rename variant
    db.transaction(|txn| async move {
        txn.insert(&MixedVariantRename::ThirdVariant).await?;
        Ok(())
    })
    .await
    .unwrap();

    let fetched: Option<MixedVariantRename> =
        db.one("custom_tag").await.unwrap();
    assert_eq!(fetched, Some(MixedVariantRename::ThirdVariant));

    // Test no rename variant
    db.transaction(|txn| async move {
        txn.insert(&MixedVariantRename::PlainVariant).await?;
        Ok(())
    })
    .await
    .unwrap();

    let fetched: Option<MixedVariantRename> =
        db.one("PlainVariant").await.unwrap();
    assert_eq!(fetched, Some(MixedVariantRename::PlainVariant));
}

#[test]
fn test_unit_enum_field_level_case_transformation() {
    // Test snake_case
    assert_eq!(
        MixedUnitRename::FirstOption.to_val(),
        Value::String("first_option".into())
    );
    assert_eq!(
        MixedUnitRename::from_val(&Value::String("first_option".into())),
        Ok(MixedUnitRename::FirstOption)
    );

    // Test uppercase
    assert_eq!(
        MixedUnitRename::SecondOption.to_val(),
        Value::String("SECONDOPTION".into())
    );
    assert_eq!(
        MixedUnitRename::from_val(&Value::String("SECONDOPTION".into())),
        Ok(MixedUnitRename::SecondOption)
    );

    // Test literal
    assert_eq!(
        MixedUnitRename::ThirdOption.to_val(),
        Value::String("literal_name".into())
    );
    assert_eq!(
        MixedUnitRename::from_val(&Value::String("literal_name".into())),
        Ok(MixedUnitRename::ThirdOption)
    );

    // Test no rename
    assert_eq!(
        MixedUnitRename::FourthOption.to_val(),
        Value::String("FourthOption".into())
    );
    assert_eq!(
        MixedUnitRename::from_val(&Value::String("FourthOption".into())),
        Ok(MixedUnitRename::FourthOption)
    );
}

#[test]
fn test_unit_enum_variant_rename_override_value() {
    // Test container-level rename_all = "lowercase" with variant override
    assert_eq!(PriorityValue::Low.to_val(), Value::String("low".into()));
    assert_eq!(
        PriorityValue::Medium.to_val(),
        Value::String("medium".into())
    );
    assert_eq!(PriorityValue::High.to_val(), Value::String("high".into()));
    // Test variant-level override with literal string
    assert_eq!(
        PriorityValue::Urgent.to_val(),
        Value::String("CRITICAL".into())
    );

    // Test FromValue round-trip
    assert_eq!(
        PriorityValue::from_val(&Value::String("low".into())),
        Ok(PriorityValue::Low)
    );
    assert_eq!(
        PriorityValue::from_val(&Value::String("CRITICAL".into())),
        Ok(PriorityValue::Urgent)
    );

    // Error: original variant name shouldn't work when renamed
    assert!(PriorityValue::from_val(&Value::String("Urgent".into())).is_err());
}

#[test]
fn test_unit_enum_variant_rename_override_subscript() {
    // Test container-level rename_all = "snake-case" with variant override
    assert_eq!(
        StatusSubscript::InProgress.to_sub(),
        Subscript::String("in_progress".into())
    );
    assert_eq!(
        StatusSubscript::OnHold.to_sub(),
        Subscript::String("on_hold".into())
    );
    // Test variant-level override with literal string
    assert_eq!(
        StatusSubscript::WasCancelled.to_sub(),
        Subscript::String("CANCELLED".into())
    );

    // Test FromSubscript round-trip
    assert_eq!(
        StatusSubscript::from_sub(&Subscript::String("in_progress".into())),
        Ok(StatusSubscript::InProgress)
    );
    assert_eq!(
        StatusSubscript::from_sub(&Subscript::String("CANCELLED".into())),
        Ok(StatusSubscript::WasCancelled)
    );

    // Error: original variant name shouldn't work when renamed
    assert!(StatusSubscript::from_sub(&Subscript::String(
        "WasCancelled".into()
    ))
    .is_err());
}
