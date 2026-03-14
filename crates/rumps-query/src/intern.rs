//! String interning for efficient string storage and comparison.
//!
//! Provides `StringId` (an opaque handle to an interned string) and
//! `StringInterner` (the intern table). Used by both the interpreter
//! (`ValueArena`) and type checker (`TypeEnv`, `Ty::Object`).

use indexmap::IndexSet;
use smallvec::SmallVec;

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
}

/// A structured module-qualified type name (e.g., `Math.Vector`).
///
/// Stores path segments as `SmallVec<[StringId; 3]>`, eliminating
/// string munging for qualified name operations.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct QualifiedName {
    segs: SmallVec<[StringId; 3]>,
}

impl QualifiedName {
    /// Single-segment (unqualified) name.
    pub(crate) fn local(name: StringId) -> Self {
        Self {
            segs: SmallVec::from_elem(name, 1),
        }
    }

    /// From segments.
    pub(crate) fn new(segs: impl Into<SmallVec<[StringId; 3]>>) -> Self {
        Self { segs: segs.into() }
    }

    /// Append a segment, producing a child name.
    pub(crate) fn child(&self, name: StringId) -> Self {
        let mut s = self.segs.clone();
        s.push(name);
        Self { segs: s }
    }

    /// Whether this name has more than one segment.
    pub(crate) fn is_qualified(&self) -> bool {
        self.segs.len() > 1
    }

    /// Last segment (the local/unqualified name).
    pub(crate) fn local_name(&self) -> StringId {
        self.segs
            .last()
            .copied()
            .unwrap_or_else(|| invariant!("QualifiedName is non-empty"))
    }

    /// All but the last segment, or `None` if single.
    pub(crate) fn parent(&self) -> Option<Self> {
        if self.segs.len() > 1 {
            Some(Self {
                segs: self.segs[..self.segs.len() - 1].into(),
            })
        } else {
            None
        }
    }

    /// Borrow the segments.
    pub(crate) fn segments(&self) -> &[StringId] {
        &self.segs
    }

    /// Iterate parent, grandparent, ... (for module walk-up).
    pub(crate) fn ancestors(&self) -> impl Iterator<Item = Self> {
        let segs = self.segs.clone();
        (1..segs.len()).rev().map(move |i| Self {
            segs: segs[..i].into(),
        })
    }

    /// Dot-joined display string for error messages.
    pub(crate) fn display(&self, interner: &StringInterner) -> String {
        self.segs
            .iter()
            .filter_map(|id| interner.get(*id))
            .collect::<Vec<_>>()
            .join(".")
    }

    /// Whether `self` is a direct child of `parent`.
    pub(crate) fn is_direct_child_of(&self, parent: &Self) -> bool {
        self.parent().as_ref() == Some(parent)
    }
}

impl From<StringId> for QualifiedName {
    fn from(id: StringId) -> Self {
        Self::local(id)
    }
}
