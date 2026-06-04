use smallvec::smallvec;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::{Array, Prim};
use crate::typecheck::{Scheme, Ty, TyArena, TyId, TyVar};

impl Environment {
    pub(super) fn register_array_builtin(&mut self) {
        let a = &mut self.ty_arena;

        // Pre-allocate common type variables and compound types
        let v0 = a.var(0);
        let v1 = a.var(1);
        let v2 = a.var(2);
        let arr_v0 = a.array(v0);
        let arr_v1 = a.array(v1);
        let opt_v0 = a.option(v0);

        // Helper: unconstrained poly1 scheme
        let poly1 = |ty: TyId| Scheme {
            vars: smallvec![TyVar::new(0)],
            ty,
            constraints: smallvec![],
        };

        // Helper: unconstrained poly2 scheme
        let poly2 = |ty: TyId| Scheme {
            vars: smallvec![TyVar::new(0), TyVar::new(1)],
            ty,
            constraints: smallvec![],
        };

        // Helper: unconstrained poly3 scheme
        let poly3 = |ty: TyId| Scheme {
            vars: smallvec![TyVar::new(0), TyVar::new(1), TyVar::new(2)],
            ty,
            constraints: smallvec![],
        };

        // Array module
        // `push: (Array[T], T) -> Array[T]`
        let push_ty = a.func(smallvec![arr_v0, v0], arr_v0);
        // `pop: (Array[T]) -> Array[T]`
        let pop_ty = a.func(smallvec![arr_v0], arr_v0);
        // `head: (Array[T]) -> Option[T]`
        let head_ty = a.func(smallvec![arr_v0], opt_v0);
        // `tail: (Array[T]) -> Array[T]`
        let tail_ty = pop_ty;
        // `sort: (Array[T]) -> Array[T]`
        let sort_ty = pop_ty;
        // `slice: (Array[T], Int, Int) -> Array[T]`
        let slice_ty =
            a.func(smallvec![arr_v0, TyArena::INT, TyArena::INT], arr_v0);
        // `concat: (Array[T], Array[T]) -> Array[T]`
        let concat_ty = a.func(smallvec![arr_v0, arr_v0], arr_v0);
        // `sort-by: ((T, T) -> Ordering, Array[T]) -> Array[T]`
        let cmp_cb = a.func(smallvec![v0, v0], TyArena::ORDERING);
        let sort_by_ty = a.func(smallvec![cmp_cb, arr_v0], arr_v0);
        // `zip: (Array[T], Array[U]) -> Array[(T, U)]`
        let pair_tu = a.alloc(Ty::Tuple(smallvec![v0, v1]));
        let arr_pair = a.array(pair_tu);
        let zip_ty = a.func(smallvec![arr_v0, arr_v1], arr_pair);
        // `zip-with: ((T, U) -> V, Array[T], Array[U]) -> Array[V]`
        let arr_v2 = a.array(v2);
        let zip_cb = a.func(smallvec![v0, v1], v2);
        let zip_with_ty = a.func(smallvec![zip_cb, arr_v0, arr_v1], arr_v2);
        // `unzip: (Array[(T, U)]) -> (Array[T], Array[U])`
        let arr_pair_in = a.array(pair_tu);
        let tup_out = a.alloc(Ty::Tuple(smallvec![arr_v0, arr_v1]));
        let unzip_ty = a.func(smallvec![arr_pair_in], tup_out);
        // `intersperse: (T, Array[T]) -> Array[T]`
        let intersperse_ty = a.func(smallvec![v0, arr_v0], arr_v0);

        let arr_id = self.consts.strings.intern("Array");
        self.modules.insert(
            arr_id,
            Module::from_prims(
                &[
                    PrimDef {
                        name: "push",
                        f: Array::push,
                        ty: poly1(push_ty),
                    },
                    PrimDef {
                        name: "pop",
                        f: Array::pop,
                        ty: poly1(pop_ty),
                    },
                    PrimDef {
                        name: "head",
                        f: Array::head,
                        ty: poly1(head_ty),
                    },
                    PrimDef {
                        name: "tail",
                        f: Array::tail,
                        ty: poly1(tail_ty),
                    },
                    PrimDef {
                        name: "sort",
                        f: Array::sort,
                        ty: poly1(sort_ty),
                    },
                    PrimDef {
                        name: "slice",
                        f: Array::slice,
                        ty: poly1(slice_ty),
                    },
                    PrimDef {
                        name: "concat",
                        f: Array::concat,
                        ty: poly1(concat_ty),
                    },
                    PrimDef {
                        name: "sort-by",
                        f: Array::placeholder,
                        ty: poly1(sort_by_ty),
                    },
                    PrimDef {
                        name: "zip",
                        f: Array::zip,
                        ty: poly2(zip_ty),
                    },
                    PrimDef {
                        name: "zip-with",
                        f: Array::placeholder,
                        ty: poly3(zip_with_ty),
                    },
                    PrimDef {
                        name: "unzip",
                        f: Array::unzip,
                        ty: poly2(unzip_ty),
                    },
                    PrimDef {
                        name: "intersperse",
                        f: Array::intersperse,
                        ty: poly1(intersperse_ty),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
