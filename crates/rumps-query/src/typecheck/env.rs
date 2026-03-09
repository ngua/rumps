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
    imported_modules: HashSet<String>,
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
    user_modules: HashSet<String>,
    /// User module member types and visibility: `module_path -> member_name -> ModuleMember`.
    user_module_members: HashMap<String, HashMap<String, ModuleMember>>,
    /// User module type visibility: qualified type name (e.g., `Mod.Type`) -> visibility.
    ///
    /// Used to enforce visibility for `TYPE`, `NEWTYPE`, `UNION` inside modules.
    user_module_type_vis: HashMap<String, Visibility>,
    /// Imported type aliases: unqualified name -> qualified name.
    ///
    /// When `IMPORT M.{ MyType }` is processed, maps `"MyType"` -> `"M.MyType"`.
    /// Checked first during type name resolution.
    imported_types: HashMap<String, String>,
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
    pub(crate) fn register_user_module(&mut self, name: &str) {
        self.user_modules.insert(name.to_string());
    }

    /// Check if a name is a registered user module.
    pub(crate) fn is_user_module(&self, name: &str) -> bool {
        self.user_modules.contains(name)
    }

    /// Register a member (function or constant) of a user module.
    ///
    /// Called when typechecking `FUN` and `LET` inside a `MODULE` block.
    pub(crate) fn register_user_module_member(
        &mut self,
        module: &str,
        member: &str,
        scheme: Scheme,
        vis: Visibility,
    ) {
        self.user_module_members
            .entry(module.to_string())
            .or_default()
            .insert(member.to_string(), ModuleMember { scheme, vis });
    }

    /// Look up a user module member by path.
    ///
    /// Path should be like `["Counter", "new"]` for `Counter.new`, or
    /// `["Outer", "Inner", "fn"]` for `Outer.Inner.fn`.
    ///
    /// Returns the member (scheme + visibility) if found.
    pub(crate) fn lookup_user_module_member(
        &self,
        path: &[&str],
    ) -> Option<&ModuleMember> {
        // Split into module path (all but last) and member (last)
        path.split_last().and_then(|(member, mod_path)| {
            // Join module path with dots (e.g., `["Outer", "Inner"]` -> `"Outer.Inner"`)
            let mod_key = mod_path.join(".");
            self.user_module_members
                .get(&mod_key)
                .and_then(|m| m.get(*member))
        })
    }

    /// Register visibility for a type inside a user module.
    ///
    /// Called for `TYPE`, `NEWTYPE`, `UNION` inside `MODULE` blocks.
    /// The `qname` is the qualified name (e.g., `Mod.MyType`).
    pub(crate) fn register_user_module_type_vis(
        &mut self,
        qname: &str,
        vis: Visibility,
    ) {
        self.user_module_type_vis.insert(qname.to_string(), vis);
    }

    /// Look up visibility for a module-qualified type name.
    ///
    /// Returns `Some(vis)` if this is a user module type, `None` otherwise.
    pub(crate) fn lookup_user_module_type_vis(
        &self,
        qname: &str,
    ) -> Option<Visibility> {
        self.user_module_type_vis.get(qname).copied()
    }

    /// Get all public members of a user module.
    ///
    /// Returns `(name, scheme)` pairs for all public members.
    pub(crate) fn get_public_user_module_members(
        &self,
        mod_path: &str,
    ) -> Vec<(String, Scheme)> {
        self.user_module_members
            .get(mod_path)
            .map(|members| {
                members
                    .iter()
                    .filter(|(_, m)| m.vis == Visibility::Public)
                    .map(|(name, m)| (name.clone(), m.scheme.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all public types in a user module.
    ///
    /// Returns `(local_name, qualified_name)` pairs for direct children only.
    pub(crate) fn get_public_user_module_types(
        &self,
        mod_path: &str,
    ) -> Vec<(String, String)> {
        let prefix = format!("{}.", mod_path);
        self.user_module_type_vis
            .iter()
            .filter_map(|(qname, vis)| {
                qname
                    .strip_prefix(&prefix)
                    .filter(|local| {
                        !local.contains('.') && *vis == Visibility::Public
                    })
                    .map(|local| (local.to_string(), qname.clone()))
            })
            .collect()
    }

    /// Register an imported type alias.
    ///
    /// Maps a local (unqualified) name to its qualified name. Used when
    /// processing `IMPORT M.{ MyType }`.
    pub(crate) fn import_type(&mut self, local: &str, qualified: &str) {
        self.imported_types
            .insert(local.to_string(), qualified.to_string());
    }

    /// Look up an imported type by its local name.
    ///
    /// Returns the qualified name if this type was imported.
    pub(crate) fn lookup_imported_type(&self, local: &str) -> Option<&str> {
        self.imported_types.get(local).map(String::as_str)
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
    pub(crate) fn mark_module_imported(&mut self, module: &str) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.imported_modules.insert(module.to_string());
        }
    }

    /// Check if a module has been imported in any enclosing scope.
    pub(crate) fn is_module_imported(&self, module: &str) -> bool {
        self.scopes
            .iter()
            .rev()
            .any(|s| s.imported_modules.contains(module))
    }

    /// Bind a name to a type scheme in the current scope.
    pub(crate) fn bind(&mut self, name: &str, scheme: Scheme) {
        let id = self.strings.intern(name);
        if let Some(scope) = self.scopes.last_mut() {
            scope.bindings.insert(id, scheme);
        }
    }

    /// Look up a name, searching from innermost to outermost scope.
    pub(crate) fn lookup(&self, name: &str) -> Option<&Scheme> {
        self.strings.lookup(name).and_then(|id| {
            self.scopes
                .iter()
                .rev()
                .find_map(|scope| scope.bindings.get(&id))
        })
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
        env.bind("x", Scheme::mono(TyArena::INT));
        assert_eq!(env.lookup("x"), Some(&Scheme::mono(TyArena::INT)));
        assert_eq!(env.lookup("y"), None);
    }

    #[test]
    fn shadowing() {
        let mut arena = TyArena::new();
        let mut env = TypeEnv::new(StringInterner::new(), &mut arena);
        env.bind("x", Scheme::mono(TyArena::INT));
        env.push_scope();
        env.bind("x", Scheme::mono(TyArena::STRING));
        assert_eq!(env.lookup("x"), Some(&Scheme::mono(TyArena::STRING)));
        env.pop_scope();
        assert_eq!(env.lookup("x"), Some(&Scheme::mono(TyArena::INT)));
    }

    #[test]
    fn inner_scope_sees_outer() {
        let mut arena = TyArena::new();
        let mut env = TypeEnv::new(StringInterner::new(), &mut arena);
        env.bind("x", Scheme::mono(TyArena::INT));
        env.push_scope();
        env.bind("y", Scheme::mono(TyArena::STRING));
        assert_eq!(env.lookup("x"), Some(&Scheme::mono(TyArena::INT)));
        assert_eq!(env.lookup("y"), Some(&Scheme::mono(TyArena::STRING)));
    }

    #[test]
    fn free_vars_collects_all() {
        let mut arena = TyArena::new();
        let mut env = TypeEnv::new(StringInterner::new(), &mut arena);
        let a = TyVar::new(0);
        let b = TyVar::new(1);
        let va = arena.var(0);
        let vb = arena.var(1);
        env.bind("x", Scheme::mono(va));
        env.push_scope();
        env.bind("y", Scheme::mono(vb));
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
        env.bind("existing", Scheme::mono(va));
        // `b` is free in ty but not in env; `a` is in both
        let ty = arena.func(smallvec::smallvec![va], vb);
        let scheme = env.generalize(ty, &arena);
        assert!(!scheme.vars.contains(&a)); // `a` in env, not generalized
        assert!(scheme.vars.contains(&b)); // `b` free, generalized
    }
}
