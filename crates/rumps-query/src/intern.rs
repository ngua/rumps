//! String interning for efficient string storage and comparison.
//!
//! Provides `StringId` (an opaque handle to an interned string) and
//! `StringInterner` (the intern table). Used by both the interpreter
//! (`ValueArena`) and type checker (`TypeEnv`, `Ty::Object`).

use indexmap::IndexSet;

/// Index into a string intern table.
///
/// `StringId`s are only valid within the `StringInterner` that created them.
/// Comparing `StringId`s from different interners is undefined behavior
/// (will compile but produce nonsense results).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub(crate) struct StringId(u32);

impl StringId {
    pub(crate) fn idx(self) -> usize {
        self.0 as usize
    }
}

/// String intern table for deduplication and O(1) comparison.
///
/// Strings are stored once; repeated calls to `intern` with the same string
/// return the same `StringId`. Looking up a `StringId` is O(1).
#[derive(Clone, Debug, Default)]
pub(crate) struct StringInterner {
    strings: IndexSet<String>,
}

impl StringInterner {
    /// Create an empty interner.
    pub(crate) fn new() -> Self {
        Self {
            strings: IndexSet::new(),
        }
    }

    /// Intern a string, returning its ID.
    ///
    /// If the string is already interned, returns the existing ID.
    pub(crate) fn intern(&mut self, s: &str) -> StringId {
        self.strings.get_index_of(s).map_or_else(
            || {
                let (idx, _) = self.strings.insert_full(s.to_owned());
                StringId(idx as u32)
            },
            |idx| StringId(idx as u32),
        )
    }

    /// Get a string by its interned ID.
    pub(crate) fn get(&self, id: StringId) -> Option<&str> {
        self.strings.get_index(id.idx()).map(String::as_str)
    }

    /// Look up a string's ID without interning it.
    ///
    /// Returns `None` if the string has not been interned.
    pub(crate) fn lookup(&self, s: &str) -> Option<StringId> {
        self.strings.get_index_of(s).map(|idx| StringId(idx as u32))
    }

    /// Number of interned strings.
    pub(crate) fn len(&self) -> usize {
        self.strings.len()
    }

    /// Resolve a `StringId` to an owned `String`.
    pub(crate) fn resolve(&self, id: StringId) -> String {
        self.get(id).unwrap_or_default().to_owned()
    }

    /// Join a slice of `StringId`s with `"."` separators.
    pub(crate) fn join_path(&self, segs: &[StringId]) -> String {
        segs.iter()
            .filter_map(|id| self.get(*id))
            .collect::<Vec<_>>()
            .join(".")
    }
}
