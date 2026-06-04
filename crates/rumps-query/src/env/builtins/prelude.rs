use rumps_query_macros::scheme;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::{Prelude, Prim};

impl Environment {
    pub(super) fn register_prelude_builtin(&mut self) {
        // Prelude module
        // Auto-imported into every scope.
        let a = &mut self.ty_arena;

        let prelude_id = self.consts.strings.intern("Prelude");
        self.modules.insert(
            prelude_id,
            Module::from_prims(
                &[
                    PrimDef {
                        name: "foreach",
                        f: Prelude::placeholder,
                        ty: scheme!(
                            a,
                            forall T, U, F: Mappable. ((T) -> U, F[T]) -> Unit
                        ),
                    },
                    PrimDef {
                        name: "contains",
                        f: Prelude::contains,
                        ty: scheme!(
                            a,
                            forall T. (Array[T], T) -> Bool
                        ),
                    },
                    PrimDef {
                        name: "identity",
                        f: Prelude::identity,
                        ty: scheme!(a, forall T. (T) -> T),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
