//! Core ORM traits for RUMPS.
//!
//! # Storage Layout
//!
//! Records are stored as a tree of key-value pairs under a global. The derive
//! macro writes an **empty-string marker** at the record's prefix key to ensure
//! the record exists even if all fields are optional/`None`.
//!
//! For example, a `Person { id: 42, name: "Alice", email: "a@b.com" }` is stored as:
//!
//! ```text
//! ^person(42)         = ""              <- marker (empty string)
//! ^person(42, "name") = "Alice"
//! ^person(42, "email") = "a@b.com"
//! ```
//!
//! The marker serves two purposes:
//! 1. Ensures records with all-optional fields still exist in storage
//! 2. Enables efficient record boundary detection when streaming multiple records
//!
//! # Compatibility with Manual Writes
//!
//! The ORM can also read data written directly via [`Transaction::set`] without
//! markers. In this case, record boundaries are inferred from the key structure
//! (assuming the last subscript is a field name).

use async_trait::async_trait;
use futures::TryStreamExt;
use rumps_types::orm::{DecodeError, IntoKey};
use rumps_types::{global, Key, Result, Value};

use super::sealed::Sealed;
use crate::database::Database;
use crate::transaction::Transaction;

/// Convert a Rust type into RUMPS key-value pairs.
///
/// Types implementing this trait can be stored in RUMPS as a tree of
/// key-value pairs under a named global.
///
/// # Example
///
/// ```ignore
/// use rumps_storage::orm::ToRumps;
/// use rumps_types::{Key, Value, Name};
///
/// struct User {
///     id: u64,
///     name: String,
///     age: u32,
/// }
///
/// impl ToRumps for User {
///     const GLOBAL: &'static str = "user";
///
///     fn to_key(&self) -> Key {
///         self.id.into_key()
///     }
///
///     fn to_pairs(&self, prefix: &Key) -> Vec<(Key, Value)> {
///         vec![
///             (prefix.clone().push("name".to_sub()), self.name.to_val()),
///             (prefix.clone().push("age".to_sub()), self.age.to_val()),
///         ]
///     }
/// }
/// ```
pub trait ToRumps {
    /// The global name this type is stored under (e.g., `"user"` for `^user`).
    const GLOBAL: &'static str;

    /// Extracts the key portion from the struct.
    ///
    /// This is used to identify a record for lookups and deletes.
    fn to_key(&self) -> Key;

    /// Expands the struct into key-value pairs.
    ///
    /// The `prefix` is prepended to each key (typically the record's key fields).
    ///
    /// # Marker
    ///
    /// The derive macro implementation writes an empty-string marker at the
    /// exact `prefix` key (i.e., `(prefix.clone(), Value::String("".into()))`).
    /// This ensures the record exists even if all fields are `Option::None`.
    fn to_pairs(&self, prefix: &Key) -> Vec<(Key, Value)>;
}

/// Reconstruct a Rust type from RUMPS key-value pairs.
///
/// Types implementing this trait can be read from RUMPS storage.
///
/// # Example
///
/// ```ignore
/// use rumps_storage::orm::FromRumps;
/// use rumps_types::{Key, Value};
///
/// struct User {
///     id: u64,
///     name: String,
///     age: u32,
/// }
///
/// impl FromRumps for User {
///     const GLOBAL: &'static str = "user";
///
///     fn from_pairs<I>(prefix: &Key, pairs: I) -> Result<Self, DecodeError>
///     where
///         I: Iterator<Item = (Key, Value)>,
///     {
///         let mut name = None;
///         let mut age = None;
///
///         for (key, val) in pairs {
///             // Extract field name from key suffix
///             if let Some(field) = key.strip_prefix(prefix) {
///                 match field.first() {
///                     Some(s) if s == &"name".to_sub() => {
///                         name = Some(String::from_val(&val)?);
///                     }
///                     Some(s) if s == &"age".to_sub() => {
///                         age = Some(u32::from_val(&val)?);
///                     }
///                     _ => {}
///                 }
///             }
///         }
///
///         let id = u64::from_sub(prefix.first().ok_or_else(|| /* error */)?)?;
///
///         Ok(User {
///             id,
///             name: name.ok_or_else(|| DecodeError::MissingField { field: "name" })?,
///             age: age.ok_or_else(|| DecodeError::MissingField { field: "age" })?,
///         })
///     }
/// }
/// ```
pub trait FromRumps: Sized {
    /// The global name to query.
    const GLOBAL: &'static str;

