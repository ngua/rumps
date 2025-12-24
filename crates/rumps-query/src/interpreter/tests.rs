//! Interpreter unit tests.
//!
//! These tests create ASTs directly (bypassing parsing) and use in-memory databases.

use ordered_float::OrderedFloat;
use rumps_storage::Database;
use smallvec::smallvec;

use super::Interpreter;
use crate::ast::{
    Ast, AstTypeExpr, BinOp, Expr, ExprId, Literal, Stmt, TypePattern, UnOp,
};
use crate::io::TestIo;
use crate::value::{TypeExprArena, TypeRegistry, Value, ValueArena};
use crate::Span;

/// Create a test interpreter with an in-memory database.
///
/// Uses `with_arena` since tests build ASTs directly (no parsing/resolution).
fn test_interp(ast: &Ast) -> Interpreter<'_, TestIo> {
    let db = Database::in_memory().expect("in-memory db");
    let mut arena = ValueArena::new();
    let mut type_exprs = TypeExprArena::new();
    let registry =
        TypeRegistry::new(&mut arena, &mut type_exprs).expect("registry");
    Interpreter::with_arena(ast, db, TestIo::new(), arena, registry, type_exprs)
}

/// Build a simple AST with a single expression.
fn ast_with_expr(expr: Expr) -> (Ast, ExprId) {
    let mut ast = Ast::new();
    let id = ast.add_expr(expr, Span::new(0, 10)).unwrap();
    (ast, id)
}
#[tokio::test]
async fn eval_int_literal() {
    let (ast, id) = ast_with_expr(Expr::Literal(Literal::Int(42)));
    let mut interp = test_interp(&ast);
    let result = interp.eval(id).await.unwrap();
    assert_eq!(result, Value::Int(42));
}

#[tokio::test]
async fn eval_float_literal() {
    let (ast, id) = ast_with_expr(Expr::Literal(Literal::Float(3.14)));
    let mut interp = test_interp(&ast);
    let result = interp.eval(id).await.unwrap();
    assert_eq!(result, Value::Float(OrderedFloat(3.14)));
}

#[tokio::test]
async fn eval_bool_literal() {
    let (ast, id) = ast_with_expr(Expr::Literal(Literal::Bool(true)));
    let mut interp = test_interp(&ast);
    let result = interp.eval(id).await.unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[tokio::test]
async fn eval_string_literal() {
    let (ast, id) =
        ast_with_expr(Expr::Literal(Literal::String("hello".into())));
    let mut interp = test_interp(&ast);
    let result = interp.eval(id).await.unwrap();
    match result {
        Value::String(id) => {
            assert_eq!(interp.arena.get_str(id), Some("hello"));
        }
        _ => panic!("expected string"),
    }
}

#[tokio::test]
async fn eval_add_int() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(0, 2))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(20)), Span::new(5, 7))
        .unwrap();
    let add = ast
        .add_expr(Expr::Binary(lhs, BinOp::Add, rhs), Span::new(0, 7))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(add).await.unwrap();
    assert_eq!(result, Value::Int(30));
}

#[tokio::test]
async fn eval_add_float() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Float(1.5)), Span::new(0, 3))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Float(2.5)), Span::new(6, 9))
        .unwrap();
    let add = ast
        .add_expr(Expr::Binary(lhs, BinOp::Add, rhs), Span::new(0, 9))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(add).await.unwrap();
    assert_eq!(result, Value::Float(OrderedFloat(4.0)));
}

#[tokio::test]
async fn eval_add_mixed_coercion() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(0, 2))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Float(2.5)), Span::new(5, 8))
        .unwrap();
    let add = ast
        .add_expr(Expr::Binary(lhs, BinOp::Add, rhs), Span::new(0, 8))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(add).await.unwrap();
    assert_eq!(result, Value::Float(OrderedFloat(12.5)));
}

#[tokio::test]
async fn eval_sub() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(50)), Span::new(0, 2))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(30)), Span::new(5, 7))
        .unwrap();
    let sub = ast
        .add_expr(Expr::Binary(lhs, BinOp::Sub, rhs), Span::new(0, 7))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(sub).await.unwrap();
    assert_eq!(result, Value::Int(20));
}

#[tokio::test]
async fn eval_mul() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(6)), Span::new(0, 1))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(7)), Span::new(4, 5))
        .unwrap();
    let mul = ast
        .add_expr(Expr::Binary(lhs, BinOp::Mul, rhs), Span::new(0, 5))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(mul).await.unwrap();
    assert_eq!(result, Value::Int(42));
}

#[tokio::test]
async fn eval_div() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(0, 2))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(4)), Span::new(5, 6))
        .unwrap();
    let div = ast
        .add_expr(Expr::Binary(lhs, BinOp::Div, rhs), Span::new(0, 6))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(div).await.unwrap();
    assert_eq!(result, Value::Float(OrderedFloat(2.5)));
}

#[tokio::test]
async fn eval_div_by_zero() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(0, 2))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(0)), Span::new(5, 6))
        .unwrap();
    let div = ast
        .add_expr(Expr::Binary(lhs, BinOp::Div, rhs), Span::new(0, 6))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(div).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn eval_floor_div() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(0, 2))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(3)), Span::new(5, 6))
        .unwrap();
    let div = ast
        .add_expr(Expr::Binary(lhs, BinOp::FloorDiv, rhs), Span::new(0, 6))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(div).await.unwrap();
    assert_eq!(result, Value::Int(3));
}

#[tokio::test]
async fn eval_mod() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(0, 2))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(3)), Span::new(5, 6))
        .unwrap();
    let m = ast
        .add_expr(Expr::Binary(lhs, BinOp::Mod, rhs), Span::new(0, 6))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(m).await.unwrap();
    assert_eq!(result, Value::Int(1));
}

#[tokio::test]
async fn eval_pow_int() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(2)), Span::new(0, 1))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(5, 7))
        .unwrap();
    let pow = ast
        .add_expr(Expr::Binary(lhs, BinOp::Pow, rhs), Span::new(0, 7))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(pow).await.unwrap();
    assert_eq!(result, Value::Int(1024));
}

#[tokio::test]
async fn eval_pow_float() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Float(2.0)), Span::new(0, 3))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Float(0.5)), Span::new(7, 10))
        .unwrap();
    let pow = ast
        .add_expr(Expr::Binary(lhs, BinOp::Pow, rhs), Span::new(0, 10))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(pow).await.unwrap();
    // 2.0 ** 0.5 = sqrt(2) ≈ 1.41421...
    match result {
        Value::Float(f) => assert!((f.0 - 1.41421356).abs() < 0.0001),
        _ => panic!("expected Float"),
    }
}

#[tokio::test]
async fn eval_pow_negative_exp() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(2)), Span::new(0, 1))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(-1)), Span::new(5, 7))
        .unwrap();
    let pow = ast
        .add_expr(Expr::Binary(lhs, BinOp::Pow, rhs), Span::new(0, 7))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(pow).await.unwrap();
    // 2 ** -1 = 0.5
    assert_eq!(result, Value::Float(OrderedFloat(0.5)));
}

#[tokio::test]
async fn eval_pow_mixed() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(4)), Span::new(0, 1))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Float(0.5)), Span::new(5, 8))
        .unwrap();
    let pow = ast
        .add_expr(Expr::Binary(lhs, BinOp::Pow, rhs), Span::new(0, 8))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(pow).await.unwrap();
    // 4 ** 0.5 = 2.0
    assert_eq!(result, Value::Float(OrderedFloat(2.0)));
}

#[tokio::test]
async fn eval_eq() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(5, 7))
        .unwrap();
    let eq = ast
        .add_expr(Expr::Binary(lhs, BinOp::Eq, rhs), Span::new(0, 7))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(eq).await.unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[tokio::test]
async fn eval_eq_mixed_numeric() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Float(42.0)), Span::new(5, 9))
        .unwrap();
    let eq = ast
        .add_expr(Expr::Binary(lhs, BinOp::Eq, rhs), Span::new(0, 9))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(eq).await.unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[tokio::test]
async fn eval_ne() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(0, 1))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(2)), Span::new(5, 6))
        .unwrap();
    let ne = ast
        .add_expr(Expr::Binary(lhs, BinOp::Ne, rhs), Span::new(0, 6))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(ne).await.unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[tokio::test]
async fn eval_lt() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(5)), Span::new(0, 1))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(4, 6))
        .unwrap();
    let lt = ast
        .add_expr(Expr::Binary(lhs, BinOp::Lt, rhs), Span::new(0, 6))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(lt).await.unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[tokio::test]
async fn eval_gt() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(0, 2))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(5)), Span::new(5, 6))
        .unwrap();
    let gt = ast
        .add_expr(Expr::Binary(lhs, BinOp::Gt, rhs), Span::new(0, 6))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(gt).await.unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[tokio::test]
async fn eval_le() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(5)), Span::new(0, 1))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(5)), Span::new(5, 6))
        .unwrap();
    let le = ast
        .add_expr(Expr::Binary(lhs, BinOp::Le, rhs), Span::new(0, 6))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(le).await.unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[tokio::test]
async fn eval_ge() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(0, 2))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(5)), Span::new(5, 6))
        .unwrap();
    let ge = ast
        .add_expr(Expr::Binary(lhs, BinOp::Ge, rhs), Span::new(0, 6))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(ge).await.unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[tokio::test]
async fn eval_and() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), Span::new(0, 4))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Bool(false)), Span::new(8, 13))
        .unwrap();
    let and = ast
        .add_expr(Expr::Binary(lhs, BinOp::And, rhs), Span::new(0, 13))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(and).await.unwrap();
    assert_eq!(result, Value::Bool(false));
}

#[tokio::test]
async fn eval_or() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Bool(false)), Span::new(0, 5))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), Span::new(9, 13))
        .unwrap();
    let or = ast
        .add_expr(Expr::Binary(lhs, BinOp::Or, rhs), Span::new(0, 13))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(or).await.unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[tokio::test]
async fn eval_concat() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(
            Expr::Literal(Literal::String("Hello".into())),
            Span::new(0, 7),
        )
        .unwrap();
    let rhs = ast
        .add_expr(
            Expr::Literal(Literal::String(" World".into())),
            Span::new(11, 19),
        )
        .unwrap();
    let cat = ast
        .add_expr(Expr::Binary(lhs, BinOp::Concat, rhs), Span::new(0, 19))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(cat).await.unwrap();
    match result {
        Value::String(id) => {
            assert_eq!(interp.arena.get_str(id), Some("Hello World"));
        }
        _ => panic!("expected string"),
    }
}

