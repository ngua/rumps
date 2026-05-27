//! Name resolution pass: convert type-qualified paths to proper AST nodes.
//!
//! This pass runs after parsing (CST -> AST lowering) and before interpretation.
//! It separates compile-time namespace resolution from runtime operations:
//!
//! - `Option.None` (zero-arity variant) -> `Expr::Variant("Option", "None", [])`
//! - `Option.Some(x)` (variant with args) -> `Expr::Variant("Option", "Some", [x])`
//! - `String.length` (module function) -> `Expr::Path(["String", "length"])`
//! - `Math.pi` (module constant) -> `Expr::Path(["Math", "pi"])`
//! - `obj.field` (runtime field access) -> remains `Expr::Field`
//! - `obj.method(args)` (runtime call) -> remains `Expr::Call`
//!
//! The parser emits generic `Expr::Field` and `Expr::Call` nodes; this pass
//! converts type-qualified names to `Expr::Variant` or `Expr::Path` that
//! the interpreter handles without runtime lookups.
//!
//! Module function calls like `String.length(s)` become `Expr::Call(Path(...), args)`,
//! where the `Path` is evaluated to a `Value::ModuleFn` that can then be called.
//! Module constants like `Math.pi` become `Expr::Path(["Math", "pi"])`, evaluated
//! to the constant value at runtime.
//!
//! # Architecture
//!
//! ```text
//! Tokens -> CST -> AST -> [resolve] -> Interpreter
//!                         ^^^^^^^^^^
//!                         this pass
//! ```

use std::collections::{HashMap, HashSet};

use smallvec::{smallvec, SmallVec};

use crate::ast::{Ast, AstTypeExpr, AstTypeExprId, Expr, ExprId, Stmt, StmtId};
use crate::env::BUILTIN_MODULE_NAMES;
use crate::intern::{QualifiedName, StringId};
use crate::interpreter::instance::RuntimeInstance;
use crate::typecheck::ClassRegistry;
use crate::value::{TypeRegistry, ValueArena};
use crate::{ClassId, StringInterner};

/// Resolved class instance info.
///
/// Populated during resolution; used during hoisting to register functions
/// and in typechecking to validate signatures.
#[derive(Clone, Debug)]
pub(crate) struct ResolvedInstance {
    /// The class being implemented.
    pub(crate) class: ClassId,
    /// The qualified name of the implementing type (e.g., `Point`, `MyModule.Point`).
    pub(crate) type_name: QualifiedName,
    /// Method mappings: `(method_name, generated_fn_name)`.
    pub(crate) methods: Vec<(StringId, StringId)>,
}

/// Map from `StmtId` to resolved instance info.
pub(crate) type InstanceMap = HashMap<StmtId, ResolvedInstance>;

/// Name resolution context.
///
/// Converts type-qualified paths (`Option.None`, `String.length`) to proper
/// AST nodes (`Expr::Variant`, `Expr::Path`) before interpretation.
pub(crate) struct ResolveCtx<'a> {
    ast: &'a mut Ast,
    arena: &'a mut ValueArena,
    registry: &'a TypeRegistry,
    class_registry: &'a ClassRegistry,
    user_modules: HashSet<StringId>,
}

impl<'a> ResolveCtx<'a> {
    pub(crate) fn new(
        ast: &'a mut Ast,
        arena: &'a mut ValueArena,
        registry: &'a TypeRegistry,
        class_registry: &'a ClassRegistry,
    ) -> Self {
        let user_modules = Self::collect_user_modules(ast);
        Self {
            ast,
            arena,
            registry,
            class_registry,
            user_modules,
        }
    }

