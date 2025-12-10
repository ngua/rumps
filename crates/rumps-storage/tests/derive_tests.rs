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
use rumps_types::{global, key, value, Key, Subscript};

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
            p.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Read back
    let fetched = Person::one(&db, 42u64).await.unwrap();
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
            e1.insert(&txn).await?;
            e2.insert(&txn).await?;
            e3.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Get by composite key
    let fetched = Employee::one(&db, ("engineering", 1u64)).await.unwrap();
    assert_eq!(fetched, Some(emp1.clone()));

    // Query by prefix (all engineering employees)
    let eng_emps = Employee::query(&db, ("engineering",)).await.unwrap();
    assert_eq!(eng_emps.len(), 2);

    // Get all employees
    let all_emps = Employee::all(&db).await.unwrap();
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
            f.insert(&txn).await?;
            p.insert(&txn).await?;
            m.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify round-trip
    let fetched1 = Profile::one(&db, 1u64).await.unwrap();
    assert_eq!(fetched1, Some(full_profile));

    let fetched2 = Profile::one(&db, 2u64).await.unwrap();
    assert_eq!(fetched2, Some(partial_profile));

    let fetched3 = Profile::one(&db, 3u64).await.unwrap();
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
            i.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify round-trip
    let fetched = Item::one(&db, 1u64).await.unwrap();
    assert_eq!(fetched, Some(item));

    // Verify the key uses "desc" not "description"
    // (Check via raw storage access)
    let desc_key = key![1i64, "desc"];
    let val = db.get(&global!("item"), &desc_key).await.unwrap();
    assert_eq!(val, Some(value!("A fancy widget")));
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
            e.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Read back; `cached_at` should be default (`0`)
    let fetched = CacheEntry::one(&db, "foo").await.unwrap();
    assert!(fetched.is_some());
    let f = fetched.unwrap();
    assert_eq!(f.key, "foo");
    assert_eq!(f.value, "bar");
    assert_eq!(f.cached_at, 0); // Default value, not 12345
}

#[test]
fn test_unit_enum_to_value() {
    // ToValue
    assert_eq!(Status::Active.to_val(), value!("Active"));
    assert_eq!(Status::Inactive.to_val(), value!("Inactive"));
    assert_eq!(Status::Pending.to_val(), value!("Pending"));

    // FromValue
    assert_eq!(Status::from_val(&value!("Active")), Ok(Status::Active));
    assert_eq!(Status::from_val(&value!("Inactive")), Ok(Status::Inactive));
    assert_eq!(Status::from_val(&value!("Pending")), Ok(Status::Pending));

    // Invalid value
    assert!(Status::from_val(&value!("Unknown")).is_err());
    assert!(Status::from_val(&value!(42)).is_err());
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
        Person {
            id: 1,
            name: "Alice".into(),
            email: "alice@test.com".into(),
        }
        .insert(&txn)
        .await?;
        Person {
            id: 2,
            name: "Bob".into(),
            email: "bob@test.com".into(),
        }
        .insert(&txn)
        .await?;
        Person {
            id: 3,
            name: "Charlie".into(),
            email: "charlie@test.com".into(),
        }
        .insert(&txn)
        .await?;
        Ok(())
    })
    .await
    .unwrap();

    // Get all
    let all = Person::all(&db).await.unwrap();
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
            p.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify exists
    assert!(Person::exists(&db, 1u64).await.unwrap());

    // Delete
    db.transaction(|txn| async move {
        Person::delete(&txn, 1u64).await?;
        Ok(())
    })
    .await
    .unwrap();

    // Verify deleted
    assert!(!Person::exists(&db, 1u64).await.unwrap());
}

#[tokio::test]
async fn test_derive_default_values() {
    let db = Database::in_memory().unwrap();

    // Insert a Settings record with all fields
    db.transaction(|txn| async move {
        Settings {
            user_id: 1,
            theme: "dark".into(),
            page_size: 25,
        }
        .insert(&txn)
        .await?;
        Ok(())
    })
    .await
    .unwrap();

    // Read it back - should match what we inserted
    let s = Settings::one(&db, 1u64).await.unwrap();
    assert_eq!(s.as_ref().map(|s| s.theme.as_str()), Some("dark"));
    assert_eq!(s.as_ref().map(|s| s.page_size), Some(25));

    // Now manually insert a record with missing fields to test defaults
    // We'll use raw set operations to skip some fields
    db.transaction(|txn| async move {
        // Only set the marker at prefix, no theme or page_size
        let prefix = key![2i64];
        txn.set(&global!("settings"), &prefix, value!("")).await?;
        Ok(())
    })
    .await
    .unwrap();

    // Read the partial record - defaults should kick in
    let s2 = Settings::one(&db, 2u64).await.unwrap();
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
            c.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify round-trip
    let fetched = Customer::one(&db, 1u64).await.unwrap();
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
    let street_key = key![1i64, "street"];
    let val = db.get(&global!("customer"), &street_key).await.unwrap();
    assert_eq!(val, Some(value!("123 Main St")));
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
            v.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify round-trip
    let fetched = Vendor::one(&db, 1u64).await.unwrap();
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
    let phone_key = key![1i64, "contact", "phone"];
    let val = db.get(&global!("vendor"), &phone_key).await.unwrap();
    assert_eq!(val, Some(value!("555-1234")));
}

