use std::collections::HashMap;

use super::{array, prelude, result, MethodFn, ResultMode};
use crate::intern::{StringId, StringInterner};

/// Registry for module-level HoFs (e.g., `Array.sort-by`).
///
/// Keyed by `(module_name, function_name)` pairs as `StringId`s.
pub(crate) struct Registry {
    fns: HashMap<(StringId, StringId), (MethodFn, ResultMode)>,
}

impl Registry {
    pub(crate) fn new(interner: &mut StringInterner) -> Self {
        let mut m = Self {
            fns: HashMap::new(),
        };
        m.register_all(interner);
        m
    }

    pub(super) fn register(
        &mut self,
        module: StringId,
        name: StringId,
        f: MethodFn,
        result: ResultMode,
    ) {
        self.fns.insert((module, name), (f, result));
    }

    /// Look up a module HoF by path.
    pub(crate) fn lookup(
        &self,
        path: &[StringId],
    ) -> Option<(MethodFn, ResultMode)> {
        match path {
            [module, name] => self.fns.get(&(*module, *name)).copied(),
            _ => None,
        }
    }

    fn register_all(&mut self, interner: &mut StringInterner) {
        array::Fns::register(self, interner);
        result::Fns::register(self, interner);
        prelude::Fns::register(self, interner);
    }
}
