//! Type environment for type checking.
//!
//! Manages scoped bindings from variable names to type schemes.

use std::collections::{HashMap, HashSet};

use super::ty::{
    BuiltinClassDef, BuiltinClassDefs, BuiltinClassTag, Scheme, Subst, TyArena,
    TyId, TyVar,
};
use crate::ast::Visibility;
use crate::intern::{StringId, StringInterner};

/// A module member entry with type scheme and visibility.
#[derive(Clone, Debug)]
pub(crate) struct ModuleMember {
    pub(crate) scheme: Scheme,
    pub(crate) vis: Visibility,
}

/// A single scope in the type environment.
#[derive(Clone, Debug, Default)]
struct Scope {
    bindings: HashMap<StringId, Scheme>,
    /// Whether a non-import statement has been seen in this scope.
    ///
    /// Used to enforce that imports appear at the top of each scope.
    seen_non_import: bool,
    /// Modules imported in this scope.
    ///
    /// Used to track which module instances are available.
    imported_modules: HashSet<StringId>,
}

/// Scoped type environment mapping names to type schemes.
///
/// Uses a stack of scopes for lexical scoping (blocks, functions, etc.).
pub(crate) struct TypeEnv {
    scopes: Vec<Scope>,
    pub(super) strings: StringInterner,
    /// Builtin class definitions, indexed by `BuiltinClassTag as usize`.
    class_defs: BuiltinClassDefs,
    /// User-defined module names registered during typechecking.
    user_modules: HashSet<StringId>,
    /// User module member types and visibility: `module_path -> member_name -> ModuleMember`.
    user_module_members: HashMap<StringId, HashMap<StringId, ModuleMember>>,
    /// User module type visibility: qualified type name (e.g., `Mod.Type`) -> visibility.
    ///
    /// Used to enforce visibility for `type`, `newtype`, `union` inside modules.
    user_module_type_vis: HashMap<StringId, Visibility>,
    /// Imported type aliases: unqualified name -> qualified name.
    ///
    /// When `IMPORT M.{ MyType }` is processed, maps `"MyType"` -> `"M.MyType"`.
    /// Checked first during type name resolution.
    imported_types: HashMap<StringId, StringId>,
}

impl TypeEnv {
    /// Create an empty environment with one global scope.
    ///
    /// The interner should be shared with `TypeRegistry` so `StringId`
    /// lookups are consistent. The `arena` is used to build class
    /// definitions that reference interned types.
    pub(crate) fn new(
        mut strings: StringInterner,
        arena: &mut TyArena,
    ) -> Self {
        let class_defs =
            BuiltinClassDef::build_all(&mut |s| strings.intern(s), arena);
        Self {
            scopes: vec![Scope::default()],
            strings,
            class_defs,
            user_modules: HashSet::new(),
            user_module_members: HashMap::new(),
            user_module_type_vis: HashMap::new(),
            imported_types: HashMap::new(),
        }
    }

    /// Look up a builtin class definition by tag.
    pub(crate) fn class_def(&self, tag: BuiltinClassTag) -> &BuiltinClassDef {
        &self.class_defs[tag as usize]
    }

    /// Register a user-defined module name.
    ///
    /// This tracks that a module with this name has been defined so that
    /// paths like `ModuleName.fn` can be resolved.
    pub(crate) fn register_user_module(&mut self, name: StringId) {
        self.user_modules.insert(name);
    }

    /// Check if a name is a registered user module.
    pub(crate) fn is_user_module(&self, name: StringId) -> bool {
        self.user_modules.contains(&name)
    }

    /// Register a member (function or constant) of a user module.
    ///
    /// Called when typechecking `fun` and `let` inside a `module` block.
    pub(crate) fn register_user_module_member(
        &mut self,
        module: StringId,
        member: StringId,
        scheme: Scheme,
        vis: Visibility,
    ) {
        self.user_module_members
            .entry(module)
            .or_default()
            .insert(member, ModuleMember { scheme, vis });
    }

    /// Look up a user module member by module path and member name.
    ///
    /// Returns the member (scheme + visibility) if found.
    pub(crate) fn lookup_user_module_member(
        &self,
        module: StringId,
        member: StringId,
    ) -> Option<&ModuleMember> {
        self.user_module_members
            .get(&module)
            .and_then(|m| m.get(&member))
    }

    /// Register visibility for a type inside a user module.
    ///
    /// Called for `type`, `newtype`, `union` inside `module` blocks.
    /// The `qname` is the qualified name (e.g., `Mod.MyType`).
    pub(crate) fn register_user_module_type_vis(
        &mut self,
        qname: StringId,
        vis: Visibility,
    ) {
        self.user_module_type_vis.insert(qname, vis);
    }

    /// Look up visibility for a module-qualified type name.
    ///
    /// Returns `Some(vis)` if this is a user module type, `None` otherwise.
    pub(crate) fn lookup_user_module_type_vis(
        &self,
        qname: StringId,
    ) -> Option<Visibility> {
        self.user_module_type_vis.get(&qname).copied()
    }