#[tokio::test]
async fn eval_concat_coercion() {
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(
            Expr::Literal(Literal::String("value: ".into())),
            Span::new(0, 9),
        )
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(13, 15))
        .unwrap();
    let cat = ast
        .add_expr(Expr::Binary(lhs, BinOp::Concat, rhs), Span::new(0, 15))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(cat).await.unwrap();
    match result {
        Value::String(id) => {
            assert_eq!(interp.arena.get_str(id), Some("value: 42"));
        }
        _ => panic!("expected string"),
    }
}

#[tokio::test]
async fn eval_neg_int() {
    let mut ast = Ast::new();
    let operand = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(1, 3))
        .unwrap();
    let neg = ast
        .add_expr(Expr::Unary(UnOp::Neg, operand), Span::new(0, 3))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(neg).await.unwrap();
    assert_eq!(result, Value::Int(-42));
}

#[tokio::test]
async fn eval_neg_float() {
    let mut ast = Ast::new();
    let operand = ast
        .add_expr(Expr::Literal(Literal::Float(3.14)), Span::new(1, 5))
        .unwrap();
    let neg = ast
        .add_expr(Expr::Unary(UnOp::Neg, operand), Span::new(0, 5))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(neg).await.unwrap();
    assert_eq!(result, Value::Float(OrderedFloat(-3.14)));
}

#[tokio::test]
async fn eval_not() {
    let mut ast = Ast::new();
    let operand = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), Span::new(1, 5))
        .unwrap();
    let not = ast
        .add_expr(Expr::Unary(UnOp::Not, operand), Span::new(0, 5))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(not).await.unwrap();
    assert_eq!(result, Value::Bool(false));
}

#[tokio::test]
async fn let_and_lookup() {
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(100)), Span::new(8, 11))
        .unwrap();
    let let_stmt = ast
        .add_stmt(Stmt::Let("x".into(), None, val), Span::new(0, 11))
        .unwrap();
    let var = ast
        .add_expr(Expr::Var("x".into()), Span::new(0, 1))
        .unwrap();

    let mut interp = test_interp(&ast);
    interp.exec(let_stmt).await.unwrap();
    let result = interp.eval(var).await.unwrap();
    assert_eq!(result, Value::Int(100));
}

#[tokio::test]
async fn let_shadowing() {
    let mut ast = Ast::new();

    // LET x = 10
    let val1 = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(8, 10))
        .unwrap();
    let let1 = ast
        .add_stmt(Stmt::Let("x".into(), None, val1), Span::new(0, 10))
        .unwrap();

    // Block expr with LET x = 20, returning x
    let val2 = ast
        .add_expr(Expr::Literal(Literal::Int(20)), Span::new(20, 22))
        .unwrap();
    let let2 = ast
        .add_stmt(Stmt::Let("x".into(), None, val2), Span::new(12, 22))
        .unwrap();
    let x_ref = ast
        .add_expr(Expr::Var("x".into()), Span::new(24, 25))
        .unwrap();
    let blk_expr = ast
        .add_expr(Expr::Block(vec![let2], Some(x_ref)), Span::new(10, 26))
        .unwrap();
    let blk_stmt = ast
        .add_stmt(Stmt::Expr(blk_expr), Span::new(10, 26))
        .unwrap();

    // Reference outer x
    let var = ast
        .add_expr(Expr::Var("x".into()), Span::new(28, 29))
        .unwrap();

    let mut interp = test_interp(&ast);
    interp.exec(let1).await.unwrap();
    interp.exec(blk_stmt).await.unwrap();
    // After block exits, x should be 10 again
    let result = interp.eval(var).await.unwrap();
    assert_eq!(result, Value::Int(10));
}

#[tokio::test]
async fn array() {
    let mut ast = Ast::new();
    let e1 = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(1, 2))
        .unwrap();
    let e2 = ast
        .add_expr(Expr::Literal(Literal::Int(2)), Span::new(4, 5))
        .unwrap();
    let e3 = ast
        .add_expr(Expr::Literal(Literal::Int(3)), Span::new(7, 8))
        .unwrap();
    let arr = ast
        .add_expr(Expr::Array(vec![e1, e2, e3]), Span::new(0, 9))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(arr).await.unwrap();
    match result {
        Value::Array(_, elems) => {
            assert_eq!(elems.len(), 3);
        }
        _ => panic!("expected array"),
    }
}

#[tokio::test]
async fn object() {
    let mut ast = Ast::new();
    let v1 = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(6, 8))
        .unwrap();
    let v2 = ast
        .add_expr(
            Expr::Literal(Literal::String("John".into())),
            Span::new(17, 23),
        )
        .unwrap();
    let obj = ast
        .add_expr(
            Expr::Object(vec![("id".into(), v1), ("name".into(), v2)]),
            Span::new(0, 25),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(obj).await.unwrap();
    match result {
        Value::Object(map) => {
            assert_eq!(map.len(), 2);
        }
        _ => panic!("expected object"),
    }
}

#[tokio::test]
async fn index_array() {
    let mut ast = Ast::new();
    let e1 = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(1, 3))
        .unwrap();
    let e2 = ast
        .add_expr(Expr::Literal(Literal::Int(20)), Span::new(5, 7))
        .unwrap();
    let arr = ast
        .add_expr(Expr::Array(vec![e1, e2]), Span::new(0, 8))
        .unwrap();
    let idx = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(9, 10))
        .unwrap();
    let access = ast
        .add_expr(Expr::Index(arr, idx), Span::new(0, 11))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(access).await.unwrap();
    assert_eq!(result, Value::Int(20));
}

#[tokio::test]
async fn field_access() {
    let mut ast = Ast::new();
    let v = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(6, 8))
        .unwrap();
    let obj = ast
        .add_expr(Expr::Object(vec![("x".into(), v)]), Span::new(0, 10))
        .unwrap();
    let field = ast
        .add_expr(Expr::Field(obj, "x".into()), Span::new(0, 12))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(field).await.unwrap();
    assert_eq!(result, Value::Int(42));
}

#[tokio::test]
async fn if_true() {
    let mut ast = Ast::new();

    // LET result = 0
    let zero = ast
        .add_expr(Expr::Literal(Literal::Int(0)), Span::new(13, 14))
        .unwrap();
    let let_result = ast
        .add_stmt(Stmt::Let("result".into(), None, zero), Span::new(0, 14))
        .unwrap();

    // IF true { LET result = 1 }
    let cond = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), Span::new(3, 7))
        .unwrap();
    let one = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(25, 26))
        .unwrap();
    let set_one = ast
        .add_stmt(Stmt::Let("result".into(), None, one), Span::new(10, 26))
        .unwrap();
    let then_blk = ast
        .add_expr(Expr::Block(vec![set_one], None), Span::new(8, 28))
        .unwrap();
    let if_expr = ast
        .add_expr(Expr::If(cond, then_blk, None), Span::new(0, 28))
        .unwrap();
    let if_stmt = ast.add_stmt(Stmt::Expr(if_expr), Span::new(0, 28)).unwrap();

    let var = ast
        .add_expr(Expr::Var("result".into()), Span::new(0, 6))
        .unwrap();

    let mut interp = test_interp(&ast);
    interp.exec(let_result).await.unwrap();
    interp.exec(if_stmt).await.unwrap();
    let result = interp.eval(var).await.unwrap();
    // In the block, we shadowed result. After block exit, it's 0 again.
    assert_eq!(result, Value::Int(0));
}

#[tokio::test]
async fn if_else() {
    let mut ast = Ast::new();

    // IF false { } ELSE { LET x = 42 }
    let cond = ast
        .add_expr(Expr::Literal(Literal::Bool(false)), Span::new(3, 8))
        .unwrap();
    let then_blk = ast
        .add_expr(Expr::Block(vec![], None), Span::new(9, 12))
        .unwrap();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(30, 32))
        .unwrap();
    let let_x = ast
        .add_stmt(Stmt::Let("x".into(), None, val), Span::new(22, 32))
        .unwrap();
    let else_blk = ast
        .add_expr(Expr::Block(vec![let_x], None), Span::new(18, 35))
        .unwrap();
    let if_expr = ast
        .add_expr(Expr::If(cond, then_blk, Some(else_blk)), Span::new(0, 35))
        .unwrap();
    let if_stmt = ast.add_stmt(Stmt::Expr(if_expr), Span::new(0, 35)).unwrap();

    // After IF, check x
    let var = ast
        .add_expr(Expr::Var("x".into()), Span::new(0, 1))
        .unwrap();

    let mut interp = test_interp(&ast);
    interp.exec(if_stmt).await.unwrap();
    // x was set in else block which exited, so x is not visible
    let result = interp.eval(var).await;
    assert!(result.is_err()); // x is not defined outside the block
}

#[tokio::test]
async fn run_program() {
    let mut ast = Ast::new();

    // LET x = 10
    let v1 = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(8, 10))
        .unwrap();
    let let_x = ast
        .add_stmt(Stmt::Let("x".into(), None, v1), Span::new(0, 10))
        .unwrap();

    // LET y = 20
    let v2 = ast
        .add_expr(Expr::Literal(Literal::Int(20)), Span::new(20, 22))
        .unwrap();
    let let_y = ast
        .add_stmt(Stmt::Let("y".into(), None, v2), Span::new(12, 22))
        .unwrap();

    // LET sum = x + y
    let x = ast
        .add_expr(Expr::Var("x".into()), Span::new(34, 35))
        .unwrap();
    let y = ast
        .add_expr(Expr::Var("y".into()), Span::new(38, 39))
        .unwrap();
    let add = ast
        .add_expr(Expr::Binary(x, BinOp::Add, y), Span::new(34, 39))
        .unwrap();
    let let_sum = ast
        .add_stmt(Stmt::Let("sum".into(), None, add), Span::new(24, 39))
        .unwrap();

    // Reference to check result (create before interpreter borrows ast)
    let sum_var = ast
        .add_expr(Expr::Var("sum".into()), Span::new(0, 3))
        .unwrap();

    let stmts = vec![let_x, let_y, let_sum];

    let interp = test_interp(&ast);
    let mut interp = interp.run(&stmts).await.unwrap();

    // Check sum
    let result = interp.eval(sum_var).await.unwrap();
    assert_eq!(result, Value::Int(30));
}

#[tokio::test]
async fn if_expr_true_branch() {
    // IF true { 42 } ELSE { 0 }
    let mut ast = Ast::new();
    let cond = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), Span::new(3, 7))
        .unwrap();
    let then_val = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(10, 12))
        .unwrap();
    let then_blk = ast
        .add_expr(Expr::Block(vec![], Some(then_val)), Span::new(9, 14))
        .unwrap();
    let else_val = ast
        .add_expr(Expr::Literal(Literal::Int(0)), Span::new(22, 23))
        .unwrap();
    let else_blk = ast
        .add_expr(Expr::Block(vec![], Some(else_val)), Span::new(21, 25))
        .unwrap();
    let if_expr = ast
        .add_expr(Expr::If(cond, then_blk, Some(else_blk)), Span::new(0, 25))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(if_expr).await.unwrap();
    assert_eq!(result, Value::Int(42));
}