    /// Reconstruct from an iterator of key-value pairs.
    ///
    /// The `prefix` is the key prefix for this record (the key fields).
    fn from_pairs<I>(
        prefix: &Key,
        pairs: I,
    ) -> std::result::Result<Self, DecodeError>
    where
        I: Iterator<Item = (Key, Value)>;
}

/// Read operations for types implementing [`FromRumps`].
///
/// This trait is implemented for both [`Database`] and [`Transaction`],
/// allowing reads from either context.
///
/// # Example
///
/// ```ignore
/// use rumps_storage::{Database, RumpsRead};
///
/// let db = Database::in_memory()?;
///
/// // Get a single record by key
/// let user: Option<User> = db.one::<User, _>(123u64).await?;
///
/// // Get all records of this type
/// let users: Vec<User> = db.all::<User>().await?;
///
/// // Check if a record exists
/// let exists: bool = db.exists::<User, _>(123u64).await?;
/// ```
#[async_trait]
pub trait RumpsRead: Sealed {
    /// Gets a single record by key.
    ///
    /// Returns `None` if the record doesn't exist.
    async fn one<T, K>(&self, key: K) -> Result<Option<T>>
    where
        T: FromRumps + Send,
        K: IntoKey + Send;

    /// Checks if a record exists.
    async fn exists<T, K>(&self, key: K) -> Result<bool>
    where
        T: FromRumps + Send,
        K: IntoKey + Send;

    /// Gets all records of type `T`.
    async fn all<T>(&self) -> Result<Vec<T>>
    where
        T: FromRumps + Send;

    /// Queries records matching a key prefix.
    async fn query<T, K>(&self, prefix: K) -> Result<Vec<T>>
    where
        T: FromRumps + Send,
        K: IntoKey + Send;
}

/// Write operations for types implementing [`ToRumps`].
///
/// This trait is ONLY implemented for [`Transaction`], enforcing at compile
/// time that writes must go through transactions.
///
/// # Example
///
/// ```ignore
/// use rumps_storage::{Database, RumpsWrite};
///
/// let db = Database::in_memory()?;
///
/// // Writes require a transaction
/// db.transaction(|txn| async move {
///     txn.insert(&User { id: 1, name: "Alice".into(), age: 30 }).await?;
///     Ok(())
/// }).await?;
///
/// // This would NOT compile:
/// // db.insert(&user).await?;  // Error: RumpsWrite not impl for Database
/// ```
#[async_trait]
pub trait RumpsWrite: Sealed {
    /// Inserts a new record.
    async fn insert<T>(&self, val: &T) -> Result<()>
    where
        T: ToRumps + Sync;

    /// Deletes a record by key.
    async fn delete<T, K>(&self, key: K) -> Result<()>
    where
        T: ToRumps + Send,
        K: IntoKey + Send;

    /// Inserts or updates a record.
    async fn upsert<T>(&self, val: &T) -> Result<()>
    where
        T: ToRumps + Sync;
}

#[async_trait]
impl RumpsRead for Database {
    async fn one<T, K>(&self, key: K) -> Result<Option<T>>
    where
        T: FromRumps + Send,
        K: IntoKey + Send,
    {
        let name = global!(T::GLOBAL);
        let prefix = key.into_key();

        // Use prefix-optimized collection (seeks to prefix, terminates early)
        let pairs: Vec<(Key, Value)> = self
            .collects_prefix_vec(&name, &prefix, |k, v| {
                v.clone().map(|val| (k.clone(), val))
            })
            .await?;

        if pairs.is_empty() {
            Ok(None)
        } else {
            T::from_pairs(&prefix, pairs.into_iter())
                .map(Some)
                .map_err(Into::into)
        }
    }

