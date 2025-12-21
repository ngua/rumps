//! Variable environments for lexical scope and primitive functions.
//!
//! The `Environment` tracks lexical scope for `LET` bindings and callable names.
//! `SET` variables (both local and global) go through the `Database`, not here.

#![allow(dead_code)]

use std::collections::HashMap;

use futures::future::BoxFuture;
use smallvec::SmallVec;

use crate::value::{StringId, ValueArena, ValueId};
use crate::Result;

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
/// Contains references to the value arena for creating/looking up values
/// and the type expression arena for constructing type annotations.
pub(crate) struct PrimCtx<'a> {
    pub(crate) arena: &'a mut ValueArena,
    pub(crate) type_exprs: &'a mut crate::value::TypeExprArena,
}

/// A built-in primitive function.
///
/// Primitives are callable built-in functions like `MAP`, `FILTER`, `REDUCE`,
/// etc. They take a context and arguments, returning a future that resolves
/// to a `ValueId`.
///
/// Note: `GET`/`SET`/`KILL` are keywords with special syntax, so they are
/// AST constructs (`Expr::Get`, `Stmt::Set`, `Stmt::Kill`), not primitives.
pub(crate) type PrimFn =
    for<'a> fn(&'a mut PrimCtx<'a>, SmallVec<[ValueId; 4]>) -> PrimResult<'a>;

/// Variable environment for the interpreter.
///
/// Tracks:
/// - Lexical scopes for `LET` bindings (via `Scopes`)
/// - Built-in primitive functions (e.g., `MAP`, `FILTER`, `REDUCE`)
///
/// Note: `SET` variables (both local and global) are stored in the `Database`,
/// not in the environment. Only `LET` bindings live here. You can `GET` a `SET`
/// (local or global), but not a `LET`; `LET`s can be referenced by name directly.
pub(crate) struct Environment {
    /// Lexical scope stack for `LET` bindings.
    pub(crate) scopes: Scopes,

    /// Built-in primitive functions.
    primitives: HashMap<String, PrimFn>,
}

impl Default for Environment {
    fn default() -> Self {
        Self::new()
    }
}

impl Environment {
    /// Create a new environment with built-in primitives registered.
    pub(crate) fn new() -> Self {
        let mut env = Self {
            scopes: Scopes::new(),
            primitives: HashMap::new(),
        };
        env.register_builtins();
        env
    }

    /// Look up a primitive function by name (case-insensitive).
    pub(crate) fn get_primitive(&self, name: &str) -> Option<&PrimFn> {
        self.primitives.get(&name.to_ascii_uppercase())
    }

    /// Register a primitive function (stored uppercase for case-insensitive lookup).
    pub(crate) fn register_primitive(&mut self, name: &str, f: PrimFn) {
        self.primitives.insert(name.to_ascii_uppercase(), f);
    }

    /// Register built-in primitive functions.
    ///
    /// Primitives are callable built-in functions like `MAP`, `FILTER`, `REDUCE`.
    /// They are looked up case-insensitively, like keywords.
    fn register_builtins(&mut self) {
        use crate::primitives::Prim;

        // Object conversion primitives
        self.register_primitive("KEYS", Prim::keys);
        self.register_primitive("VALUES", Prim::values);
        self.register_primitive("ENTRIES", Prim::entries);
        self.register_primitive("FROM-ENTRIES", Prim::from_entries);

        // TODO: Register collection primitives:
        // - MAP(fn, array) -> array
        // - FILTER(predicate, array) -> array
        // - REDUCE(reducer, init, array) -> value
        // - FOLD, TAKE, DROP, etc.
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
        // No primitives registered yet
        assert!(env.get_primitive("MAP").is_none());
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
}
