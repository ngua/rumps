//! Shared handler logic for global and local operations.

use rumps_types::{DataStatus, Increment, Key, Name, Subscript, Value};

use crate::error::Result;

/// Abstraction over `Transaction` (globals) and session-scoped `Database`
/// (locals), allowing shared handler implementations for both namespaces.
pub(crate) trait Context {
    async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>>;
    async fn set(&self, name: &Name, key: &Key, val: Value) -> Result<()>;
    async fn increment(
        &self,
        name: &Name,
        key: &Key,
        by: Increment,
    ) -> Result<Value>;
    async fn kill(&self, name: &Name, key: &Key) -> Result<()>;
    async fn data(&self, name: &Name, key: &Key) -> Result<DataStatus>;
    async fn order(
        &self,
        name: &Name,
        prefix: &Key,
        after: Option<&Subscript>,
    ) -> Result<Option<Subscript>>;
    async fn query(
        &self,
        name: &Name,
        after: Option<&Key>,
    ) -> Result<Option<Key>>;
}
