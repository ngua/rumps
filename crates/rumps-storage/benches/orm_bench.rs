//! ORM benchmarks for RUMPS storage.
//!
//! Benchmarks for ORM operations like `one`, `all`, `insert`, `delete`, etc.
//! Requires both `bench` and `derive` features.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{
    black_box, criterion_group, criterion_main, Criterion, Throughput,
};
use rumps_storage::orm::{FromRumps, RumpsRead, RumpsWrite, ToRumps};
use rumps_storage::Database;
use rumps_types::orm::{
    DecodeError, FromSubscript, FromValue, ToSubscript, ToValue,
};
use rumps_types::{value, Key, Value};
use tempfile::TempDir;
use tokio::runtime::Runtime;

/// Test struct with derived ORM traits.
#[derive(Debug, Clone, PartialEq)]
struct User {
    id: u64,
    name: String,
    email: String,
    age: u32,
}

impl ToRumps for User {
    const GLOBAL: &'static str = "user";

    fn to_key(&self) -> Key {
        Key::from(vec![self.id.to_sub()])
    }

    fn to_pairs(&self, prefix: &Key) -> Vec<(Key, Value)> {
        let mut name_key = prefix.clone();
        name_key.push("name".to_sub());

        let mut email_key = prefix.clone();
        email_key.push("email".to_sub());

        let mut age_key = prefix.clone();
        age_key.push("age".to_sub());

        vec![
            (prefix.clone(), value!("")),
            (name_key, self.name.to_val()),
            (email_key, self.email.to_val()),
            (age_key, self.age.to_val()),
        ]
    }
}

impl FromRumps for User {
    const GLOBAL: &'static str = "user";

    fn from_pairs<I>(
        prefix: &Key,
        pairs: I,
    ) -> std::result::Result<Self, DecodeError>
    where
        I: Iterator<Item = (Key, Value)>,
    {
        let mut name: Option<String> = None;
        let mut email: Option<String> = None;
        let mut age: Option<u32> = None;

        pairs.for_each(|(k, v)| {
            if k.len() == prefix.len() + 1 {
                k.get(prefix.len()).into_iter().for_each(|field| {
                    if field == &"name".to_sub() {
                        name = String::from_val(&v).ok();
                    } else if field == &"email".to_sub() {
                        email = String::from_val(&v).ok();
                    } else if field == &"age".to_sub() {
                        age = u32::from_val(&v).ok();
                    }
                });
            }
        });

        let id = prefix
            .get(0)
            .ok_or(DecodeError::MissingField { field: "id" })
            .and_then(u64::from_sub)?;

        Ok(Self {
            id,
            name: name.ok_or(DecodeError::MissingField { field: "name" })?,
            email: email.ok_or(DecodeError::MissingField { field: "email" })?,
            age: age.ok_or(DecodeError::MissingField { field: "age" })?,
        })
    }
}

fn make_user(id: u64) -> User {
    User {
        id,
        name: format!("User{id}"),
        email: format!("user{id}@example.com"),
        age: (id % 80 + 18) as u32,
    }
}

/// Helper to insert users into a transaction.
async fn insert_users(
    txn: &rumps_storage::Transaction,
    users: &[User],
) -> rumps_types::Result<()> {
    futures::future::try_join_all(users.iter().map(|u| u.insert(txn))).await?;
    Ok(())
}

/// Benchmark `insert` operation.
fn bench_orm_insert(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let dir = TempDir::new().unwrap();
    let db = rt.block_on(Database::create(dir.path())).unwrap();
    let counter = std::sync::RwLock::new(0u64);

    c.bench_function("orm_insert_single", |b| {
        b.iter(|| {
            let id = {
                let mut c = counter.write().unwrap();
                let v = *c;
                *c += 1;
                v
            };
            let user = make_user(id);
            rt.block_on(async {
                db.transaction(|txn| {
                    let u = user.clone();
                    async move {
                        u.insert(&txn).await?;
                        Ok(())
                    }
                })
                .await
                .unwrap();
            })
        })
    });
}

/// Benchmark batch `insert` operations.
fn bench_orm_insert_batch(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("orm_insert_batch");

    [10u64, 100].into_iter().for_each(|batch_size| {
        group.throughput(Throughput::Elements(batch_size));
        group.bench_function(format!("{batch_size}_records"), |b| {
            b.iter_batched(
                || {
                    let dir = TempDir::new().unwrap();
                    let db = rt.block_on(Database::create(dir.path())).unwrap();
                    let users: Vec<User> =
                        (0..batch_size).map(make_user).collect();
                    (dir, db, users)
                },
                |(_dir, db, users)| {
                    rt.block_on(async {
                        db.transaction(|txn| async move {
                            futures::future::try_join_all(
                                users.iter().map(|u| u.insert(&txn)),
                            )
                            .await?;
                            Ok(())
                        })
                        .await
                        .unwrap();
                    })
                },
                criterion::BatchSize::SmallInput,
            )
        });
    });

    group.finish();
}

/// Benchmark `insert_many` operations.
fn bench_orm_insert_many(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("orm_insert_many");

    [10u64, 100, 1000].into_iter().for_each(|batch_size| {
        group.throughput(Throughput::Elements(batch_size));
        group.bench_function(format!("{batch_size}_records"), |b| {
            b.iter_batched(
                || {
                    let dir = TempDir::new().unwrap();
                    let db = rt.block_on(Database::create(dir.path())).unwrap();
                    let users: Vec<User> =
                        (0..batch_size).map(make_user).collect();
                    (dir, db, users)
                },
                |(_dir, db, users)| {
                    rt.block_on(async {
                        db.transaction(|txn| async move {
                            User::insert_many(&txn, &users).await?;
                            Ok(())
                        })
                        .await
                        .unwrap();
                    })
                },
                criterion::BatchSize::SmallInput,
            )
        });
    });

    group.finish();
}

