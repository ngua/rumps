use std::sync::Arc;

use smallvec::SmallVec;

use crate::typecheck::RuntimeTyId;
use crate::value::{Map, MapNode, Payload, TypeId, ValueId};

/// Result from a higher-order method that may need closure invocation.
pub(crate) enum Step {
    /// Method completed synchronously with a value.
    Done(Payload),
    /// Method completed by forwarding a value already stored in the arena.
    DoneValue(ValueId),
    /// Method needs to compare two values with `Ord:compare` and continue.
    Compare(Compare),
    /// Method needs to invoke a callable and continue.
    Invoke(Continuation),
}

/// Internal `Ord:compare` request for HoFs.
pub(crate) struct Compare {
    pub(crate) args: SmallVec<[ValueId; 2]>,
    pub(crate) state: State,
}

/// Continuation for HoF methods; uses zero-copy index tracking.
pub(crate) struct Continuation {
    /// The callable to invoke.
    pub(crate) callee: ValueId,
    /// Arguments to pass to the callable.
    pub(crate) args: SmallVec<[ValueId; 2]>,
    /// HoF-specific state for resumption.
    pub(crate) state: State,
}

/// Iteration kind for iterable HoFs.
pub(crate) enum IterKind {
    Array { source: ValueId, idx: usize },
}

/// HoF-specific state for resumption after closure invocation.
pub(crate) enum State {
    /// `Mappable:map` over `Array`.
    MapIter {
        kind: IterKind,
        acc: SmallVec<[ValueId; 4]>,
    },
    MapTuple {
        first: ValueId,
    },
    /// `Mappable:map` over single-value containers (Option.Some, Result.Ok).
    MapContainer {
        /// Type constructor (`OPTION` or `RESULT`).
        ctor_ty: TypeId,
        /// Variant tag (`1` for Some, `0` for Ok).
        tag: u8,
    },
    /// `Filterable:filter` over array.
    FilterArray {
        source: ValueId,
        idx: usize,
        acc: SmallVec<[ValueId; 4]>,
        /// Last element tested (to add to `acc` if predicate was true).
        pending: ValueId,
    },
    /// `Foldable:reduce` over array.
    ReduceArray {
        source: ValueId,
        idx: usize,
        acc: ValueId,
    },
    /// `Foldable:reduce` over range.
    ReduceRange {
        current: i64,
        end: i64,
        acc: ValueId,
    },
    /// `Chainable:chain`; single invocation, wraps result.
    Chain {
        wrapper: ChainWrapper,
    },
    /// `Array.zip-with`.
    ArrayZipWith {
        arr_a: ValueId,
        arr_b: ValueId,
        idx: usize,
        acc: SmallVec<[ValueId; 4]>,
    },
    /// `Prelude.foreach` over an array; discards each invocation result.
    PreludeForeachArray {
        source: ValueId,
        idx: usize,
    },
    /// `Prelude.foreach` over a single-value container.
    PreludeForeachOnce,
    /// `Array.sort-by` merge sort; stack-based to avoid recursion.
    ArraySortBy {
        source: ValueId,
        cmp: SortCmp,
        stack: Vec<SortFrame>,
    },
    /// `Result.map-err`; wraps mapped error back into `Result.Err`.
    ResultMapErr {
        /// Original `Ok` type.
        ok_ty: RuntimeTyId,
    },
    /// `Bimappable:bimap` over a 2-element container (tuple).
    BimapTuple {
        /// Second function to apply (`g`).
        second_fn: ValueId,
        /// Second element to transform (`b`).
        second_elem: ValueId,
        /// First result (after `f(a)` completes); `None` = awaiting first call.
        first_result: Option<ValueId>,
    },
    /// `Bimappable:bimap` over `Result` (single invocation).
    BimapResult {
        /// Which variant: `0` = Ok, `1` = Err.
        tag: u8,
    },
    /// `Map.map` and `Map.map-with-key`.
    MapModuleMap {
        source: ValueId,
        entries: SmallVec<[(ValueId, ValueId); 8]>,
        idx: usize,
        acc: SmallVec<[ValueId; 4]>,
        with_key: bool,
    },
    /// `Map.foreach` and `Map.foreach-with-key`.
    MapModuleForeach {
        source: ValueId,
        entries: SmallVec<[(ValueId, ValueId); 8]>,
        idx: usize,
        with_key: bool,
    },
    MapModuleEntriesCollect {
        entries: SmallVec<[(ValueId, ValueId); 8]>,
        idx: usize,
        acc: SmallVec<[(ValueId, ValueId); 8]>,
    },
    MapModuleEntriesInsert {
        map: Map,
        entries: SmallVec<[(ValueId, ValueId); 8]>,
        idx: usize,
        node: Arc<MapNode>,
        path: Vec<MapInsertFrame>,
    },
}

pub(crate) enum MapInsertFrame {
    Left {
        key: ValueId,
        val: ValueId,
        right: Option<Arc<MapNode>>,
    },
    Right {
        key: ValueId,
        val: ValueId,
        left: Option<Arc<MapNode>>,
    },
}

/// Wrapper kind for `Chainable:chain` result.
pub(crate) enum ChainWrapper {
    OptionSome,
    ResultOk,
    ResultErr(ValueId),
}

/// Stack frame for merge sort (replaces recursion).
pub(crate) enum SortFrame {
    /// Need to sort `[lo..hi)` of source array.
    Sort { lo: usize, hi: usize },
    /// Left half sorted; need to sort right half, then merge.
    MergeAfterRight {
        left: SmallVec<[ValueId; 4]>,
        lo: usize,
        hi: usize,
    },
    /// Both halves sorted; merge them.
    Merge {
        left: SmallVec<[ValueId; 4]>,
        right: SmallVec<[ValueId; 4]>,
        li: usize,
        ri: usize,
        merged: SmallVec<[ValueId; 4]>,
    },
}

/// Comparison source for `Array` sorting.
#[derive(Clone, Copy)]
pub(crate) enum SortCmp {
    Ord,
    Fn(ValueId),
}

/// Whether to keep or discard a module HoF's result after the trampoline
/// completes. Used to implement functions like `foreach` that delegate to an
/// existing HoF (e.g. `Mappable:map`) but discard the produced value.
#[derive(Clone, Copy)]
pub(crate) enum ResultMode {
    /// Return the value produced by the HoF directly.
    Keep,
    /// Discard the result, returning `Unit`.
    Discard,
}
