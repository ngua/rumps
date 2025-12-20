//! Name resolution pass: convert type-qualified paths to proper AST nodes.
//!
//! This pass runs after parsing (CST -> AST lowering) and before interpretation.
//! It separates compile-time namespace resolution from runtime operations:
//!
//! - `Option.None` (zero-arity variant) -> `Expr::Variant("Option", "None", [])`
//! - `Option.Some(x)` (variant with args) -> `Expr::Variant("Option", "Some", [x])`
//! - `obj.field` (runtime field access) -> remains `Expr::Field`
//! - `obj.method(args)` (runtime call) -> remains `Expr::Call`
//!
//! The parser emits generic `Expr::Field` and `Expr::Call` nodes; this pass
//! converts type-qualified names to `Expr::Variant` that the interpreter
//! handles without runtime type registry lookups.
//!
//! # Architecture
//!
//! ```text
//! Tokens -> CST -> AST -> [resolve] -> Interpreter
//!                         ^^^^^^^^^^
//!                         this pass
//! ```

use smallvec::smallvec;

use crate::ast::{Ast, Expr, ExprId};
use crate::value::{TypeRegistry, ValueArena};

/// Run name resolution on the AST.
///
/// Converts:
/// - `Expr::Field(Var(type), variant)` to `Expr::Variant` for zero-arity variants
/// - `Expr::Call(Field(Var(type), variant), args)` to `Expr::Variant` for
///   variant constructors with arguments
pub(crate) fn resolve(
    ast: &mut Ast,
    arena: &mut ValueArena,
    registry: &TypeRegistry,
) {
    // Collect replacements first to avoid borrowing issues
    let replacements: Vec<(ExprId, Expr)> = ast
        .expr_ids()
        .filter_map(|id| {
            resolve_expr(ast, arena, registry, id).map(|e| (id, e))
        })
        .collect();

    // Apply replacements
    replacements
        .into_iter()
        .for_each(|(id, expr)| ast.set_expr(id, expr));
}

/// Resolve a single expression, returning `Some(replacement)` if it should
/// be replaced.
fn resolve_expr(
    ast: &Ast,
    arena: &mut ValueArena,
    registry: &TypeRegistry,
    id: ExprId,
) -> Option<Expr> {
    ast.get_expr(id).and_then(|expr| match expr {
        // Zero-arity variants: `Type.Variant` -> `Variant(Type, Variant, [])`
        Expr::Field(base_id, field) => {
            ast.get_expr(*base_id).and_then(|base| match base {
                Expr::Var(ty_name) => {
                    let ty_id = arena.intern(ty_name);
                    let var_id = arena.intern(field);
                    registry.lookup(ty_id).and_then(|type_id| {
                        registry.lookup_variant(type_id, var_id).and_then(|v| {
                            (v.arity == 0).then(|| {
                                Expr::Variant(
                                    ty_name.clone(),
                                    field.clone(),
                                    smallvec![],
                                )
                            })
                        })
                    })
                }
                _ => None,
            })
        }

        // Variant constructors: `Call(Field(Var(Type), Variant), args)`
        // -> `Variant(Type, Variant, args)`
        Expr::Call(callee_id, args) => {
            ast.get_expr(*callee_id).and_then(|callee| match callee {
                Expr::Field(base_id, var_name) => {
                    ast.get_expr(*base_id).and_then(|base| match base {
                        Expr::Var(ty_name) => {
                            let ty_id = arena.intern(ty_name);
                            let var_id = arena.intern(var_name);
                            registry.lookup(ty_id).and_then(|type_id| {
                                registry.lookup_variant(type_id, var_id).map(
                                    |_| {
                                        Expr::Variant(
                                            ty_name.clone(),
                                            var_name.clone(),
                                            args.clone(),
                                        )
                                    },
                                )
                            })
                        }
                        _ => None,
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
        let registry = TypeRegistry::new(&mut arena).expect("registry failed");
        resolve(&mut result.ast, &mut arena, &registry);
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
}