#[tokio::test]
async fn if_expr_false_branch() {
    // IF false { 42 } ELSE { 0 }
    let mut ast = Ast::new();
    let cond = ast
        .add_expr(Expr::Literal(Literal::Bool(false)), Span::new(3, 8))
        .unwrap();
    let then_val = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(11, 13))
        .unwrap();
    let then_blk = ast
        .add_expr(Expr::Block(vec![], Some(then_val)), Span::new(10, 15))
        .unwrap();
    let else_val = ast
        .add_expr(Expr::Literal(Literal::Int(0)), Span::new(23, 24))
        .unwrap();
    let else_blk = ast
        .add_expr(Expr::Block(vec![], Some(else_val)), Span::new(22, 26))
        .unwrap();
    let if_expr = ast
        .add_expr(Expr::If(cond, then_blk, Some(else_blk)), Span::new(0, 26))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(if_expr).await.unwrap();
    assert_eq!(result, Value::Int(0));
}

#[tokio::test]
async fn if_expr_no_else_true() {
    // IF true { } (no else, body is Unit block, returns Unit)
    let mut ast = Ast::new();
    let cond = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), Span::new(3, 7))
        .unwrap();
    // Block without tail expression evaluates to Unit
    let then_blk = ast
        .add_expr(Expr::Block(vec![], None), Span::new(9, 11))
        .unwrap();
    let if_expr = ast
        .add_expr(Expr::If(cond, then_blk, None), Span::new(0, 11))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(if_expr).await.unwrap();
    assert_eq!(result, Value::Unit);
}

#[tokio::test]
async fn if_expr_no_else_non_unit_error() {
    // IF true { 42 } (no else, body is Int) -> type error
    let mut ast = Ast::new();
    let cond = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), Span::new(3, 7))
        .unwrap();
    let then_val = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(10, 12))
        .unwrap();
    let then_blk = ast
        .add_expr(Expr::Block(vec![], Some(then_val)), Span::new(9, 14))
        .unwrap();
    let if_expr = ast
        .add_expr(Expr::If(cond, then_blk, None), Span::new(0, 14))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(if_expr).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("must be Unit"));
}

#[tokio::test]
async fn if_expr_no_else_false() {
    // IF false { } (no else, condition false, returns Unit)
    let mut ast = Ast::new();
    let cond = ast
        .add_expr(Expr::Literal(Literal::Bool(false)), Span::new(3, 8))
        .unwrap();
    // Block without tail expression evaluates to Unit
    let then_blk = ast
        .add_expr(Expr::Block(vec![], None), Span::new(10, 12))
        .unwrap();
    let if_expr = ast
        .add_expr(Expr::If(cond, then_blk, None), Span::new(0, 12))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(if_expr).await.unwrap();
    assert_eq!(result, Value::Unit);
}

#[tokio::test]
async fn if_expr_as_value() {
    // LET x = IF true { 10 } ELSE { 20 }
    let mut ast = Ast::new();
    let cond = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), Span::new(12, 16))
        .unwrap();
    let then_val = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(19, 21))
        .unwrap();
    let then_blk = ast
        .add_expr(Expr::Block(vec![], Some(then_val)), Span::new(18, 23))
        .unwrap();
    let else_val = ast
        .add_expr(Expr::Literal(Literal::Int(20)), Span::new(31, 33))
        .unwrap();
    let else_blk = ast
        .add_expr(Expr::Block(vec![], Some(else_val)), Span::new(30, 35))
        .unwrap();
    let if_expr = ast
        .add_expr(Expr::If(cond, then_blk, Some(else_blk)), Span::new(8, 35))
        .unwrap();

    let let_x = ast
        .add_stmt(Stmt::Let("x".into(), None, if_expr), Span::new(0, 35))
        .unwrap();
    let x_var = ast
        .add_expr(Expr::Var("x".into()), Span::new(0, 1))
        .unwrap();

    let mut interp = test_interp(&ast);
    interp.exec(let_x).await.unwrap();
    let result = interp.eval(x_var).await.unwrap();
    assert_eq!(result, Value::Int(10));
}

#[tokio::test]
async fn block_expr_with_tail() {
    // { 42 }
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(2, 4))
        .unwrap();
    let blk = ast
        .add_expr(Expr::Block(vec![], Some(val)), Span::new(0, 6))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(blk).await.unwrap();
    assert_eq!(result, Value::Int(42));
}

#[tokio::test]
async fn block_expr_no_tail() {
    // { } (empty block, returns Unit)
    let mut ast = Ast::new();
    let blk = ast
        .add_expr(Expr::Block(vec![], None), Span::new(0, 3))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(blk).await.unwrap();
    assert_eq!(result, Value::Unit);
}

#[tokio::test]
async fn block_expr_with_stmts() {
    // { LET x = 10; x + 1 }
    let mut ast = Ast::new();
    let ten = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(10, 12))
        .unwrap();
    let let_x = ast
        .add_stmt(Stmt::Let("x".into(), None, ten), Span::new(2, 12))
        .unwrap();

    let x = ast
        .add_expr(Expr::Var("x".into()), Span::new(14, 15))
        .unwrap();
    let one = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(18, 19))
        .unwrap();
    let tail = ast
        .add_expr(Expr::Binary(x, BinOp::Add, one), Span::new(14, 19))
        .unwrap();

    let blk = ast
        .add_expr(Expr::Block(vec![let_x], Some(tail)), Span::new(0, 21))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(blk).await.unwrap();
    assert_eq!(result, Value::Int(11));
}

#[tokio::test]
async fn block_expr_scope_isolated() {
    // LET x = 1; { LET x = 10; x } evaluates to 10, outer x still 1
    let mut ast = Ast::new();

    // outer LET x = 1
    let one = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(8, 9))
        .unwrap();
    let let_outer = ast
        .add_stmt(Stmt::Let("x".into(), None, one), Span::new(0, 9))
        .unwrap();

    // inner block: { LET x = 10; x }
    let ten = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(22, 24))
        .unwrap();
    let let_inner = ast
        .add_stmt(Stmt::Let("x".into(), None, ten), Span::new(13, 24))
        .unwrap();
    let x_inner = ast
        .add_expr(Expr::Var("x".into()), Span::new(26, 27))
        .unwrap();
    let blk = ast
        .add_expr(
            Expr::Block(vec![let_inner], Some(x_inner)),
            Span::new(11, 29),
        )
        .unwrap();

    // outer x reference
    let x_outer = ast
        .add_expr(Expr::Var("x".into()), Span::new(31, 32))
        .unwrap();

    let mut interp = test_interp(&ast);
    interp.exec(let_outer).await.unwrap();
    let blk_result = interp.eval(blk).await.unwrap();
    assert_eq!(blk_result, Value::Int(10));

    let outer_result = interp.eval(x_outer).await.unwrap();
    assert_eq!(outer_result, Value::Int(1));
}

#[tokio::test]
async fn coalesce_option_none() {
    // Option.None ?? 0 -> 0
    let mut ast = Ast::new();

    let none = ast
        .add_expr(
            Expr::Variant("Option".into(), "None".into(), smallvec![]),
            Span::new(0, 11),
        )
        .unwrap();
    let fallback = ast
        .add_expr(Expr::Literal(Literal::Int(0)), Span::new(15, 16))
        .unwrap();
    let coalesce = ast
        .add_expr(
            Expr::Binary(none, BinOp::Coalesce, fallback),
            Span::new(0, 16),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(coalesce).await.unwrap();
    assert_eq!(result, Value::Int(0));
}

#[tokio::test]
async fn coalesce_non_option_error() {
    // 42 ?? 0 -> type error (Int is not Option or Result)
    let mut ast = Ast::new();

    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(0)), Span::new(6, 7))
        .unwrap();
    let coalesce = ast
        .add_expr(Expr::Binary(lhs, BinOp::Coalesce, rhs), Span::new(0, 7))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(coalesce).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("Option or Result"));
}

#[tokio::test]
async fn coalesce_string_error() {
    // "hello" ?? "fallback" -> type error
    let mut ast = Ast::new();

    let lhs = ast
        .add_expr(
            Expr::Literal(Literal::String("hello".into())),
            Span::new(0, 7),
        )
        .unwrap();
    let rhs = ast
        .add_expr(
            Expr::Literal(Literal::String("fallback".into())),
            Span::new(11, 21),
        )
        .unwrap();
    let coalesce = ast
        .add_expr(Expr::Binary(lhs, BinOp::Coalesce, rhs), Span::new(0, 21))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(coalesce).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn coalesce_short_circuit() {
    // Option.None ?? 99 -> 99 (rhs evaluated when lhs is None)
    let mut ast = Ast::new();

    let none = ast
        .add_expr(
            Expr::Variant("Option".into(), "None".into(), smallvec![]),
            Span::new(0, 11),
        )
        .unwrap();
    let fallback = ast
        .add_expr(Expr::Literal(Literal::Int(99)), Span::new(15, 17))
        .unwrap();
    let coalesce = ast
        .add_expr(
            Expr::Binary(none, BinOp::Coalesce, fallback),
            Span::new(0, 17),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(coalesce).await.unwrap();
    assert_eq!(result, Value::Int(99));
}

#[tokio::test]
async fn coalesce_chain() {
    // Option.None ?? Option.None ?? 3 -> 3
    let mut ast = Ast::new();

    let none1 = ast
        .add_expr(
            Expr::Variant("Option".into(), "None".into(), smallvec![]),
            Span::new(0, 11),
        )
        .unwrap();
    let none2 = ast
        .add_expr(
            Expr::Variant("Option".into(), "None".into(), smallvec![]),
            Span::new(15, 26),
        )
        .unwrap();
    let three = ast
        .add_expr(Expr::Literal(Literal::Int(3)), Span::new(30, 31))
        .unwrap();

    // Build: (none1 ?? none2) ?? 3
    let c1 = ast
        .add_expr(
            Expr::Binary(none1, BinOp::Coalesce, none2),
            Span::new(0, 26),
        )
        .unwrap();
    let c2 = ast
        .add_expr(Expr::Binary(c1, BinOp::Coalesce, three), Span::new(0, 31))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(c2).await.unwrap();
    assert_eq!(result, Value::Int(3));
}

#[tokio::test]
async fn variant_option_some() {
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(12, 14))
        .unwrap();
    let variant = ast
        .add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![val],
            ),
            Span::new(0, 15),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(variant).await.unwrap();
    match result {
        Value::Tagged(ty_expr, idx, payloads) => {
            let base = interp.type_exprs.base_type(ty_expr);
            assert_eq!(base, Some(crate::value::TypeId::OPTION));
            assert_eq!(idx, 1); // Some is index 1
            assert_eq!(payloads.len(), 1);
        }
        _ => panic!("expected Tagged"),
    }
}

