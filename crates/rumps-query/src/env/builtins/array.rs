use rumps_query_macros::scheme;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::{Array, Prim};

impl Environment {
    pub(super) fn register_array_builtin(&mut self) {
        let a = &mut self.ty_arena;

        let arr_id = self.consts.strings.intern("Array");
        self.modules.insert(
            arr_id,
            Module::from_prims(
                &[
                    PrimDef {
                        name: "push",
                        f: Array::push,
                        ty: scheme!(a, forall T. (Array[T], T) -> Array[T]),
                    },
                    PrimDef {
                        name: "pop",
                        f: Array::pop,
                        ty: scheme!(a, forall T. (Array[T]) -> Array[T]),
                    },
                    PrimDef {
                        name: "head",
                        f: Array::head,
                        ty: scheme!(a, forall T. (Array[T]) -> Option[T]),
                    },
                    PrimDef {
                        name: "tail",
                        f: Array::tail,
                        ty: scheme!(a, forall T. (Array[T]) -> Array[T]),
                    },
                    PrimDef {
                        name: "sort",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T: Ord. (Array[T]) -> Array[T]
                        ),
                    },
                    PrimDef {
                        name: "contains",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T: Eq. (Array[T], T) -> Bool
                        ),
                    },
                    PrimDef {
                        name: "slice",
                        f: Array::slice,
                        ty: scheme!(
                            a,
                            forall T. (Array[T], Int, Int) -> Array[T]
                        ),
                    },
                    PrimDef {
                        name: "concat",
                        f: Array::concat,
                        ty: scheme!(
                            a,
                            forall T. (Array[T], Array[T]) -> Array[T]
                        ),
                    },
                    PrimDef {
                        name: "sort-by",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T. ((T, T) -> Ordering, Array[T]) -> Array[T]
                        ),
                    },
                    PrimDef {
                        name: "zip",
                        f: Array::zip,
                        ty: scheme!(
                            a,
                            forall T, U. (Array[T], Array[U]) -> Array[(T, U)]
                        ),
                    },
                    PrimDef {
                        name: "zip-with",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T, U, V. ((T, U) -> V, Array[T], Array[U]) -> Array[V]
                        ),
                    },
                    PrimDef {
                        name: "unzip",
                        f: Array::unzip,
                        ty: scheme!(
                            a,
                            forall T, U. (Array[(T, U)]) -> (Array[T], Array[U])
                        ),
                    },
                    PrimDef {
                        name: "intersperse",
                        f: Array::intersperse,
                        ty: scheme!(a, forall T. (T, Array[T]) -> Array[T]),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