    async fn exists<T, K>(&self, key: K) -> Result<bool>
    where
        T: FromRumps + Send,
        K: IntoKey + Send,
    {
        use futures::StreamExt;

        let name = global!(T::GLOBAL);
        let prefix = key.into_key();

        // Just check if any entry exists - take first only
        let first = self
            .collects(
                &name,
                None,
                |k, _| k.starts_with(&prefix),
                |k, _| Some(k.clone()),
            )
            .await?
            .boxed()
            .next()
            .await;

        Ok(first.is_some())
    }

    async fn all<T>(&self) -> Result<Vec<T>>
    where
        T: FromRumps + Send,
    {
        let name = global!(T::GLOBAL);

        let stream = self
            .collects(
                &name,
                None,
                |_, _| true,
                |k, v| v.clone().map(|val| (k.clone(), val)),
            )
            .await?;
        stream_and_parse::<T, _>(stream).await
    }

    async fn query<T, K>(&self, prefix: K) -> Result<Vec<T>>
    where
        T: FromRumps + Send,
        K: IntoKey + Send,
    {
        let name = global!(T::GLOBAL);
        let prefix_key = prefix.into_key();

        let stream = self
            .collects(
                &name,
                None,
                {
                    let prefix_key = prefix_key.clone();
                    move |k, _| k.starts_with(&prefix_key)
                },
                |k, v| v.clone().map(|val| (k.clone(), val)),
            )
            .await?;
        stream_and_parse::<T, _>(stream).await
    }
}

#[async_trait]
impl RumpsRead for Transaction {
    async fn one<T, K>(&self, key: K) -> Result<Option<T>>
    where
        T: FromRumps + Send,
        K: IntoKey + Send,
    {
        let name = global!(T::GLOBAL);
        let prefix = key.into_key();

        // Use prefix-based collection (merges buffered writes correctly)
        let pairs: Vec<(Key, Value)> = self
            .collects_prefix_vec(&name, &prefix, |k, v| {
                v.clone().map(|val| (k.clone(), val))
            })
            .await?;

        if pairs.is_empty() {
            Ok(None)
        } else {
            T::from_pairs(&prefix, pairs.into_iter())
                .map(Some)
                .map_err(Into::into)
        }
    }

    async fn exists<T, K>(&self, key: K) -> Result<bool>
    where
        T: FromRumps + Send,
        K: IntoKey + Send,
    {
        use futures::StreamExt;

        let name = global!(T::GLOBAL);
        let prefix = key.into_key();

        // Just check if any entry exists - take first only
        let first = self
            .collects(
                &name,
                None,
                |k, _| k.starts_with(&prefix),
                |k, _| Some(k.clone()),
            )
            .await?
            .boxed()
            .next()
            .await;

        Ok(first.is_some())
    }

    async fn all<T>(&self) -> Result<Vec<T>>
    where
        T: FromRumps + Send,
    {
        let name = global!(T::GLOBAL);

        let stream = self
            .collects(
                &name,
                None,
                |_, _| true,
                |k, v| v.clone().map(|val| (k.clone(), val)),
            )
            .await?;
        stream_and_parse::<T, _>(stream).await
    }

    async fn query<T, K>(&self, prefix: K) -> Result<Vec<T>>
    where
        T: FromRumps + Send,
        K: IntoKey + Send,
    {
        let name = global!(T::GLOBAL);
        let prefix_key = prefix.into_key();

        let stream = self
            .collects(
                &name,
                None,
                {
                    let prefix_key = prefix_key.clone();
                    move |k, _| k.starts_with(&prefix_key)
                },
                |k, v| v.clone().map(|val| (k.clone(), val)),
            )
            .await?;
        stream_and_parse::<T, _>(stream).await
    }
}

#[async_trait]
impl RumpsWrite for Transaction {
    async fn insert<T>(&self, val: &T) -> Result<()>
    where
        T: ToRumps + Sync,
    {
        let name = global!(T::GLOBAL);
        let key = val.to_key();
        let pairs = val.to_pairs(&key);

        // Insert all key-value pairs
        futures::future::try_join_all(pairs.into_iter().map(|(k, v)| {
            let name = name.clone();
            async move { self.set(&name, &k, v).await }
        }))
        .await?;

        Ok(())
    }

