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

use std::future;

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

/// Read operations trait for storage backends.
///
/// This trait is sealed and implemented for [`Database`] and [`Transaction`].
/// Prefer using [`RumpsRead`] methods on your types for a more ergonomic API.
#[async_trait]
pub trait RumpsReader: Sealed {
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

/// Write operations trait for storage backends.
///
/// This trait is sealed and only implemented for [`Transaction`].
/// Prefer using [`RumpsWrite`] methods on your types for a more ergonomic API.
#[async_trait]
pub trait RumpsWriter: Sealed {
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
        F: FnOnce(T) -> Option<T> + Send;
}

/// Read operations for RUMPS entity types.
///
/// This trait is automatically implemented for any type implementing
/// [`FromRumps`]. Methods are called on the type itself rather than
/// on `Database`/`Transaction`.
///
/// # Ordering Guarantee
///
/// Methods returning multiple records ([`all`](Self::all), [`query`](Self::query))
/// return results in key-collation order. This is inherited from RUMPS's B-tree
/// storage; data is stored sorted, so no explicit sorting is required.
///
/// # Example
///
/// ```ignore
/// use rumps_storage::{Database, RumpsRead};
///
/// let db = Database::in_memory()?;
///
/// // Get a single record by key
/// let user = User::one(&db, 123u64).await?;
///
/// // Get all records (returned in key order)
/// let users = User::all(&db).await?;
///
/// // Check if a record exists
/// let exists = User::exists(&db, 123u64).await?;
///
/// // Query by prefix
/// let subset = User::query(&db, "prefix").await?;
/// ```
#[async_trait]
pub trait RumpsRead: FromRumps + Send + Sync + Sized {
    /// Gets a single record by key.
    ///
    /// Returns `None` if the record doesn't exist.
    async fn one<K, R>(r: &R, key: K) -> Result<Option<Self>>
    where
        K: IntoKey + Send,
        R: RumpsReader + Sync;

    /// Checks if a record exists.
    async fn exists<K, R>(r: &R, key: K) -> Result<bool>
    where
        K: IntoKey + Send,
        R: RumpsReader + Sync;

    /// Gets all records of this type.
    async fn all<R>(r: &R) -> Result<Vec<Self>>
    where
        R: RumpsReader + Sync;

    /// Queries records matching a key prefix.
    async fn query<K, R>(r: &R, prefix: K) -> Result<Vec<Self>>
    where
        K: IntoKey + Send,
        R: RumpsReader + Sync;
}

/// Write operations for RUMPS entity types.
///
/// This trait is automatically implemented for any type implementing
/// [`ToRumps`]. Write methods require a [`Transaction`] reference.
///
/// # Example
///
/// ```ignore
/// use rumps_storage::{Database, RumpsWrite};
///
/// let db = Database::in_memory()?;
///
/// db.transaction(|tx| async move {
///     // Insert a record
///     user.insert(&tx).await?;
///
///     // Delete by key
///     User::delete(&tx, 123u64).await?;
///
///     // Batch insert
///     User::insert_many(&tx, &users).await?;
///
///     // Upsert
///     User::upsert(&tx, user, |old| Some(modified)).await?;
///
///     Ok(())
/// }).await?;
/// ```
#[async_trait]
pub trait RumpsWrite: ToRumps + Send + Sync + Sized {
    /// Inserts this record.
    async fn insert<W>(&self, w: &W) -> Result<()>
    where
        W: RumpsWriter + Sync;

    /// Deletes a record by key.
    async fn delete<W, K>(w: &W, key: K) -> Result<()>
    where
        W: RumpsWriter + Sync,
        K: IntoKey + Send;

    /// Inserts multiple records efficiently in a single batch.
    async fn insert_many<W>(w: &W, vals: &[Self]) -> Result<()>
    where
        W: RumpsWriter + Sync;

