use smallvec::smallvec;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::Opt;
use crate::typecheck::{Scheme, TyId, TyVar};

impl Environment {
    pub(super) fn register_option_builtin(&mut self) {
        // Option module
        let a = &mut self.ty_arena;
        let v0 = a.var(0);
        let v1 = a.var(1);
        let opt_v0 = a.option(v0);

        // `unwrap-or: (Option[T], T) -> T`
        let opt_unwrap_or_ty = a.func(smallvec![opt_v0, v0], v0);
        // `flatten: (Option[Option[T]]) -> Option[T]`
        let opt_opt_v0 = a.option(opt_v0);
        let opt_flatten_ty = a.func(smallvec![opt_opt_v0], opt_v0);
        // `note: (U, Option[T]) -> Result[T, U]`
        let res_tu = a.result(v0, v1);
        let opt_note_ty = a.func(smallvec![v1, opt_v0], res_tu);

        let poly1 = |ty: TyId| Scheme {
            vars: smallvec![TyVar::new(0)],
            ty,
            constraints: smallvec![],
        };
        let poly2 = |ty: TyId| Scheme {
            vars: smallvec![TyVar::new(0), TyVar::new(1)],
            ty,
            constraints: smallvec![],
        };

        let opt_id = self.consts.strings.intern("Option");
        self.modules.insert(
            opt_id,
            Module::from_prims(
                &[
                    PrimDef {
                        name: "unwrap-or",
                        f: Opt::unwrap_or,
                        ty: poly1(opt_unwrap_or_ty),
                    },
                    PrimDef {
                        name: "flatten",
                        f: Opt::flatten,
                        ty: poly1(opt_flatten_ty),
                    },
                    PrimDef {
                        name: "note",
                        f: Opt::note,
                        ty: poly2(opt_note_ty),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