#[tokio::test]
async fn variant_option_none() {
    // Option.None is resolved to Expr::Variant by the resolution pass
    let mut ast = Ast::new();
    let none = ast
        .add_expr(
            Expr::Variant("Option".into(), "None".into(), smallvec![]),
            Span::new(0, 11),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(none).await.unwrap();
    match result {
        Value::Tagged(ty_expr, idx, payloads) => {
            let base = interp.type_exprs.base_type(ty_expr);
            assert_eq!(base, Some(crate::value::TypeId::OPTION));
            assert_eq!(idx, 0); // None is index 0
            assert!(payloads.is_empty());
        }
        _ => panic!("expected Tagged"),
    }
}

#[tokio::test]
async fn variant_result_ok() {
    let mut ast = Ast::new();
    let val = ast
        .add_expr(
            Expr::Literal(Literal::String("success".into())),
            Span::new(10, 19),
        )
        .unwrap();
    let variant = ast
        .add_expr(
            Expr::Variant(
                "Result".into(),
                "Ok".into(),
                smallvec::smallvec![val],
            ),
            Span::new(0, 20),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(variant).await.unwrap();
    match result {
        Value::Tagged(ty_expr, idx, payloads) => {
            let base = interp.type_exprs.base_type(ty_expr);
            assert_eq!(base, Some(crate::value::TypeId::RESULT));
            assert_eq!(idx, 0); // Ok is index 0
            assert_eq!(payloads.len(), 1);
        }
        _ => panic!("expected Tagged"),
    }
}

#[tokio::test]
async fn variant_result_err() {
    let mut ast = Ast::new();
    let val = ast
        .add_expr(
            Expr::Literal(Literal::String("oops".into())),
            Span::new(11, 17),
        )
        .unwrap();
    let variant = ast
        .add_expr(
            Expr::Variant(
                "Result".into(),
                "Err".into(),
                smallvec::smallvec![val],
            ),
            Span::new(0, 18),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(variant).await.unwrap();
    match result {
        Value::Tagged(ty_expr, idx, payloads) => {
            let base = interp.type_exprs.base_type(ty_expr);
            assert_eq!(base, Some(crate::value::TypeId::RESULT));
            assert_eq!(idx, 1); // Err is index 1
            assert_eq!(payloads.len(), 1);
        }
        _ => panic!("expected Tagged"),
    }
}

#[tokio::test]
async fn variant_unknown_type_error() {
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(10, 11))
        .unwrap();
    let variant = ast
        .add_expr(
            Expr::Variant(
                "Unknown".into(),
                "Foo".into(),
                smallvec::smallvec![val],
            ),
            Span::new(0, 12),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(variant).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn variant_unknown_variant_error() {
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(10, 11))
        .unwrap();
    let variant = ast
        .add_expr(
            Expr::Variant(
                "Option".into(),
                "Foo".into(),
                smallvec::smallvec![val],
            ),
            Span::new(0, 12),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(variant).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn variant_arity_mismatch_error() {
    // Option.Some expects 1 arg, giving 0
    let mut ast = Ast::new();
    let variant = ast
        .add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![],
            ),
            Span::new(0, 11),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(variant).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn variant_some_requires_args() {
    // Accessing Option.Some without args (as field) is an error
    let mut ast = Ast::new();
    let base = ast
        .add_expr(Expr::Var("Option".into()), Span::new(0, 6))
        .unwrap();
    let field = ast
        .add_expr(Expr::Field(base, "Some".into()), Span::new(0, 11))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(field).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn optional_field_on_object() {
    // { x: 42 }?.x -> Option.Some(42)
    let mut ast = Ast::new();
    let v = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(6, 8))
        .unwrap();
    let obj = ast
        .add_expr(Expr::Object(vec![("x".into(), v)]), Span::new(0, 10))
        .unwrap();
    let opt_field = ast
        .add_expr(Expr::OptionalField(obj, "x".into()), Span::new(0, 13))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(opt_field).await.unwrap();
    assert!(result.is_some(&interp.type_exprs));
    // Unwrap the Some to get 42
    match result {
        Value::Tagged(_, 1, payloads) => {
            let inner = interp.arena.get(payloads[0]).unwrap();
            assert_eq!(*inner, Value::Int(42));
        }
        _ => panic!("expected Option.Some"),
    }
}

#[tokio::test]
async fn optional_field_on_none() {
    // Option.None?.x -> Option.None
    let mut ast = Ast::new();
    let none = ast
        .add_expr(
            Expr::Variant("Option".into(), "None".into(), smallvec![]),
            Span::new(0, 11),
        )
        .unwrap();
    let opt_field = ast
        .add_expr(Expr::OptionalField(none, "x".into()), Span::new(0, 14))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(opt_field).await.unwrap();
    assert!(result.is_none(&interp.type_exprs));
}

#[tokio::test]
async fn optional_field_on_some_with_object() {
    // Option.Some({ x: 99 })?.x -> Option.Some(99)
    let mut ast = Ast::new();
    let v = ast
        .add_expr(Expr::Literal(Literal::Int(99)), Span::new(20, 22))
        .unwrap();
    let obj = ast
        .add_expr(Expr::Object(vec![("x".into(), v)]), Span::new(12, 24))
        .unwrap();
    let some = ast
        .add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![obj],
            ),
            Span::new(0, 25),
        )
        .unwrap();
    let opt_field = ast
        .add_expr(Expr::OptionalField(some, "x".into()), Span::new(0, 28))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(opt_field).await.unwrap();
    assert!(result.is_some(&interp.type_exprs));
    match result {
        Value::Tagged(_, 1, payloads) => {
            let inner = interp.arena.get(payloads[0]).unwrap();
            assert_eq!(*inner, Value::Int(99));
        }
        _ => panic!("expected Option.Some"),
    }
}

#[tokio::test]
async fn optional_field_missing_field() {
    // { x: 42 }?.y -> error (field not found)
    let mut ast = Ast::new();
    let v = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(6, 8))
        .unwrap();
    let obj = ast
        .add_expr(Expr::Object(vec![("x".into(), v)]), Span::new(0, 10))
        .unwrap();
    let opt_field = ast
        .add_expr(Expr::OptionalField(obj, "y".into()), Span::new(0, 13))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(opt_field).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn optional_field_on_non_object() {
    // 42?.x -> type error
    let mut ast = Ast::new();
    let num = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2))
        .unwrap();
    let opt_field = ast
        .add_expr(Expr::OptionalField(num, "x".into()), Span::new(0, 5))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(opt_field).await;
    assert!(result.is_err());
}

// ===== `is` operator tests =====

#[tokio::test]
async fn is_simple_type_int() {
    // 42 is Int -> true
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2))
        .unwrap();
    let is_expr = ast
        .add_expr(
            Expr::Is(val, TypePattern::Type("Int".into())),
            Span::new(0, 8),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(is_expr).await.unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[tokio::test]
async fn is_simple_type_mismatch() {
    // 42 is String -> false
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2))
        .unwrap();
    let is_expr = ast
        .add_expr(
            Expr::Is(val, TypePattern::Type("String".into())),
            Span::new(0, 11),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(is_expr).await.unwrap();
    assert_eq!(result, Value::Bool(false));
}

#[tokio::test]
async fn is_variant_none() {
    // Option.None is Option.None -> true
    let mut ast = Ast::new();
    let none = ast
        .add_expr(
            Expr::Variant("Option".into(), "None".into(), smallvec![]),
            Span::new(0, 11),
        )
        .unwrap();
    let is_expr = ast
        .add_expr(
            Expr::Is(
                none,
                TypePattern::Variant("Option".into(), "None".into()),
            ),
            Span::new(0, 26),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(is_expr).await.unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[tokio::test]
async fn is_variant_some_wildcard() {
    // Option.Some(42) is Option.Some(_) -> true
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(12, 14))
        .unwrap();
    let some = ast
        .add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![val],
            ),
            Span::new(0, 15),
        )
        .unwrap();
    let is_expr = ast
        .add_expr(
            Expr::Is(
                some,
                TypePattern::VariantWildcard("Option".into(), "Some".into()),
            ),
            Span::new(0, 30),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(is_expr).await.unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[tokio::test]
async fn is_variant_mismatch() {
    // Option.None is Option.Some(_) -> false
    let mut ast = Ast::new();
    let none = ast
        .add_expr(
            Expr::Variant("Option".into(), "None".into(), smallvec![]),
            Span::new(0, 11),
        )
        .unwrap();
    let is_expr = ast
        .add_expr(
            Expr::Is(
                none,
                TypePattern::VariantWildcard("Option".into(), "Some".into()),
            ),
            Span::new(0, 26),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(is_expr).await.unwrap();
    assert_eq!(result, Value::Bool(false));
}

#[tokio::test]
async fn is_variant_bind_in_if() {
    // IF Option.Some(42) is Option.Some(val) { val } ELSE { 0 }
    // -> 42
    let mut ast = Ast::new();

    // Option.Some(42)
    let forty_two = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(12, 14))
        .unwrap();
    let some = ast
        .add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![forty_two],
            ),
            Span::new(0, 15),
        )
        .unwrap();

    // is Option.Some(val)
    let is_expr = ast
        .add_expr(
            Expr::Is(
                some,
                TypePattern::VariantBind(
                    "Option".into(),
                    "Some".into(),
                    smallvec::smallvec!["val".into()],
                ),
            ),
            Span::new(0, 35),
        )
        .unwrap();

    // then: { val }
    let val_ref = ast
        .add_expr(Expr::Var("val".into()), Span::new(38, 41))
        .unwrap();
    let then_blk = ast
        .add_expr(Expr::Block(vec![], Some(val_ref)), Span::new(37, 43))
        .unwrap();

    // else: { 0 }
    let zero = ast
        .add_expr(Expr::Literal(Literal::Int(0)), Span::new(51, 52))
        .unwrap();
    let else_blk = ast
        .add_expr(Expr::Block(vec![], Some(zero)), Span::new(50, 54))
        .unwrap();

    // IF
    let if_expr = ast
        .add_expr(
            Expr::If(is_expr, then_blk, Some(else_blk)),
            Span::new(0, 54),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(if_expr).await.unwrap();
    assert_eq!(result, Value::Int(42));
}

