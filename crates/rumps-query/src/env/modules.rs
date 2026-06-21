use std::collections::HashMap;

use crate::builtins;
use crate::intern::{StringId, StringInterner};
use crate::typecheck::{Scheme, TyId};
use crate::value::{FunctionDef, ValueId};

/// A built-in module containing builtin functions, constants, and submodules.
///
/// Modules group related functions under a namespace (e.g., `Iter.map`,
/// `String.split`). Supports nested modules for future extensibility
/// (e.g., `Math.Trig.sin`).
///
/// Constants are module-level values (e.g., `Math.pi`, `Math.e`) that are
/// evaluated to their stored `ValueId` when accessed.
///
/// Built-in modules are registered at interpreter startup; user-defined
/// modules will be supported in a future phase.
#[derive(Default)]
pub(crate) struct Module {
    /// Functions in this module.
    functions: HashMap<StringId, builtins::Def>,

    /// Constants in this module: `(value_id, type)`.
    /// `ValueId`s index into `Environment::consts`.
    constants: HashMap<StringId, (ValueId, TyId)>,

    /// Submodules, keyed by submodule name.
    submodules: HashMap<StringId, Self>,
}

impl Module {
    /// Create a module from builtin definitions.
    pub(crate) fn from_defs(
        defs: &[builtins::Def],
        interner: &mut StringInterner,
    ) -> Self {
        let functions = defs
            .iter()
            .map(|d| {
                (
                    interner.intern(d.name),
                    builtins::Def {
                        name: d.name,
                        imp: d.imp,
                        ty: d.ty.clone(),
                    },
                )
            })
            .collect();
        Self {
            functions,
            constants: HashMap::new(),
            submodules: HashMap::new(),
        }
    }

    /// Builder method to add a submodule.
    pub(crate) fn with_submodule(mut self, name: StringId, m: Self) -> Self {
        self.submodules.insert(name, m);
        self
    }

    /// Builder method to add a constant with its type.
    pub(crate) fn with_const(
        mut self,
        name: StringId,
        id: ValueId,
        ty: TyId,
    ) -> Self {
        self.constants.insert(name, (id, ty));
        self
    }

    /// Mutably add a constant with its type.
    pub(crate) fn add_const(&mut self, name: StringId, id: ValueId, ty: TyId) {
        self.constants.insert(name, (id, ty));
    }

    /// Look up a function by path within this module.
    ///
    /// For a single-segment path, looks up the function directly.
    /// For multi-segment paths, traverses submodules.
    pub(crate) fn get_fn(&self, path: &[StringId]) -> Option<&builtins::Impl> {
        match path {
            [] => None,
            [name] => self.functions.get(name).map(|d| &d.imp),
            [first, rest @ ..] => {
                self.submodules.get(first).and_then(|m| m.get_fn(rest))
            }
        }
    }

    /// Look up a function's type scheme by path within this module.
    pub(crate) fn get_fn_type(&self, path: &[StringId]) -> Option<&Scheme> {
        match path {
            [] => None,
            [name] => self.functions.get(name).map(|d| &d.ty),
            [first, rest @ ..] => {
                self.submodules.get(first).and_then(|m| m.get_fn_type(rest))
            }
        }
    }

    /// Look up a constant by path within this module.
    pub(crate) fn get_const(&self, path: &[StringId]) -> Option<ValueId> {
        match path {
            [] => None,
            [name] => self.constants.get(name).map(|(id, _)| *id),
            [first, rest @ ..] => {
                self.submodules.get(first).and_then(|m| m.get_const(rest))
            }
        }
    }

    /// Look up a constant's type by path within this module.
    pub(crate) fn get_const_type(&self, path: &[StringId]) -> Option<TyId> {
        match path {
            [] => None,
            [name] => self.constants.get(name).map(|(_, ty)| *ty),
            [first, rest @ ..] => self
                .submodules
                .get(first)
                .and_then(|m| m.get_const_type(rest)),
        }
    }

    /// Check if a path resolves to a function within this module.
    pub(crate) fn contains_fn(&self, path: &[StringId]) -> bool {
        self.get_fn(path).is_some()
    }