#[test]
fn test_newtype_to_value() {
    // ToValue - delegates to inner u64
    assert_eq!(UserId(42).to_val(), value!(42));
    assert_eq!(UserId(0).to_val(), value!(0));

    // FromValue - delegates to inner u64
    assert_eq!(UserId::from_val(&value!(42)), Ok(UserId(42)));
    assert_eq!(UserId::from_val(&value!(0)), Ok(UserId(0)));

    // Error cases
    assert!(UserId::from_val(&value!("not a number")).is_err());
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
            w.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Read back as `WrappedPerson` (stored in "WrappedPerson" global)
    let fetched = WrappedPerson::one(&db, 99u64).await.unwrap();
    assert_eq!(fetched, Some(wrapped));

    // `Person` global should be empty (separate storage)
    let fetched_person = Person::one(&db, 99u64).await.unwrap();
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
            a.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Both types can read the same data
    let fetched_alias = PersonAlias::one(&db, 100u64).await.unwrap();
    assert_eq!(fetched_alias, Some(alias));

    let fetched_person2 = Person::one(&db, 100u64).await.unwrap();
    assert_eq!(fetched_person2, Some(person2));
}

// Tests for reading manually-written data (no ORM markers)
//
// These tests verify that the ORM can read data written directly via
// `txn.set()` without the empty-string marker that ORM `insert()` writes.
// This exercises the heuristic fallback path in `stream_and_parse`.

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
        txn.set(&global!("raw_user"), &key![1i64, "name"], value!("Alice"))
            .await?;
        txn.set(
            &global!("raw_user"),
            &key![1i64, "email"],
            value!("alice@test.com"),
        )
        .await?;
        Ok(())
    })
    .await
    .unwrap();

    // Read back using ORM
    let user = RawUser::one(&db, 1u64).await.unwrap();
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
        txn.set(&global!("raw_user"), &key![1i64, "name"], value!("Alice"))
            .await?;
        txn.set(
            &global!("raw_user"),
            &key![1i64, "email"],
            value!("alice@test.com"),
        )
        .await?;

        // User 2
        txn.set(&global!("raw_user"), &key![2i64, "name"], value!("Bob"))
            .await?;
        txn.set(
            &global!("raw_user"),
            &key![2i64, "email"],
            value!("bob@test.com"),
        )
        .await?;

        // User 3
        txn.set(&global!("raw_user"), &key![3i64, "name"], value!("Charlie"))
            .await?;
        txn.set(
            &global!("raw_user"),
            &key![3i64, "email"],
            value!("charlie@test.com"),
        )
        .await?;

        Ok(())
    })
    .await
    .unwrap();

    // Read all using ORM
    let users = RawUser::all(&db).await.unwrap();
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
            value!("Widget"),
        )
        .await?;
        txn.set(&global!("raw_order"), &key![1i64, 1i64, "qty"], value!(5))
            .await?;

        // Customer 1, Order 2
        txn.set(
            &global!("raw_order"),
            &key![1i64, 2i64, "product"],
            value!("Gadget"),
        )
        .await?;
        txn.set(&global!("raw_order"), &key![1i64, 2i64, "qty"], value!(3))
            .await?;

        // Customer 2, Order 1
        txn.set(
            &global!("raw_order"),
            &key![2i64, 1i64, "product"],
            value!("Gizmo"),
        )
        .await?;
        txn.set(&global!("raw_order"), &key![2i64, 1i64, "qty"], value!(10))
            .await?;

        Ok(())
    })
    .await
    .unwrap();

    // Get specific order by composite key
    let order = RawOrder::one(&db, (1u64, 2u64)).await.unwrap();
    assert!(order.is_some());
    let o = order.unwrap();
    assert_eq!(o.customer_id, 1);
    assert_eq!(o.order_id, 2);
    assert_eq!(o.product, "Gadget");
    assert_eq!(o.qty, 3);

    // Query all orders for customer 1
    let cust1_orders = RawOrder::query(&db, (1u64,)).await.unwrap();
    assert_eq!(cust1_orders.len(), 2);
    assert_eq!(cust1_orders[0].product, "Widget");
    assert_eq!(cust1_orders[1].product, "Gadget");

    // Get all orders
    let all_orders = RawOrder::all(&db).await.unwrap();
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
            value!("enabled"),
        )
        .await?;
        txn.set(&global!("raw_config"), &key!["full", "version"], value!(2))
            .await?;

        // Config with only value (version uses default)
        txn.set(
            &global!("raw_config"),
            &key!["partial", "value"],
            value!("some_val"),
        )
        .await?;

        // Config with only version (value is Option, defaults to None)
        txn.set(
            &global!("raw_config"),
            &key!["minimal", "version"],
            value!(1),
        )
        .await?;

        Ok(())
    })
    .await
    .unwrap();

    // Read all configs
    let configs = RawConfig::all(&db).await.unwrap();
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
        txn.set(&global!("raw_user"), &key![42i64, "name"], value!("Test"))
            .await?;
        txn.set(
            &global!("raw_user"),
            &key![42i64, "email"],
            value!("test@test.com"),
        )
        .await?;
        Ok(())
    })
    .await
    .unwrap();

    // Check existence
    assert!(RawUser::exists(&db, 42u64).await.unwrap());
    assert!(!RawUser::exists(&db, 99u64).await.unwrap());
}

