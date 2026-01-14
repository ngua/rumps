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

use crate::ast::{Ast, AstTypeExpr, Expr, ExprId, Stmt, StmtId};
use crate::env::BUILTIN_MODULE_NAMES;
use crate::typecheck::ClassKind;
#[cfg(test)]
use crate::value::TypeExprArena;
use crate::value::{TypeRegistry, ValueArena};

/// Check if a name is a known module (builtin or user-defined).
fn is_known_module(name: &str, user_modules: &HashSet<String>) -> bool {
    BUILTIN_MODULE_NAMES.contains(&name) || user_modules.contains(name)
}

/// Resolved class instance info.
///
/// Populated during resolution; used during hoisting to register functions
/// and in typechecking to validate signatures.
#[derive(Clone, Debug)]
pub(crate) struct ResolvedInstance {
    /// The class being implemented.
    pub(crate) class: ClassKind,
    /// The name of the implementing type (e.g., `"Point"`, `"MyModule.Point"`).
    pub(crate) type_name: String,
    /// Method mappings: (method_name, generated_fn_name).
    pub(crate) methods: Vec<(String, String)>,
}

/// Map from `StmtId` to resolved instance info.
pub(crate) type InstanceMap = HashMap<StmtId, ResolvedInstance>;

/// Extract the type name from an `AstTypeExpr`.
///
/// Returns the simple name for `Named` types and the qualified name for
/// parameterized types (e.g., `"Point"` or `"Either"`).
fn extract_type_name(
    ast: &Ast,
    id: crate::ast::AstTypeExprId,
) -> Option<String> {
    ast.get_type_expr(id).and_then(|te| match te {
        AstTypeExpr::Named(name) => Some(name.clone()),
        AstTypeExpr::App(name, _) => Some(name.clone()),
        _ => None,
    })
}

