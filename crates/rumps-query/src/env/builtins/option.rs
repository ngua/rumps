use rumps_query_macros::scheme;

use super::super::{Environment, Module};
use crate::builtins::{Def, Impl, Opt};

impl Environment {
    pub(super) fn register_option_builtin(&mut self) {
        let a = &mut self.ty_arena;

        let opt_id = self.consts.strings.intern("Option");
        self.modules.insert(
            opt_id,
            Module::from_defs(
                &[
                    Def {
                        name: "unwrap-or",
                        imp: Impl::Sync(Opt::unwrap_or),
                        ty: scheme!(a, forall T. (Option[T], T) -> T),
                    },
                    Def {
                        name: "flatten",
                        imp: Impl::Sync(Opt::flatten),
                        ty: scheme!(
                            a,
                            forall T. (Option[Option[T]]) -> Option[T]
                        ),
                    },
                    Def {
                        name: "note",
                        imp: Impl::Sync(Opt::note),
                        ty: scheme!(
                            a,
                            forall T, U. (U, Option[T]) -> Result[T, U]
                        ),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