#[tokio::test]
async fn is_variant_bind_else_branch() {
    // IF Option.None is Option.Some(val) { val } ELSE { 99 }
    // -> 99 (bindings not visible in else)
    let mut ast = Ast::new();

    // Option.None (resolved to Variant)
    let none = ast
        .add_expr(
            Expr::Variant("Option".into(), "None".into(), smallvec![]),
            Span::new(3, 14),
        )
        .unwrap();

    // is Option.Some(val)
    let is_expr = ast
        .add_expr(
            Expr::Is(
                none,
                TypePattern::VariantBind(
                    "Option".into(),
                    "Some".into(),
                    smallvec::smallvec!["val".into()],
                ),
            ),
            Span::new(0, 35),
        )
        .unwrap();

    // then: { val }
    let val_ref = ast
        .add_expr(Expr::Var("val".into()), Span::new(38, 41))
        .unwrap();
    let then_blk = ast
        .add_expr(Expr::Block(vec![], Some(val_ref)), Span::new(37, 43))
        .unwrap();

    // else: { 99 }
    let ninety_nine = ast
        .add_expr(Expr::Literal(Literal::Int(99)), Span::new(51, 53))
        .unwrap();
    let else_blk = ast
        .add_expr(Expr::Block(vec![], Some(ninety_nine)), Span::new(50, 55))
        .unwrap();

    // IF
    let if_expr = ast
        .add_expr(
            Expr::If(is_expr, then_blk, Some(else_blk)),
            Span::new(0, 55),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(if_expr).await.unwrap();
    assert_eq!(result, Value::Int(99));
}

#[tokio::test]
async fn is_variant_bind_scope_isolated() {
    // `val` should NOT be visible after the IF
    // IF Option.Some(42) is Option.Some(val) { val } ELSE { 0 }
    // val  // should error
    let mut ast = Ast::new();

    // Option.Some(42)
    let forty_two = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(12, 14))
        .unwrap();
    let some = ast
        .add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![forty_two],
            ),
            Span::new(0, 15),
        )
        .unwrap();

    // is Option.Some(val)
    let is_expr = ast
        .add_expr(
            Expr::Is(
                some,
                TypePattern::VariantBind(
                    "Option".into(),
                    "Some".into(),
                    smallvec::smallvec!["val".into()],
                ),
            ),
            Span::new(0, 35),
        )
        .unwrap();

    // then: { val }
    let val_ref1 = ast
        .add_expr(Expr::Var("val".into()), Span::new(38, 41))
        .unwrap();
    let then_blk = ast
        .add_expr(Expr::Block(vec![], Some(val_ref1)), Span::new(37, 43))
        .unwrap();

    // else: { 0 }
    let zero = ast
        .add_expr(Expr::Literal(Literal::Int(0)), Span::new(51, 52))
        .unwrap();
    let else_blk = ast
        .add_expr(Expr::Block(vec![], Some(zero)), Span::new(50, 54))
        .unwrap();

    // IF
    let if_expr = ast
        .add_expr(
            Expr::If(is_expr, then_blk, Some(else_blk)),
            Span::new(0, 54),
        )
        .unwrap();
    let if_stmt = ast.add_stmt(Stmt::Expr(if_expr), Span::new(0, 54)).unwrap();

    // val (after if)
    let val_ref2 = ast
        .add_expr(Expr::Var("val".into()), Span::new(56, 59))
        .unwrap();

    let mut interp = test_interp(&ast);
    interp.exec(if_stmt).await.unwrap();
    // val should NOT be visible
    let result = interp.eval(val_ref2).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn is_unknown_type_error() {
    // 42 is Unknown -> error
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2))
        .unwrap();
    let is_expr = ast
        .add_expr(
            Expr::Is(val, TypePattern::Type("Unknown".into())),
            Span::new(0, 12),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(is_expr).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn is_result_ok() {
    // Result.Ok(1) is Result.Ok(_) -> true
    let mut ast = Ast::new();
    let one = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(10, 11))
        .unwrap();
    let ok = ast
        .add_expr(
            Expr::Variant(
                "Result".into(),
                "Ok".into(),
                smallvec::smallvec![one],
            ),
            Span::new(0, 12),
        )
        .unwrap();
    let is_expr = ast
        .add_expr(
            Expr::Is(
                ok,
                TypePattern::VariantWildcard("Result".into(), "Ok".into()),
            ),
            Span::new(0, 25),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(is_expr).await.unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[tokio::test]
async fn is_result_err() {
    // Result.Err("oops") is Result.Ok(_) -> false
    let mut ast = Ast::new();
    let msg = ast
        .add_expr(
            Expr::Literal(Literal::String("oops".into())),
            Span::new(11, 17),
        )
        .unwrap();
    let err = ast
        .add_expr(
            Expr::Variant(
                "Result".into(),
                "Err".into(),
                smallvec::smallvec![msg],
            ),
            Span::new(0, 18),
        )
        .unwrap();
    let is_expr = ast
        .add_expr(
            Expr::Is(
                err,
                TypePattern::VariantWildcard("Result".into(), "Ok".into()),
            ),
            Span::new(0, 30),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(is_expr).await.unwrap();
    assert_eq!(result, Value::Bool(false));
}

#[tokio::test]
async fn is_variant_with_payload_requires_parens() {
    // `is Option.Some` without parens is an error (must use `(_)` or `(name)`)
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(12, 14))
        .unwrap();
    let some = ast
        .add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![val],
            ),
            Span::new(0, 15),
        )
        .unwrap();
    let is_expr = ast
        .add_expr(
            Expr::Is(
                some,
                TypePattern::Variant("Option".into(), "Some".into()),
            ),
            Span::new(0, 30),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(is_expr).await;
    assert!(result.is_err());
}

// ===== Structural type equality tests =====

#[tokio::test]
async fn tagged_values_structural_equality() {
    // Two Option.Some(1) values created separately should be equal,
    // even though they have different TypeExprIds.
    let mut ast = Ast::new();
    let one_a = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(12, 13))
        .unwrap();
    let some_a = ast
        .add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![one_a],
            ),
            Span::new(0, 14),
        )
        .unwrap();
    let one_b = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(32, 33))
        .unwrap();
    let some_b = ast
        .add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![one_b],
            ),
            Span::new(20, 34),
        )
        .unwrap();
    let eq_expr = ast
        .add_expr(Expr::Binary(some_a, BinOp::Eq, some_b), Span::new(0, 40))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(eq_expr).await.unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[tokio::test]
async fn tagged_values_different_payloads_not_equal() {
    // Option.Some(1) != Option.Some(2)
    let mut ast = Ast::new();
    let one = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(12, 13))
        .unwrap();
    let some_one = ast
        .add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![one],
            ),
            Span::new(0, 14),
        )
        .unwrap();
    let two = ast
        .add_expr(Expr::Literal(Literal::Int(2)), Span::new(32, 33))
        .unwrap();
    let some_two = ast
        .add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![two],
            ),
            Span::new(20, 34),
        )
        .unwrap();
    let eq_expr = ast
        .add_expr(
            Expr::Binary(some_one, BinOp::Eq, some_two),
            Span::new(0, 40),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(eq_expr).await.unwrap();
    assert_eq!(result, Value::Bool(false));
}

// ===== Type cast (as) tests =====

#[tokio::test]
async fn as_int_to_float() {
    // 42 as Float -> 42.0
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2))
        .unwrap();
    let ty = ast
        .add_type_expr(AstTypeExpr::Named("Float".into()), Span::new(6, 11))
        .unwrap();
    let cast = ast.add_expr(Expr::As(val, ty), Span::new(0, 11)).unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(cast).await.unwrap();
    assert_eq!(result, Value::Float(OrderedFloat(42.0)));
}

#[tokio::test]
async fn as_float_to_int_truncates() {
    // 3.7 as Int -> 3
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Float(3.7)), Span::new(0, 3))
        .unwrap();
    let ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(7, 10))
        .unwrap();
    let cast = ast.add_expr(Expr::As(val, ty), Span::new(0, 10)).unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(cast).await.unwrap();
    assert_eq!(result, Value::Int(3));
}

#[tokio::test]
async fn as_bool_to_int() {
    // true as Int -> 1
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), Span::new(0, 4))
        .unwrap();
    let ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(8, 11))
        .unwrap();
    let cast = ast.add_expr(Expr::As(val, ty), Span::new(0, 11)).unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(cast).await.unwrap();
    assert_eq!(result, Value::Int(1));

    // false as Int -> 0
    let mut ast2 = Ast::new();
    let val2 = ast2
        .add_expr(Expr::Literal(Literal::Bool(false)), Span::new(0, 5))
        .unwrap();
    let ty2 = ast2
        .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(9, 12))
        .unwrap();
    let cast2 = ast2
        .add_expr(Expr::As(val2, ty2), Span::new(0, 12))
        .unwrap();

    let mut interp2 = test_interp(&ast2);
    let result2 = interp2.eval(cast2).await.unwrap();
    assert_eq!(result2, Value::Int(0));
}

#[tokio::test]
async fn as_int_to_string() {
    // 42 as String -> "42"
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2))
        .unwrap();
    let ty = ast
        .add_type_expr(AstTypeExpr::Named("String".into()), Span::new(6, 12))
        .unwrap();
    let cast = ast.add_expr(Expr::As(val, ty), Span::new(0, 12)).unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(cast).await.unwrap();
    match result {
        Value::String(id) => {
            assert_eq!(interp.arena.get_str(id), Some("42"));
        }
        _ => panic!("expected String"),
    }
}

#[tokio::test]
async fn as_float_to_string() {
    // 3.14 as String -> "3.14"
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Float(3.14)), Span::new(0, 4))
        .unwrap();
    let ty = ast
        .add_type_expr(AstTypeExpr::Named("String".into()), Span::new(8, 14))
        .unwrap();
    let cast = ast.add_expr(Expr::As(val, ty), Span::new(0, 14)).unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(cast).await.unwrap();
    match result {
        Value::String(id) => {
            assert_eq!(interp.arena.get_str(id), Some("3.14"));
        }
        _ => panic!("expected String"),
    }
}

#[tokio::test]
async fn as_bool_to_string() {
    // true as String -> "TRUE"
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), Span::new(0, 4))
        .unwrap();
    let ty = ast
        .add_type_expr(AstTypeExpr::Named("String".into()), Span::new(8, 14))
        .unwrap();
    let cast = ast.add_expr(Expr::As(val, ty), Span::new(0, 14)).unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(cast).await.unwrap();
    match result {
        Value::String(id) => {
            assert_eq!(interp.arena.get_str(id), Some("TRUE"));
        }
        _ => panic!("expected String"),
    }
}

