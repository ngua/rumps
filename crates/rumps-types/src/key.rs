//! Key types for MUMPS variables (globals and locals).

use std::fmt;

use serde::{Deserialize, Serialize};

/// A MUMPS variable name, either Global (persistent) or Local (ephemeral).
///
/// # Examples
///
/// ```
/// use rumps_types::Name;
///
/// // Global variable (persistent, prefixed with ^)
/// let global = Name::Global("PATIENT".to_string());
/// assert_eq!(global.to_string(), "^PATIENT");
///
/// // Local variable (ephemeral, no prefix)
/// let local = Name::Local("TEMP".to_string());
/// assert_eq!(local.to_string(), "TEMP");
/// ```
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize
)]
pub enum Name {
    /// A global variable (persistent, stored on disk).
    /// Example: `^PATIENT`
    Global(String),

    /// A local variable (ephemeral, memory-only).
    /// Example: `PATIENT`
    Local(String),
}

impl Name {
    /// Returns the inner name string without the namespace prefix.
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_types::Name;
    ///
    /// let global = Name::Global("PATIENT".to_string());
    /// assert_eq!(global.name(), "PATIENT");
    ///
    /// let local = Name::Local("TEMP".to_string());
    /// assert_eq!(local.name(), "TEMP");
    /// ```
    pub fn name(&self) -> &str {
        match self {
            Self::Global(name) | Self::Local(name) => name,
        }
    }

    /// Returns `true` if this is a global variable.
    pub fn is_global(&self) -> bool {
        matches!(self, Self::Global(_))
    }

    /// Returns `true` if this is a local variable.
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local(_))
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Global(name) => write!(f, "^{}", name),
            Self::Local(name) => write!(f, "{}", name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_global_display() {
        let name = Name::Global("PATIENT".to_string());
        assert_eq!(name.to_string(), "^PATIENT");
    }

    #[test]
    fn test_local_display() {
        let name = Name::Local("TEMP".to_string());
        assert_eq!(name.to_string(), "TEMP");
    }

    #[test]
    fn test_name_accessor() {
        let global = Name::Global("PATIENT".to_string());
        assert_eq!(global.name(), "PATIENT");

        let local = Name::Local("TEMP".to_string());
        assert_eq!(local.name(), "TEMP");
    }

    #[test]
    fn test_is_global() {
        let global = Name::Global("PATIENT".to_string());
        assert!(global.is_global());
        assert!(!global.is_local());
    }

    #[test]
    fn test_is_local() {
        let local = Name::Local("TEMP".to_string());
        assert!(local.is_local());
        assert!(!local.is_global());
    }

    #[test]
    fn test_ordering() {
        let global1 = Name::Global("A".to_string());
        let global2 = Name::Global("B".to_string());
        let local1 = Name::Local("A".to_string());
        let local2 = Name::Local("B".to_string());

        // Globals should sort before locals (based on enum variant order)
        assert!(global1 < local1);
        assert!(global2 < local2);

        // Within same variant, sort by name
        assert!(global1 < global2);
        assert!(local1 < local2);
    }

    #[test]
    fn test_equality() {
        let global1 = Name::Global("PATIENT".to_string());
        let global2 = Name::Global("PATIENT".to_string());
        let local = Name::Local("PATIENT".to_string());

        assert_eq!(global1, global2);
        assert_ne!(global1, local);
    }

    #[test]
    fn test_serialization() {
        let global = Name::Global("PATIENT".to_string());
        let serialized = bincode::serialize(&global).unwrap();
        let deserialized: Name = bincode::deserialize(&serialized).unwrap();
        assert_eq!(global, deserialized);

        let local = Name::Local("TEMP".to_string());
        let serialized = bincode::serialize(&local).unwrap();
        let deserialized: Name = bincode::deserialize(&serialized).unwrap();
        assert_eq!(local, deserialized);
    }
}
