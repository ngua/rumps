//! Variable environments for lexical scope and primitive functions.
//!
//! The `Environment` tracks lexical scope for `LET` bindings and callable names.
//! `SET` variables (both local and global) go through the `Database`, not here.

#![allow(dead_code)]

use std::collections::HashMap;

/// Names of built-in modules.
///
/// This is the single source of truth for which module names are recognized
/// during resolution and registered at interpreter startup.
pub(crate) const BUILTIN_MODULE_NAMES: &[&str] = &["Object", "Array"];

use futures::future::BoxFuture;
use smallvec::{smallvec, SmallVec};

use crate::value::{StringId, TypeId, Value, ValueArena, ValueId};
use crate::{Result, Span};

/// Stack of lexical scopes for `LET` bindings.
///
/// Each frame is a `HashMap` mapping variable names to values.
/// The stack grows downward; the top (last) frame is the current scope.
pub(crate) struct Scopes {
    stack: Vec<HashMap<StringId, ValueId>>,
}

impl Default for Scopes {
    fn default() -> Self {
        Self::new()
    }
}

impl Scopes {
    /// Create a new scope stack with a single empty frame.
    pub(crate) fn new() -> Self {
        Self {
            stack: vec![HashMap::new()],
        }
    }

    /// Bind a name to a value in the current (top) frame.
    pub(crate) fn bind(&mut self, name: StringId, val: ValueId) {
        // Note that there's always a root stack frame (i.e. stack is never
        // empty), we could `expect` or `unwrap` here, but `map` is fine IMO
        self.stack.last_mut().map(|frame| frame.insert(name, val));
    }

    /// Look up a name, searching from the top frame down.
    ///
    /// Returns the first binding found, or `None` if not bound.
    pub(crate) fn lookup(&self, name: StringId) -> Option<ValueId> {
        self.stack
            .iter()
            .rev()
            .find_map(|frame| frame.get(&name).copied())
    }

    /// Push a new empty frame onto the stack (enter nested scope).
    pub(crate) fn push(&mut self) {
        self.stack.push(HashMap::new());
    }

    /// Pop the top frame from the stack (exit scope).
    ///
    /// Returns the popped frame, or `None` if only one frame remains
    /// (we never pop the root frame).
    pub(crate) fn pop(&mut self) -> Option<HashMap<StringId, ValueId>> {
        (self.stack.len() > 1).then(|| self.stack.pop()).flatten()
    }

    /// Current depth of the scope stack.
    fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Get a reference to the scope stack (for closure capture).
    pub(crate) fn stack(&self) -> &[HashMap<StringId, ValueId>] {
        &self.stack
    }

    /// Save the current scope stack (for closure calls).
    ///
    /// Returns the saved stack, leaving the current stack with a single empty frame.
    pub(crate) fn save(&mut self) -> Vec<HashMap<StringId, ValueId>> {
        std::mem::replace(&mut self.stack, vec![HashMap::new()])
    }

    /// Restore a previously saved scope stack.
    pub(crate) fn restore(&mut self, saved: Vec<HashMap<StringId, ValueId>>) {
        self.stack = saved;
    }

    /// Restore scope from a captured environment (for closure calls).
    ///
    /// Creates a fresh scope stack with the captured bindings.
    pub(crate) fn restore_from_captured(
        &mut self,
        env: &crate::value::CapturedEnv,
    ) {
        let frame = env.bindings().iter().map(|(k, v)| (*k, *v)).collect();
        self.stack = vec![frame];
    }
}

/// Result type for primitive function execution.
pub(crate) type PrimResult<'a> = BoxFuture<'a, Result<ValueId>>;

/// Context passed to primitive functions during execution.
///
/// Contains references to the value arena for creating/looking up values,
/// the type expression arena for constructing type annotations, and the
/// call-site span for error reporting.
pub(crate) struct PrimCtx<'a> {
    pub(crate) arena: &'a mut ValueArena,
    pub(crate) type_exprs: &'a mut crate::value::TypeExprArena,
    pub(crate) span: Span,
}