#[tokio::test]
async fn as_identity_int() {
    // 42 as Int -> 42 (identity)
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2))
        .unwrap();
    let ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(6, 9))
        .unwrap();
    let cast = ast.add_expr(Expr::As(val, ty), Span::new(0, 9)).unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(cast).await.unwrap();
    assert_eq!(result, Value::Int(42));
}

#[tokio::test]
async fn as_unsupported_conversion_error() {
    // "hello" as Int -> error (use `read` for fallible conversions)
    let mut ast = Ast::new();
    let val = ast
        .add_expr(
            Expr::Literal(Literal::String("hello".into())),
            Span::new(0, 7),
        )
        .unwrap();
    let ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(11, 14))
        .unwrap();
    let cast = ast.add_expr(Expr::As(val, ty), Span::new(0, 14)).unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(cast).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("cannot cast"));
}

// ===== Fallible conversion (read) tests =====

#[tokio::test]
async fn read_string_to_int_ok() {
    // "42" read Int -> Result.Ok(42)
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::String("42".into())), Span::new(0, 4))
        .unwrap();
    let ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(10, 13))
        .unwrap();
    let read = ast.add_expr(Expr::Read(val, ty), Span::new(0, 13)).unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(read).await.unwrap();

    // Should be Result.Ok(42)
    assert!(result.is_ok(&interp.type_exprs));
    match &result {
        Value::Tagged(_, 0, payload) => {
            let inner = interp.arena.get(payload[0]).unwrap();
            assert_eq!(inner, &Value::Int(42));
        }
        _ => panic!("expected Result.Ok"),
    }
}

#[tokio::test]
async fn read_string_to_int_err() {
    // "abc" read Int -> Result.Err("invalid integer: abc")
    let mut ast = Ast::new();
    let val = ast
        .add_expr(
            Expr::Literal(Literal::String("abc".into())),
            Span::new(0, 5),
        )
        .unwrap();
    let ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(11, 14))
        .unwrap();
    let read = ast.add_expr(Expr::Read(val, ty), Span::new(0, 14)).unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(read).await.unwrap();

    // Should be Result.Err
    assert!(result.is_err(&interp.type_exprs));
    match &result {
        Value::Tagged(_, 1, payload) => {
            let inner = interp.arena.get(payload[0]).unwrap();
            match inner {
                Value::String(sid) => {
                    let msg = interp.arena.get_str(*sid).unwrap();
                    assert!(msg.contains("invalid integer"));
                }
                _ => panic!("expected error message string"),
            }
        }
        _ => panic!("expected Result.Err"),
    }
}

#[tokio::test]
async fn read_string_to_float_ok() {
    // "3.14" read Float -> Result.Ok(3.14)
    let mut ast = Ast::new();
    let val = ast
        .add_expr(
            Expr::Literal(Literal::String("3.14".into())),
            Span::new(0, 6),
        )
        .unwrap();
    let ty = ast
        .add_type_expr(AstTypeExpr::Named("Float".into()), Span::new(12, 17))
        .unwrap();
    let read = ast.add_expr(Expr::Read(val, ty), Span::new(0, 17)).unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(read).await.unwrap();

    // Should be Result.Ok(3.14)
    assert!(result.is_ok(&interp.type_exprs));
    match &result {
        Value::Tagged(_, 0, payload) => {
            let inner = interp.arena.get(payload[0]).unwrap();
            assert_eq!(inner, &Value::Float(OrderedFloat(3.14)));
        }
        _ => panic!("expected Result.Ok"),
    }
}

#[tokio::test]
async fn read_string_to_float_err() {
    // "xyz" read Float -> Result.Err(...)
    let mut ast = Ast::new();
    let val = ast
        .add_expr(
            Expr::Literal(Literal::String("xyz".into())),
            Span::new(0, 5),
        )
        .unwrap();
    let ty = ast
        .add_type_expr(AstTypeExpr::Named("Float".into()), Span::new(11, 16))
        .unwrap();
    let read = ast.add_expr(Expr::Read(val, ty), Span::new(0, 16)).unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(read).await.unwrap();

    // Should be Result.Err
    assert!(result.is_err(&interp.type_exprs));
}

#[tokio::test]
async fn read_int_to_bool_zero() {
    // 0 read Bool -> Result.Ok(false)
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(0)), Span::new(0, 1))
        .unwrap();
    let ty = ast
        .add_type_expr(AstTypeExpr::Named("Bool".into()), Span::new(7, 11))
        .unwrap();
    let read = ast.add_expr(Expr::Read(val, ty), Span::new(0, 11)).unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(read).await.unwrap();

    assert!(result.is_ok(&interp.type_exprs));
    match &result {
        Value::Tagged(_, 0, payload) => {
            let inner = interp.arena.get(payload[0]).unwrap();
            assert_eq!(inner, &Value::Bool(false));
        }
        _ => panic!("expected Result.Ok"),
    }
}

#[tokio::test]
async fn read_int_to_bool_one() {
    // 1 read Bool -> Result.Ok(true)
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(0, 1))
        .unwrap();
    let ty = ast
        .add_type_expr(AstTypeExpr::Named("Bool".into()), Span::new(7, 11))
        .unwrap();
    let read = ast.add_expr(Expr::Read(val, ty), Span::new(0, 11)).unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(read).await.unwrap();

    assert!(result.is_ok(&interp.type_exprs));
    match &result {
        Value::Tagged(_, 0, payload) => {
            let inner = interp.arena.get(payload[0]).unwrap();
            assert_eq!(inner, &Value::Bool(true));
        }
        _ => panic!("expected Result.Ok"),
    }
}

#[tokio::test]
async fn read_int_to_bool_invalid() {
    // 42 read Bool -> Result.Err(...)
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2))
        .unwrap();
    let ty = ast
        .add_type_expr(AstTypeExpr::Named("Bool".into()), Span::new(8, 12))
        .unwrap();
    let read = ast.add_expr(Expr::Read(val, ty), Span::new(0, 12)).unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(read).await.unwrap();

    // Should be Result.Err
    assert!(result.is_err(&interp.type_exprs));
    match &result {
        Value::Tagged(_, 1, payload) => {
            let inner = interp.arena.get(payload[0]).unwrap();
            match inner {
                Value::String(sid) => {
                    let msg = interp.arena.get_str(*sid).unwrap();
                    assert!(msg.contains("expected 0 or 1"));
                }
                _ => panic!("expected error message string"),
            }
        }
        _ => panic!("expected Result.Err"),
    }
}

#[tokio::test]
async fn read_unsupported_conversion_error() {
    // 42 read Int -> runtime error (not a fallible conversion)
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2))
        .unwrap();
    let ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(8, 11))
        .unwrap();
    let read = ast.add_expr(Expr::Read(val, ty), Span::new(0, 11)).unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(read).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("cannot read"));
}

// ---- Closure tests ----

#[tokio::test]
async fn closure_creation_simple() {
    // x => x * 2
    let mut ast = Ast::new();
    let x = ast
        .add_expr(Expr::Var("x".into()), Span::new(5, 6))
        .unwrap();
    let two = ast
        .add_expr(Expr::Literal(Literal::Int(2)), Span::new(9, 10))
        .unwrap();
    let body = ast
        .add_expr(Expr::Binary(x, BinOp::Mul, two), Span::new(5, 10))
        .unwrap();
    let closure = ast
        .add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body,
            },
            Span::new(0, 10),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(closure).await.unwrap();

    match result {
        Value::Closure { params, ret, .. } => {
            assert_eq!(params.len(), 1);
            assert!(ret.is_none());
        }
        _ => panic!("expected Closure"),
    }
}

#[tokio::test]
async fn closure_creation_with_types() {
    // (x: Int) -> Int => x * x
    let mut ast = Ast::new();
    let int_ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(4, 7))
        .unwrap();
    let ret_ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(12, 15))
        .unwrap();
    let x = ast
        .add_expr(Expr::Var("x".into()), Span::new(19, 20))
        .unwrap();
    let x2 = ast
        .add_expr(Expr::Var("x".into()), Span::new(23, 24))
        .unwrap();
    let body = ast
        .add_expr(Expr::Binary(x, BinOp::Mul, x2), Span::new(19, 24))
        .unwrap();
    let closure = ast
        .add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), Some(int_ty))],
                ret: Some(ret_ty),
                body,
            },
            Span::new(0, 24),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(closure).await.unwrap();

    match result {
        Value::Closure { params, ret, .. } => {
            assert_eq!(params.len(), 1);
            assert!(params[0].1.is_some()); // has type annotation
            assert!(ret.is_some()); // has return type
        }
        _ => panic!("expected Closure"),
    }
}

#[tokio::test]
async fn closure_captures_environment() {
    // LET factor = 3
    // LET triple = x => x * factor
    // triple is a closure that captures `factor`
    let mut ast = Ast::new();
    let three = ast
        .add_expr(Expr::Literal(Literal::Int(3)), Span::new(13, 14))
        .unwrap();
    let let_factor = ast
        .add_stmt(Stmt::Let("factor".into(), None, three), Span::new(0, 14))
        .unwrap();

    let x = ast
        .add_expr(Expr::Var("x".into()), Span::new(25, 26))
        .unwrap();
    let factor = ast
        .add_expr(Expr::Var("factor".into()), Span::new(29, 35))
        .unwrap();
    let body = ast
        .add_expr(Expr::Binary(x, BinOp::Mul, factor), Span::new(25, 35))
        .unwrap();
    let closure = ast
        .add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body,
            },
            Span::new(16, 35),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    interp.exec(let_factor).await.unwrap();
    let result = interp.eval(closure).await.unwrap();

    match &result {
        Value::Closure { env, .. } => {
            // The closure should have captured `factor`
            let factor_id = interp.arena.lookup_string("factor").unwrap();
            assert!(env.lookup(factor_id).is_some());
        }
        _ => panic!("expected Closure"),
    }
}

#[tokio::test]
async fn closure_display() {
    // Closure displays as <closure(n)>
    let mut ast = Ast::new();
    let body = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(5, 7))
        .unwrap();
    let closure = ast
        .add_expr(
            Expr::Closure {
                params: smallvec::smallvec![
                    ("x".into(), None),
                    ("y".into(), None)
                ],
                ret: None,
                body,
            },
            Span::new(0, 7),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(closure).await.unwrap();
    let displayed = interp.display(&result);
    assert_eq!(displayed, "<closure(2)>");
}

// ---- FUN statement tests ----