    /// Check if a path resolves to a constant within this module.
    pub(crate) fn contains_const(&self, path: &[StringId]) -> bool {
        self.get_const(path).is_some()
    }

    /// Get all public members (functions and constants) with their type schemes.
    ///
    /// Returns `(name, scheme)` pairs for all top-level members.
    /// All builtin module members are public.
    pub(crate) fn public_members(&self) -> Vec<(StringId, Scheme)> {
        let fns = self.functions.iter().map(|(name, d)| (*name, d.ty.clone()));
        let consts = self
            .constants
            .iter()
            .map(|(name, (_, ty))| (*name, Scheme::mono(*ty)));
        fns.chain(consts).collect()
    }

    /// Navigate to a submodule by path.
    ///
    /// An empty path returns `self`. Otherwise, navigates through submodules.
    pub(crate) fn get_submodule(&self, path: &[StringId]) -> Option<&Self> {
        match path {
            [] => Some(self),
            [first, rest @ ..] => self
                .submodules
                .get(first)
                .and_then(|m| m.get_submodule(rest)),
        }
    }

    pub(super) fn collect_fn_types(
        &self,
        prefix: &mut Vec<StringId>,
        out: &mut HashMap<Vec<StringId>, Scheme>,
    ) {
        self.functions.iter().for_each(|(&name, d)| {
            prefix.push(name);
            out.insert(prefix.clone(), d.ty.clone());
            let _ = prefix.pop();
        });
        self.submodules.iter().for_each(|(&name, m)| {
            prefix.push(name);
            m.collect_fn_types(prefix, out);
            let _ = prefix.pop();
        });
    }

    pub(super) fn collect_const_types(
        &self,
        prefix: &mut Vec<StringId>,
        out: &mut HashMap<Vec<StringId>, TyId>,
    ) {
        self.constants.iter().for_each(|(&name, (_, ty))| {
            prefix.push(name);
            out.insert(prefix.clone(), *ty);
            let _ = prefix.pop();
        });
        self.submodules.iter().for_each(|(&name, m)| {
            prefix.push(name);
            m.collect_const_types(prefix, out);
            let _ = prefix.pop();
        });
    }
}

/// A user-defined module containing functions and constants.
///
/// Unlike builtin `Module`s which use `builtins::Impl`, user modules store:
/// - Functions as `FunctionDef`s (not closures; siblings are bound at call time)
/// - Constants as `ValueId`s pointing to evaluated values
#[derive(Default, Clone)]
pub(crate) struct UserModule {
    /// Functions in this module, keyed by function name.
    /// Stored as `FunctionDef`s so sibling lookup happens at call time,
    /// enabling mutual recursion between module functions.
    pub(crate) functions: HashMap<StringId, FunctionDef>,

    /// Constants in this module, keyed by constant name.
    pub(crate) constants: HashMap<StringId, ValueId>,

    /// Submodules, keyed by submodule name.
    pub(crate) submodules: HashMap<StringId, Self>,
}

impl UserModule {
    /// Look up a function by path within this module.
    pub(crate) fn get_fn(&self, path: &[StringId]) -> Option<&FunctionDef> {
        match path {
            [] => None,
            [name] => self.functions.get(name),
            [first, rest @ ..] => {
                self.submodules.get(first).and_then(|m| m.get_fn(rest))
            }
        }
    }

    /// Look up a constant by path within this module.
    pub(crate) fn get_const(&self, path: &[StringId]) -> Option<ValueId> {
        match path {
            [] => None,
            [name] => self.constants.get(name).copied(),
            [first, rest @ ..] => {
                self.submodules.get(first).and_then(|m| m.get_const(rest))
            }
        }
    }

    /// Check if a path resolves to a function within this module.
    pub(crate) fn contains_fn(&self, path: &[StringId]) -> bool {
        self.get_fn(path).is_some()
    }

    /// Check if a path resolves to a constant within this module.
    pub(crate) fn contains_const(&self, path: &[StringId]) -> bool {
        self.get_const(path).is_some()
    }
}