impl PrimCtx<'_> {
    /// Create a `Result.Ok(v)` value from a `ValueId` already in the arena.
    pub(crate) fn result_ok(&mut self, v: ValueId) -> ValueId {
        let unknown = self.type_exprs.named(TypeId::UNKNOWN);
        let val = self.arena.get(v).cloned().unwrap_or(Value::Int(0));
        let val_ty = self.type_exprs.named(val.base_type());
        let res_ty = self
            .type_exprs
            .app(TypeId::RESULT, smallvec![val_ty, unknown]);
        let ok = Value::ok(res_ty, v);
        self.arena.add(ok, self.span)
    }

    /// Create a `Result.Err(msg)` value from a `ValueId` already in the arena.
    pub(crate) fn result_err(&mut self, msg: ValueId) -> ValueId {
        let unknown = self.type_exprs.named(TypeId::UNKNOWN);
        let str_ty = self.type_exprs.named(TypeId::STRING);
        let res_ty = self
            .type_exprs
            .app(TypeId::RESULT, smallvec![unknown, str_ty]);
        let err = Value::err(res_ty, msg);
        self.arena.add(err, self.span)
    }
}

/// A built-in primitive function.
///
/// Primitives are callable built-in functions like `Object.keys`, `Array.map`,
/// etc. They take a context and arguments, returning a future that resolves
/// to a `ValueId`.
///
/// Note: `GET`/`SET`/`KILL` are keywords with special syntax, so they are
/// AST constructs (`Expr::Get`, `Stmt::Set`, `Stmt::Kill`), not primitives.
pub(crate) type PrimFn =
    for<'a> fn(&'a mut PrimCtx<'a>, SmallVec<[ValueId; 4]>) -> PrimResult<'a>;

/// A built-in module containing primitive functions and submodules.
///
/// Modules group related functions under a namespace (e.g., `Object.keys`,
/// `Array.map`). Supports nested modules for future extensibility
/// (e.g., `Math.Trig.sin`).
///
/// Built-in modules are registered at interpreter startup; user-defined
/// modules will be supported in a future phase.
#[derive(Default)]
pub(crate) struct Module {
    /// Functions in this module, keyed by function name.
    functions: HashMap<String, PrimFn>,

    /// Submodules, keyed by submodule name.
    submodules: HashMap<String, Self>,
}

impl Module {
    /// Register a function in this module.
    fn register(&mut self, name: &str, f: PrimFn) {
        self.functions.insert(name.to_string(), f);
    }

    /// Register a submodule.
    #[allow(dead_code)]
    fn register_submodule(&mut self, name: &str, m: Self) {
        self.submodules.insert(name.to_string(), m);
    }

    /// Look up a function by path within this module.
    ///
    /// For a single-segment path, looks up the function directly.
    /// For multi-segment paths, traverses submodules.
    pub(crate) fn get_fn(&self, path: &[&str]) -> Option<&PrimFn> {
        match path {
            [] => None,
            [name] => self.functions.get(*name),
            [first, rest @ ..] => {
                self.submodules.get(*first).and_then(|m| m.get_fn(rest))
            }
        }
    }

    /// Check if a path resolves to a function within this module.
    pub(crate) fn contains_path(&self, path: &[&str]) -> bool {
        self.get_fn(path).is_some()
    }
}

/// Variable environment for the interpreter.
///
/// Tracks:
/// - Lexical scopes for `LET` bindings (via `Scopes`)
/// - Built-in modules containing primitive functions (e.g., `Object`, `Array`)
///
/// Note: `SET` variables (both local and global) are stored in the `Database`,
/// not in the environment. Only `LET` bindings live here. You can `GET` a `SET`
/// (local or global), but not a `LET`; `LET`s can be referenced by name directly.
pub(crate) struct Environment {
    /// Lexical scope stack for `LET` bindings.
    pub(crate) scopes: Scopes,

    /// Built-in modules (e.g., `Object`, `Array`).
    modules: HashMap<String, Module>,
}

impl Default for Environment {
    fn default() -> Self {
        Self::new()
    }
}

impl Environment {
    /// Create a new environment with built-in modules registered.
    pub(crate) fn new() -> Self {
        let mut env = Self {
            scopes: Scopes::new(),
            modules: HashMap::new(),
        };
        env.register_builtins();
        env
    }

