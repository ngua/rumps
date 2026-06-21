use rumps_query_macros::scheme;

use super::super::{Environment, Module};
use crate::builtins::{Array, Def, Impl};

impl Environment {
    pub(super) fn register_array_builtin(&mut self) {
        let a = &mut self.ty_arena;

        let arr_id = self.consts.strings.intern("Array");
        self.modules.insert(
            arr_id,
            Module::from_defs(
                &[
                    Def {
                        name: "push",
                        imp: Impl::Sync(Array::push),
                        ty: scheme!(a, forall T. (Array[T], T) -> Array[T]),
                    },
                    Def {
                        name: "pop",
                        imp: Impl::Sync(Array::pop),
                        ty: scheme!(a, forall T. (Array[T]) -> Array[T]),
                    },
                    Def {
                        name: "head",
                        imp: Impl::Sync(Array::head),
                        ty: scheme!(a, forall T. (Array[T]) -> Option[T]),
                    },
                    Def {
                        name: "tail",
                        imp: Impl::Sync(Array::tail),
                        ty: scheme!(a, forall T. (Array[T]) -> Array[T]),
                    },
                    Def {
                        name: "last",
                        imp: Impl::Sync(Array::last),
                        ty: scheme!(a, forall T. (Array[T]) -> Option[T]),
                    },
                    Def {
                        name: "init",
                        imp: Impl::Sync(Array::init),
                        ty: scheme!(a, forall T. (Array[T]) -> Array[T]),
                    },
                    Def {
                        name: "uncons",
                        imp: Impl::Sync(Array::uncons),
                        ty: scheme!(
                            a,
                            forall T. (Array[T]) -> Option[(T, Array[T])]
                        ),
                    },
                    Def {
                        name: "unsnoc",
                        imp: Impl::Sync(Array::unsnoc),
                        ty: scheme!(
                            a,
                            forall T. (Array[T]) -> Option[(Array[T], T)]
                        ),
                    },
                    Def {
                        name: "take",
                        imp: Impl::Sync(Array::take),
                        ty: scheme!(a, forall T. (Int, Array[T]) -> Array[T]),
                    },
                    Def {
                        name: "drop",
                        imp: Impl::Sync(Array::drop),
                        ty: scheme!(a, forall T. (Int, Array[T]) -> Array[T]),
                    },
                    Def {
                        name: "split-at",
                        imp: Impl::Sync(Array::split_at),
                        ty: scheme!(
                            a,
                            forall T. (Int, Array[T]) -> (Array[T], Array[T])
                        ),
                    },
                    Def {
                        name: "indexed",
                        imp: Impl::Sync(Array::indexed),
                        ty: scheme!(a, forall T. (Array[T]) -> Array[(Int, T)]),
                    },
                    Def {
                        name: "singleton",
                        imp: Impl::Sync(Array::singleton),
                        ty: scheme!(a, forall T. (T) -> Array[T]),
                    },
                    Def {
                        name: "cons",
                        imp: Impl::Sync(Array::cons),
                        ty: scheme!(a, forall T. (T, Array[T]) -> Array[T]),
                    },
                    Def {
                        name: "set-at",
                        imp: Impl::Sync(Array::set_at),
                        ty: scheme!(
                            a,
                            forall T. (Array[T], Int, T) -> Option[Array[T]]
                        ),
                    },
                    Def {
                        name: "remove-at",
                        imp: Impl::Sync(Array::remove_at),
                        ty: scheme!(
                            a,
                            forall T. (Array[T], Int) -> Option[Array[T]]
                        ),
                    },
                    Def {
                        name: "insert-at",
                        imp: Impl::Sync(Array::insert_at),
                        ty: scheme!(
                            a,
                            forall T. (Array[T], Int, T) -> Array[T]
                        ),
                    },
                    Def {
                        name: "adjust-at",
                        imp: Impl::Async(Array::adjust_at),
                        ty: scheme!(
                            a,
                            forall T. ((T) -> T, Array[T], Int) -> Option[Array[T]]
                        ),
                    },
                    Def {
                        name: "flatten",
                        imp: Impl::Sync(Array::flatten),
                        ty: scheme!(
                            a,
                            forall T. (Array[Array[T]]) -> Array[T]
                        ),
                    },
                    Def {
                        name: "chunks-of",
                        imp: Impl::Sync(Array::chunks_of),
                        ty: scheme!(
                            a,
                            forall T. (Word, Array[T]) -> Array[Array[T]]
                        ),
                    },
                    Def {
                        name: "windows",
                        imp: Impl::Sync(Array::windows),
                        ty: scheme!(
                            a,
                            forall T. (Word, Array[T]) -> Array[Array[T]]
                        ),
                    },
                    Def {
                        name: "replicate",
                        imp: Impl::Sync(Array::replicate),
                        ty: scheme!(a, forall T. (Int, T) -> Array[T]),
                    },
                    Def {
                        name: "sort",
                        imp: Impl::Async(Array::sort),
                        ty: scheme!(
                            a,
                            forall T: Ord. (Array[T]) -> Array[T]
                        ),
                    },
                    Def {
                        name: "minimum",
                        imp: Impl::Async(Array::minimum),
                        ty: scheme!(
                            a,
                            forall T: Ord. (Array[T]) -> Option[T]
                        ),
                    },
                    Def {
                        name: "maximum",
                        imp: Impl::Async(Array::maximum),
                        ty: scheme!(
                            a,
                            forall T: Ord. (Array[T]) -> Option[T]
                        ),
                    },
                    Def {
                        name: "contains",
                        imp: Impl::Async(Array::contains),
                        ty: scheme!(
                            a,
                            forall T: Eq. (Array[T], T) -> Bool
                        ),
                    },
                    Def {
                        name: "elem-index",
                        imp: Impl::Async(Array::elem_index),
                        ty: scheme!(
                            a,
                            forall T: Eq. (Array[T], T) -> Option[Int]
                        ),
                    },
                    Def {
                        name: "slice",
                        imp: Impl::Sync(Array::slice),
                        ty: scheme!(
                            a,
                            forall T. (Array[T], Int, Int) -> Array[T]
                        ),
                    },
                    Def {
                        name: "concat",
                        imp: Impl::Sync(Array::concat),
                        ty: scheme!(
                            a,
                            forall T. (Array[T], Array[T]) -> Array[T]
                        ),
                    },
                    Def {
                        name: "sort-by",
                        imp: Impl::Async(Array::sort_by),
                        ty: scheme!(
                            a,
                            forall T. ((T, T) -> Ordering, Array[T]) -> Array[T]
                        ),
                    },
                    Def {
                        name: "zip",
                        imp: Impl::Sync(Array::zip),
                        ty: scheme!(
                            a,
                            forall T, U. (Array[T], Array[U]) -> Array[(T, U)]
                        ),
                    },
                    Def {
                        name: "zip-with",
                        imp: Impl::Async(Array::zip_with),
                        ty: scheme!(
                            a,
                            forall T, U, V. ((T, U) -> V, Array[T], Array[U]) -> Array[V]
                        ),
                    },
                    Def {
                        name: "any",
                        imp: Impl::Async(Array::any),
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> Bool
                        ),
                    },
                    Def {
                        name: "all",
                        imp: Impl::Async(Array::all),
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> Bool
                        ),
                    },
                    Def {
                        name: "find",
                        imp: Impl::Async(Array::find),
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> Option[T]
                        ),
                    },
                    Def {
                        name: "find-index",
                        imp: Impl::Async(Array::find_index),
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> Option[Int]
                        ),
                    },
                    Def {
                        name: "find-indices",
                        imp: Impl::Async(Array::find_indices),
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> Array[Int]
                        ),
                    },
                    Def {
                        name: "take-while",
                        imp: Impl::Async(Array::take_while),
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> Array[T]
                        ),
                    },
                    Def {
                        name: "drop-while",
                        imp: Impl::Async(Array::drop_while),
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> Array[T]
                        ),
                    },
                    Def {
                        name: "span",
                        imp: Impl::Async(Array::span),
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> (Array[T], Array[T])
                        ),
                    },
                    Def {
                        name: "break",
                        imp: Impl::Async(Array::break_),
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> (Array[T], Array[T])
                        ),
                    },
                    Def {
                        name: "partition",
                        imp: Impl::Async(Array::partition),
                        ty: scheme!(
                            a,
                            forall T. ((T) -> Bool, Array[T]) -> (Array[T], Array[T])
                        ),
                    },
                    Def {
                        name: "concat-map",
                        imp: Impl::Async(Array::concat_map),
                        ty: scheme!(
                            a,
                            forall T, U. ((T) -> Array[U], Array[T]) -> Array[U]
                        ),
                    },
                    Def {
                        name: "map-option",
                        imp: Impl::Async(Array::map_option),
                        ty: scheme!(
                            a,
                            forall T, U. ((T) -> Option[U], Array[T]) -> Array[U]
                        ),
                    },
                    Def {
                        name: "unzip",
                        imp: Impl::Sync(Array::unzip),
                        ty: scheme!(
                            a,
                            forall T, U. (Array[(T, U)]) -> (Array[T], Array[U])
                        ),
                    },
                    Def {
                        name: "intersperse",
                        imp: Impl::Sync(Array::intersperse),
                        ty: scheme!(a, forall T. (T, Array[T]) -> Array[T]),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