    /// Inserts or updates a record.
    ///
    /// If the record doesn't exist, inserts `val`. If it exists, applies the
    /// callback `f` to the existing record:
    /// - `Some(new)` replaces with `new`
    /// - `None` deletes the record
    async fn upsert<W, F>(w: &W, val: Self, f: F) -> Result<()>
    where
        Self: FromRumps,
        W: RumpsWriter + RumpsReader + Sync,
        F: FnOnce(Self) -> Option<Self> + Send;
}

#[async_trait]
impl<T> RumpsRead for T
where
    T: FromRumps + Send + Sync,
{
    async fn one<K, R>(r: &R, key: K) -> Result<Option<Self>>
    where
        K: IntoKey + Send,
        R: RumpsReader + Sync,
    {
        r.one(key).await
    }

    async fn exists<K, R>(r: &R, key: K) -> Result<bool>
    where
        K: IntoKey + Send,
        R: RumpsReader + Sync,
    {
        r.exists::<Self, _>(key).await
    }

    async fn all<R>(r: &R) -> Result<Vec<Self>>
    where
        R: RumpsReader + Sync,
    {
        r.all().await
    }

    async fn query<K, R>(r: &R, prefix: K) -> Result<Vec<Self>>
    where
        K: IntoKey + Send,
        R: RumpsReader + Sync,
    {
        r.query(prefix).await
    }
}

#[async_trait]
impl<T> RumpsWrite for T
where
    T: ToRumps + Send + Sync,
{
    async fn insert<W>(&self, w: &W) -> Result<()>
    where
        W: RumpsWriter + Sync,
    {
        w.insert(self).await
    }

    async fn delete<W, K>(w: &W, key: K) -> Result<()>
    where
        W: RumpsWriter + Sync,
        K: IntoKey + Send,
    {
        w.delete::<Self, _>(key).await
    }

    async fn insert_many<W>(w: &W, vals: &[Self]) -> Result<()>
    where
        W: RumpsWriter + Sync,
    {
        w.insert_many(vals).await
    }

    async fn upsert<W, F>(w: &W, val: Self, f: F) -> Result<()>
    where
        Self: FromRumps,
        W: RumpsWriter + RumpsReader + Sync,
        F: FnOnce(Self) -> Option<Self> + Send,
    {
        w.upsert(val, f).await
    }
}

#[async_trait]
impl RumpsReader for Database {
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
impl RumpsReader for Transaction {
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
impl RumpsWriter for Transaction {
    async fn insert<T>(&self, val: &T) -> Result<()>
    where
        T: ToRumps + Sync,
    {
        let name = global!(T::GLOBAL);
        let key = val.to_key();
        let pairs = val.to_pairs(&key);

        // Insert all key-value pairs with single lock acquisition
        self.set_many(&name, &pairs).await
    }

    async fn insert_many<T>(&self, vals: &[T]) -> Result<()>
    where
        T: ToRumps + Sync,
    {
        let name = global!(T::GLOBAL);

        // Flatten all pairs from all values into one batch
        let pairs: Vec<(Key, Value)> = vals
            .iter()
            .flat_map(|val| {
                let key = val.to_key();
                val.to_pairs(&key)
            })
            .collect();

        // Insert all with single lock acquisition
        self.set_many(&name, &pairs).await
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
        F: FnOnce(T) -> Option<T> + Send,
    {
        let key = val.to_key();

        match self.one::<T, _>(key.clone()).await? {
            None => self.insert(&val).await,
            Some(old) => {
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

                future::ready(res)
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
                u.insert(&txn).await?;
                Ok(())
            }
        })
        .await
        .unwrap();

        // Get record
        let fetched = User::one(&db, 1u64).await.unwrap();
        assert_eq!(fetched, Some(user));
    }