#[tokio::test]
async fn fun_definition_simple() {
    // FUN double (x) { x * 2 }
    let mut ast = Ast::new();
    let x = ast
        .add_expr(Expr::Var("x".into()), Span::new(17, 18))
        .unwrap();
    let two = ast
        .add_expr(Expr::Literal(Literal::Int(2)), Span::new(21, 22))
        .unwrap();
    let body_expr = ast
        .add_expr(Expr::Binary(x, BinOp::Mul, two), Span::new(17, 22))
        .unwrap();
    let body = ast
        .add_expr(Expr::Block(vec![], Some(body_expr)), Span::new(15, 24))
        .unwrap();
    let fun = ast
        .add_stmt(
            Stmt::Fun {
                name: "double".into(),
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body,
            },
            Span::new(0, 24),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    interp.exec(fun).await.unwrap();

    // Function should be registered
    let name_id = interp.arena.intern("double");
    assert!(interp.functions.contains_key(&name_id));
}

#[tokio::test]
async fn fun_call_simple() {
    // FUN double (x) { x * 2 }
    // double(21)
    let mut ast = Ast::new();

    // Function body: x * 2
    let x = ast
        .add_expr(Expr::Var("x".into()), Span::new(17, 18))
        .unwrap();
    let two = ast
        .add_expr(Expr::Literal(Literal::Int(2)), Span::new(21, 22))
        .unwrap();
    let body_expr = ast
        .add_expr(Expr::Binary(x, BinOp::Mul, two), Span::new(17, 22))
        .unwrap();
    let body = ast
        .add_expr(Expr::Block(vec![], Some(body_expr)), Span::new(15, 24))
        .unwrap();
    let fun = ast
        .add_stmt(
            Stmt::Fun {
                name: "double".into(),
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body,
            },
            Span::new(0, 24),
        )
        .unwrap();

    // Call: double(21)
    let arg = ast
        .add_expr(Expr::Literal(Literal::Int(21)), Span::new(32, 34))
        .unwrap();
    let callee = ast
        .add_expr(Expr::Var("double".into()), Span::new(26, 32))
        .unwrap();
    let call = ast
        .add_expr(
            Expr::Call(callee, smallvec::smallvec![arg]),
            Span::new(26, 35),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    interp.exec(fun).await.unwrap();
    let result = interp.eval(call).await.unwrap();

    assert_eq!(result, Value::Int(42));
}

#[tokio::test]
async fn fun_as_value() {
    // FUN square (x) { x * x }
    // LET f = square
    // f should be a Value::Function
    let mut ast = Ast::new();

    let x1 = ast
        .add_expr(Expr::Var("x".into()), Span::new(17, 18))
        .unwrap();
    let x2 = ast
        .add_expr(Expr::Var("x".into()), Span::new(21, 22))
        .unwrap();
    let body_expr = ast
        .add_expr(Expr::Binary(x1, BinOp::Mul, x2), Span::new(17, 22))
        .unwrap();
    let body = ast
        .add_expr(Expr::Block(vec![], Some(body_expr)), Span::new(15, 24))
        .unwrap();
    let fun = ast
        .add_stmt(
            Stmt::Fun {
                name: "square".into(),
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body,
            },
            Span::new(0, 24),
        )
        .unwrap();

    // Reference: square (no call)
    let square_ref = ast
        .add_expr(Expr::Var("square".into()), Span::new(36, 42))
        .unwrap();

    let mut interp = test_interp(&ast);
    interp.exec(fun).await.unwrap();
    let result = interp.eval(square_ref).await.unwrap();

    assert!(matches!(result, Value::Function { .. }));
}

#[tokio::test]
async fn fun_recursive_factorial() {
    // FUN factorial (n) {
    //   IF n <= 1 { 1 }
    //   ELSE { n * factorial(n - 1) }
    // }
    // factorial(5) should be 120
    let mut ast = Ast::new();

    // n <= 1
    let n1 = ast
        .add_expr(Expr::Var("n".into()), Span::new(0, 1))
        .unwrap();
    let one1 = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(5, 6))
        .unwrap();
    let cond = ast
        .add_expr(Expr::Binary(n1, BinOp::Le, one1), Span::new(0, 6))
        .unwrap();

    // Then: 1
    let then_expr = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(10, 11))
        .unwrap();
    let then_block = ast
        .add_expr(Expr::Block(vec![], Some(then_expr)), Span::new(8, 12))
        .unwrap();

    // Else: n * factorial(n - 1)
    let n2 = ast
        .add_expr(Expr::Var("n".into()), Span::new(20, 21))
        .unwrap();
    let n3 = ast
        .add_expr(Expr::Var("n".into()), Span::new(35, 36))
        .unwrap();
    let one2 = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(39, 40))
        .unwrap();
    let n_minus_1 = ast
        .add_expr(Expr::Binary(n3, BinOp::Sub, one2), Span::new(35, 40))
        .unwrap();
    let rec_callee = ast
        .add_expr(Expr::Var("factorial".into()), Span::new(24, 33))
        .unwrap();
    let rec_call = ast
        .add_expr(
            Expr::Call(rec_callee, smallvec::smallvec![n_minus_1]),
            Span::new(24, 41),
        )
        .unwrap();
    let else_expr = ast
        .add_expr(Expr::Binary(n2, BinOp::Mul, rec_call), Span::new(20, 41))
        .unwrap();
    let else_block = ast
        .add_expr(Expr::Block(vec![], Some(else_expr)), Span::new(18, 43))
        .unwrap();

    // IF expr
    let if_expr = ast
        .add_expr(
            Expr::If(cond, then_block, Some(else_block)),
            Span::new(0, 43),
        )
        .unwrap();
    let body = ast
        .add_expr(Expr::Block(vec![], Some(if_expr)), Span::new(0, 45))
        .unwrap();

    let fun = ast
        .add_stmt(
            Stmt::Fun {
                name: "factorial".into(),
                params: smallvec::smallvec![("n".into(), None)],
                ret: None,
                body,
            },
            Span::new(0, 50),
        )
        .unwrap();

    // Call: factorial(5)
    let five = ast
        .add_expr(Expr::Literal(Literal::Int(5)), Span::new(60, 61))
        .unwrap();
    let callee = ast
        .add_expr(Expr::Var("factorial".into()), Span::new(52, 61))
        .unwrap();
    let call = ast
        .add_expr(
            Expr::Call(callee, smallvec::smallvec![five]),
            Span::new(52, 62),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    interp.exec(fun).await.unwrap();
    let result = interp.eval(call).await.unwrap();

    assert_eq!(result, Value::Int(120));
}

#[tokio::test]
async fn fun_display() {
    // Function displays as <function name(n)>
    let mut ast = Ast::new();
    let body = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(15, 17))
        .unwrap();
    let body_block = ast
        .add_expr(Expr::Block(vec![], Some(body)), Span::new(13, 19))
        .unwrap();
    let fun = ast
        .add_stmt(
            Stmt::Fun {
                name: "test".into(),
                params: smallvec::smallvec![
                    ("x".into(), None),
                    ("y".into(), None)
                ],
                ret: None,
                body: body_block,
            },
            Span::new(0, 19),
        )
        .unwrap();

    let fun_ref = ast
        .add_expr(Expr::Var("test".into()), Span::new(20, 24))
        .unwrap();

    let mut interp = test_interp(&ast);
    interp.exec(fun).await.unwrap();
    let result = interp.eval(fun_ref).await.unwrap();
    let displayed = interp.display(&result);
    assert_eq!(displayed, "<function test(2)>");
}

// ---- Expression-based callees tests ----

#[tokio::test]
async fn call_field_closure() {
    // LET ops = { inc: x => x + 1 }
    // ops.inc(5) should be 6
    let mut ast = Ast::new();

    // Closure: x => x + 1
    let x = ast
        .add_expr(Expr::Var("x".into()), Span::new(0, 1))
        .unwrap();
    let one = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(4, 5))
        .unwrap();
    let body = ast
        .add_expr(Expr::Binary(x, BinOp::Add, one), Span::new(0, 5))
        .unwrap();
    let closure = ast
        .add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body,
            },
            Span::new(0, 10),
        )
        .unwrap();

    // Object: { inc: closure }
    let obj = ast
        .add_expr(
            Expr::Object(vec![("inc".into(), closure)]),
            Span::new(10, 30),
        )
        .unwrap();

    // LET ops = obj
    let let_ops = ast
        .add_stmt(Stmt::Let("ops".into(), None, obj), Span::new(0, 35))
        .unwrap();

    // ops.inc
    let ops_var = ast
        .add_expr(Expr::Var("ops".into()), Span::new(40, 43))
        .unwrap();
    let field_access = ast
        .add_expr(Expr::Field(ops_var, "inc".into()), Span::new(40, 47))
        .unwrap();

    // ops.inc(5)
    let five = ast
        .add_expr(Expr::Literal(Literal::Int(5)), Span::new(48, 49))
        .unwrap();
    let call = ast
        .add_expr(
            Expr::Call(field_access, smallvec::smallvec![five]),
            Span::new(40, 50),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    interp.exec(let_ops).await.unwrap();
    let result = interp.eval(call).await.unwrap();

    assert_eq!(result, Value::Int(6));
}

#[tokio::test]
async fn call_chained() {
    // FUN make_adder (n) { x => x + n }
    // make_adder(5)(10) should be 15
    let mut ast = Ast::new();

    // Closure body: x + n
    let x = ast
        .add_expr(Expr::Var("x".into()), Span::new(0, 1))
        .unwrap();
    let n = ast
        .add_expr(Expr::Var("n".into()), Span::new(4, 5))
        .unwrap();
    let add_expr = ast
        .add_expr(Expr::Binary(x, BinOp::Add, n), Span::new(0, 5))
        .unwrap();

    // Closure: x => x + n
    let closure = ast
        .add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body: add_expr,
            },
            Span::new(0, 10),
        )
        .unwrap();

    // Function body block containing closure
    let body = ast
        .add_expr(Expr::Block(vec![], Some(closure)), Span::new(0, 15))
        .unwrap();

    // FUN make_adder (n) { ... }
    let fun = ast
        .add_stmt(
            Stmt::Fun {
                name: "make_adder".into(),
                params: smallvec::smallvec![("n".into(), None)],
                ret: None,
                body,
            },
            Span::new(0, 20),
        )
        .unwrap();

    // make_adder(5)
    let five = ast
        .add_expr(Expr::Literal(Literal::Int(5)), Span::new(30, 31))
        .unwrap();
    let callee1 = ast
        .add_expr(Expr::Var("make_adder".into()), Span::new(25, 35))
        .unwrap();
    let call1 = ast
        .add_expr(
            Expr::Call(callee1, smallvec::smallvec![five]),
            Span::new(25, 32),
        )
        .unwrap();

    // make_adder(5)(10)
    let ten = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(33, 35))
        .unwrap();
    let call2 = ast
        .add_expr(
            Expr::Call(call1, smallvec::smallvec![ten]),
            Span::new(25, 36),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    interp.exec(fun).await.unwrap();
    let result = interp.eval(call2).await.unwrap();

    assert_eq!(result, Value::Int(15));
}

