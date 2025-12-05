//! Sealed trait for extension trait bounds.
//!
//! This prevents external crates from implementing our extension traits.

use crate::database::Database;
use crate::transaction::Transaction;

/// Marker trait for types that can use ORM extension methods.
///
/// This trait is sealed - only `Database` and `Transaction` can implement it.
pub trait Sealed {}

impl Sealed for Database {}
impl Sealed for Transaction {}