    async fn delete<T, K>(&self, key: K) -> Result<()>
    where
        T: ToRumps + Send,
        K: IntoKey + Send,
    {
        let name = global!(T::GLOBAL);
        let prefix = key.into_key();

        // Kill the entire subtree under this key
        self.kill(&name, &prefix).await
    }

    async fn upsert<T>(&self, val: &T) -> Result<()>
    where
        T: ToRumps + Sync,
    {
        // For now, just insert (overwrites existing values)
        self.insert(val).await
    }
}

/// Stream pairs and parse records at boundaries (when key prefix changes).
///
/// This processes the stream in a single pass, parsing each record as soon as
/// we detect a new record boundary. More memory-efficient than collecting all
/// pairs first.
///
/// Record boundaries are detected by inferring the key length from the first
/// entry in the stream:
/// - If the first entry is a marker (empty string value), its key length is the key length
/// - Otherwise, assume the last subscript is a field name (key length = `len - 1`)
async fn stream_and_parse<T, S>(stream: S) -> Result<Vec<T>>
where
    T: FromRumps,
    S: futures::Stream<Item = Result<(Key, Value)>> + Send,
{
    // State: (inferred key_len, current prefix, current pairs, parsed results)
    type State<T> = (Option<usize>, Option<Key>, Vec<(Key, Value)>, Vec<T>);

    let is_empty_string =
        |v: &Value| matches!(v, Value::String(s) if s.is_empty());

    let (_, maybe_prefix, pairs, mut results): State<T> = Box::pin(stream)
        .try_fold(
            (None, None, Vec::new(), Vec::new()),
            |(key_len, cur_prefix, mut cur_pairs, mut results), (k, v)| {
                // Infer key_len from first entry
                let key_len = key_len.unwrap_or_else(|| {
                    // If first entry is a marker (empty value), its key IS the prefix
                    // Otherwise, assume last subscript is field name
                    if is_empty_string(&v) {
                        k.len()
                    } else {
                        k.len().saturating_sub(1)
                    }
                });

                // Extract prefix using inferred key_len
                let prefix = (k.len() >= key_len).then(|| {
                    Key::from(
                        (0..key_len)
                            .filter_map(|i| k.get(i).cloned())
                            .collect::<Vec<_>>(),
                    )
                });

                let res: Result<State<T>> = match (&cur_prefix, &prefix) {
                    (Some(p), Some(f)) if p != f => {
                        // Record boundary - parse accumulated pairs
                        T::from_pairs(p, cur_pairs.into_iter())
                            .map_err(rumps_types::Error::from)
                            .map(|rec| {
                                results.push(rec);
                                (Some(key_len), prefix, vec![(k, v)], results)
                            })
                    }
                    _ => {
                        // Same record or first entry
                        cur_pairs.push((k, v));
                        Ok((
                            Some(key_len),
                            prefix.or(cur_prefix),
                            cur_pairs,
                            results,
                        ))
                    }
                };

                std::future::ready(res)
            },
        )
        .await?;

    // Parse final accumulated record
    maybe_prefix
        .filter(|_| !pairs.is_empty())
        .map(|prefix| {
            T::from_pairs(&prefix, pairs.into_iter())
                .map_err(rumps_types::Error::from)
                .map(|rec| results.push(rec))
        })
        .transpose()?;

    Ok(results)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use rumps_types::orm::{FromSubscript, FromValue, ToSubscript, ToValue};

    use super::*;
    use crate::Database;

    // A simple test struct for ORM (manual implementation)
    #[derive(Debug, Clone, PartialEq)]
    struct User {
        id: u64,
        name: String,
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

            let mut age_key = prefix.clone();
            age_key.push("age".to_sub());

            // Include marker at prefix (as derive macro does)
            vec![
                (prefix.clone(), Value::String(String::new())),
                (name_key, self.name.to_val()),
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
            let mut age: Option<u32> = None;

            pairs.for_each(|(k, v)| {
                // Check suffix after prefix
                if k.len() == prefix.len() + 1 {
                    k.get(prefix.len()).into_iter().for_each(|field| {
                        if field == &"name".to_sub() {
                            name = String::from_val(&v).ok();
                        } else if field == &"age".to_sub() {
                            age = u32::from_val(&v).ok();
                        }
                    });
                }
            });

            // Extract id from prefix
            let id = prefix
                .get(0)
                .ok_or(DecodeError::MissingField { field: "id" })
                .and_then(|s| u64::from_sub(s))?;

            Ok(User {
                id,
                name: name
                    .ok_or(DecodeError::MissingField { field: "name" })?,
                age: age.ok_or(DecodeError::MissingField { field: "age" })?,
            })
        }
    }

    #[tokio::test]
    async fn test_insert_and_one() {
        let db = Database::in_memory().unwrap();

        let user = User {
            id: 1,
            name: "Alice".into(),
            age: 30,
        };

        // Insert via transaction
        db.transaction(|txn| {
            let u = user.clone();
            async move {
                txn.insert(&u).await?;
                Ok(())
            }
        })
        .await
        .unwrap();

        // Get record
        let fetched: Option<User> = db.one(1u64).await.unwrap();
        assert_eq!(fetched, Some(user));
    }

    #[tokio::test]
    async fn test_exists() {
        let db = Database::in_memory().unwrap();

        // Doesn't exist yet
        assert!(!db.exists::<User, _>(1u64).await.unwrap());

        // Insert
        db.transaction(|txn| async move {
            txn.insert(&User {
                id: 1,
                name: "Bob".into(),
                age: 25,
            })
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        // Now exists
        assert!(db.exists::<User, _>(1u64).await.unwrap());
    }

    #[tokio::test]
    async fn test_all() {
        let db = Database::in_memory().unwrap();

        // Insert multiple users
        db.transaction(|txn| async move {
            txn.insert(&User {
                id: 1,
                name: "Alice".into(),
                age: 30,
            })
            .await?;
            txn.insert(&User {
                id: 2,
                name: "Bob".into(),
                age: 25,
            })
            .await?;
            txn.insert(&User {
                id: 3,
                name: "Charlie".into(),
                age: 35,
            })
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        // Get all records
        let users: Vec<User> = db.all().await.unwrap();
        assert_eq!(users.len(), 3);

        // Verify order (by id)
        assert_eq!(users[0].id, 1);
        assert_eq!(users[1].id, 2);
        assert_eq!(users[2].id, 3);
    }

    #[tokio::test]
    async fn test_delete() {
        let db = Database::in_memory().unwrap();

        // Insert
        db.transaction(|txn| async move {
            txn.insert(&User {
                id: 1,
                name: "Alice".into(),
                age: 30,
            })
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        // Verify exists
        assert!(db.exists::<User, _>(1u64).await.unwrap());

        // Delete
        db.transaction(|txn| async move {
            txn.delete::<User, _>(1u64).await?;
            Ok(())
        })
        .await
        .unwrap();

        // Verify deleted
        assert!(!db.exists::<User, _>(1u64).await.unwrap());
    }

    #[tokio::test]
    async fn test_upsert() {
        let db = Database::in_memory().unwrap();

        // Insert initial
        db.transaction(|txn| async move {
            txn.insert(&User {
                id: 1,
                name: "Alice".into(),
                age: 30,
            })
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        // Upsert with new values
        db.transaction(|txn| async move {
            txn.upsert(&User {
                id: 1,
                name: "Alicia".into(),
                age: 31,
            })
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        // Verify updated
        let user: Option<User> = db.one(1u64).await.unwrap();
        assert_eq!(user.as_ref().map(|u| &u.name), Some(&"Alicia".to_string()));
        assert_eq!(user.as_ref().map(|u| u.age), Some(31));
    }

    #[tokio::test]
    async fn test_transaction_read() {
        let db = Database::in_memory().unwrap();

        // Insert via transaction
        db.transaction(|txn| async move {
            txn.insert(&User {
                id: 1,
                name: "Alice".into(),
                age: 30,
            })
            .await?;

            // Read within same transaction
            let user: Option<User> = txn.one(1u64).await?;
            assert_eq!(
                user.as_ref().map(|u| &u.name),
                Some(&"Alice".to_string())
            );

            Ok(())
        })
        .await
        .unwrap();
    }
}
