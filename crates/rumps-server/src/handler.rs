//! Shared handler logic for global and local operations.

use rumps_storage::Transaction;
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

impl Context for Transaction {
    async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>> {
        Ok(self.get(name, key).await?)
    }

    async fn set(&self, name: &Name, key: &Key, val: Value) -> Result<()> {
        Ok(self.set(name, key, val).await?)
    }

    async fn increment(
        &self,
        name: &Name,
        key: &Key,
        by: Increment,
    ) -> Result<Value> {
        Ok(self.increment(name, key, by).await?)
    }

    async fn kill(&self, name: &Name, key: &Key) -> Result<()> {
        Ok(self.kill(name, key).await?)
    }

    async fn data(&self, name: &Name, key: &Key) -> Result<DataStatus> {
        Ok(self.data(name, key).await?)
    }

    async fn order(
        &self,
        name: &Name,
        prefix: &Key,
        after: Option<&Subscript>,
    ) -> Result<Option<Subscript>> {
        Ok(self.order(name, prefix, after).await?)
    }

    async fn query(
        &self,
        name: &Name,
        after: Option<&Key>,
    ) -> Result<Option<Key>> {
        Ok(self.query(name, after).await?)
    }
}
