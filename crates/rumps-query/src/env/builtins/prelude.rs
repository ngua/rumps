use smallvec::smallvec;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::{Prelude, Prim};
use crate::typecheck::{Scheme, Ty, TyArena, TyVar, TypeClass};
use crate::ClassId;

impl Environment {
    pub(super) fn register_prelude_builtin(&mut self) {
        // Prelude module
        // Auto-imported into every scope.
        let a = &mut self.ty_arena;
        let v0 = a.var(0);
        let v1 = a.var(1);

        // `foreach: forall A, B, F: Mappable. ((A) -> B, F[A]) -> Unit`
        let foreach_cb = a.func(smallvec![v0], v1);
        let tv2_of_v0 = a.hkt(TyVar::new(2), smallvec![v0]);
        let foreach_ty =
            a.func(smallvec![foreach_cb, tv2_of_v0], TyArena::UNIT);

        // `contains: forall T, F: Iterable. (F[T], T) -> Bool`
        let tv1_of_v0 = a.hkt(TyVar::new(1), smallvec![v0]);
        let contains_ty = a.func(smallvec![tv1_of_v0, v0], TyArena::BOOL);

        // `reverse: forall T. (Array[T] | Range) -> Array[T] | Range`
        let arr_v0 = a.array(v0);
        let rev_union =
            a.alloc(Ty::Union(None, smallvec![arr_v0, TyArena::RANGE]));
        let reverse_ty = a.func(smallvec![rev_union], rev_union);

        // `identity: forall A. (A) -> A`
        let identity_ty = a.func(smallvec![v0], v0);

        let prelude_id = self.consts.strings.intern("Prelude");
        self.modules.insert(
            prelude_id,
            Module::from_prims(
                &[
                    PrimDef {
                        name: "foreach",
                        f: Prelude::placeholder,
                        ty: Scheme {
                            vars: smallvec![
                                TyVar::new(0),
                                TyVar::new(1),
                                TyVar::new(2),
                            ],
                            ty: foreach_ty,
                            constraints: smallvec![(
                                TyVar::new(2),
                                TypeClass::hkt(ClassId::MAPPABLE)
                            )],
                        },
                    },
                    PrimDef {
                        name: "contains",
                        f: Prelude::contains,
                        ty: Scheme {
                            vars: smallvec![TyVar::new(0), TyVar::new(1)],
                            ty: contains_ty,
                            constraints: smallvec![(
                                TyVar::new(1),
                                TypeClass::hkt(ClassId::ITERABLE)
                            )],
                        },
                    },
                    PrimDef {
                        name: "reverse",
                        f: Prelude::reverse,
                        ty: Scheme {
                            vars: smallvec![TyVar::new(0)],
                            ty: reverse_ty,
                            constraints: smallvec![],
                        },
                    },
                    PrimDef {
                        name: "identity",
                        f: Prelude::identity,
                        ty: Scheme {
                            vars: smallvec![TyVar::new(0)],
                            ty: identity_ty,
                            constraints: smallvec![],
                        },
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