/// Benchmark `one` operation.
fn bench_orm_one(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let dir = TempDir::new().unwrap();
    let db = rt.block_on(Database::create(dir.path())).unwrap();

    // Setup: insert 1000 users
    let users: Vec<User> = (0..1000u64).map(make_user).collect();
    rt.block_on(async {
        db.transaction(|txn| {
            let users = &users;
            async move {
                insert_users(&txn, users).await?;
                Ok(())
            }
        })
        .await
        .unwrap();
    });

    c.bench_function("orm_one", |b| {
        b.iter(|| {
            rt.block_on(async {
                let user: Option<User> =
                    User::one(&db, black_box(500u64)).await.unwrap();
                black_box(user);
            })
        })
    });
}

/// Benchmark `exists` operation.
fn bench_orm_exists(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let dir = TempDir::new().unwrap();
    let db = rt.block_on(Database::create(dir.path())).unwrap();

    // Setup: insert 1000 users
    let users: Vec<User> = (0..1000u64).map(make_user).collect();
    rt.block_on(async {
        db.transaction(|txn| {
            let users = &users;
            async move {
                insert_users(&txn, users).await?;
                Ok(())
            }
        })
        .await
        .unwrap();
    });

    c.bench_function("orm_exists", |b| {
        b.iter(|| {
            rt.block_on(async {
                let exists =
                    User::exists(&db, black_box(500u64)).await.unwrap();
                black_box(exists);
            })
        })
    });
}

/// Benchmark `all` with 1000 records.
fn bench_orm_all_1k(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let dir = TempDir::new().unwrap();
    let db = rt.block_on(Database::create(dir.path())).unwrap();

    // Setup: insert 1000 users
    let users: Vec<User> = (0..1000u64).map(make_user).collect();
    rt.block_on(async {
        db.transaction(|txn| {
            let users = &users;
            async move {
                insert_users(&txn, users).await?;
                Ok(())
            }
        })
        .await
        .unwrap();
    });

    c.bench_function("orm_all_1000", |b| {
        b.iter(|| {
            rt.block_on(async {
                let users: Vec<User> = User::all(&db).await.unwrap();
                black_box(users.len());
            })
        })
    });
}

/// Benchmark `all` with 10,000 records.
fn bench_orm_all_10k(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let dir = TempDir::new().unwrap();
    let db = rt.block_on(Database::create(dir.path())).unwrap();

    // Setup: insert 10,000 users in batches
    let all_users: Vec<User> = (0..10_000u64).map(make_user).collect();
    // Insert in batches of 1000 to avoid too-large transactions
    (0..10).for_each(|batch| {
        let start = batch * 1000;
        let end = start + 1000;
        let batch_users = &all_users[start..end];
        rt.block_on(async {
            db.transaction(|txn| {
                let users = batch_users;
                async move {
                    insert_users(&txn, users).await?;
                    Ok(())
                }
            })
            .await
            .unwrap();
        });
    });

    c.bench_function("orm_all_10000", |b| {
        b.iter(|| {
            rt.block_on(async {
                let users: Vec<User> = User::all(&db).await.unwrap();
                black_box(users.len());
            })
        })
    });
}

/// Benchmark `delete` operation.
fn bench_orm_delete(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    c.bench_function("orm_delete", |b| {
        b.iter_batched(
            || {
                let dir = TempDir::new().unwrap();
                let db = rt.block_on(Database::create(dir.path())).unwrap();

                // Setup: insert 100 users
                let users: Vec<User> = (0..100u64).map(make_user).collect();
                rt.block_on(async {
                    db.transaction(|txn| {
                        let users = &users;
                        async move {
                            insert_users(&txn, users).await?;
                            Ok(())
                        }
                    })
                    .await
                    .unwrap();
                });

                (dir, db)
            },
            |(_dir, db)| {
                rt.block_on(async {
                    db.transaction(|txn| async move {
                        User::delete(&txn, 50u64).await?;
                        Ok(())
                    })
                    .await
                    .unwrap();
                })
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

/// Benchmark `query` with prefix.
fn bench_orm_query(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let dir = TempDir::new().unwrap();
    let db = rt.block_on(Database::create(dir.path())).unwrap();

    // Setup: insert 1000 users
    let users: Vec<User> = (0..1000u64).map(make_user).collect();
    rt.block_on(async {
        db.transaction(|txn| {
            let users = &users;
            async move {
                insert_users(&txn, users).await?;
                Ok(())
            }
        })
        .await
        .unwrap();
    });

    // Query for users with id >= 500 (using prefix matching)
    c.bench_function("orm_query_prefix", |b| {
        b.iter(|| {
            rt.block_on(async {
                // Query for a specific user by id prefix
                let users: Vec<User> =
                    User::query(&db, black_box(500u64)).await.unwrap();
                black_box(users.len());
            })
        })
    });
}

criterion_group!(
    benches,
    bench_orm_insert,
    bench_orm_insert_batch,
    bench_orm_insert_many,
    bench_orm_one,
    bench_orm_exists,
    bench_orm_all_1k,
    bench_orm_all_10k,
    bench_orm_delete,
    bench_orm_query,
);

criterion_main!(benches);