/// Collect all user-defined module names from the AST.
///
/// Recursively finds `Stmt::Module` statements and extracts their names.
fn collect_user_module_names(ast: &Ast) -> HashSet<String> {
    fn collect_from_stmt(ast: &Ast, stmt: &Stmt, names: &mut HashSet<String>) {
        if let Stmt::Module { name, body } = stmt {
            names.insert(name.clone());
            // Also collect nested modules
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
/// Returns `None` if the base is not a `Var` or `Field` chain.
fn collect_path_segments(
    ast: &Ast,
    id: ExprId,
) -> Option<SmallVec<[String; 4]>> {
    ast.get_expr(id).and_then(|e| match e {
        Expr::Var(name) => Some(smallvec![name.clone()]),
        Expr::Field(base_id, field) => collect_path_segments(ast, *base_id)
            .map(|mut segs: SmallVec<[String; 4]>| {
                segs.push(field.clone());
                segs
            }),
        _ => None,
    })
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
pub(crate) fn resolve(
    ast: &mut Ast,
    arena: &mut ValueArena,
    registry: &TypeRegistry,
) -> InstanceMap {
    // First, collect user-defined module names from MODULE statements
    let user_modules = collect_user_module_names(ast);

    // Collect replacements first to avoid borrowing issues
    let replacements: Vec<(ExprId, Expr)> = ast
        .expr_ids()
        .filter_map(|id| {
            resolve_expr(ast, arena, registry, &user_modules, id)
                .map(|e| (id, e))
        })
        .collect();

    // Apply replacements
    replacements
        .into_iter()
        .for_each(|(id, expr)| ast.set_expr(id, expr));

    // Process class instances
    resolve_class_instances(ast)
}

/// Process all `Stmt::ClassInstance` statements and return resolved info.
fn resolve_class_instances(ast: &Ast) -> InstanceMap {
    ast.stmt_ids()
        .filter_map(|id| resolve_class_instance(ast, id).map(|inst| (id, inst)))
        .collect()
}

/// Resolve a single `Stmt::ClassInstance`, generating function names.
fn resolve_class_instance(ast: &Ast, id: StmtId) -> Option<ResolvedInstance> {
    let stmt = ast.get_stmt(id)?;

    match stmt {
        Stmt::ClassInstance {
            class_name,
            for_type,
            methods,
            ..
        } => {
            // Validate class name
            let class = ClassKind::from_str(class_name)?;

            // Extract the implementing type name
            let type_name = extract_type_name(ast, *for_type)?;

            // Generate function names for each method
            let mappings: Vec<(String, String)> = methods
                .iter()
                .map(|m| {
                    let fn_name =
                        crate::interpreter::instance::instance_fn_name(
                            class, &type_name, &m.name,
                        );
                    (m.name.clone(), fn_name)
                })
                .collect();

            Some(ResolvedInstance {
                class,
                type_name,
                methods: mappings,
            })
        }
        _ => None,
    }
}

/// Resolve a single expression, returning `Some(replacement)` if it should
/// be replaced.
fn resolve_expr(
    ast: &Ast,
    arena: &mut ValueArena,
    registry: &TypeRegistry,
    user_modules: &HashSet<String>,
    id: ExprId,
) -> Option<Expr> {
    ast.get_expr(id).and_then(|expr| match expr {
        // Field access: `Name.field` or `A.B.C` (nested modules/types)
        // - If base is `Var(Type)` with zero-arity variant -> `Variant(Type, field, [])`
        // - If base path is a qualified type with zero-arity variant -> `Variant`
        // - If full path starts with a module -> `Path([...])`
        // - Otherwise -> leave as Field (runtime field access)
        //
        // Important: Type variants take priority over module paths. This allows
        // `Option.None` and `Result.Err` to work even though `Option` and `Result`
        // are also module names (for `Option.map`, `Result.map`, etc.).
        Expr::Field(base_id, field) => {
            // First check if it's a simple Type.Variant pattern (takes priority)
            let variant_expr =
                ast.get_expr(*base_id).and_then(|base| match base {
                    Expr::Var(name) => {
                        let name_id = arena.intern(name);
                        let field_id = arena.intern(field);

                        // Check if it's a zero-arity type variant
                        registry.lookup(name_id).and_then(|type_id| {
                            registry.lookup_variant(type_id, field_id).and_then(
                                |v| {
                                    (v.arity == 0).then(|| {
                                        Expr::Variant(
                                            name.clone(),
                                            field.clone(),
                                            smallvec![],
                                        )
                                    })
                                },
                            )
                        })
                    }
                    _ => None,
                });

            // Check for module-qualified type variant: Module.Type.Variant
            let qualified_variant_expr = variant_expr.or_else(|| {
                // Collect the full path and check if prefix is a qualified type
                let base_path = collect_path_segments(ast, *base_id)?;
                let qtype = base_path.join(".");
                let qtype_id = arena.intern(&qtype);
                let field_id = arena.intern(field);

                registry.lookup(qtype_id).and_then(|type_id| {
                    registry.lookup_variant(type_id, field_id).and_then(|v| {
                        (v.arity == 0).then(|| {
                            Expr::Variant(qtype, field.clone(), smallvec![])
                        })
                    })
                })
            });

            // If it's a type variant (simple or qualified), use that
            qualified_variant_expr.or_else(|| {
                // Otherwise, check if this is a module path
                let full_path = collect_path_segments(ast, id);

                let is_module_path = full_path
                    .as_ref()
                    .and_then(|segs: &SmallVec<[String; 4]>| segs.first())
                    .is_some_and(|first| is_known_module(first, user_modules));

                is_module_path.then(|| full_path.map(Expr::Path)).flatten()
            })
        }

        // Function calls: resolve variant constructors
        // `Call(Field(Var(Type), Variant), args)` -> `Variant(Type, Variant, args)`
        // `Call(Field(Field(...), Variant), args)` -> `Variant(QualifiedType, Variant, args)`
        //
        // Module calls like `Object.keys(x)` don't need special handling:
        // the Field becomes Path (above), so it becomes `Call(Path(...), args)`
        // which the interpreter handles normally.
        Expr::Call(callee_id, args) => {
            ast.get_expr(*callee_id).and_then(|callee| match callee {
                Expr::Field(base_id, var_name) => {
                    // First try simple Type.Variant(args) pattern
                    let simple_variant =
                        ast.get_expr(*base_id).and_then(|base| match base {
                            Expr::Var(ty_name) => {
                                let ty_id = arena.intern(ty_name);
                                let var_id = arena.intern(var_name);
                                registry.lookup(ty_id).and_then(|type_id| {
                                    registry
                                        .lookup_variant(type_id, var_id)
                                        .map(|_| {
                                            Expr::Variant(
                                                ty_name.clone(),
                                                var_name.clone(),
                                                args.clone(),
                                            )
                                        })
                                })
                            }
                            _ => None,
                        });

                    // Try module-qualified type: Module.Type.Variant(args)
                    simple_variant.or_else(|| {
                        let base_path = collect_path_segments(ast, *base_id)?;
                        let qtype = base_path.join(".");
                        let qtype_id = arena.intern(&qtype);
                        let var_id = arena.intern(var_name);

                        registry.lookup(qtype_id).and_then(|type_id| {
                            registry.lookup_variant(type_id, var_id).map(|_| {
                                Expr::Variant(
                                    qtype,
                                    var_name.clone(),
                                    args.clone(),
                                )
                            })
                        })
                    })
                }
                _ => None,
            })
        }

        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    fn parse_and_resolve(src: &str) -> Ast {
        let mut result = Parser::parse(src).expect("parse failed");
        let mut arena = ValueArena::new();
        let mut type_exprs = TypeExprArena::new();
        let registry = TypeRegistry::new(&mut arena, &mut type_exprs);
        let _ = resolve(&mut result.ast, &mut arena, &registry);
        result.ast
    }

    #[test]
    fn resolve_option_none() {
        let ast = parse_and_resolve("LET x = Option.None");
        let has_variant = ast.expr_ids().any(|id| {
            matches!(
                ast.get_expr(id),
                Some(Expr::Variant(ty, var, args))
                    if ty == "Option" && var == "None" && args.is_empty()
            )
        });
        assert!(
            has_variant,
            "Option.None should become Variant with no args"
        );
    }

    #[test]
    fn resolve_option_some_becomes_variant() {
        let ast = parse_and_resolve("LET x = Option.Some(42)");
        let has_variant = ast.expr_ids().any(|id| {
            matches!(
                ast.get_expr(id),
                Some(Expr::Variant(ty, var, _)) if ty == "Option" && var == "Some"
            )
        });
        assert!(has_variant, "Option.Some(42) should become Variant");
    }

    #[test]
    fn resolve_result_ok_becomes_variant() {
        let ast = parse_and_resolve("LET x = Result.Ok(42)");
        let has_variant = ast.expr_ids().any(|id| {
            matches!(
                ast.get_expr(id),
                Some(Expr::Variant(ty, var, _)) if ty == "Result" && var == "Ok"
            )
        });
        assert!(has_variant, "Result.Ok(42) should become Variant");
    }

    #[test]
    fn resolve_result_err_becomes_variant() {
        let ast = parse_and_resolve("LET x = Result.Err(\"oops\")");
        let has_variant = ast.expr_ids().any(|id| {
            matches!(
                ast.get_expr(id),
                Some(Expr::Variant(ty, var, _)) if ty == "Result" && var == "Err"
            )
        });
        assert!(has_variant, "Result.Err(\"oops\") should become Variant");
    }

    #[test]
    fn resolve_field_access_not_converted() {
        let ast = parse_and_resolve("LET obj = { x: 1 }\nLET y = obj.x");
        // obj.x should remain as Field, not become Variant
        let has_variant_obj_x = ast.expr_ids().any(|id| {
            matches!(
                ast.get_expr(id),
                Some(Expr::Variant(ty, var, _)) if ty == "obj" && var == "x"
            )
        });
        assert!(!has_variant_obj_x, "obj.x should not become Variant");
    }

    #[test]
    fn resolve_unknown_type_not_converted() {
        let ast = parse_and_resolve("LET x = Unknown.Foo");
        // Unknown.Foo should remain as Field since Unknown is not a registered type
        let has_variant = ast.expr_ids().any(|id| {
            matches!(
                ast.get_expr(id),
                Some(Expr::Variant(ty, _, _)) if ty == "Unknown"
            )
        });
        assert!(!has_variant, "Unknown.Foo should not become Variant");
    }

    #[test]
    fn resolve_array_push_becomes_path() {
        let ast = parse_and_resolve("LET r = Array.push([1, 2], 3)");
        let has_path = ast.expr_ids().any(|id| {
            matches!(
                ast.get_expr(id),
                Some(Expr::Path(segs)) if segs.as_slice() == ["Array", "push"]
            )
        });
        assert!(has_path, "Array.push should become Path([Array, push])");
    }

    #[test]
    fn resolve_module_fn_without_call() {
        // Module function used as value (e.g., for pipeline)
        let ast = parse_and_resolve("LET f = String.length");
        let has_path = ast.expr_ids().any(|id| {
            matches!(
                ast.get_expr(id),
                Some(Expr::Path(segs)) if segs.as_slice() == ["String", "length"]
            )
        });
        assert!(has_path, "String.length (no call) should become Path");
    }

    #[test]
    fn resolve_unknown_module_not_converted() {
        let ast = parse_and_resolve("LET x = Foo.bar(1)");
        // Foo.bar should remain as Field since Foo is not a known module
        let has_path = ast.expr_ids().any(|id| {
            matches!(
                ast.get_expr(id),
                Some(Expr::Path(segs)) if segs.first() == Some(&"Foo".to_string())
            )
        });
        assert!(!has_path, "Foo.bar should not become Path");
    }
}