    #[tokio::test]
    async fn test_exists() {
        let db = Database::in_memory().unwrap();

        // Doesn't exist yet
        assert!(!User::exists(&db, 1u64).await.unwrap());

        // Insert
        db.transaction(|txn| async move {
            User {
                id: 1,
                name: "Bob".into(),
                age: 25,
            }
            .insert(&txn)
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        // Now exists
        assert!(User::exists(&db, 1u64).await.unwrap());
    }

    #[tokio::test]
    async fn test_all() {
        let db = Database::in_memory().unwrap();

        // Insert multiple users
        db.transaction(|txn| async move {
            User {
                id: 1,
                name: "Alice".into(),
                age: 30,
            }
            .insert(&txn)
            .await?;
            User {
                id: 2,
                name: "Bob".into(),
                age: 25,
            }
            .insert(&txn)
            .await?;
            User {
                id: 3,
                name: "Charlie".into(),
                age: 35,
            }
            .insert(&txn)
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        // Get all records
        let users = User::all(&db).await.unwrap();
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
            User {
                id: 1,
                name: "Alice".into(),
                age: 30,
            }
            .insert(&txn)
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        // Verify exists
        assert!(User::exists(&db, 1u64).await.unwrap());

        // Delete
        db.transaction(|txn| async move {
            User::delete(&txn, 1u64).await?;
            Ok(())
        })
        .await
        .unwrap();

        // Verify deleted
        assert!(!User::exists(&db, 1u64).await.unwrap());
    }

    #[tokio::test]
    async fn test_upsert_insert_when_not_exists() {
        let db = Database::in_memory().unwrap();

        // Upsert when record doesn't exist; should insert
        db.transaction(|txn| async move {
            User::upsert(
                &txn,
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

        let user = User::one(&db, 1u64).await.unwrap();
        assert_eq!(user.as_ref().map(|u| &u.name), Some(&"Alice".to_string()));
        assert_eq!(user.as_ref().map(|u| u.age), Some(30));
    }

    #[tokio::test]
    async fn test_upsert_update_when_exists() {
        let db = Database::in_memory().unwrap();

        // Insert initial
        db.transaction(|txn| async move {
            User {
                id: 1,
                name: "Alice".into(),
                age: 30,
            }
            .insert(&txn)
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        // Upsert with callback that modifies existing
        db.transaction(|txn| async move {
            User::upsert(
                &txn,
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

        let user = User::one(&db, 1u64).await.unwrap();
        assert_eq!(user.as_ref().map(|u| &u.name), Some(&"Alicia".to_string()));
        assert_eq!(user.as_ref().map(|u| u.age), Some(31));
    }

    #[tokio::test]
    async fn test_upsert_delete_when_callback_returns_none() {
        let db = Database::in_memory().unwrap();

        // Insert initial
        db.transaction(|txn| async move {
            User {
                id: 1,
                name: "Alice".into(),
                age: 30,
            }
            .insert(&txn)
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        // Upsert with callback that returns `None`; should delete
        db.transaction(|txn| async move {
            User::upsert(
                &txn,
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

        let user = User::one(&db, 1u64).await.unwrap();
        assert!(user.is_none());
    }

    #[tokio::test]
    async fn test_transaction_read() {
        let db = Database::in_memory().unwrap();

        // Insert via transaction
        db.transaction(|txn| async move {
            User {
                id: 1,
                name: "Alice".into(),
                age: 30,
            }
            .insert(&txn)
            .await?;

            // Read within same transaction
            let user = User::one(&txn, 1u64).await?;
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

        // Insert all via `insert_many`
        db.transaction(|txn| {
            let users = users.clone();
            async move {
                User::insert_many(&txn, &users).await?;
                Ok(())
            }
        })
        .await
        .unwrap();

        // Verify all records
        let all = User::all(&db).await.unwrap();
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

    #[tokio::test]
    async fn test_entity_one() {
        let db = Database::in_memory().unwrap();

        let user = User {
            id: 1,
            name: "Alice".into(),
            age: 30,
        };

        db.transaction(|txn| {
            let u = user.clone();
            async move {
                u.insert(&txn).await?;
                Ok(())
            }
        })
        .await
        .unwrap();

        // Entity-centric read
        let fetched = User::one(&db, 1u64).await.unwrap();
        assert_eq!(fetched, Some(user));
    }

    #[tokio::test]
    async fn test_entity_exists() {
        let db = Database::in_memory().unwrap();

        // Doesn't exist yet
        assert!(!User::exists(&db, 1u64).await.unwrap());

        db.transaction(|txn| async move {
            User {
                id: 1,
                name: "Bob".into(),
                age: 25,
            }
            .insert(&txn)
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        // Now exists
        assert!(User::exists(&db, 1u64).await.unwrap());
    }

    #[tokio::test]
    async fn test_entity_all() {
        let db = Database::in_memory().unwrap();

        db.transaction(|txn| async move {
            User {
                id: 1,
                name: "Alice".into(),
                age: 30,
            }
            .insert(&txn)
            .await?;
            User {
                id: 2,
                name: "Bob".into(),
                age: 25,
            }
            .insert(&txn)
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        let all = User::all(&db).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn test_entity_delete() {
        let db = Database::in_memory().unwrap();

        db.transaction(|txn| async move {
            User {
                id: 1,
                name: "Alice".into(),
                age: 30,
            }
            .insert(&txn)
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        assert!(User::exists(&db, 1u64).await.unwrap());

        // Delete by key
        db.transaction(|txn| async move {
            User::delete(&txn, 1u64).await?;
            Ok(())
        })
        .await
        .unwrap();

        assert!(!User::exists(&db, 1u64).await.unwrap());
    }

    #[tokio::test]
    async fn test_entity_insert_many() {
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
        ];

        db.transaction(|txn| {
            let users = users.clone();
            async move {
                User::insert_many(&txn, &users).await?;
                Ok(())
            }
        })
        .await
        .unwrap();

        let all = User::all(&db).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn test_entity_upsert() {
        let db = Database::in_memory().unwrap();

        // Insert new
        db.transaction(|txn| async move {
            User::upsert(
                &txn,
                User {
                    id: 1,
                    name: "Alice".into(),
                    age: 30,
                },
                |_| panic!("should not be called for new record"),
            )
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        let user = User::one(&db, 1u64).await.unwrap().unwrap();
        assert_eq!(user.name, "Alice");

        // Update existing
        db.transaction(|txn| async move {
            User::upsert(
                &txn,
                User {
                    id: 1,
                    name: "ignored".into(),
                    age: 999,
                },
                |old| {
                    Some(User {
                        name: "Alicia".into(),
                        age: old.age + 1,
                        ..old
                    })
                },
            )
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        let user = User::one(&db, 1u64).await.unwrap().unwrap();
        assert_eq!(user.name, "Alicia");
        assert_eq!(user.age, 31);
    }

    #[tokio::test]
    async fn test_transaction_atomicity() {
        let db = Database::in_memory().unwrap();

        // Insert initial user
        db.transaction(|txn| async move {
            User {
                id: 1,
                name: "Alice".into(),
                age: 30,
            }
            .insert(&txn)
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        // Transaction that fails should not commit any changes
        let result: rumps_types::Result<()> = db
            .transaction(|txn| async move {
                User {
                    id: 2,
                    name: "Bob".into(),
                    age: 25,
                }
                .insert(&txn)
                .await?;

                // Simulate error; this should cause rollback
                Err(rumps_types::StorageError::InvalidConfiguration(
                    "simulated error".into(),
                )
                .into())
            })
            .await;

        assert!(result.is_err());

        // User 2 should NOT exist (transaction rolled back)
        assert!(!User::exists(&db, 2u64).await.unwrap());

        // User 1 should still exist
        assert!(User::exists(&db, 1u64).await.unwrap());
    }

    #[tokio::test]
    async fn test_transaction_isolation_read_own_writes() {
        let db = Database::in_memory().unwrap();

        db.transaction(|txn| async move {
            // Insert within transaction
            User {
                id: 1,
                name: "Alice".into(),
                age: 30,
            }
            .insert(&txn)
            .await?;

            // Should be able to read own write within same transaction
            let user = User::one(&txn, 1u64).await?;
            assert!(user.is_some());
            assert_eq!(user.unwrap().name, "Alice");

            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_multiple_writes_same_transaction() {
        let db = Database::in_memory().unwrap();

        // Multiple writes to same and different keys in one transaction
        db.transaction(|txn| async move {
            User {
                id: 1,
                name: "Alice".into(),
                age: 30,
            }
            .insert(&txn)
            .await?;

            User {
                id: 2,
                name: "Bob".into(),
                age: 25,
            }
            .insert(&txn)
            .await?;

            // Update user 1
            User::delete(&txn, 1u64).await?;
            User {
                id: 1,
                name: "Alicia".into(),
                age: 31,
            }
            .insert(&txn)
            .await?;

            Ok(())
        })
        .await
        .unwrap();

        // Verify final state
        let user1 = User::one(&db, 1u64).await.unwrap().unwrap();
        assert_eq!(user1.name, "Alicia");
        assert_eq!(user1.age, 31);

        let user2 = User::one(&db, 2u64).await.unwrap().unwrap();
        assert_eq!(user2.name, "Bob");
    }
}