    /// Check if a top-level module exists.
    pub(crate) fn has_module(&self, name: &str) -> bool {
        self.modules.contains_key(name)
    }

    /// Check if a path resolves to a module function.
    ///
    /// The path must have at least two segments: the first is the module name,
    /// and the remaining segments form the path within that module.
    pub(crate) fn module_fn_exists(&self, path: &[&str]) -> bool {
        path.split_first().is_some_and(|(module, rest)| {
            self.modules
                .get(*module)
                .is_some_and(|m| m.contains_path(rest))
        })
    }

    /// Look up a function by its full path.
    ///
    /// The path must have at least two segments: the first is the module name,
    /// and the remaining segments form the path within that module.
    ///
    /// Examples:
    /// - `["Object", "keys"]` → `Object.keys`
    /// - `["Math", "Trig", "sin"]` → `Math.Trig.sin`
    pub(crate) fn get_module_fn(&self, path: &[&str]) -> Option<&PrimFn> {
        path.split_first().and_then(|(module, rest)| {
            self.modules.get(*module).and_then(|m| m.get_fn(rest))
        })
    }

    /// Register built-in modules.
    ///
    /// Built-in modules provide primitive functions grouped by category:
    /// - `Object`: `keys`, `values`, `entries`, `from-entries`
    /// - `Array`: `map`, `filter`, `reduce`
    ///
    /// Note: Array functions are higher-order (they invoke closures) and are
    /// handled specially by the interpreter. We register placeholders here so
    /// that `module_fn_exists` returns true for name resolution.
    fn register_builtins(&mut self) {
        use crate::primitives::Prim;

        // Object module
        let mut object = Module::default();
        object.register("keys", Prim::keys);
        object.register("values", Prim::values);
        object.register("entries", Prim::entries);
        object.register("from-entries", Prim::from_entries);
        self.modules.insert("Object".to_string(), object);

        // Array module: placeholder functions for higher-order primitives.
        // These are intercepted in `invoke_module_fn` and handled specially.
        let mut array = Module::default();
        array.register("map", Prim::placeholder);
        array.register("filter", Prim::placeholder);
        array.register("reduce", Prim::placeholder);
        self.modules.insert("Array".to_string(), array);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Span, Value};

    fn setup() -> (ValueArena, Scopes) {
        (ValueArena::new(), Scopes::new())
    }

    #[test]
    fn scope_bind_and_lookup() {
        let (mut arena, mut scopes) = setup();

        let name = arena.intern("x");
        let val = arena.add(Value::Int(42), Span::new(0, 2));

        scopes.bind(name, val);

        assert_eq!(scopes.lookup(name), Some(val));
    }

    #[test]
    fn scope_lookup_not_found() {
        let (mut arena, scopes) = setup();

        let name = arena.intern("undefined");
        assert_eq!(scopes.lookup(name), None);
    }

    #[test]
    fn scope_nested_lookup() {
        let (mut arena, mut scopes) = setup();

        let x = arena.intern("x");
        let outer_val = arena.add(Value::Int(1), Span::new(0, 1));
        scopes.bind(x, outer_val);

        scopes.push();

        // Inner scope can see outer binding
        assert_eq!(scopes.lookup(x), Some(outer_val));

        // Shadow with inner binding
        let inner_val = arena.add(Value::Int(2), Span::new(2, 3));
        scopes.bind(x, inner_val);
        assert_eq!(scopes.lookup(x), Some(inner_val));

        // Pop inner scope; outer binding restored
        scopes.pop();
        assert_eq!(scopes.lookup(x), Some(outer_val));
    }

    #[test]
    fn scope_pop_root_prevented() {
        let (_, mut scopes) = setup();

        assert_eq!(scopes.depth(), 1);
        assert!(scopes.pop().is_none());
        assert_eq!(scopes.depth(), 1);
    }

    #[test]
    fn scope_multiple_bindings() {
        let (mut arena, mut scopes) = setup();

        let x = arena.intern("x");
        let y = arena.intern("y");
        let z = arena.intern("z");

        let v1 = arena.add(Value::Int(1), Span::new(0, 1));
        let v2 = arena.add(Value::Int(2), Span::new(2, 3));
        let v3 = arena.add(Value::Int(3), Span::new(4, 5));

        scopes.bind(x, v1);
        scopes.bind(y, v2);
        scopes.bind(z, v3);

        assert_eq!(scopes.lookup(x), Some(v1));
        assert_eq!(scopes.lookup(y), Some(v2));
        assert_eq!(scopes.lookup(z), Some(v3));
    }