    /// Run name resolution on the AST.
    ///
    /// Converts:
    /// - `Expr::Field(Var(type), variant)` to `Expr::Variant` for zero-arity variants
    /// - `Expr::Call(Field(Var(type), variant), args)` to `Expr::Variant` for
    ///   variant constructors with arguments
    /// - Module paths (builtin and user-defined) to `Expr::Path`
    ///
    /// Also processes `Stmt::ClassInstance` to validate class names and generate
    /// internal function names. Returns a map of resolved instance info.
    pub(crate) fn resolve(mut self) -> InstanceMap {
        // Collect replacements first to avoid borrowing issues
        let replacements: Vec<(ExprId, Expr)> = self
            .ast
            .expr_ids()
            .filter_map(|id| self.resolve_expr(id).map(|e| (id, e)))
            .collect();

        // Apply replacements
        replacements
            .into_iter()
            .for_each(|(id, expr)| self.ast.set_expr(id, expr));

        // Process class instances
        self.resolve_class_instances()
    }

    /// Collect all user-defined module names from the AST.
    fn collect_user_modules(ast: &Ast) -> HashSet<StringId> {
        fn collect_from_stmt(
            ast: &Ast,
            stmt: &Stmt,
            names: &mut HashSet<StringId>,
        ) {
            if let Stmt::Module { name, body } = stmt {
                names.insert(*name);
                body.iter()
                    .filter_map(|&id| ast.get_stmt(id))
                    .for_each(|s| collect_from_stmt(ast, s, names));
            }
        }

        let mut names = HashSet::new();
        ast.stmt_ids()
            .filter_map(|id| ast.get_stmt(id))
            .for_each(|stmt| collect_from_stmt(ast, stmt, &mut names));
        names
    }

    /// Collect path segments from a chain of `Field` expressions.
    ///
    /// Given `Field(Field(Var("A"), "B"), "C")`, returns `Some(["A", "B", "C"])`.
    fn collect_path_segments(
        &self,
        id: ExprId,
    ) -> Option<SmallVec<[StringId; 4]>> {
        self.ast.get_expr(id).and_then(|e| match e {
            Expr::Var(name) => Some(smallvec![*name]),
            Expr::Field(base_id, field) => self
                .collect_path_segments(*base_id)
                .map(|mut segs: SmallVec<[StringId; 4]>| {
                    segs.push(*field);
                    segs
                }),
            _ => None,
        })
    }

    /// Resolve a single expression, returning `Some(replacement)` if needed.
    fn resolve_expr(&mut self, id: ExprId) -> Option<Expr> {
        self.ast.get_expr(id).cloned().and_then(|expr| match expr {
            // Field access: `Name.field` or `A.B.C` (nested modules/types)
            // - If base is `Var(Type)` with zero-arity variant -> `Variant`
            // - If base path is a qualified type with zero-arity variant -> `Variant`
            // - If full path starts with a module -> `Path([...])`
            // - Otherwise -> leave as Field (runtime field access)
            //
            // Type variants take priority over module paths. This allows
            // `Option.None` and `Result.Err` to work even though `Option` and
            // `Result` are also module names.
            Expr::Field(base_id, field) => {
                self.resolve_field_expr(id, base_id, field)
            }

            // Function calls: resolve variant constructors
            // `Call(Field(Var(Type), Variant), args)` -> `Variant`
            // `Call(Field(Field(...), Variant), args)` -> `Variant` (qualified)
            Expr::Call(callee_id, ref args) => {
                self.resolve_call_expr(callee_id, args)
            }

            _ => None,
        })
    }

    fn resolve_field_expr(
        &mut self,
        id: ExprId,
        base_id: ExprId,
        field: StringId,
    ) -> Option<Expr> {
        // First check if it's a type variant pattern (takes priority).
        let variant = self
            .ast
            .get_expr(base_id)
            .cloned()
            .and_then(|base| match base {
                Expr::Var(name) => self
                    .registry
                    .lookup(&QualifiedName::local(name))
                    .and_then(|type_id| {
                        self.registry
                            .lookup_variant(type_id, field)
                            .map(|v| (QualifiedName::local(name), v.arity))
                    }),
                _ => None,
            })
            .or_else(|| {
                // Check for module-qualified type variant: Module.Type.Variant
                let base_path = self.collect_path_segments(base_id)?;
                let qn = QualifiedName::new(base_path.to_vec());

                self.registry.lookup(&qn).and_then(|type_id| {
                    self.registry
                        .lookup_variant(type_id, field)
                        .map(|v| (qn.clone(), v.arity))
                })
            });

        match variant {
            Some((qn, 0)) => Some(Expr::Variant(qn, field, smallvec![])),
            Some(_) => None,
            None => {
                let full_path = self.collect_path_segments(id);

                let is_module_path =
                    full_path
                        .as_ref()
                        .and_then(|segs: &SmallVec<[StringId; 4]>| segs.first())
                        .is_some_and(|first| {
                            self.arena.strings.get(*first).is_some_and(|s| {
                                BUILTIN_MODULE_NAMES.contains(&s)
                            }) || self.user_modules.contains(first)
                        });

                is_module_path.then(|| full_path.map(Expr::Path)).flatten()
            }
        }
    }

