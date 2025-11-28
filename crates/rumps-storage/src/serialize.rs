//! Serialization configuration for RUMPS storage nodes.
//!
//! This module provides configuration for serializing/deserializing
//! B-tree nodes to/from compact binary format using bincode.
//!
//! # Serialization Format
//!
//! Nodes are serialized using bincode with the following configuration:
//! - **Endianness**: Little-endian (most common on modern hardware)
//! - **Integer encoding**: Variable-length (Varint) for compact representation
//! - **Limit**: Configurable maximum size to prevent oversized nodes

use bincode::Options;

/// Configuration for node serialization.
///
/// Controls the bincode encoding options used when serializing and
/// deserializing nodes. The defaults are optimized for compact storage
/// and compatibility with disk-based page storage.
#[derive(Debug, Clone)]
pub(crate) struct SerializeConfig {
    /// Maximum allowed serialized size in bytes.
    ///
    /// This prevents allocation of excessive memory when deserializing
    /// potentially malformed data. Default is `64 * 1024` (`64KB`), which
    /// accommodates typical page sizes with generous headroom.
    pub(crate) max_size: u64,
}

impl Default for SerializeConfig {
    fn default() -> Self {
        Self {
            // `64KB` max - generous for typical `4KB-16KB` pages
            max_size: 64 * 1024,
        }
    }
}

impl SerializeConfig {
    /// Creates a new configuration with the specified max size.
    pub(crate) fn with_max_size(max_size: u64) -> Self {
        Self { max_size }
    }

    /// Returns bincode options configured for this serialization config.
    ///
    /// The options use:
    /// - Little-endian byte order (most common on modern CPUs)
    /// - Variable-length integer encoding (compact for small values)
    /// - Size limit from `self.max_size`
    pub(crate) fn bincode_options(&self) -> impl Options {
        bincode::DefaultOptions::new()
            .with_little_endian()
            .with_varint_encoding()
            .with_limit(self.max_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_default() {
        let cfg = SerializeConfig::default();
        assert_eq!(cfg.max_size, 64 * 1024);
    }

    #[test]
    fn config_with_max_size() {
        let cfg = SerializeConfig::with_max_size(4096);
        assert_eq!(cfg.max_size, 4096);
    }
}
