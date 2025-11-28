//! Page-based storage configuration for RUMPS.
//!
//! The page size is a compile-time constant that determines the maximum
//! size of serialized B-tree nodes. This affects disk I/O alignment,
//! node splitting thresholds, and cache efficiency.
//!
//! To change the page size, set `RUMPS_PAGE_SIZE` env var at compile time:
//! ```sh
//! RUMPS_PAGE_SIZE=8192 cargo build
//! ```

/// Page size in bytes for B-tree node storage.
///
/// Common values:
/// - `4096` (4KB) - typical OS page size, good default
/// - `8192` (8KB) - PostgreSQL's default
/// - `16384` (16KB) - MySQL/InnoDB's default
///
/// Set via `RUMPS_PAGE_SIZE` env var at compile time. Defaults to `4096`.
/// Existing databases created with a different page size are incompatible.
pub(crate) const PAGE_SIZE: usize = {
    // SAFETY: build.rs guarantees this is set and valid
    match usize::from_str_radix(env!("RUMPS_PAGE_SIZE"), 10) {
        Ok(n) => n,
        Err(_) => 4096,
    }
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_size_is_power_of_two() {
        assert!(PAGE_SIZE.is_power_of_two());
    }

    #[test]
    fn page_size_reasonable() {
        assert!(PAGE_SIZE >= 512, "page size too small");
        assert!(PAGE_SIZE <= 65536, "page size too large");
    }
}