    fn resolve_call_expr(
        &mut self,
        callee_id: ExprId,
        args: &SmallVec<[ExprId; 4]>,
    ) -> Option<Expr> {
        self.ast
            .get_expr(callee_id)
            .cloned()
            .and_then(|callee| match callee {
                Expr::Field(base_id, var_name) => {
                    // First try simple Type.Variant(args) pattern
                    let simple_variant = self
                        .ast
                        .get_expr(base_id)
                        .cloned()
                        .and_then(|base| match base {
                            Expr::Var(ty_name) => self
                                .registry
                                .lookup(&QualifiedName::local(ty_name))
                                .and_then(|type_id| {
                                    self.registry
                                        .lookup_variant(type_id, var_name)
                                        .map(|_| {
                                            Expr::Variant(
                                                QualifiedName::local(ty_name),
                                                var_name,
                                                args.clone(),
                                            )
                                        })
                                }),
                            _ => None,
                        });

                    // Try module-qualified type: Module.Type.Variant(args)
                    simple_variant.or_else(|| {
                        let base_path = self.collect_path_segments(base_id)?;
                        let qn = QualifiedName::new(base_path.to_vec());

                        self.registry.lookup(&qn).and_then(|type_id| {
                            self.registry.lookup_variant(type_id, var_name).map(
                                |_| {
                                    Expr::Variant(
                                        qn.clone(),
                                        var_name,
                                        args.clone(),
                                    )
                                },
                            )
                        })
                    })
                }
                _ => None,
            })
    }

    /// Process all `Stmt::ClassInstance` statements and return resolved info.
    fn resolve_class_instances(&mut self) -> InstanceMap {
        let mut map = InstanceMap::new();
        let ids: Vec<_> = self.ast.stmt_ids().collect();
        self.resolve_class_instances_rec(&ids, None, &mut map);
        map
    }

