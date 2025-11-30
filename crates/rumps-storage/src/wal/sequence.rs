//! WAL sequence number type.

use std::fmt::{self, Display};

use serde::{Deserialize, Serialize};

/// A WAL sequence number - monotonically increasing across all WAL files.
///
/// Sequence numbers uniquely identify each record in the WAL and are used to:
/// - Order records for replay during recovery
/// - Detect gaps in the WAL (indicating corruption or missing files)
/// - Determine which archived files can be safely deleted after checkpointing
///
/// The sequence is formatted as 16-digit hex for consistency with archive filenames.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    Serialize,
    Deserialize
)]
#[repr(transparent)]
pub struct WalSequence(u64);

impl WalSequence {
    /// The zero sequence number (start of a fresh WAL).
    pub const ZERO: Self = Self(0);

    /// Create a new sequence number.
    pub fn new(n: u64) -> Self {
        Self(n)
    }

    /// Get the raw `u64` value.
    pub fn get(self) -> u64 {
        self.0
    }

    /// Get the next sequence number.
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }

    /// Saturating subtraction - returns `ZERO` if result would underflow.
    pub fn saturating_sub(self, n: u64) -> Self {
        Self(self.0.saturating_sub(n))
    }
}

impl From<u64> for WalSequence {
    fn from(n: u64) -> Self {
        Self(n)
    }
}

impl From<WalSequence> for u64 {
    fn from(seq: WalSequence) -> Self {
        seq.0
    }
}

impl Display for WalSequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn zero_constant() {
        assert_eq!(WalSequence::ZERO.get(), 0);
    }

    #[test]
    fn new_and_get() {
        let seq = WalSequence::new(42);
        assert_eq!(seq.get(), 42);
    }

    #[test]
    fn next_increments() {
        let seq = WalSequence::new(10);
        assert_eq!(seq.next().get(), 11);
    }

    #[test]
    fn saturating_sub_normal() {
        let seq = WalSequence::new(10);
        assert_eq!(seq.saturating_sub(3).get(), 7);
    }

    #[test]
    fn saturating_sub_underflow() {
        let seq = WalSequence::new(5);
        assert_eq!(seq.saturating_sub(10), WalSequence::ZERO);
    }

    #[test]
    fn from_u64() {
        let seq: WalSequence = 123u64.into();
        assert_eq!(seq.get(), 123);
    }

    #[test]
    fn into_u64() {
        let seq = WalSequence::new(456);
        let n: u64 = seq.into();
        assert_eq!(n, 456);
    }

    #[test]
    fn display_hex_format() {
        assert_eq!(format!("{}", WalSequence::ZERO), "0000000000000000");
        assert_eq!(format!("{}", WalSequence::new(255)), "00000000000000ff");
        assert_eq!(
            format!("{}", WalSequence::new(u64::MAX)),
            "ffffffffffffffff"
        );
    }

    #[test]
    fn ordering() {
        let a = WalSequence::new(1);
        let b = WalSequence::new(2);
        let c = WalSequence::new(2);

        assert!(a < b);
        assert!(b > a);
        assert_eq!(b, c);
    }

    #[test]
    fn serde_roundtrip() {
        let seq = WalSequence::new(12345);
        let bytes = bincode::serialize(&seq).expect("serialize");
        let decoded: WalSequence =
            bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(seq, decoded);
    }
}
