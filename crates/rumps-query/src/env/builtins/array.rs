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
                        name: "last",
                        f: Array::last,
                        ty: scheme!(a, forall T. (Array[T]) -> Option[T]),
                    },
                    PrimDef {
                        name: "init",
                        f: Array::init,
                        ty: scheme!(a, forall T. (Array[T]) -> Array[T]),
                    },
                    PrimDef {
                        name: "uncons",
                        f: Array::uncons,
                        ty: scheme!(
                            a,
                            forall T. (Array[T]) -> Option[(T, Array[T])]
                        ),
                    },
                    PrimDef {
                        name: "unsnoc",
                        f: Array::unsnoc,
                        ty: scheme!(
                            a,
                            forall T. (Array[T]) -> Option[(Array[T], T)]
                        ),
                    },
                    PrimDef {
                        name: "take",
                        f: Array::take,
                        ty: scheme!(a, forall T. (Int, Array[T]) -> Array[T]),
                    },
                    PrimDef {
                        name: "drop",
                        f: Array::drop,
                        ty: scheme!(a, forall T. (Int, Array[T]) -> Array[T]),
                    },
                    PrimDef {
                        name: "split-at",
                        f: Array::split_at,
                        ty: scheme!(
                            a,
                            forall T. (Int, Array[T]) -> (Array[T], Array[T])
                        ),
                    },
                    PrimDef {
                        name: "indexed",
                        f: Array::indexed,
                        ty: scheme!(a, forall T. (Array[T]) -> Array[(Int, T)]),
                    },
                    PrimDef {
                        name: "singleton",
                        f: Array::singleton,
                        ty: scheme!(a, forall T. (T) -> Array[T]),
                    },
                    PrimDef {
                        name: "cons",
                        f: Array::cons,
                        ty: scheme!(a, forall T. (T, Array[T]) -> Array[T]),
                    },
                    PrimDef {
                        name: "set-at",
                        f: Array::set_at,
                        ty: scheme!(
                            a,
                            forall T. (Array[T], Int, T) -> Option[Array[T]]
                        ),
                    },
                    PrimDef {
                        name: "remove-at",
                        f: Array::remove_at,
                        ty: scheme!(
                            a,
                            forall T. (Array[T], Int) -> Option[Array[T]]
                        ),
                    },
                    PrimDef {
                        name: "insert-at",
                        f: Array::insert_at,
                        ty: scheme!(
                            a,
                            forall T. (Array[T], Int, T) -> Array[T]
                        ),
                    },
                    PrimDef {
                        name: "adjust-at",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T. ((T) -> T, Array[T], Int) -> Option[Array[T]]
                        ),
                    },
                    PrimDef {
                        name: "flatten",
                        f: Array::flatten,
                        ty: scheme!(
                            a,
                            forall T. (Array[Array[T]]) -> Array[T]
                        ),
                    },
                    PrimDef {
                        name: "chunks-of",
                        f: Array::chunks_of,
                        ty: scheme!(
                            a,
                            forall T. (Word, Array[T]) -> Array[Array[T]]
                        ),
                    },
                    PrimDef {
                        name: "windows",
                        f: Array::windows,
                        ty: scheme!(
                            a,
                            forall T. (Word, Array[T]) -> Array[Array[T]]
                        ),
                    },
                    PrimDef {
                        name: "replicate",
                        f: Array::replicate,
                        ty: scheme!(a, forall T. (Int, T) -> Array[T]),
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
                        name: "minimum",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T: Ord. (Array[T]) -> Option[T]
                        ),
                    },
                    PrimDef {
                        name: "maximum",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T: Ord. (Array[T]) -> Option[T]
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
                        name: "elem-index",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T: Eq. (Array[T], T) -> Option[Int]
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
                        name: "any",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> Bool
                        ),
                    },
                    PrimDef {
                        name: "all",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> Bool
                        ),
                    },
                    PrimDef {
                        name: "find",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> Option[T]
                        ),
                    },
                    PrimDef {
                        name: "find-index",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> Option[Int]
                        ),
                    },
                    PrimDef {
                        name: "find-indices",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> Array[Int]
                        ),
                    },
                    PrimDef {
                        name: "take-while",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> Array[T]
                        ),
                    },
                    PrimDef {
                        name: "drop-while",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> Array[T]
                        ),
                    },
                    PrimDef {
                        name: "span",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> (Array[T], Array[T])
                        ),
                    },
                    PrimDef {
                        name: "break",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> (Array[T], Array[T])
                        ),
                    },
                    PrimDef {
                        name: "partition",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> (Array[T], Array[T])
                        ),
                    },
                    PrimDef {
                        name: "concat-map",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T, U. ((T) -> Array[U], Array[T]) -> Array[U]
                        ),
                    },
                    PrimDef {
                        name: "map-option",
                        f: Array::placeholder,
                        ty: scheme!(
                            a,
                            forall T, U. ((T) -> Option[U], Array[T]) -> Array[U]
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
