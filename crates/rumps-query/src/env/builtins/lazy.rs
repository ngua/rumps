use rumps_query_macros::scheme;

use super::super::{Environment, Module};
use crate::builtins::{Def, Impl, Lazy};

impl Environment {
    pub(super) fn register_lazy_builtin(&mut self) {
        let a = &mut self.ty_arena;

        let id = self.consts.strings.intern("Lazy");
        self.modules.insert(
            id,
            Module::from_defs(
                &[Def {
                    name: "force",
                    imp: Impl::Async(Lazy::force),
                    ty: scheme!(a, forall T. (Lazy[T]) -> T),
                }],
                &mut self.consts.strings,
            ),
        );
    }
}
