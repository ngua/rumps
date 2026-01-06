//! Type environment for type checking.
//!
//! Manages scoped bindings from variable names to type schemes.

use std::collections::{HashMap, HashSet};

use super::ty::{Scheme, Subst, Ty, TyVar};
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
}

/// Scoped type environment mapping names to type schemes.
///
/// Uses a stack of scopes for lexical scoping (blocks, functions, etc.).
#[derive(Clone, Debug, Default)]
pub(crate) struct TypeEnv {
    scopes: Vec<Scope>,
    pub(super) strings: StringInterner,
    /// User-defined module names registered during typechecking.
    user_modules: HashSet<String>,
    /// User module member types and visibility: `module_path -> member_name -> ModuleMember`.
    user_module_members: HashMap<String, HashMap<String, ModuleMember>>,
    /// User module type visibility: qualified type name (e.g., `Mod.Type`) -> visibility.
    ///
    /// Used to enforce visibility for `TYPE`, `NEWTYPE`, `UNION` inside modules.
    user_module_type_vis: HashMap<String, Visibility>,
}

impl TypeEnv {
    /// Create an empty environment with one global scope.
    ///
    /// The interner should be shared with `TypeRegistry` so `StringId`
    /// lookups are consistent.
    pub(crate) fn new(strings: StringInterner) -> Self {
        Self {
            scopes: vec![Scope::default()],
            strings,
            user_modules: HashSet::new(),
            user_module_members: HashMap::new(),
            user_module_type_vis: HashMap::new(),
        }
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

    /// Push a new scope (e.g., entering a function body or block).
    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
    }

    /// Pop the current scope (e.g., leaving a function body or block).
    ///
    /// # Panics
    ///
    /// Panics if there's only one scope remaining (the global scope).
    pub(crate) fn pop_scope(&mut self) {
        debug_assert!(self.scopes.len() > 1, "cannot pop global scope");
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
    pub(crate) fn free_vars(&self) -> HashSet<TyVar> {
        self.scopes
            .iter()
            .flat_map(|scope| scope.bindings.values())
            .flat_map(|scheme| scheme.free_vars())
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
    pub(crate) fn generalize(&self, ty: &Ty) -> Scheme {
        let env_fv = self.free_vars();
        let ty_fv = ty.free_vars();
        let vars: Vec<TyVar> = ty_fv.difference(&env_fv).copied().collect();
        Scheme {
            vars,
            ty: ty.clone(),
            constraints: smallvec::SmallVec::new(),
        }
    }

    /// Apply a substitution to all schemes in the environment.
    pub(crate) fn apply(&mut self, subst: &Subst) {
        self.scopes.iter_mut().for_each(|scope| {
            scope.bindings.values_mut().for_each(|scheme| {
                *scheme = scheme.apply(subst);
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_has_one_scope() {
        let env = TypeEnv::new(StringInterner::new());
        assert_eq!(env.scopes.len(), 1);
    }

    #[test]
    fn bind_and_lookup() {
        let mut env = TypeEnv::new(StringInterner::new());
        env.bind("x", Scheme::mono(Ty::Int));
        assert_eq!(env.lookup("x"), Some(&Scheme::mono(Ty::Int)));
        assert_eq!(env.lookup("y"), None);
    }

    #[test]
    fn shadowing() {
        let mut env = TypeEnv::new(StringInterner::new());
        env.bind("x", Scheme::mono(Ty::Int));
        env.push_scope();
        env.bind("x", Scheme::mono(Ty::String));
        assert_eq!(env.lookup("x"), Some(&Scheme::mono(Ty::String)));
        env.pop_scope();
        assert_eq!(env.lookup("x"), Some(&Scheme::mono(Ty::Int)));
    }

    #[test]
    fn inner_scope_sees_outer() {
        let mut env = TypeEnv::new(StringInterner::new());
        env.bind("x", Scheme::mono(Ty::Int));
        env.push_scope();
        env.bind("y", Scheme::mono(Ty::String));
        assert_eq!(env.lookup("x"), Some(&Scheme::mono(Ty::Int)));
        assert_eq!(env.lookup("y"), Some(&Scheme::mono(Ty::String)));
    }

    #[test]
    fn free_vars_collects_all() {
        let mut env = TypeEnv::new(StringInterner::new());
        let a = TyVar::new(0);
        let b = TyVar::new(1);
        env.bind("x", Scheme::mono(Ty::Var(a)));
        env.push_scope();
        env.bind("y", Scheme::mono(Ty::Var(b)));
        let fv = env.free_vars();
        assert!(fv.contains(&a));
        assert!(fv.contains(&b));
    }

    #[test]
    fn generalize_no_env_vars() {
        let env = TypeEnv::new(StringInterner::new());
        let a = TyVar::new(0);
        let ty = Ty::Array(Box::new(Ty::Var(a)));
        let scheme = env.generalize(&ty);
        assert!(scheme.vars.contains(&a));
    }

    #[test]
    fn generalize_excludes_env_vars() {
        let mut env = TypeEnv::new(StringInterner::new());
        let a = TyVar::new(0);
        let b = TyVar::new(1);
        env.bind("existing", Scheme::mono(Ty::Var(a)));
        // `b` is free in ty but not in env; `a` is in both
        let ty = Ty::Fn(vec![Ty::Var(a)], Box::new(Ty::Var(b)));
        let scheme = env.generalize(&ty);
        assert!(!scheme.vars.contains(&a)); // `a` in env, not generalized
        assert!(scheme.vars.contains(&b)); // `b` free, generalized
    }
}