    #[test]
    fn environment_creation() {
        let env = Environment::new();

        assert_eq!(env.scopes.depth(), 1);
        // Object module is registered with functions
        assert!(env.has_module("Object"));
        assert!(env.get_module_fn(&["Object", "keys"]).is_some());
        assert!(env.module_fn_exists(&["Object", "keys"]));
        // Array module is registered with placeholder functions
        assert!(env.has_module("Array"));
        assert!(env.get_module_fn(&["Array", "map"]).is_some());
        assert!(env.module_fn_exists(&["Array", "map"]));
        assert!(env.module_fn_exists(&["Array", "filter"]));
        assert!(env.module_fn_exists(&["Array", "reduce"]));
    }

    #[test]
    fn environment_scope_operations() {
        let mut arena = ValueArena::new();
        let mut env = Environment::new();

        let name = arena.intern("test");
        let val = arena.add(Value::Bool(true), Span::new(0, 4));

        env.scopes.bind(name, val);
        assert_eq!(env.scopes.lookup(name), Some(val));
    }

    // Dummy primitive for testing
    fn dummy_prim<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move { Ok(ctx.arena.add(Value::Int(42), ctx.span)) })
    }

    #[test]
    fn module_submodule_lookup() {
        // Create a module with a submodule: Math.Trig.sin
        let mut trig = Module::default();
        trig.register("sin", dummy_prim);
        trig.register("cos", dummy_prim);

        let mut math = Module::default();
        math.register("sqrt", dummy_prim);
        math.register("abs", dummy_prim);
        math.register_submodule("Trig", trig);

        // Direct function lookup
        assert!(math.get_fn(&["sqrt"]).is_some());
        assert!(math.get_fn(&["abs"]).is_some());
        assert!(math.get_fn(&["unknown"]).is_none());

        // Submodule function lookup
        assert!(math.get_fn(&["Trig", "sin"]).is_some());
        assert!(math.get_fn(&["Trig", "cos"]).is_some());
        assert!(math.get_fn(&["Trig", "tan"]).is_none());

        // contains_path
        assert!(math.contains_path(&["sqrt"]));
        assert!(math.contains_path(&["Trig", "sin"]));
        assert!(!math.contains_path(&["Trig", "tan"]));
        assert!(!math.contains_path(&["Unknown", "fn"]));
    }

    #[test]
    fn module_deeply_nested_lookup() {
        // Create deeply nested: A.B.C.fn
        let mut c = Module::default();
        c.register("fn", dummy_prim);

        let mut b = Module::default();
        b.register_submodule("C", c);

        let mut a = Module::default();
        a.register_submodule("B", b);

        // Should find A.B.C.fn
        assert!(a.get_fn(&["B", "C", "fn"]).is_some());
        assert!(a.contains_path(&["B", "C", "fn"]));

        // Should not find partial paths
        assert!(a.get_fn(&["B"]).is_none());
        assert!(a.get_fn(&["B", "C"]).is_none());

        // Should not find wrong paths
        assert!(a.get_fn(&["B", "C", "other"]).is_none());
        assert!(a.get_fn(&["B", "D", "fn"]).is_none());
    }

    #[test]
    fn environment_module_fn_exists_with_path() {
        let env = Environment::new();

        // Valid paths
        assert!(env.module_fn_exists(&["Object", "keys"]));
        assert!(env.module_fn_exists(&["Object", "values"]));
        assert!(env.module_fn_exists(&["Object", "entries"]));
        assert!(env.module_fn_exists(&["Object", "from-entries"]));

        // Invalid paths
        assert!(!env.module_fn_exists(&["Object", "unknown"]));
        assert!(!env.module_fn_exists(&["Unknown", "keys"]));
        assert!(!env.module_fn_exists(&[]));
        assert!(!env.module_fn_exists(&["Object"]));
    }
}