#[tokio::test]
async fn call_iife() {
    // (x => x * 2)(21) should be 42
    let mut ast = Ast::new();

    // Closure body: x * 2
    let x = ast
        .add_expr(Expr::Var("x".into()), Span::new(0, 1))
        .unwrap();
    let two = ast
        .add_expr(Expr::Literal(Literal::Int(2)), Span::new(4, 5))
        .unwrap();
    let body = ast
        .add_expr(Expr::Binary(x, BinOp::Mul, two), Span::new(0, 5))
        .unwrap();

    // Closure: x => x * 2
    let closure = ast
        .add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body,
            },
            Span::new(0, 10),
        )
        .unwrap();

    // (closure)(21)
    let arg = ast
        .add_expr(Expr::Literal(Literal::Int(21)), Span::new(12, 14))
        .unwrap();
    let call = ast
        .add_expr(
            Expr::Call(closure, smallvec::smallvec![arg]),
            Span::new(0, 15),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(call).await.unwrap();

    assert_eq!(result, Value::Int(42));
}

// ---- Type checking tests ----

#[tokio::test]
async fn type_check_param_error() {
    // FUN add (a: Int, b: Int) { a + b }
    // add("x", 1) should fail
    let mut ast = Ast::new();

    // Type expression for Int
    let int_ty = ast
        .add_type_expr(
            crate::ast::AstTypeExpr::Named("Int".into()),
            Span::new(0, 3),
        )
        .unwrap();

    // Function body: a + b
    let a = ast
        .add_expr(Expr::Var("a".into()), Span::new(20, 21))
        .unwrap();
    let b = ast
        .add_expr(Expr::Var("b".into()), Span::new(24, 25))
        .unwrap();
    let body_expr = ast
        .add_expr(Expr::Binary(a, BinOp::Add, b), Span::new(20, 25))
        .unwrap();
    let body = ast
        .add_expr(Expr::Block(vec![], Some(body_expr)), Span::new(18, 27))
        .unwrap();

    let fun = ast
        .add_stmt(
            Stmt::Fun {
                name: "add".into(),
                params: smallvec::smallvec![
                    ("a".into(), Some(int_ty)),
                    ("b".into(), Some(int_ty))
                ],
                ret: None,
                body,
            },
            Span::new(0, 30),
        )
        .unwrap();

    // Call: add("x", 1)
    let str_arg = ast
        .add_expr(
            Expr::Literal(Literal::String("x".into())),
            Span::new(35, 38),
        )
        .unwrap();
    let int_arg = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(40, 41))
        .unwrap();
    let callee = ast
        .add_expr(Expr::Var("add".into()), Span::new(32, 35))
        .unwrap();
    let call = ast
        .add_expr(
            Expr::Call(callee, smallvec::smallvec![str_arg, int_arg]),
            Span::new(32, 42),
        )
        .unwrap();

    let mut interp = test_interp(&ast);
    interp.exec(fun).await.unwrap();
    let result = interp.eval(call).await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("expected"));
    assert!(err.to_string().contains("Int"));
}

#[tokio::test]
async fn type_check_return_error() {
    // FUN bad () -> Int { "not an int" }
    // bad() should fail return type check
    let mut ast = Ast::new();

    // Type expression for Int
    let int_ty = ast
        .add_type_expr(
            crate::ast::AstTypeExpr::Named("Int".into()),
            Span::new(0, 3),
        )
        .unwrap();

    // Function body: "not an int"
    let str_lit = ast
        .add_expr(
            Expr::Literal(Literal::String("not an int".into())),
            Span::new(20, 32),
        )
        .unwrap();
    let body = ast
        .add_expr(Expr::Block(vec![], Some(str_lit)), Span::new(18, 34))
        .unwrap();

    let fun = ast
        .add_stmt(
            Stmt::Fun {
                name: "bad".into(),
                params: smallvec::smallvec![],
                ret: Some(int_ty),
                body,
            },
            Span::new(0, 35),
        )
        .unwrap();

    // Call: bad()
    let callee = ast
        .add_expr(Expr::Var("bad".into()), Span::new(40, 43))
        .unwrap();
    let call = ast
        .add_expr(Expr::Call(callee, smallvec::smallvec![]), Span::new(40, 45))
        .unwrap();

    let mut interp = test_interp(&ast);
    interp.exec(fun).await.unwrap();
    let result = interp.eval(call).await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("expected return type"));
}

// ===== Pipeline (|>) tests =====

#[tokio::test]
async fn pipe_with_closure() {
    // 5 |> (x => x * 2) -> 10
    let mut ast = Ast::new();

    let five = ast
        .add_expr(Expr::Literal(Literal::Int(5)), Span::new(0, 1))
        .unwrap();

    // Closure: x => x * 2
    let x = ast
        .add_expr(Expr::Var("x".into()), Span::new(6, 7))
        .unwrap();
    let two = ast
        .add_expr(Expr::Literal(Literal::Int(2)), Span::new(12, 13))
        .unwrap();
    let mul = ast
        .add_expr(Expr::Binary(x, BinOp::Mul, two), Span::new(6, 13))
        .unwrap();
    let closure = ast
        .add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body: mul,
            },
            Span::new(4, 14),
        )
        .unwrap();

    let pipe = ast
        .add_expr(Expr::Binary(five, BinOp::Pipe, closure), Span::new(0, 14))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(pipe).await.unwrap();
    assert_eq!(result, Value::Int(10));
}

#[tokio::test]
async fn pipe_with_named_function() {
    // FUN double (x) { x * 2 }
    // 5 |> double -> 10
    let mut ast = Ast::new();

    // Function body: x * 2
    let x_body = ast
        .add_expr(Expr::Var("x".into()), Span::new(18, 19))
        .unwrap();
    let two = ast
        .add_expr(Expr::Literal(Literal::Int(2)), Span::new(22, 23))
        .unwrap();
    let mul = ast
        .add_expr(Expr::Binary(x_body, BinOp::Mul, two), Span::new(18, 23))
        .unwrap();
    let body = ast
        .add_expr(Expr::Block(vec![], Some(mul)), Span::new(16, 25))
        .unwrap();

    let fun = ast
        .add_stmt(
            Stmt::Fun {
                name: "double".into(),
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body,
            },
            Span::new(0, 26),
        )
        .unwrap();

    // 5 |> double
    let five = ast
        .add_expr(Expr::Literal(Literal::Int(5)), Span::new(30, 31))
        .unwrap();
    let func_ref = ast
        .add_expr(Expr::Var("double".into()), Span::new(35, 41))
        .unwrap();
    let pipe = ast
        .add_expr(Expr::Binary(five, BinOp::Pipe, func_ref), Span::new(30, 41))
        .unwrap();

    let mut interp = test_interp(&ast);
    interp.exec(fun).await.unwrap();
    let result = interp.eval(pipe).await.unwrap();
    assert_eq!(result, Value::Int(10));
}

#[tokio::test]
async fn pipe_chain() {
    // 5 |> (x => x * 2) |> (x => x + 1) -> 11
    let mut ast = Ast::new();

    let five = ast
        .add_expr(Expr::Literal(Literal::Int(5)), Span::new(0, 1))
        .unwrap();

    // Closure 1: x => x * 2
    let x1 = ast
        .add_expr(Expr::Var("x".into()), Span::new(6, 7))
        .unwrap();
    let two = ast
        .add_expr(Expr::Literal(Literal::Int(2)), Span::new(12, 13))
        .unwrap();
    let mul = ast
        .add_expr(Expr::Binary(x1, BinOp::Mul, two), Span::new(6, 13))
        .unwrap();
    let c1 = ast
        .add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body: mul,
            },
            Span::new(4, 14),
        )
        .unwrap();

    // Closure 2: x => x + 1
    let x2 = ast
        .add_expr(Expr::Var("x".into()), Span::new(22, 23))
        .unwrap();
    let one = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(28, 29))
        .unwrap();
    let add = ast
        .add_expr(Expr::Binary(x2, BinOp::Add, one), Span::new(22, 29))
        .unwrap();
    let c2 = ast
        .add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body: add,
            },
            Span::new(20, 30),
        )
        .unwrap();

    // (5 |> c1) |> c2
    let p1 = ast
        .add_expr(Expr::Binary(five, BinOp::Pipe, c1), Span::new(0, 15))
        .unwrap();
    let p2 = ast
        .add_expr(Expr::Binary(p1, BinOp::Pipe, c2), Span::new(0, 31))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(p2).await.unwrap();
    assert_eq!(result, Value::Int(11));
}

#[tokio::test]
async fn pipe_non_function_error() {
    // 5 |> 10 -> error (10 is not a function)
    let mut ast = Ast::new();

    let five = ast
        .add_expr(Expr::Literal(Literal::Int(5)), Span::new(0, 1))
        .unwrap();
    let ten = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(5, 7))
        .unwrap();
    let pipe = ast
        .add_expr(Expr::Binary(five, BinOp::Pipe, ten), Span::new(0, 7))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(pipe).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("|>"));
    assert!(err.to_string().contains("function"));
}

#[tokio::test]
async fn pipe_arity_mismatch_error() {
    // 5 |> ((a, b) => a + b) -> error (arity mismatch)
    let mut ast = Ast::new();

    let five = ast
        .add_expr(Expr::Literal(Literal::Int(5)), Span::new(0, 1))
        .unwrap();

    // Closure: (a, b) => a + b (expects 2 args)
    let a = ast
        .add_expr(Expr::Var("a".into()), Span::new(12, 13))
        .unwrap();
    let b = ast
        .add_expr(Expr::Var("b".into()), Span::new(16, 17))
        .unwrap();
    let add = ast
        .add_expr(Expr::Binary(a, BinOp::Add, b), Span::new(12, 17))
        .unwrap();
    let closure = ast
        .add_expr(
            Expr::Closure {
                params: smallvec::smallvec![
                    ("a".into(), None),
                    ("b".into(), None)
                ],
                ret: None,
                body: add,
            },
            Span::new(5, 18),
        )
        .unwrap();

    let pipe = ast
        .add_expr(Expr::Binary(five, BinOp::Pipe, closure), Span::new(0, 18))
        .unwrap();

    let mut interp = test_interp(&ast);
    let result = interp.eval(pipe).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("expected 2 arguments"));
}
