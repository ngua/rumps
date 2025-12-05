//! Type environment for type checking.
//!
//! Manages scoped bindings from variable names to type schemes.

use std::collections::{HashMap, HashSet};

use smallvec::SmallVec;

use super::ty::{ClassDef, ClassRegistry, Scheme, TyArena, TyId, TyVar};
use super::uf::UnionFind;
use crate::ast::Visibility;
use crate::intern::{QualifiedName, StringId, StringInterner};
use crate::ClassId;

/// A module member entry with type scheme and visibility.
#[derive(Clone, Debug)]
pub(crate) struct ModuleMember {
    pub(crate) scheme: Scheme,
    pub(crate) vis: Visibility,
    pub(crate) method_origin: Option<MethodRefOrigin>,
}

/// Origin metadata for a let-bound class method value.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MethodRefOrigin {
    pub(crate) class: ClassId,
    pub(crate) class_arg: Option<TyId>,
    pub(crate) applied: usize,
    pub(crate) method: StringId,
}

/// A single scope in the type environment.
#[derive(Clone, Debug, Default)]
struct Scope {
    bindings: HashMap<StringId, Scheme>,
    method_refs: HashMap<StringId, MethodRefOrigin>,
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
    /// Class registry (indexed by `ClassId`).
    pub(super) class_registry: ClassRegistry,
    /// User-defined module names registered during typechecking.
    user_modules: HashSet<QualifiedName>,
    /// User module member types and visibility: `module_path -> member_name -> ModuleMember`.
    user_module_members:
        HashMap<QualifiedName, HashMap<StringId, ModuleMember>>,
    /// User module type visibility: qualified type name (e.g., `Mod.Type`) -> visibility.
    ///
    /// Used to enforce visibility for `variant`, `newtype`, `union` inside modules.
    user_module_type_vis: HashMap<QualifiedName, Visibility>,
    /// Imported type aliases: unqualified name -> qualified name.
    ///
    /// When `import M.{ MyType }` is processed, maps `"MyType"` -> `"M.MyType"`.
    /// Checked first during type name resolution.
    imported_types: HashMap<StringId, QualifiedName>,
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
        let class_registry =
            ClassRegistry::builtins(&mut |s| strings.intern(s), arena);
        Self {
            scopes: vec![Scope::default()],
            strings,
            class_registry,
            user_modules: HashSet::new(),
            user_module_members: HashMap::new(),
            user_module_type_vis: HashMap::new(),
            imported_types: HashMap::new(),
        }
    }

    /// Look up a class definition by id.
    pub(crate) fn class_def(&self, id: ClassId) -> &ClassDef {
        self.class_registry.get(id)
    }

    /// Access the class registry.
    pub(crate) fn class_registry(&self) -> &ClassRegistry {
        &self.class_registry
    }

    /// Register a user-defined module name.
    ///
    /// This tracks that a module with this name has been defined so that
    /// paths like `ModuleName.fn` can be resolved.
    pub(crate) fn register_user_module(&mut self, name: QualifiedName) {
        self.user_modules.insert(name);
    }

    /// Check if a name is a registered user module.
    pub(crate) fn is_user_module(&self, name: &QualifiedName) -> bool {
        self.user_modules.contains(name)
    }

    /// Register a member (function or constant) of a user module.
    ///
    /// Called when typechecking `fun` and `let` inside a `module` block.
    pub(crate) fn register_user_module_member(
        &mut self,
        module: QualifiedName,
        member: StringId,
        scheme: Scheme,
        vis: Visibility,
    ) {
        self.user_module_members.entry(module).or_default().insert(
            member,
            ModuleMember {
                scheme,
                vis,
                method_origin: None,
            },
        );
    }

    pub(crate) fn set_user_module_member_method_origin(
        &mut self,
        module: &QualifiedName,
        member: StringId,
        origin: MethodRefOrigin,
    ) {
        if let Some(member) = self
            .user_module_members
            .get_mut(module)
            .and_then(|members| members.get_mut(&member))
        {
            member.method_origin = Some(origin);
        }
    }

    /// Look up a user module member by module path and member name.
    ///
    /// Returns the member (scheme + visibility) if found.
    pub(crate) fn lookup_user_module_member(
        &self,
        module: &QualifiedName,
        member: StringId,
    ) -> Option<&ModuleMember> {
        self.user_module_members
            .get(module)
            .and_then(|m| m.get(&member))
    }

    /// Register visibility for a type inside a user module.
    ///
    /// Called for `variant`, `newtype`, `union` inside `module` blocks.
    pub(crate) fn register_user_module_type_vis(
        &mut self,
        qname: QualifiedName,
        vis: Visibility,
    ) {
        self.user_module_type_vis.insert(qname, vis);
    }

    /// Look up visibility for a module-qualified type name.
    ///
    /// Returns `Some(vis)` if this is a user module type, `None` otherwise.
    pub(crate) fn lookup_user_module_type_vis(
        &self,
        qname: &QualifiedName,
    ) -> Option<Visibility> {
        self.user_module_type_vis.get(qname).copied()
    }

    /// Get all public members of a user module.
    ///
    /// Returns `(name_id, scheme)` pairs for all public members.
    pub(crate) fn get_public_user_module_members(
        &self,
        mod_path: &QualifiedName,
    ) -> Vec<(StringId, Scheme, Option<MethodRefOrigin>)> {
        self.user_module_members
            .get(mod_path)
            .map(|members| {
                members
                    .iter()
                    .filter(|(_, m)| m.vis == Visibility::Public)
                    .map(|(&name, m)| (name, m.scheme.clone(), m.method_origin))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Look up the visibility of a specific module member.
    pub(crate) fn module_member_vis(
        &self,
        mod_path: &QualifiedName,
        member: StringId,
    ) -> Option<Visibility> {
        self.user_module_members
            .get(mod_path)
            .and_then(|members| members.get(&member))
            .map(|m| m.vis)
    }

    /// Get all public types in a user module.
    ///
    /// Returns `(local_name, qualified_name)` pairs for direct children only.
    pub(crate) fn get_public_user_module_types(
        &self,
        mod_path: &QualifiedName,
    ) -> Vec<(StringId, QualifiedName)> {
        self.user_module_type_vis
            .iter()
            .filter(|(qn, &vis)| {
                qn.is_direct_child_of(mod_path) && vis == Visibility::Public
            })
            .map(|(qn, _)| (qn.local_name(), qn.clone()))
            .collect()
    }

    /// Register an imported type alias.
    ///
    /// Maps a local (unqualified) name to its qualified name. Used when
    /// processing `import M.{ MyType }`.
    pub(crate) fn import_type(
        &mut self,
        local: StringId,
        qualified: QualifiedName,
    ) {
        self.imported_types.insert(local, qualified);
    }

    /// Look up an imported type by its local `StringId`.
    ///
    /// Returns the qualified `QualifiedName` if this type was imported.
    pub(crate) fn lookup_imported_type(
        &self,
        local: StringId,
    ) -> Option<&QualifiedName> {
        self.imported_types.get(&local)
    }

    /// Check if a qualified type is imported under any local name.
    pub(crate) fn imports_type(&self, qn: &QualifiedName) -> bool {
        self.imported_types.values().any(|imported| imported == qn)
    }

    /// Push a new scope (e.g., entering a function body or block).
    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
    }

    /// Pop the current scope (e.g., leaving a function body or block).
    pub(crate) fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    pub(crate) fn scope_depth(&self) -> usize {
        self.scopes.len()
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
            scope.method_refs.remove(&id);
        }
    }

    /// Mark an existing binding as a class method value.
    pub(crate) fn bind_method_ref_origin(
        &mut self,
        id: StringId,
        origin: MethodRefOrigin,
    ) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.method_refs.insert(id, origin);
        }
    }

    /// Look up by `StringId`, searching from innermost to outermost scope.
    pub(crate) fn lookup(&self, id: StringId) -> Option<&Scheme> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.bindings.get(&id))
    }

    /// Look up method-reference origin metadata by `StringId`.
    pub(crate) fn lookup_method_ref_origin(
        &self,
        id: StringId,
    ) -> Option<MethodRefOrigin> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.method_refs.get(&id).copied())
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
    /// Chases through UF bindings.
    pub(crate) fn free_vars(
        &self,
        arena: &TyArena,
        uf: &mut UnionFind,
    ) -> HashSet<TyVar> {
        self.scopes
            .iter()
            .flat_map(|scope| scope.bindings.values())
            .flat_map(|scheme| scheme.free_vars(arena, uf))
            .collect()
    }

    /// Generalize a type over variables not free in the environment.
    ///
    /// Creates a polymorphic scheme by quantifying over type variables that
    /// are free in `ty` but not in any existing binding. Chases through UF
    /// bindings so that bound variables are not quantified.
    ///
    /// Note: This produces a scheme with no constraints. For user-defined
    /// functions, constraints are added separately in `InferCtx::fun`. This
    /// means closures with constrained type params will only be checked at
    /// definition time, not at call sites.
    pub(crate) fn generalize(
        &self,
        ty: TyId,
        arena: &TyArena,
        uf: &mut UnionFind,
    ) -> Scheme {
        let env_fv = self.free_vars(arena, uf);
        let ty_fv = uf.free_vars(ty, arena);
        let vars: SmallVec<[TyVar; 4]> =
            ty_fv.difference(&env_fv).copied().collect();
        Scheme {
            vars,
            ty,
            constraints: smallvec::SmallVec::new(),
        }
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
        let mut uf = UnionFind::new();
        uf.reserve_through(1);
        let fv = env.free_vars(&arena, &mut uf);
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
        let mut uf = UnionFind::new();
        uf.reserve_through(0);
        let scheme = env.generalize(ty, &arena, &mut uf);
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
        let mut uf = UnionFind::new();
        uf.reserve_through(1);
        let scheme = env.generalize(ty, &arena, &mut uf);
        assert!(!scheme.vars.contains(&a)); // `a` in env, not generalized
        assert!(scheme.vars.contains(&b)); // `b` free, generalized
    }
}
