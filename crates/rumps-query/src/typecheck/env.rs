//! Type environment for type checking.
//!
//! Manages scoped bindings from variable names to type schemes.

use std::collections::{HashMap, HashSet};

use super::ty::{Scheme, Subst, Ty, TyVar};
use crate::intern::{StringId, StringInterner};

/// Scoped type environment mapping names to type schemes.
///
/// Uses a stack of scopes for lexical scoping (blocks, functions, etc.).
#[derive(Clone, Debug, Default)]
pub(crate) struct TypeEnv {
    scopes: Vec<HashMap<StringId, Scheme>>,
    strings: StringInterner,
}

impl TypeEnv {
    /// Create an empty environment with one global scope.
    pub(crate) fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
            strings: StringInterner::new(),
        }
    }

    /// Push a new scope (e.g., entering a function body or block).
    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
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

    /// Bind a name to a type scheme in the current scope.
    pub(crate) fn bind(&mut self, name: &str, scheme: Scheme) {
        let id = self.strings.intern(name);
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(id, scheme);
        }
    }

    /// Look up a name, searching from innermost to outermost scope.
    pub(crate) fn lookup(&self, name: &str) -> Option<&Scheme> {
        self.strings.lookup(name).and_then(|id| {
            self.scopes.iter().rev().find_map(|scope| scope.get(&id))
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
            .flat_map(|scope| scope.values())
            .flat_map(|scheme| scheme.free_vars())
            .collect()
    }

    /// Generalize a type over variables not free in the environment.
    ///
    /// Creates a polymorphic scheme by quantifying over type variables that
    /// are free in `ty` but not in any existing binding.
    pub(crate) fn generalize(&self, ty: &Ty) -> Scheme {
        let env_fv = self.free_vars();
        let ty_fv = ty.free_vars();
        let vars: Vec<TyVar> = ty_fv.difference(&env_fv).copied().collect();
        Scheme {
            vars,
            ty: ty.clone(),
        }
    }

    /// Apply a substitution to all schemes in the environment.
    pub(crate) fn apply(&mut self, subst: &Subst) {
        self.scopes.iter_mut().for_each(|scope| {
            scope.values_mut().for_each(|scheme| {
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
        let env = TypeEnv::new();
        assert_eq!(env.scopes.len(), 1);
    }

    #[test]
    fn bind_and_lookup() {
        let mut env = TypeEnv::new();
        env.bind("x", Scheme::mono(Ty::Int));
        assert_eq!(env.lookup("x"), Some(&Scheme::mono(Ty::Int)));
        assert_eq!(env.lookup("y"), None);
    }

    #[test]
    fn shadowing() {
        let mut env = TypeEnv::new();
        env.bind("x", Scheme::mono(Ty::Int));
        env.push_scope();
        env.bind("x", Scheme::mono(Ty::String));
        assert_eq!(env.lookup("x"), Some(&Scheme::mono(Ty::String)));
        env.pop_scope();
        assert_eq!(env.lookup("x"), Some(&Scheme::mono(Ty::Int)));
    }

    #[test]
    fn inner_scope_sees_outer() {
        let mut env = TypeEnv::new();
        env.bind("x", Scheme::mono(Ty::Int));
        env.push_scope();
        env.bind("y", Scheme::mono(Ty::String));
        assert_eq!(env.lookup("x"), Some(&Scheme::mono(Ty::Int)));
        assert_eq!(env.lookup("y"), Some(&Scheme::mono(Ty::String)));
    }

    #[test]
    fn free_vars_collects_all() {
        let mut env = TypeEnv::new();
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
        let env = TypeEnv::new();
        let a = TyVar::new(0);
        let ty = Ty::Array(Box::new(Ty::Var(a)));
        let scheme = env.generalize(&ty);
        assert!(scheme.vars.contains(&a));
    }

    #[test]
    fn generalize_excludes_env_vars() {
        let mut env = TypeEnv::new();
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