    fn resolve_class_instances_rec(
        &mut self,
        ids: &[StmtId],
        module: Option<&QualifiedName>,
        map: &mut InstanceMap,
    ) {
        ids.iter().for_each(|&id| {
            self.resolve_class_instance(id, module)
                .into_iter()
                .for_each(|inst| {
                    map.insert(id, inst);
                });

            self.ast
                .get_stmt(id)
                .into_iter()
                .filter_map(|s| match s {
                    Stmt::Module { name, body } => Some((*name, body.clone())),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .into_iter()
                .for_each(|(name, body)| {
                    let mod_path = module.map_or_else(
                        || QualifiedName::local(name),
                        |m| m.child(name),
                    );
                    self.resolve_class_instances_rec(
                        &body,
                        Some(&mod_path),
                        map,
                    );
                });
        });
    }

    fn resolve_class_instance(
        &mut self,
        id: StmtId,
        module: Option<&QualifiedName>,
    ) -> Option<ResolvedInstance> {
        let stmt = self.ast.get_stmt(id)?;

        match stmt {
            Stmt::ClassInstance {
                class_name,
                class_args,
                for_type,
                methods,
                ..
            } => {
                let class = self.class_registry.lookup_by_name(*class_name)?;
                let raw_qn = Self::extract_type_qn(
                    self.ast,
                    &mut self.arena.strings,
                    *for_type,
                )?;

                let type_qn = match module {
                    Some(m) if !raw_qn.is_qualified() => {
                        m.child(raw_qn.local_name())
                    }
                    _ => raw_qn,
                };
                let fn_type_name = match self.ast.get_type_expr(*for_type) {
                    Some(AstTypeExpr::TupleConstructor { arity, .. }) => {
                        format!("Tuple{arity}")
                    }
                    _ => type_qn.display(&self.arena.strings),
                };
                let class_name_str = self
                    .arena
                    .strings
                    .get(self.class_registry.name(class))
                    .unwrap_or_default()
                    .to_owned();
                let ca_names: Vec<String> = class_args
                    .iter()
                    .filter_map(|id| Self::extract_type_qn_named(self.ast, *id))
                    .map(|qn| qn.display(&self.arena.strings))
                    .collect();
                let methods = methods.clone();
                let mappings: Vec<(StringId, StringId)> = methods
                    .iter()
                    .map(|m| {
                        let mn =
                            self.arena.strings.get(m.name).unwrap_or_default();
                        let fn_name = RuntimeInstance::fn_name_owned(
                            &class_name_str,
                            &fn_type_name,
                            mn,
                            &ca_names,
                        );
                        let fn_id = self.arena.strings.intern(&fn_name);
                        (m.name, fn_id)
                    })
                    .collect();

                Some(ResolvedInstance {
                    class,
                    type_name: type_qn,
                    methods: mappings,
                })
            }
            _ => None,
        }
    }

    /// Extract the type name from an `AstTypeExpr`, including tuple ctors.
    fn extract_type_qn(
        ast: &Ast,
        strings: &mut StringInterner,
        id: AstTypeExprId,
    ) -> Option<QualifiedName> {
        ast.get_type_expr(id).and_then(|te| match te {
            AstTypeExpr::Named(name) | AstTypeExpr::App(name, _) => {
                Some(name.clone())
            }
            AstTypeExpr::TupleConstructor { .. } => {
                Some(QualifiedName::local(strings.intern("Tuple")))
            }
            _ => None,
        })
    }

    fn extract_type_qn_named(
        ast: &Ast,
        id: AstTypeExprId,
    ) -> Option<QualifiedName> {
        ast.get_type_expr(id).and_then(|te| match te {
            AstTypeExpr::Named(name) | AstTypeExpr::App(name, _) => {
                Some(name.clone())
            }
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;
    use crate::typecheck::TyArena;
    fn parse_and_resolve(src: &str) -> (Ast, ValueArena) {
        let mut interner = StringInterner::new();
        let mut result =
            Parser::parse(src, &mut interner).expect("parse failed");
        let mut arena = ValueArena::with_interner(interner);
        let registry = TypeRegistry::new(&mut arena);
        let class_registry = {
            let mut tmp = TyArena::new();
            ClassRegistry::builtins(&mut |s| arena.strings.intern(s), &mut tmp)
        };
        let _ = ResolveCtx::new(
            &mut result.ast,
            &mut arena,
            &registry,
            &class_registry,
        )
        .resolve();
        (result.ast, arena)
    }

    #[test]
    fn resolve_option_none() {
        let (ast, arena) = parse_and_resolve("let x = Option.None");
        let has_variant = ast.expr_ids().any(|id| match ast.get_expr(id) {
            Some(Expr::Variant(ty, var, args)) => {
                arena.strings.get(ty.local_name()) == Some("Option")
                    && arena.strings.get(*var) == Some("None")
                    && args.is_empty()
            }
            _ => false,
        });
        assert!(
            has_variant,
            "Option.None should become Variant with no args"
        );
    }

    #[test]
    fn resolve_option_some_becomes_variant() {
        let (ast, arena) = parse_and_resolve("let x = Option.Some(42)");
        let has_variant = ast.expr_ids().any(|id| match ast.get_expr(id) {
            Some(Expr::Variant(ty, var, _)) => {
                arena.strings.get(ty.local_name()) == Some("Option")
                    && arena.strings.get(*var) == Some("Some")
            }
            _ => false,
        });
        assert!(has_variant, "Option.Some(42) should become Variant");
    }

    #[test]
    fn resolve_result_ok_becomes_variant() {
        let (ast, arena) = parse_and_resolve("let x = Result.Ok(42)");
        let has_variant = ast.expr_ids().any(|id| match ast.get_expr(id) {
            Some(Expr::Variant(ty, var, _)) => {
                arena.strings.get(ty.local_name()) == Some("Result")
                    && arena.strings.get(*var) == Some("Ok")
            }
            _ => false,
        });
        assert!(has_variant, "Result.Ok(42) should become Variant");
    }

    #[test]
    fn resolve_result_err_becomes_variant() {
        let (ast, arena) = parse_and_resolve("let x = Result.Err(\"oops\")");
        let has_variant = ast.expr_ids().any(|id| match ast.get_expr(id) {
            Some(Expr::Variant(ty, var, _)) => {
                arena.strings.get(ty.local_name()) == Some("Result")
                    && arena.strings.get(*var) == Some("Err")
            }
            _ => false,
        });
        assert!(has_variant, "Result.Err(\"oops\") should become Variant");
    }

    #[test]
    fn resolve_field_access_not_converted() {
        let (ast, arena) =
            parse_and_resolve("let obj = { x: 1 }\nlet y = obj.x");
        // obj.x should remain as Field, not become Variant
        let has_variant_obj_x =
            ast.expr_ids().any(|id| match ast.get_expr(id) {
                Some(Expr::Variant(ty, var, _)) => {
                    arena.strings.get(ty.local_name()) == Some("obj")
                        && arena.strings.get(*var) == Some("x")
                }
                _ => false,
            });
        assert!(!has_variant_obj_x, "obj.x should not become Variant");
    }

    #[test]
    fn resolve_unknown_type_not_converted() {
        let (ast, arena) = parse_and_resolve("let x = Unknown.Foo");
        // Unknown.Foo should remain as Field since Unknown is not a registered type
        let has_variant = ast.expr_ids().any(|id| match ast.get_expr(id) {
            Some(Expr::Variant(ty, _, _)) => {
                arena.strings.get(ty.local_name()) == Some("Unknown")
            }
            _ => false,
        });
        assert!(!has_variant, "Unknown.Foo should not become Variant");
    }

    #[test]
    fn resolve_array_push_becomes_path() {
        let (ast, arena) = parse_and_resolve("let r = Array.push([1, 2], 3)");
        let has_path = ast.expr_ids().any(|id| match ast.get_expr(id) {
            Some(Expr::Path(segs)) => {
                segs.len() == 2
                    && arena.strings.get(segs[0]) == Some("Array")
                    && arena.strings.get(segs[1]) == Some("push")
            }
            _ => false,
        });
        assert!(has_path, "Array.push should become Path([Array, push])");
    }

    #[test]
    fn resolve_module_fn_without_call() {
        // Module function used as value (e.g., for pipeline)
        let (ast, arena) = parse_and_resolve("let f = String.length");
        let has_path = ast.expr_ids().any(|id| match ast.get_expr(id) {
            Some(Expr::Path(segs)) => {
                segs.len() == 2
                    && arena.strings.get(segs[0]) == Some("String")
                    && arena.strings.get(segs[1]) == Some("length")
            }
            _ => false,
        });
        assert!(has_path, "String.length (no call) should become Path");
    }

    #[test]
    fn resolve_unknown_module_not_converted() {
        let (ast, arena) = parse_and_resolve("let x = Foo.bar(1)");
        // Foo.bar should remain as Field since Foo is not a known module
        let has_path = ast.expr_ids().any(|id| match ast.get_expr(id) {
            Some(Expr::Path(segs)) => segs
                .first()
                .and_then(|s| arena.strings.get(*s))
                .is_some_and(|s| s == "Foo"),
            _ => false,
        });
        assert!(!has_path, "Foo.bar should not become Path");
    }
}