#[tokio::test]
async fn test_mixed_orm_and_manual_writes() {
    let db = Database::in_memory().unwrap();

    // Insert via ORM (will write marker)
    db.transaction(|txn| async move {
        Person {
            id: 1,
            name: "ORM Alice".into(),
            email: "orm@test.com".into(),
        }
        .insert(&txn)
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
            value!("Manual Bob"),
        )
        .await?;
        txn.set(
            &global!("person"),
            &key![2i64, "email"],
            value!("manual@test.com"),
        )
        .await?;
        Ok(())
    })
    .await
    .unwrap();

    // Read all; both should work
    let people = Person::all(&db).await.unwrap();
    assert_eq!(people.len(), 2);
    assert_eq!(people[0].name, "ORM Alice");
    assert_eq!(people[1].name, "Manual Bob");

    // Read individual records
    let p1 = Person::one(&db, 1u64).await.unwrap();
    assert_eq!(p1.as_ref().map(|p| p.name.as_str()), Some("ORM Alice"));

    let p2 = Person::one(&db, 2u64).await.unwrap();
    assert_eq!(p2.as_ref().map(|p| p.name.as_str()), Some("Manual Bob"));
}

// Tests for data-carrying enums (ToRumps/FromRumps)

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
            s.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // The key is just `["Active"]`
    let fetched = EmploymentStatus::one(&db, "Active").await.unwrap();
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
            s.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // The key is `["OnLeave"]`
    let fetched = EmploymentStatus::one(&db, "OnLeave").await.unwrap();
    assert_eq!(fetched, Some(status));

    // Verify storage layout: ^status("OnLeave", "reason") = "vacation"
    let reason_key = key!["OnLeave", "reason"];
    let val = db.get(&global!("status"), &reason_key).await.unwrap();
    assert_eq!(val, Some(value!("vacation")));
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
            s.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    let fetched = EmploymentStatus::one(&db, "Terminated").await.unwrap();
    assert_eq!(fetched, Some(status));
}

