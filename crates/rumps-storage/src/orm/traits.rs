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
    ///
    /// # Ordering
    ///
    /// The `pairs` iterator yields entries in key-collation order (from the
    /// B-tree). Implementations should preserve this order when building
    /// nested collections and **must not** use unordered types like `HashMap`.
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
/// # Ordering Guarantee
///
/// Methods returning multiple records ([`all`](Self::all), [`query`](Self::query))
/// **must** return results in key-collation order. This is a fundamental property
/// inherited from RUMPS's B-tree storage — data is stored sorted, so no explicit
/// sorting is required or desired.
///
/// Implementations **must not** use unordered collections (e.g., `HashMap`,
/// `HashSet`) in code paths that produce results. Use `Vec`, `BTreeMap`, or
/// other ordered types only.
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
/// // Get all records of this type (returned in key order)
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

    /// Inserts multiple records efficiently in a single batch.
    async fn insert_many<T>(&self, vals: &[T]) -> Result<()>
    where
        T: ToRumps + Sync;

    /// Deletes a record by key.
    async fn delete<T, K>(&self, key: K) -> Result<()>
    where
        T: ToRumps + Send,
        K: IntoKey + Send;

    /// Inserts or updates a record.
    ///
    /// If the record doesn't exist, inserts `val`. If it exists, applies the
    /// callback `f` to the existing record:
    /// - `Some(new)` → replaces with `new`
    /// - `None` → deletes the record
    async fn upsert<T, F>(&self, val: T, f: F) -> Result<()>
    where
        T: ToRumps + FromRumps + Sync + Send,
        F: FnOnce(&T) -> Option<T> + Send;
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

        // Use prefix-optimized stream (seeks to prefix, terminates early)
        let first = self
            .collects_prefix(&name, &prefix, |k, _| Some(k.clone()))
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
        let name = global!(T::GLOBAL);
        let prefix = key.into_key();

        // Use prefix-based collection and check if any entry exists
        let entries: Vec<Key> = self
            .collects_prefix_vec(&name, &prefix, |k, _| Some(k.clone()))
            .await?;

        Ok(!entries.is_empty())
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

    async fn insert_many<T>(&self, vals: &[T]) -> Result<()>
    where
        T: ToRumps + Sync,
    {
        let name = global!(T::GLOBAL);

        // Flatten all pairs from all values into one batch
        futures::future::try_join_all(vals.iter().flat_map(|val| {
            let key = val.to_key();
            val.to_pairs(&key)
                .into_iter()
                .map(|(k, v)| {
                    let name = name.clone();
                    async move { self.set(&name, &k, v).await }
                })
                .collect::<Vec<_>>()
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

    async fn upsert<T, F>(&self, val: T, f: F) -> Result<()>
    where
        T: ToRumps + FromRumps + Sync + Send,
        F: FnOnce(&T) -> Option<T> + Send,
    {
        let key = val.to_key();

        match self.one::<T, _>(key.clone()).await? {
            None => self.insert(&val).await,
            Some(ref old) => {
                let name = global!(<T as ToRumps>::GLOBAL);
                match f(old) {
                    Some(ref new) => {
                        self.kill(&name, &key).await?;
                        self.insert(new).await
                    }
                    None => self.kill(&name, &key).await,
                }
            }
        }
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
///
/// # State Machine
///
/// The function uses `try_fold` with a 4-tuple state:
///
/// ```text
/// State = (key_len, cur_prefix, cur_pairs, results)
///          ^^^^^^^  ^^^^^^^^^^  ^^^^^^^^^  ^^^^^^^
///          |        |           |          Fully parsed records
///          |        |           Pairs accumulated for current record
///          |        Key prefix of current record (e.g., `[42]` for user 42)
///          Inferred key length (number of subscripts forming the primary key)
/// ```
///
/// ## Key Length Inference
///
/// On the first pair, we infer `key_len` to determine record boundaries:
///
/// ```text
/// Case 1: First pair is a marker (derive macro format)
///   ^user(42)         = ""        <- marker, key = [42], key_len = 1
///   ^user(42, "name") = "Alice"
///   ^user(42, "age")  = 30
///
/// Case 2: No marker (manual writes)
///   ^user(42, "name") = "Alice"   <- first pair, key = [42, "name"], key_len = 1
///   ^user(42, "age")  = 30
/// ```
///
/// ## State Transitions
///
/// For each incoming `(key, value)` pair:
///
/// ```text
///                              ┌─────────────────────────────┐
///                              │ Extract prefix from key     │
///                              │ (first `key_len` subscripts)│
///                              └─────────────┬───────────────┘
///                                            │
///                     ┌──────────────────────┴──────────────────────┐
///                     ▼                                             ▼
///          ┌──────────────────────┐                    ┌──────────────────────┐
///          │ prefix == cur_prefix │                    │ prefix != cur_prefix │
///          │ (same record)        │                    │ (record boundary)    │
///          └──────────┬───────────┘                    └──────────┬───────────┘
///                     │                                           │
///                     ▼                                           ▼
///          ┌──────────────────────┐                    ┌──────────────────────┐
///          │ Accumulate pair:     │                    │ 1. Parse cur_pairs   │
///          │ cur_pairs.push(k, v) │                    │    into record       │
///          └──────────────────────┘                    │ 2. Push to results   │
///                                                      │ 3. Reset cur_pairs   │
///                                                      │    with new (k, v)   │
///                                                      │ 4. Update cur_prefix │
///                                                      └──────────────────────┘
/// ```
///
/// ## Example Trace
///
/// Input stream for two users:
/// ```text
/// ^user(1)         = ""
/// ^user(1, "name") = "Alice"
/// ^user(2)         = ""           <- boundary! prefix [2] != [1]
/// ^user(2, "name") = "Bob"
/// (end of stream)
/// ```
///
/// State evolution:
/// ```text
/// Initial:  (None, None, [], [])
/// After 1:  (Some(1), Some([1]), [(1,"")], [])
/// After 2:  (Some(1), Some([1]), [(1,""), (1,"name","Alice")], [])
/// After 3:  (Some(1), Some([2]), [(2,"")], [User{id:1, name:"Alice"}])
///           ^^^^^^^^ boundary detected, parsed user 1
/// After 4:  (Some(1), Some([2]), [(2,""), (2,"name","Bob")], [User{...}])
/// Final:    parse remaining pairs -> [User{id:1}, User{id:2}]
/// ```
async fn stream_and_parse<T, S>(stream: S) -> Result<Vec<T>>
where
    T: FromRumps,
    S: futures::Stream<Item = Result<(Key, Value)>> + Send,
{
    // State tuple: (inferred key_len, current prefix, current pairs, parsed results)
    type State<T> = (Option<usize>, Option<Key>, Vec<(Key, Value)>, Vec<T>);

    let is_empty_string =
        |v: &Value| matches!(v, Value::String(s) if s.is_empty());

    let (_, maybe_prefix, pairs, mut results): State<T> = Box::pin(stream)
        .try_fold(
            (None, None, Vec::new(), Vec::new()),
            |(key_len, cur_prefix, mut cur_pairs, mut results), (k, v)| {
                // Key Length Inference (first pair only)
                //
                // `key_len` determines how many leading subscripts form the record's
                // primary key. Once inferred, it's fixed for the entire stream.
                let key_len = key_len.unwrap_or_else(|| {
                    // Marker case: `^user(42) = ""` -> key_len = 1
                    // The key IS the prefix, no field subscript present.
                    //
                    // Non-marker case: `^user(42, "name") = "Alice"` -> key_len = 1
                    // Last subscript is field name, so prefix = key[..len-1].
                    if is_empty_string(&v) {
                        k.len()
                    } else {
                        k.len().saturating_sub(1)
                    }
                });

                // Prefix Extraction
                //
                // Take the first `key_len` subscripts to get the record's prefix.
                // E.g., for key `[42, "name"]` with key_len=1, prefix = `[42]`.
                let prefix = (k.len() >= key_len).then(|| {
                    Key::from(
                        (0..key_len)
                            .filter_map(|i| k.get(i).cloned())
                            .collect::<Vec<_>>(),
                    )
                });

                // State Transition
                let res: Result<State<T>> = match (&cur_prefix, &prefix) {
                    // Record boundary: prefix changed from previous pair.
                    // Parse the accumulated pairs into a record, then start fresh.
                    (Some(p), Some(f)) if p != f => {
                        T::from_pairs(p, cur_pairs.into_iter())
                            .map_err(rumps_types::Error::from)
                            .map(|rec| {
                                results.push(rec);
                                // Start new accumulator with this pair
                                (Some(key_len), prefix, vec![(k, v)], results)
                            })
                    }
                    // Same record (or first entry): accumulate this pair.
                    _ => {
                        cur_pairs.push((k, v));
                        Ok((
                            Some(key_len),
                            // First entry: set prefix; otherwise keep current
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

    // Final Record
    //
    // The loop above only parses when it sees a NEW prefix. The last record's
    // pairs are still in `pairs` and need to be parsed here.
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
    async fn test_upsert_insert_when_not_exists() {
        let db = Database::in_memory().unwrap();

        // Upsert when record doesn't exist - should insert
        db.transaction(|txn| async move {
            txn.upsert(
                User {
                    id: 1,
                    name: "Alice".into(),
                    age: 30,
                },
                |_| panic!("callback should not be called when record doesn't exist"),
            )
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        let user: Option<User> = db.one(1u64).await.unwrap();
        assert_eq!(user.as_ref().map(|u| &u.name), Some(&"Alice".to_string()));
        assert_eq!(user.as_ref().map(|u| u.age), Some(30));
    }

    #[tokio::test]
    async fn test_upsert_update_when_exists() {
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

        // Upsert with callback that modifies existing
        db.transaction(|txn| async move {
            txn.upsert(
                User {
                    id: 1,
                    name: "ignored".into(),
                    age: 999,
                },
                |existing| {
                    Some(User {
                        id: existing.id,
                        name: "Alicia".into(),
                        age: existing.age + 1,
                    })
                },
            )
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        let user: Option<User> = db.one(1u64).await.unwrap();
        assert_eq!(user.as_ref().map(|u| &u.name), Some(&"Alicia".to_string()));
        assert_eq!(user.as_ref().map(|u| u.age), Some(31));
    }

    #[tokio::test]
    async fn test_upsert_delete_when_callback_returns_none() {
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

        // Upsert with callback that returns None - should delete
        db.transaction(|txn| async move {
            txn.upsert(
                User {
                    id: 1,
                    name: "ignored".into(),
                    age: 999,
                },
                |_| None,
            )
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        let user: Option<User> = db.one(1u64).await.unwrap();
        assert!(user.is_none());
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

    #[tokio::test]
    async fn test_insert_many() {
        let db = Database::in_memory().unwrap();

        let users = vec![
            User {
                id: 1,
                name: "Alice".into(),
                age: 30,
            },
            User {
                id: 2,
                name: "Bob".into(),
                age: 25,
            },
            User {
                id: 3,
                name: "Charlie".into(),
                age: 35,
            },
        ];

        // Insert all via insert_many
        db.transaction(|txn| {
            let users = users.clone();
            async move {
                txn.insert_many(&users).await?;
                Ok(())
            }
        })
        .await
        .unwrap();

        // Verify all records
        let all: Vec<User> = db.all().await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(
            all.iter().find(|u| u.id == 1).map(|u| &u.name),
            Some(&"Alice".to_string())
        );
        assert_eq!(
            all.iter().find(|u| u.id == 2).map(|u| &u.name),
            Some(&"Bob".to_string())
        );
        assert_eq!(
            all.iter().find(|u| u.id == 3).map(|u| &u.name),
            Some(&"Charlie".to_string())
        );
    }
}
