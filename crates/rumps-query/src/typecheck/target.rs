use std::iter;

use smallvec::SmallVec;

use super::ty::{TyArena, TyId};
use super::uf::UnionFind;
use crate::intern::StringId;
use crate::ClassId;

/// A terminal class call target selected by the typechecker.
///
/// `T` is `TyId` while inference owns the target and `RuntimeTyId` after
/// lowering. The target carries all checked dispatch evidence; runtime execution
/// must not search for another instance or walk representation edges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target<T> {
    /// A single terminal implementation.
    Direct {
        /// The terminal receiver for which the implementation was selected.
        recv: T,
        /// The exact generated user or default method; `None` denotes the
        /// registered builtin method for `recv`.
        fun: Option<StringId>,
    },
    /// Terminal implementations selected by one receiver argument's active
    /// representation.
    Union {
        /// The receiver position in the fully instantiated method signature.
        arg: usize,
        /// Ordered `(active_repr, recv, fun)` entries. `active_repr` only
        /// discriminates among already checked entries; `recv` and `fun` have
        /// `Target::Direct` semantics. Every entry is terminal.
        entries: SmallVec<[(T, T, Option<StringId>); 4]>,
    },
}

/// A flat set of checker proven targets for internal class operations.
///
/// Each entry is `(class, method, visible_recv, target)` and belongs to the
/// expression whose implementation performs the operation. The first `3`
/// elements form the lookup key; duplicate keys must identify the same target.
/// The visible receiver may differ from the terminal `recv`.
pub(crate) type Targets<T> = SmallVec<[(ClassId, StringId, T, Target<T>); 4]>;

impl Target<TyId> {
    /// Canonicalize every `TyId` owned by this target through `UnionFind`.
    ///
    /// This includes each terminal `recv` and each union entry's `active_repr`;
    /// both must be solved before runtime alias expansion collection and
    /// lowering.
    pub(super) fn resolve(&mut self, uf: &mut UnionFind, arena: &mut TyArena) {
        match self {
            Self::Direct { recv, .. } => {
                *recv = uf.resolve(*recv, arena);
            }
            Self::Union { entries, .. } => {
                entries.iter_mut().for_each(|(active, recv, _)| {
                    *active = uf.resolve(*active, arena);
                    *recv = uf.resolve(*recv, arena);
                });
            }
        }
    }

    /// Iterate over every `TyId` owned by this target.
    ///
    /// The sequence includes each union entry's `active_repr` followed by
    /// `recv`, preserving entry order so alias expansion collection sees every
    /// dispatch type.
    pub(super) fn ty_ids(&self) -> Box<dyn Iterator<Item = TyId> + '_> {
        match self {
            Self::Direct { recv, .. } => Box::new(iter::once(*recv)),
            Self::Union { entries, .. } => Box::new(
                entries
                    .iter()
                    .flat_map(|(active, recv, _)| [*active, *recv]),
            ),
        }
    }
}