#[tokio::test]
async fn test_enum_single_tuple_variant() {
    let db = Database::in_memory().unwrap();

    let msg = Message::Text("hello world".into());

    db.transaction(|txn| {
        let m = msg.clone();
        async move {
            m.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    let fetched = Message::one(&db, "Text").await.unwrap();
    assert_eq!(fetched, Some(msg));

    // Verify storage: single-field tuple stores value directly
    // ^message("Text") = "hello world"
    let key = key!["Text"];
    let val = db.get(&global!("message"), &key).await.unwrap();
    assert_eq!(val, Some(value!("hello world")));
}

#[tokio::test]
async fn test_enum_multi_tuple_variant() {
    let db = Database::in_memory().unwrap();

    let msg = Message::Coords(10, 20);

    db.transaction(|txn| {
        let m = msg.clone();
        async move {
            m.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    let fetched = Message::one(&db, "Coords").await.unwrap();
    assert_eq!(fetched, Some(msg));

    // Verify storage: multi-field tuple uses numeric indices
    // ^message("Coords", 0) = 10
    // ^message("Coords", 1) = 20
    let key0 = key!["Coords", 0i64];
    let val0 = db.get(&global!("message"), &key0).await.unwrap();
    assert_eq!(val0, Some(value!(10)));

    let key1 = key!["Coords", 1i64];
    let val1 = db.get(&global!("message"), &key1).await.unwrap();
    assert_eq!(val1, Some(value!(20)));
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
            u.insert(&txn).await?;
            p.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // User key is `["User", 42]`
    let fetched_user = Entity::one(&db, ("User", 42u64)).await.unwrap();
    assert_eq!(fetched_user, Some(user));

    // Product key is `["Product", "SKU-001"]`
    let fetched_product =
        Entity::one(&db, ("Product", "SKU-001")).await.unwrap();
    assert_eq!(fetched_product, Some(product));

    // Query all Users
    let users = Entity::query(&db, ("User",)).await.unwrap();
    assert_eq!(users.len(), 1);

    // Query all entities
    let all = Entity::all(&db).await.unwrap();
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn test_enum_variant_rename() {
    let db = Database::in_memory().unwrap();

    let event = Event::Created { ts: 1000 };

    db.transaction(|txn| {
        let e = event.clone();
        async move {
            e.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Key uses renamed tag "created" not "Created"
    let fetched = Event::one(&db, "created").await.unwrap();
    assert_eq!(fetched, Some(event));

    // Verify storage uses renamed tag
    let key = key!["created", "ts"];
    let val = db.get(&global!("event"), &key).await.unwrap();
    assert_eq!(val, Some(value!(1000)));
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
            w.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    let fetched = Worker::one(&db, 1u64).await.unwrap();
    assert_eq!(fetched, Some(worker));

    // Verify storage layout:
    // ^worker(1, "name") = "Bob"
    // ^worker(1, "status", "OnLeave", "reason") = "sick leave"
    let reason_key = key![1i64, "status", "OnLeave", "reason"];
    let val = db.get(&global!("worker"), &reason_key).await.unwrap();
    assert_eq!(val, Some(value!("sick leave")));
}

#[tokio::test]
async fn test_enum_all_variants_in_same_global() {
    let db = Database::in_memory().unwrap();

    // Insert all variant types
    db.transaction(|txn| async move {
        EmploymentStatus::Active.insert(&txn).await?;
        EmploymentStatus::OnLeave {
            reason: "vacation".into(),
        }
        .insert(&txn)
        .await?;
        EmploymentStatus::Terminated {
            date: 1000,
            reason: "resignation".into(),
        }
        .insert(&txn)
        .await?;
        Ok(())
    })
    .await
    .unwrap();

    // Get all; should find all 3 variants
    let all = EmploymentStatus::all(&db).await.unwrap();
    assert_eq!(all.len(), 3);
}

// Enum field attribute tests

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
            t.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify roundtrip with all fields
    let fetched = Task::one(&db, "InProgress").await.unwrap();
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

    // Fetch again; should get defaults
    let fetched = Task::one(&db, "InProgress").await.unwrap();
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
            r.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify the field is stored with renamed key "v" not "value"
    let renamed_key = key!["Complex", "v"];
    let val = db.get(&global!("record"), &renamed_key).await.unwrap();
    assert_eq!(val, Some(value!("hello")));

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
            r.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // The "cached" field should NOT be stored
    let cached_key = key!["Complex", "cached"];
    let val = db.get(&global!("record"), &cached_key).await.unwrap();
    assert_eq!(val, None);

    // Roundtrip should restore `cached` to `Default::default()` (`0`)
    let fetched = Record::one(&db, "Complex").await.unwrap();
    assert_eq!(
        fetched,
        Some(Record::Complex {
            value: "test".into(),
            cached: 0, // Default, not 999
        })
    );
}

// Tests for rename_all attribute

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
            i.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify roundtrip
    let fetched = SnakeCaseItem::one(&db, 1u64).await.unwrap();
    assert_eq!(fetched, Some(item));

    // Verify keys use snake_case (fields already snake_case stay same)
    let key = key![1i64, "item_name"];
    let val = db.get(&global!("snake_item"), &key).await.unwrap();
    assert_eq!(val, Some(value!("Widget")));
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
            i.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify roundtrip
    let fetched = CamelCaseItem::one(&db, 1u64).await.unwrap();
    assert_eq!(fetched, Some(item));

    // Verify keys use camelCase
    let key = key![1i64, "itemName"];
    let val = db.get(&global!("camel_item"), &key).await.unwrap();
    assert_eq!(val, Some(value!("Gadget")));

    let key2 = key![1i64, "isActive"];
    let val2 = db.get(&global!("camel_item"), &key2).await.unwrap();
    assert_eq!(val2, Some(value!(false)));
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
            i.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify roundtrip
    let fetched = TrainCaseItem::one(&db, 1u64).await.unwrap();
    assert_eq!(fetched, Some(item));

    // Verify keys use train-case (kebab-case)
    let key = key![1i64, "item-name"];
    let val = db.get(&global!("kebab_item"), &key).await.unwrap();
    assert_eq!(val, Some(value!("Gizmo")));
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
            i.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    let fetched = UppercaseItem::one(&db, 1u64).await.unwrap();
    assert_eq!(fetched, Some(item));

    // Verify keys use UPPERCASE
    let key = key![1i64, "NAME"];
    let val = db.get(&global!("upper_item"), &key).await.unwrap();
    assert_eq!(val, Some(value!("THING")));
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
            i.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    let fetched = LowercaseItem::one(&db, 1u64).await.unwrap();
    assert_eq!(fetched, Some(item));

    // Verify keys use lowercase (keeps underscores)
    let key = key![1i64, "item_name"];
    let val = db.get(&global!("lower_item"), &key).await.unwrap();
    assert_eq!(val, Some(value!("thing")));
}

#[tokio::test]
async fn test_lowercase_enum_variants() {
    let db = Database::in_memory().unwrap();

    let status = LowercaseStatus::IsActive;

    db.transaction(|txn| {
        let s = status.clone();
        async move {
            s.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Key uses lowercase variant name: "IsActive" -> "isactive"
    let fetched = LowercaseStatus::one(&db, "isactive").await.unwrap();
    assert_eq!(fetched, Some(LowercaseStatus::IsActive));

    // Test struct variant
    let term = LowercaseStatus::WasTerminated {
        reason: "timeout".into(),
    };

    db.transaction(|txn| {
        let t = term.clone();
        async move {
            t.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // "WasTerminated" -> "wasterminated"
    let fetched = LowercaseStatus::one(&db, "wasterminated").await.unwrap();
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
            i.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    let fetched = ScreamingSnakeItem::one(&db, 1u64).await.unwrap();
    assert_eq!(fetched, Some(item));

    // Verify keys use SCREAMING_SNAKE_CASE
    let key = key![1i64, "ITEM_NAME"];
    let val = db.get(&global!("scream_item"), &key).await.unwrap();
    assert_eq!(val, Some(value!("SCREAMER")));

    let key2 = key![1i64, "IS_ACTIVE"];
    let val2 = db.get(&global!("scream_item"), &key2).await.unwrap();
    assert_eq!(val2, Some(value!(true)));
}

#[tokio::test]
async fn test_snake_case_enum_variants() {
    let db = Database::in_memory().unwrap();

    let status = SnakeCaseStatus::IsActive;

    db.transaction(|txn| {
        let s = status.clone();
        async move {
            s.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Key uses snake_case variant name
    let fetched = SnakeCaseStatus::one(&db, "is_active").await.unwrap();
    assert_eq!(fetched, Some(SnakeCaseStatus::IsActive));

    // Test struct variant
    let term = SnakeCaseStatus::WasTerminated {
        reason: "layoff".into(),
    };

    db.transaction(|txn| {
        let t = term.clone();
        async move {
            t.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    let fetched = SnakeCaseStatus::one(&db, "was_terminated").await.unwrap();
    assert_eq!(fetched, Some(term));
}

#[test]
fn test_unit_enum_rename_all_to_value() {
    // ToValue
    assert_eq!(
        SnakeCaseUnitEnum::FirstOption.to_val(),
        value!("first_option")
    );
    assert_eq!(
        SnakeCaseUnitEnum::SecondOption.to_val(),
        value!("second_option")
    );
    assert_eq!(
        SnakeCaseUnitEnum::ThirdOption.to_val(),
        value!("third_option")
    );

    // FromValue
    assert_eq!(
        SnakeCaseUnitEnum::from_val(&value!("first_option")),
        Ok(SnakeCaseUnitEnum::FirstOption)
    );
    assert_eq!(
        SnakeCaseUnitEnum::from_val(&value!("second_option")),
        Ok(SnakeCaseUnitEnum::SecondOption)
    );

    // Invalid - original name shouldn't work
    assert!(SnakeCaseUnitEnum::from_val(&value!("FirstOption")).is_err());
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
            c.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Variant name uses container's rename_all (snake_case)
    let fetched = MixedCaseEnum::one(&db, "complex_variant").await.unwrap();
    assert_eq!(fetched, Some(complex));

    // Field names use variant's rename_all (camelCase)
    let key = key!["complex_variant", "fieldOne"];
    let val = db.get(&global!("mixed_case"), &key).await.unwrap();
    assert_eq!(val, Some(value!("value1")));

    // Test variant without override (uses container default)
    let another = MixedCaseEnum::AnotherVariant {
        some_field: "test".into(),
    };

    db.transaction(|txn| {
        let a = another.clone();
        async move {
            a.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Field names use container's rename_all (snake_case)
    let key2 = key!["another_variant", "some_field"];
    let val2 = db.get(&global!("mixed_case"), &key2).await.unwrap();
    assert_eq!(val2, Some(value!("test")));
}

#[tokio::test]
async fn test_explicit_rename_overrides_rename_all() {
    let db = Database::in_memory().unwrap();

    // Test enum variant with explicit rename
    db.transaction(|txn| async move {
        RenameCombined::OriginalName.insert(&txn).await?;
        RenameCombined::AnotherName.insert(&txn).await?;
        Ok(())
    })
    .await
    .unwrap();

    // Explicit rename takes precedence
    let fetched1 = RenameCombined::one(&db, "custom_name").await.unwrap();
    assert_eq!(fetched1, Some(RenameCombined::OriginalName));

    // `rename_all` applies when no explicit rename
    let fetched2 = RenameCombined::one(&db, "another_name").await.unwrap();
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
            i.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    let fetched = FieldOverride::one(&db, 1u64).await.unwrap();
    assert_eq!(fetched, Some(item));

    // Normal field uses `rename_all` (camelCase)
    let key1 = key![1i64, "normalField"];
    let val1 = db.get(&global!("field_override"), &key1).await.unwrap();
    assert_eq!(val1, Some(value!("normal")));

    // Override field uses explicit rename
    let key2 = key![1i64, "CUSTOM"];
    let val2 = db.get(&global!("field_override"), &key2).await.unwrap();
    assert_eq!(val2, Some(value!("override")));
}

// Tests for field-level rename with case transformation

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
            i.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify roundtrip
    let fetched = MixedFieldRename::one(&db, 1u64).await.unwrap();
    assert_eq!(fetched, Some(item));

    // Verify camelCase transformation on some_field_name
    let key1 = key![1i64, "someFieldName"];
    let val1 = db.get(&global!("mixed_field"), &key1).await.unwrap();
    assert_eq!(val1, Some(value!("camel")));

    // Verify UPPERCASE transformation on another_field (keeps underscores)
    let key2 = key![1i64, "ANOTHER_FIELD"];
    let val2 = db.get(&global!("mixed_field"), &key2).await.unwrap();
    assert_eq!(val2, Some(value!("upper")));

    // Verify literal rename on third_field
    let key3 = key![1i64, "custom_literal"];
    let val3 = db.get(&global!("mixed_field"), &key3).await.unwrap();
    assert_eq!(val3, Some(value!("literal")));

    // Verify no rename on plain_field
    let key4 = key![1i64, "plain_field"];
    let val4 = db.get(&global!("mixed_field"), &key4).await.unwrap();
    assert_eq!(val4, Some(value!("plain")));
}

#[tokio::test]
async fn test_variant_level_case_transformation() {
    let db = Database::in_memory().unwrap();

    // Test snake_case variant
    db.transaction(|txn| async move {
        MixedVariantRename::SomeVariantName.insert(&txn).await?;
        Ok(())
    })
    .await
    .unwrap();

    let fetched = MixedVariantRename::one(&db, "some_variant_name")
        .await
        .unwrap();
    assert_eq!(fetched, Some(MixedVariantRename::SomeVariantName));

    // Test camelCase variant
    db.transaction(|txn| async move {
        MixedVariantRename::AnotherVariantHere { val: 42 }
            .insert(&txn)
            .await?;
        Ok(())
    })
    .await
    .unwrap();

    let fetched = MixedVariantRename::one(&db, "anotherVariantHere")
        .await
        .unwrap();
    assert_eq!(
        fetched,
        Some(MixedVariantRename::AnotherVariantHere { val: 42 })
    );

    // Test literal rename variant
    db.transaction(|txn| async move {
        MixedVariantRename::ThirdVariant.insert(&txn).await?;
        Ok(())
    })
    .await
    .unwrap();

    let fetched = MixedVariantRename::one(&db, "custom_tag").await.unwrap();
    assert_eq!(fetched, Some(MixedVariantRename::ThirdVariant));

    // Test no rename variant
    db.transaction(|txn| async move {
        MixedVariantRename::PlainVariant.insert(&txn).await?;
        Ok(())
    })
    .await
    .unwrap();

    let fetched = MixedVariantRename::one(&db, "PlainVariant").await.unwrap();
    assert_eq!(fetched, Some(MixedVariantRename::PlainVariant));
}

#[test]
fn test_unit_enum_field_level_case_transformation() {
    // Test snake_case
    assert_eq!(
        MixedUnitRename::FirstOption.to_val(),
        value!("first_option")
    );
    assert_eq!(
        MixedUnitRename::from_val(&value!("first_option")),
        Ok(MixedUnitRename::FirstOption)
    );

    // Test uppercase
    assert_eq!(
        MixedUnitRename::SecondOption.to_val(),
        value!("SECONDOPTION")
    );
    assert_eq!(
        MixedUnitRename::from_val(&value!("SECONDOPTION")),
        Ok(MixedUnitRename::SecondOption)
    );

    // Test literal
    assert_eq!(
        MixedUnitRename::ThirdOption.to_val(),
        value!("literal_name")
    );
    assert_eq!(
        MixedUnitRename::from_val(&value!("literal_name")),
        Ok(MixedUnitRename::ThirdOption)
    );

    // Test no rename
    assert_eq!(
        MixedUnitRename::FourthOption.to_val(),
        value!("FourthOption")
    );
    assert_eq!(
        MixedUnitRename::from_val(&value!("FourthOption")),
        Ok(MixedUnitRename::FourthOption)
    );
}

#[test]
fn test_unit_enum_variant_rename_override_value() {
    // Test container-level rename_all = "lowercase" with variant override
    assert_eq!(PriorityValue::Low.to_val(), value!("low"));
    assert_eq!(PriorityValue::Medium.to_val(), value!("medium"));
    assert_eq!(PriorityValue::High.to_val(), value!("high"));
    // Test variant-level override with literal string
    assert_eq!(PriorityValue::Urgent.to_val(), value!("CRITICAL"));

    // Test FromValue round-trip
    assert_eq!(
        PriorityValue::from_val(&value!("low")),
        Ok(PriorityValue::Low)
    );
    assert_eq!(
        PriorityValue::from_val(&value!("CRITICAL")),
        Ok(PriorityValue::Urgent)
    );

    // Error: original variant name shouldn't work when renamed
    assert!(PriorityValue::from_val(&value!("Urgent")).is_err());
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

// Untagged enum tests

/// Untagged enum with unit variant
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "untagged_unit", untagged)]
enum UntaggedUnit {
    Empty,
}

/// Untagged enum with single-field tuple variants
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "json_val", untagged)]
enum JsonValue {
    Null,
    Number(f64),
    Text(String),
}

/// Untagged enum with struct variants
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "response", untagged)]
enum ApiResponse {
    Success { data: String },
    Error { code: u32, msg: String },
}

/// Untagged enum with key fields
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "keyed_untagged", untagged)]
enum KeyedUntagged {
    ById {
        #[rumps(key)]
        id: u64,
        name: String,
    },
    ByName {
        #[rumps(key)]
        name: String,
        count: u32,
    },
}

#[tokio::test]
async fn test_untagged_unit_variant() {
    let db = Database::in_memory().unwrap();

    let val = UntaggedUnit::Empty;

    db.transaction(|txn| {
        let v = val.clone();
        async move {
            v.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Untagged unit: key is empty, just a marker at root
    let fetched = UntaggedUnit::one(&db, Key::new()).await.unwrap();
    assert_eq!(fetched, Some(UntaggedUnit::Empty));
}

#[tokio::test]
async fn test_untagged_single_tuple_number() {
    let db = Database::in_memory().unwrap();

    let val = JsonValue::Number(42.5);

    db.transaction(|txn| {
        let v = val.clone();
        async move {
            v.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify storage: no tag, value stored directly at root
    let key = Key::new();
    let stored = db.get(&global!("json_val"), &key).await.unwrap();
    assert_eq!(stored, Some(value!(42.5)));

    // Round-trip
    let fetched = JsonValue::one(&db, Key::new()).await.unwrap();
    assert_eq!(fetched, Some(JsonValue::Number(42.5)));
}

#[tokio::test]
async fn test_untagged_single_tuple_text() {
    let db = Database::in_memory().unwrap();

    let val = JsonValue::Text("hello".into());

    db.transaction(|txn| {
        let v = val.clone();
        async move {
            v.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Round-trip
    let fetched = JsonValue::one(&db, Key::new()).await.unwrap();
    assert_eq!(fetched, Some(JsonValue::Text("hello".into())));
}

#[tokio::test]
async fn test_untagged_struct_variant() {
    let db = Database::in_memory().unwrap();

    let val = ApiResponse::Success { data: "ok".into() };

    db.transaction(|txn| {
        let v = val.clone();
        async move {
            v.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify storage: ^response("data") = "ok" (no variant tag)
    let key = key!["data"];
    let stored = db.get(&global!("response"), &key).await.unwrap();
    assert_eq!(stored, Some(value!("ok")));

    // Round-trip
    let fetched = ApiResponse::one(&db, Key::new()).await.unwrap();
    assert_eq!(fetched, Some(val));
}

#[tokio::test]
async fn test_untagged_error_variant() {
    let db = Database::in_memory().unwrap();

    let val = ApiResponse::Error {
        code: 404,
        msg: "not found".into(),
    };

    db.transaction(|txn| {
        let v = val.clone();
        async move {
            v.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // Verify storage: ^response("code") = 404, ^response("msg") = "not found"
    let code_key = key!["code"];
    let code_val = db.get(&global!("response"), &code_key).await.unwrap();
    assert_eq!(code_val, Some(value!(404)));

    let msg_key = key!["msg"];
    let msg_val = db.get(&global!("response"), &msg_key).await.unwrap();
    assert_eq!(msg_val, Some(value!("not found")));

    // Round-trip
    let fetched = ApiResponse::one(&db, Key::new()).await.unwrap();
    assert_eq!(fetched, Some(val));
}

#[tokio::test]
async fn test_untagged_with_keys() {
    let db = Database::in_memory().unwrap();

    let by_id = KeyedUntagged::ById {
        id: 42,
        name: "Alice".into(),
    };
    let by_name = KeyedUntagged::ByName {
        name: "Bob".into(),
        count: 10,
    };

    db.transaction(|txn| {
        let a = by_id.clone();
        let b = by_name.clone();
        async move {
            a.insert(&txn).await?;
            b.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    // ById: key is just [42], field stored at [42, "name"]
    let key = key![42i64, "name"];
    let val = db.get(&global!("keyed_untagged"), &key).await.unwrap();
    assert_eq!(val, Some(value!("Alice")));

    // ByName: key is just ["Bob"], field stored at ["Bob", "count"]
    let key = key!["Bob", "count"];
    let val = db.get(&global!("keyed_untagged"), &key).await.unwrap();
    assert_eq!(val, Some(value!(10)));

    // Round-trip
    let fetched1 = KeyedUntagged::one(&db, 42u64).await.unwrap();
    assert_eq!(fetched1, Some(by_id));

    let fetched2 = KeyedUntagged::one(&db, "Bob").await.unwrap();
    assert_eq!(fetched2, Some(by_name));
}

#[tokio::test]
async fn test_untagged_variant_order_matters() {
    // Test that variants are tried in declaration order
    // Null comes first and matches empty data
    let db = Database::in_memory().unwrap();

    // Insert just a marker (empty data)
    db.transaction(|txn| async move {
        txn.set(&global!("json_val"), &Key::new(), value!(""))
            .await?;
        Ok(())
    })
    .await
    .unwrap();

    // Should parse as `Null` (first variant that matches empty)
    let fetched = JsonValue::one(&db, Key::new()).await.unwrap();
    assert_eq!(fetched, Some(JsonValue::Null));
}

// Tests for enum struct variants with flatten/subtree

/// Nested struct for flatten tests (key type matches variant tag type - String).
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "_nested")]
struct GeoLoc {
    #[rumps(key)]
    _tag: String, // Must be String to match enum variant tag type
    city: String,
    country: String,
}

/// Nested struct for subtree tests (key type matches variant tag type - String).
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "_nested")]
struct MetaInfo {
    #[rumps(key)]
    _tag: String, // Must be String to match enum variant tag type
    created_at: u64,
    updated_at: u64,
}

/// Enum with struct variant containing flatten field.
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "ev_record")]
enum EvRecord {
    Empty,
    WithLocation {
        name: String,
        #[rumps(flatten)]
        loc: GeoLoc,
    },
}

/// Enum with struct variant containing subtree field.
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "ev_item")]
enum EvItem {
    Simple {
        name: String,
    },
    WithMeta {
        name: String,
        #[rumps(subtree)]
        meta: MetaInfo,
    },
}

/// Enum with struct variant containing optional flatten field.
#[derive(Debug, Clone, PartialEq, ToRumps, FromRumps)]
#[rumps(global = "ev_entry")]
enum EvEntry {
    Basic {
        title: String,
    },
    WithOptLoc {
        title: String,
        #[rumps(flatten)]
        loc: Option<GeoLoc>,
    },
}

#[tokio::test]
async fn test_enum_struct_variant_flatten_roundtrip() {
    let db = Database::in_memory().unwrap();

    let record = EvRecord::WithLocation {
        name: "HQ".into(),
        loc: GeoLoc {
            _tag: "WithLocation".into(), // Must match variant tag
            city: "NYC".into(),
            country: "USA".into(),
        },
    };

    db.transaction(|txn| {
        let r = record.clone();
        async move {
            r.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    let fetched = EvRecord::one(&db, "WithLocation").await.unwrap();
    assert_eq!(fetched, Some(record));
}

#[tokio::test]
async fn test_enum_struct_variant_subtree_roundtrip() {
    let db = Database::in_memory().unwrap();

    let item = EvItem::WithMeta {
        name: "Widget".into(),
        meta: MetaInfo {
            _tag: "WithMeta".into(), // Must match variant tag
            created_at: 1000,
            updated_at: 2000,
        },
    };

    db.transaction(|txn| {
        let i = item.clone();
        async move {
            i.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    let fetched = EvItem::one(&db, "WithMeta").await.unwrap();
    assert_eq!(fetched, Some(item));
}

#[tokio::test]
async fn test_enum_struct_variant_optional_flatten_some() {
    let db = Database::in_memory().unwrap();

    let entry = EvEntry::WithOptLoc {
        title: "Office".into(),
        loc: Some(GeoLoc {
            _tag: "WithOptLoc".into(), // Must match variant tag
            city: "LA".into(),
            country: "USA".into(),
        }),
    };

    db.transaction(|txn| {
        let e = entry.clone();
        async move {
            e.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    let fetched = EvEntry::one(&db, "WithOptLoc").await.unwrap();
    assert_eq!(fetched, Some(entry));
}

#[tokio::test]
async fn test_enum_struct_variant_optional_flatten_none() {
    let db = Database::in_memory().unwrap();

    let entry = EvEntry::WithOptLoc {
        title: "Remote".into(),
        loc: None,
    };

    db.transaction(|txn| {
        let e = entry.clone();
        async move {
            e.insert(&txn).await?;
            Ok(())
        }
    })
    .await
    .unwrap();

    let fetched = EvEntry::one(&db, "WithOptLoc").await.unwrap();
    assert_eq!(fetched, Some(entry));
}
