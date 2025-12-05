use rumps_query_macros::scheme;

use super::super::{Environment, Module};
use crate::builtins::{Def, Impl, Prelude};

impl Environment {
    pub(super) fn register_prelude_builtin(&mut self) {
        // Prelude module
        // Auto-imported into every scope.
        let a = &mut self.ty_arena;

        let prelude_id = self.consts.strings.intern("Prelude");
        self.modules.insert(
            prelude_id,
            Module::from_defs(
                &[
                    Def {
                        name: "foreach",
                        imp: Impl::Async(Prelude::foreach),
                        ty: scheme!(
                            a,
                            forall T, U, F: Mappable. ((T) -> U, F[T]) -> Unit
                        ),
                    },
                    Def {
                        name: "identity",
                        imp: Impl::Sync(Prelude::identity),
                        ty: scheme!(a, forall T. (T) -> T),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