    /// Get all public members of a user module.
    ///
    /// Returns `(name_id, scheme)` pairs for all public members.
    pub(crate) fn get_public_user_module_members(
        &self,
        mod_path: StringId,
    ) -> Vec<(StringId, Scheme)> {
        self.user_module_members
            .get(&mod_path)
            .map(|members| {
                members
                    .iter()
                    .filter(|(_, m)| m.vis == Visibility::Public)
                    .map(|(&name, m)| (name, m.scheme.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all public types in a user module.
    ///
    /// Returns `(local_name_id, qualified_name_id)` pairs for direct children only.
    /// Uses `intern` rather than `lookup` for the local name so that types whose
    /// unqualified name was never independently interned are still returned
    /// (e.g., a type registered only as `"Mod.Type"` where `"Type"` alone was
    /// never interned).
    pub(crate) fn get_public_user_module_types(
        &mut self,
        mod_path: StringId,
    ) -> Vec<(StringId, StringId)> {
        let prefix = self.strings.get(mod_path).map(|s| format!("{}.", s));
        // Collect `(local_name_string, qname_id)` pairs first, then intern
        // the local names; this avoids borrowing `self.strings` mutably while
        // iterating `self.user_module_type_vis`.
        let pairs: Vec<(String, StringId)> = prefix
            .map(|prefix| {
                self.user_module_type_vis
                    .iter()
                    .filter_map(|(&qname_id, &vis)| {
                        self.strings.get(qname_id).and_then(|qname| {
                            qname
                                .strip_prefix(&prefix)
                                .filter(|local| {
                                    !local.contains('.')
                                        && vis == Visibility::Public
                                })
                                .map(|local| (local.to_owned(), qname_id))
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        pairs
            .into_iter()
            .map(|(local, qname_id)| (self.strings.intern(&local), qname_id))
            .collect()
    }

    /// Register an imported type alias.
    ///
    /// Maps a local (unqualified) name to its qualified name. Used when
    /// processing `IMPORT M.{ MyType }`.
    pub(crate) fn import_type(&mut self, local: StringId, qualified: StringId) {
        self.imported_types.insert(local, qualified);
    }

    /// Look up an imported type by its local `StringId`.
    ///
    /// Returns the qualified `StringId` if this type was imported.
    pub(crate) fn lookup_imported_type(
        &self,
        local: StringId,
    ) -> Option<StringId> {
        self.imported_types.get(&local).copied()
    }

    /// Push a new scope (e.g., entering a function body or block).
    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
    }

    /// Pop the current scope (e.g., leaving a function body or block).
    pub(crate) fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Mark that a non-import statement has been seen in the current scope.
    ///
    /// After this, any import statements will be errors.
    pub(crate) fn mark_non_import(&mut self) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.seen_non_import = true;
        }
    }

    /// Check if imports are allowed in the current scope.
    ///
    /// Returns `false` if a non-import statement has already been seen.
    pub(crate) fn imports_allowed(&self) -> bool {
        self.scopes
            .last()
            .map(|s| !s.seen_non_import)
            .unwrap_or(false)
    }

    /// Mark a module as imported in the current scope.
    pub(crate) fn mark_module_imported(&mut self, module: StringId) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.imported_modules.insert(module);
        }
    }

    /// Check if a module has been imported in any enclosing scope.
    pub(crate) fn is_module_imported(&self, module: StringId) -> bool {
        self.scopes
            .iter()
            .rev()
            .any(|s| s.imported_modules.contains(&module))
    }

    /// Bind a `StringId` to a type scheme in the current scope.
    pub(crate) fn bind(&mut self, id: StringId, scheme: Scheme) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.bindings.insert(id, scheme);
        }
    }

    /// Look up by `StringId`, searching from innermost to outermost scope.
    pub(crate) fn lookup(&self, id: StringId) -> Option<&Scheme> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.bindings.get(&id))
    }

    /// Intern a string, returning its ID.
    pub(crate) fn intern(&mut self, s: &str) -> StringId {
        self.strings.intern(s)
    }

    /// Look up a string's ID without interning it.
    pub(crate) fn lookup_str(&self, s: &str) -> Option<StringId> {
        self.strings.lookup(s)
    }

    /// Get a string by its interned ID.
    pub(crate) fn get_str(&self, id: StringId) -> Option<&str> {
        self.strings.get(id)
    }

    /// Resolve a `StringId` to `&str`, panicking if not found.
    pub(crate) fn resolve_str(&self, id: StringId) -> &str {
        self.strings
            .get(id)
            .unwrap_or_else(|| invariant!("StringId lookup"))
    }

    /// Resolve a `StringId` to an owned `String`.
    pub(crate) fn resolve_string(&self, id: StringId) -> String {
        self.strings.resolve(id)
    }

    /// Collect all free type variables in the environment.
    ///
    /// A type variable is free in the environment if it's free in any binding.
    pub(crate) fn free_vars(&self, arena: &TyArena) -> HashSet<TyVar> {
        self.scopes
            .iter()
            .flat_map(|scope| scope.bindings.values())
            .flat_map(|scheme| scheme.free_vars(arena))
            .collect()
    }

    /// Generalize a type over variables not free in the environment.
    ///
    /// Creates a polymorphic scheme by quantifying over type variables that
    /// are free in `ty` but not in any existing binding.
    ///
    /// Note: This produces a scheme with no constraints. For user-defined
    /// functions, constraints are added separately in `InferCtx::fun`. This
    /// means closures with constrained type params will only be checked at
    /// definition time, not at call sites.
    pub(crate) fn generalize(&self, ty: TyId, arena: &TyArena) -> Scheme {
        let env_fv = self.free_vars(arena);
        let ty_fv = arena.free_vars(ty);
        let vars: Vec<TyVar> = ty_fv.difference(&env_fv).copied().collect();
        Scheme {
            vars,
            ty,
            constraints: smallvec::SmallVec::new(),
        }
    }

    /// Apply a substitution to all schemes in the environment.
    pub(crate) fn apply(&mut self, subst: &Subst, arena: &mut TyArena) {
        self.scopes.iter_mut().for_each(|scope| {
            scope.bindings.values_mut().for_each(|scheme| {
                *scheme = scheme.apply(subst, arena);
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::ty::TyArena;
    use super::*;

    #[test]
    fn new_has_one_scope() {
        let mut arena = TyArena::new();
        let env = TypeEnv::new(StringInterner::new(), &mut arena);
        assert_eq!(env.scopes.len(), 1);
    }

    #[test]
    fn bind_and_lookup() {
        let mut arena = TyArena::new();
        let mut env = TypeEnv::new(StringInterner::new(), &mut arena);
        let x = env.intern("x");
        let y = env.intern("y");
        env.bind(x, Scheme::mono(TyArena::INT));
        assert_eq!(env.lookup(x), Some(&Scheme::mono(TyArena::INT)));
        assert_eq!(env.lookup(y), None);
    }

    #[test]
    fn shadowing() {
        let mut arena = TyArena::new();
        let mut env = TypeEnv::new(StringInterner::new(), &mut arena);
        let x = env.intern("x");
        env.bind(x, Scheme::mono(TyArena::INT));
        env.push_scope();
        env.bind(x, Scheme::mono(TyArena::STRING));
        assert_eq!(env.lookup(x), Some(&Scheme::mono(TyArena::STRING)));
        env.pop_scope();
        assert_eq!(env.lookup(x), Some(&Scheme::mono(TyArena::INT)));
    }

    #[test]
    fn inner_scope_sees_outer() {
        let mut arena = TyArena::new();
        let mut env = TypeEnv::new(StringInterner::new(), &mut arena);
        let x = env.intern("x");
        let y = env.intern("y");
        env.bind(x, Scheme::mono(TyArena::INT));
        env.push_scope();
        env.bind(y, Scheme::mono(TyArena::STRING));
        assert_eq!(env.lookup(x), Some(&Scheme::mono(TyArena::INT)));
        assert_eq!(env.lookup(y), Some(&Scheme::mono(TyArena::STRING)));
    }

    #[test]
    fn free_vars_collects_all() {
        let mut arena = TyArena::new();
        let mut env = TypeEnv::new(StringInterner::new(), &mut arena);
        let a = TyVar::new(0);
        let b = TyVar::new(1);
        let va = arena.var(0);
        let vb = arena.var(1);
        let x = env.intern("x");
        let y = env.intern("y");
        env.bind(x, Scheme::mono(va));
        env.push_scope();
        env.bind(y, Scheme::mono(vb));
        let fv = env.free_vars(&arena);
        assert!(fv.contains(&a));
        assert!(fv.contains(&b));
    }

    #[test]
    fn generalize_no_env_vars() {
        let mut arena = TyArena::new();
        let env = TypeEnv::new(StringInterner::new(), &mut arena);
        let a = TyVar::new(0);
        let va = arena.var(0);
        let ty = arena.array(va);
        let scheme = env.generalize(ty, &arena);
        assert!(scheme.vars.contains(&a));
    }

    #[test]
    fn generalize_excludes_env_vars() {
        let mut arena = TyArena::new();
        let mut env = TypeEnv::new(StringInterner::new(), &mut arena);
        let a = TyVar::new(0);
        let b = TyVar::new(1);
        let va = arena.var(0);
        let vb = arena.var(1);
        let existing = env.intern("existing");
        env.bind(existing, Scheme::mono(va));
        // `b` is free in ty but not in env; `a` is in both
        let ty = arena.func(smallvec::smallvec![va], vb);
        let scheme = env.generalize(ty, &arena);
        assert!(!scheme.vars.contains(&a)); // `a` in env, not generalized
        assert!(scheme.vars.contains(&b)); // `b` free, generalized
    }
}
