use smallvec::{smallvec, SmallVec};

use super::*;
use crate::ast::{
    Ast, AstTypeExpr, AstTypeExprId, BinOp, BindingPattern, Expr, Literal,
    MatchArm, MatchPattern, RestPattern, Stmt, StmtId, TypePattern, UnOp,
};
use crate::value::TypeId;

/// Test helper: create minimal context components for testing.
///
/// Returns context state without needing full `InferCtx` construction,
/// since `TypeRegistry::new` requires mutable arenas.
struct TestState {
    env: TypeEnv,
    constraints: Vec<Constraint>,
    next_var: u32,
    expr_types: HashMap<ExprId, Ty>,
    errors: Vec<TypeError>,
}

impl TestState {
    fn new() -> Self {
        Self {
            env: TypeEnv::new(StringInterner::new()),
            constraints: Vec::new(),
            next_var: 0,
            expr_types: HashMap::new(),
            errors: Vec::new(),
        }
    }

    fn with_next_var(mut self, n: u32) -> Self {
        self.next_var = n;
        self
    }

    fn fresh_var(&mut self) -> TyVar {
        let v = TyVar::new(self.next_var);
        self.next_var += 1;
        v
    }

    fn fresh(&mut self) -> Ty {
        Ty::Var(self.fresh_var())
    }

    fn constrain(&mut self, c: Constraint) {
        self.constraints.push(c);
    }

    fn unify(&mut self, t1: Ty, t2: Ty, span: Span) {
        self.constrain(Constraint::Eq(t1, t2, span));
    }

    fn error(&mut self, e: TypeError) {
        self.errors.push(e);
    }

    fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}

#[test]
fn fresh_var_increments() {
    let mut state = TestState::new();

    let v0 = state.fresh_var();
    let v1 = state.fresh_var();
    let v2 = state.fresh_var();

    assert_eq!(v0, TyVar::new(0));
    assert_eq!(v1, TyVar::new(1));
    assert_eq!(v2, TyVar::new(2));
    assert_eq!(state.next_var, 3);
}

#[test]
fn fresh_returns_ty_var() {
    let mut state = TestState::new().with_next_var(10);

    let ty = state.fresh();
    assert_eq!(ty, Ty::Var(TyVar::new(10)));
}

#[test]
fn constrain_collects() {
    let mut state = TestState::new();

    let span = Span::new(0, 5);
    state.constrain(Constraint::Numeric(Ty::Int, span));
    state.constrain(Constraint::Eq(Ty::Int, Ty::Float, span));

    assert_eq!(state.constraints.len(), 2);
}

#[test]
fn unify_adds_eq_constraint() {
    let mut state = TestState::new();

    let span = Span::new(0, 5);
    state.unify(Ty::Int, Ty::String, span);

    assert_eq!(state.constraints.len(), 1);
    match &state.constraints[0] {
        Constraint::Eq(t1, t2, s) => {
            assert_eq!(*t1, Ty::Int);
            assert_eq!(*t2, Ty::String);
            assert_eq!(*s, span);
        }
        _ => panic!("expected Eq constraint"),
    }
}

#[test]
fn error_collects() {
    let mut state = TestState::new();

    assert!(!state.has_errors());

    let span = Span::new(0, 5);
    state.error(TypeError::UndefinedVar("x".to_string(), span));

    assert!(state.has_errors());
    assert_eq!(state.errors.len(), 1);
}

#[test]
fn constraint_span() {
    let span = Span::new(10, 20);

    assert_eq!(Constraint::Eq(Ty::Int, Ty::Int, span).span(), span);
    assert_eq!(Constraint::Numeric(Ty::Int, span).span(), span);
    assert_eq!(
        Constraint::Callable {
            callee: Ty::Int,
            args: SmallVec::new(),
            ret: Ty::Int,
            span
        }
        .span(),
        span
    );
    assert_eq!(Constraint::Stringable(Ty::Int, span).span(), span);
    assert_eq!(Constraint::Jsonable(Ty::Int, span).span(), span);
    assert_eq!(Constraint::Subscript(Ty::Int, span).span(), span);
    assert_eq!(Constraint::Storable(Ty::Int, span).span(), span);
    assert_eq!(
        Constraint::Unwrappable {
            ty: Ty::Int,
            inner: Ty::Int,
            span
        }
        .span(),
        span
    );
}

use crate::typecheck::Scheme;
use crate::value::{TypeExprArena, ValueArena};

/// Create an `InferCtx` for testing with a minimal AST.
fn test_ctx(ast: &Ast) -> InferCtx<'_> {
    let mut arena = ValueArena::new();
    let mut type_exprs = TypeExprArena::new();
    let registry = TypeRegistry::new(&mut arena, &mut type_exprs).unwrap();
    // Clone interner before leaking registry; shared with TypeEnv
    let strings = arena.interner();
    // Leak to get 'static lifetime; tests don't need to clean up
    let registry = Box::leak(Box::new(registry));
    let type_exprs = Box::leak(Box::new(type_exprs));
    let env = Box::leak(Box::new(crate::env::Environment::new()));
    InferCtx::new(ast, registry, type_exprs, env, strings)
}

/// Create an AST with a single expression.
fn ast_with_expr(expr: Expr) -> (Ast, ExprId) {
    let mut ast = Ast::new();
    let id = ast.add_expr(expr, Span::new(0, 10)).unwrap();
    (ast, id)
}

#[test]
fn literal_bool_true() {
    let (ast, id) = ast_with_expr(Expr::Literal(Literal::Bool(true)));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Bool);
}

#[test]
fn literal_bool_false() {
    let (ast, id) = ast_with_expr(Expr::Literal(Literal::Bool(false)));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Bool);
}

#[test]
fn literal_int() {
    let (ast, id) = ast_with_expr(Expr::Literal(Literal::Int(42)));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Int);
}

#[test]
fn literal_float() {
    let (ast, id) = ast_with_expr(Expr::Literal(Literal::Float(3.14)));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Float);
}

#[test]
fn literal_char() {
    let (ast, id) = ast_with_expr(Expr::Literal(Literal::Char('x')));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Char);
}

#[test]
fn literal_string() {
    let (ast, id) =
        ast_with_expr(Expr::Literal(Literal::String("hello".into())));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::String);
}

#[test]
fn literal_null() {
    let (ast, id) = ast_with_expr(Expr::Literal(Literal::Null));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Json);
}

#[test]
fn literal_unit() {
    let (ast, id) = ast_with_expr(Expr::Literal(Literal::Unit));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Unit);
}

#[test]
fn infer_empty_tuple_as_unit() {
    let (ast, id) = ast_with_expr(Expr::Tuple(smallvec::smallvec![]));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Unit);
}

#[test]
fn var_found() {
    let (ast, id) = ast_with_expr(Expr::Var("x".into()));
    let mut ctx = test_ctx(&ast);
    // Bind "x" to Int in the environment
    ctx.env_mut().bind("x", Scheme::mono(Ty::Int));
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Int);
    assert!(!ctx.has_errors());
}

#[test]
fn var_not_found() {
    let (ast, id) = ast_with_expr(Expr::Var("undefined".into()));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Error);
    assert!(ctx.has_errors());
    assert_eq!(ctx.errors().len(), 1);
    match &ctx.errors()[0] {
        TypeError::UndefinedVar(name, _) => assert_eq!(name, "undefined"),
        e => panic!("expected UndefinedVar, got {e:?}"),
    }
}

#[test]
fn var_instantiates_scheme() {
    let (ast, id) = ast_with_expr(Expr::Var("id".into()));
    let mut ctx = test_ctx(&ast);
    // Bind "id" to a polymorphic scheme: forall a. a -> a
    // Use a high index to avoid collision with fresh vars (which start at 0)
    let a = TyVar::new(1000);
    let scheme = Scheme {
        vars: vec![a],
        ty: Ty::Fn(vec![Ty::Var(a)], Box::new(Ty::Var(a))),
    };
    ctx.env_mut().bind("id", scheme);
    let ty = ctx.expr(id);
    // Should get Fn with fresh type variable (not the original `a`)
    match ty {
        Ty::Fn(params, ret) => {
            assert_eq!(params.len(), 1);
            // The fresh variable should have a different index
            match (&params[0], ret.as_ref()) {
                (Ty::Var(v1), Ty::Var(v2)) => {
                    assert_eq!(v1, v2); // Same fresh variable
                    assert_ne!(*v1, a); // Different from original (1000)
                }
                _ => panic!("expected Var types"),
            }
        }
        _ => panic!("expected Fn type, got {ty:?}"),
    }
}

#[test]
fn infer_expr_records_type() {
    let (ast, id) = ast_with_expr(Expr::Literal(Literal::Int(42)));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ctx.get_type(id), Some(&ty));
}

/// Helper to create AST with binary expression from two literals.
fn ast_with_binary(lhs: Literal, op: BinOp, rhs: Literal) -> (Ast, ExprId) {
    let mut ast = Ast::new();
    let l = ast.add_expr(Expr::Literal(lhs), Span::new(0, 1)).unwrap();
    let r = ast.add_expr(Expr::Literal(rhs), Span::new(4, 5)).unwrap();
    let bin = ast
        .add_expr(Expr::Binary(l, op, r), Span::new(0, 5))
        .unwrap();
    (ast, bin)
}

// Arithmetic operators

#[test]
fn infer_add_int_int() {
    let (ast, id) =
        ast_with_binary(Literal::Int(1), BinOp::Add, Literal::Int(2));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    // Both operands are concrete Int, result is Int directly
    assert_eq!(ty, Ty::Int);
    assert!(!ctx.has_errors());
}

#[test]
fn infer_add_float_float() {
    let (ast, id) =
        ast_with_binary(Literal::Float(1.0), BinOp::Add, Literal::Float(2.0));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Float);
}

#[test]
fn infer_add_int_float() {
    let (ast, id) =
        ast_with_binary(Literal::Int(1), BinOp::Add, Literal::Float(2.0));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Float); // Widening to Float
}

#[test]
fn infer_add_float_int() {
    let (ast, id) =
        ast_with_binary(Literal::Float(1.0), BinOp::Add, Literal::Int(2));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Float);
}

#[test]
fn infer_sub_int_int() {
    let (ast, id) =
        ast_with_binary(Literal::Int(5), BinOp::Sub, Literal::Int(3));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Int);
}

#[test]
fn infer_mul_int_float() {
    let (ast, id) =
        ast_with_binary(Literal::Int(2), BinOp::Mul, Literal::Float(3.5));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Float);
}

#[test]
fn infer_mod_int_int() {
    let (ast, id) =
        ast_with_binary(Literal::Int(10), BinOp::Mod, Literal::Int(3));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Int);
}

#[test]
fn infer_pow_int_int() {
    let (ast, id) =
        ast_with_binary(Literal::Int(2), BinOp::Pow, Literal::Int(3));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Int);
}

#[test]
fn infer_pow_float_int() {
    let (ast, id) =
        ast_with_binary(Literal::Float(2.0), BinOp::Pow, Literal::Int(3));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Float);
}

// Division operators

#[test]
fn infer_div_int_int_returns_float() {
    let (ast, id) =
        ast_with_binary(Literal::Int(10), BinOp::Div, Literal::Int(3));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Float); // Division always Float
}

#[test]
fn infer_floor_div_int_int_returns_int() {
    let (ast, id) =
        ast_with_binary(Literal::Int(10), BinOp::FloorDiv, Literal::Int(3));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Int); // Floor division always Int
}

#[test]
fn infer_floor_div_float_float_returns_int() {
    let (ast, id) = ast_with_binary(
        Literal::Float(10.0),
        BinOp::FloorDiv,
        Literal::Float(3.0),
    );
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Int); // Floor division always Int
}

// Comparison operators

#[test]
fn infer_eq_int_int() {
    let (ast, id) =
        ast_with_binary(Literal::Int(1), BinOp::Eq, Literal::Int(2));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Bool);
    // Should have Eq constraint unifying operands
    assert!(ctx
        .constraints()
        .iter()
        .any(|c| matches!(c, Constraint::Eq(_, _, _))));
}

#[test]
fn infer_ne_returns_bool() {
    let (ast, id) = ast_with_binary(
        Literal::String("a".into()),
        BinOp::Ne,
        Literal::String("b".into()),
    );
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Bool);
}

#[test]
fn infer_lt_returns_bool() {
    let (ast, id) =
        ast_with_binary(Literal::Int(1), BinOp::Lt, Literal::Int(2));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Bool);
}

#[test]
fn infer_gt_returns_bool() {
    let (ast, id) =
        ast_with_binary(Literal::Int(5), BinOp::Gt, Literal::Int(3));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Bool);
}

#[test]
fn infer_le_returns_bool() {
    let (ast, id) =
        ast_with_binary(Literal::Float(1.0), BinOp::Le, Literal::Float(2.0));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Bool);
}

#[test]
fn infer_ge_returns_bool() {
    let (ast, id) =
        ast_with_binary(Literal::Int(5), BinOp::Ge, Literal::Int(5));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Bool);
}

// Logical operators

#[test]
fn infer_and_bool_bool() {
    let (ast, id) =
        ast_with_binary(Literal::Bool(true), BinOp::And, Literal::Bool(false));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Bool);
    // Should have 2 Eq constraints unifying operands with Bool
    let eqs: Vec<_> = ctx
        .constraints()
        .iter()
        .filter(|c| matches!(c, Constraint::Eq(_, Ty::Bool, _)))
        .collect();
    assert_eq!(eqs.len(), 2);
}

#[test]
fn infer_or_bool_bool() {
    let (ast, id) =
        ast_with_binary(Literal::Bool(false), BinOp::Or, Literal::Bool(true));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::Bool);
}

// String concatenation

#[test]
fn infer_concat_string_string() {
    let (ast, id) = ast_with_binary(
        Literal::String("hello".into()),
        BinOp::Concat,
        Literal::String(" world".into()),
    );
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::String);
    // Should have Stringable constraint for rhs
    assert!(ctx
        .constraints()
        .iter()
        .any(|c| matches!(c, Constraint::Stringable(_, _))));
}

#[test]
fn infer_concat_string_int() {
    // String ++ Int is valid; Int is Stringable
    let (ast, id) = ast_with_binary(
        Literal::String("count: ".into()),
        BinOp::Concat,
        Literal::Int(42),
    );
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    assert_eq!(ty, Ty::String);
    // Stringable constraint on Int
    let stringables: Vec<_> = ctx
        .constraints()
        .iter()
        .filter(|c| matches!(c, Constraint::Stringable(Ty::Int, _)))
        .collect();
    assert_eq!(stringables.len(), 1);
}

// Coalesce

#[test]
fn infer_coalesce_creates_unwrappable_constraint() {
    // For coalesce, lhs needs to be Option[T] or Result[T, E]
    // Here we test with a variable that would be Option[Int]
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Var("opt".into()), Span::new(0, 3))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(0)), Span::new(7, 8))
        .unwrap();
    let coal = ast
        .add_expr(Expr::Binary(lhs, BinOp::Coalesce, rhs), Span::new(0, 8))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    // Bind "opt" to Option[Int]
    ctx.env_mut()
        .bind("opt", Scheme::mono(Ty::Option(Box::new(Ty::Int))));
    let ty = ctx.expr(coal);

    // Result should be fresh var unified with rhs (Int)
    match ty {
        Ty::Var(_) => {
            // Should have Unwrappable constraint
            assert!(ctx
                .constraints()
                .iter()
                .any(|c| matches!(c, Constraint::Unwrappable { .. })));
            // And Eq constraint unifying inner with rhs
            assert!(ctx
                .constraints()
                .iter()
                .any(|c| matches!(c, Constraint::Eq(_, _, _))));
        }
        _ => panic!("expected type variable, got {ty:?}"),
    }
}

// Pipe

#[test]
fn infer_pipe_creates_callable_constraint() {
    // 42 |> f should add Callable constraint on f
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Var("f".into()), Span::new(6, 7))
        .unwrap();
    let pipe = ast
        .add_expr(Expr::Binary(lhs, BinOp::Pipe, rhs), Span::new(0, 7))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    // Bind "f" to Int -> String
    ctx.env_mut().bind(
        "f",
        Scheme::mono(Ty::Fn(vec![Ty::Int], Box::new(Ty::String))),
    );
    let ty = ctx.expr(pipe);

    // Result is fresh var
    match ty {
        Ty::Var(_) => {
            // Should have Callable constraint
            let callables: Vec<_> = ctx
                .constraints()
                .iter()
                .filter(|c| matches!(c, Constraint::Callable { .. }))
                .collect();
            assert_eq!(callables.len(), 1);
        }
        _ => panic!("expected type variable, got {ty:?}"),
    }
}

// Unary operators

#[test]
fn infer_neg_int() {
    let mut ast = Ast::new();
    let operand = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(1, 3))
        .unwrap();
    let neg = ast
        .add_expr(Expr::Unary(UnOp::Neg, operand), Span::new(0, 3))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(neg);
    assert_eq!(ty, Ty::Int);
    // Should have Numeric constraint
    assert!(ctx
        .constraints()
        .iter()
        .any(|c| matches!(c, Constraint::Numeric(Ty::Int, _))));
}

#[test]
fn infer_neg_float() {
    let mut ast = Ast::new();
    let operand = ast
        .add_expr(Expr::Literal(Literal::Float(3.14)), Span::new(1, 5))
        .unwrap();
    let neg = ast
        .add_expr(Expr::Unary(UnOp::Neg, operand), Span::new(0, 5))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(neg);
    assert_eq!(ty, Ty::Float);
}

#[test]
fn infer_not_bool() {
    let mut ast = Ast::new();
    let operand = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), Span::new(1, 5))
        .unwrap();
    let not = ast
        .add_expr(Expr::Unary(UnOp::Not, operand), Span::new(0, 5))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(not);
    assert_eq!(ty, Ty::Bool);
    // Should have Eq constraint unifying operand with Bool
    assert!(ctx
        .constraints()
        .iter()
        .any(|c| matches!(c, Constraint::Eq(Ty::Bool, Ty::Bool, _))));
}

// Range

#[test]
fn infer_range_int_int() {
    let mut ast = Ast::new();
    let start = ast
        .add_expr(Expr::Literal(Literal::Int(0)), Span::new(0, 1))
        .unwrap();
    let end = ast
        .add_expr(Expr::Literal(Literal::Int(10)), Span::new(4, 6))
        .unwrap();
    let range = ast
        .add_expr(Expr::Range(start, end, false), Span::new(0, 6))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(range);
    assert_eq!(ty, Ty::Range);
    // Should have 2 Eq constraints unifying operands with Int
    let int_constraints: Vec<_> = ctx
        .constraints()
        .iter()
        .filter(|c| matches!(c, Constraint::Eq(Ty::Int, Ty::Int, _)))
        .collect();
    assert_eq!(int_constraints.len(), 2);
}

#[test]
fn infer_range_inclusive_int_int() {
    let mut ast = Ast::new();
    let start = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(0, 1))
        .unwrap();
    let end = ast
        .add_expr(Expr::Literal(Literal::Int(5)), Span::new(5, 6))
        .unwrap();
    let range = ast
        .add_expr(Expr::Range(start, end, true), Span::new(0, 6))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(range);
    assert_eq!(ty, Ty::Range); // Same type regardless of inclusive flag
}

// Arithmetic with type variables

#[test]
fn infer_add_with_var_unifies_with_int() {
    // x + 1 where x is a type variable; should unify x with Int
    let mut ast = Ast::new();
    let lhs = ast
        .add_expr(Expr::Var("x".into()), Span::new(0, 1))
        .unwrap();
    let rhs = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(4, 5))
        .unwrap();
    let add = ast
        .add_expr(Expr::Binary(lhs, BinOp::Add, rhs), Span::new(0, 5))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    // Bind "x" to a fresh type variable via polymorphic scheme
    let a = TyVar::new(1000);
    ctx.env_mut().bind(
        "x",
        Scheme {
            vars: vec![a],
            ty: Ty::Var(a),
        },
    );

    let ty = ctx.expr(add);
    // Since rhs is Int, lhs is unified with Int, result is Int
    assert_eq!(ty, Ty::Int);
    // Should have an Eq constraint unifying the type var with Int
    let eq_constraints: Vec<_> = ctx
        .constraints()
        .iter()
        .filter(|c| matches!(c, Constraint::Eq(_, Ty::Int, _)))
        .collect();
    assert_eq!(eq_constraints.len(), 1);
}

#[test]
fn array_empty() {
    let (ast, id) = ast_with_expr(Expr::Array(vec![]));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    match ty {
        Ty::Array(elem) => match *elem {
            Ty::Var(_) => {} // Fresh type variable is expected
            _ => panic!("expected type variable for empty array element"),
        },
        _ => panic!("expected Array type, got {ty:?}"),
    }
}

#[test]
fn array_homogeneous_int() {
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

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(arr);
    assert_eq!(ty, Ty::Array(Box::new(Ty::Int)));
}

#[test]
fn array_homogeneous_string() {
    let mut ast = Ast::new();
    let e1 = ast
        .add_expr(Expr::Literal(Literal::String("a".into())), Span::new(1, 4))
        .unwrap();
    let e2 = ast
        .add_expr(Expr::Literal(Literal::String("b".into())), Span::new(6, 9))
        .unwrap();
    let arr = ast
        .add_expr(Expr::Array(vec![e1, e2]), Span::new(0, 10))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(arr);
    assert_eq!(ty, Ty::Array(Box::new(Ty::String)));
}

#[test]
fn array_single_element() {
    let mut ast = Ast::new();
    let e1 = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), Span::new(1, 5))
        .unwrap();
    let arr = ast
        .add_expr(Expr::Array(vec![e1]), Span::new(0, 6))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(arr);
    assert_eq!(ty, Ty::Array(Box::new(Ty::Bool)));
}

#[test]
fn array_mixed_becomes_json() {
    // [1, 2.0] - mixed types become Json
    let mut ast = Ast::new();
    let e1 = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(1, 2))
        .unwrap();
    let e2 = ast
        .add_expr(Expr::Literal(Literal::Float(2.0)), Span::new(4, 7))
        .unwrap();
    let arr = ast
        .add_expr(Expr::Array(vec![e1, e2]), Span::new(0, 8))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(arr);
    // Mixed arrays become Json
    assert_eq!(ty, Ty::Json);
}

// Tuples

#[test]
fn tuple_two_elements() {
    let mut ast = Ast::new();
    let e1 = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(1, 2))
        .unwrap();
    let e2 = ast
        .add_expr(
            Expr::Literal(Literal::String("hello".into())),
            Span::new(4, 11),
        )
        .unwrap();
    let tup = ast
        .add_expr(Expr::Tuple(smallvec::smallvec![e1, e2]), Span::new(0, 12))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(tup);
    assert_eq!(ty, Ty::Tuple(vec![Ty::Int, Ty::String]));
}

#[test]
fn tuple_three_elements() {
    let mut ast = Ast::new();
    let e1 = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), Span::new(1, 5))
        .unwrap();
    let e2 = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(7, 9))
        .unwrap();
    let e3 = ast
        .add_expr(Expr::Literal(Literal::Float(3.14)), Span::new(11, 15))
        .unwrap();
    let tup = ast
        .add_expr(
            Expr::Tuple(smallvec::smallvec![e1, e2, e3]),
            Span::new(0, 16),
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(tup);
    assert_eq!(ty, Ty::Tuple(vec![Ty::Bool, Ty::Int, Ty::Float]));
}

#[test]
fn tuple_single_element() {
    // Single-element tuple (trailing comma): (42,)
    let mut ast = Ast::new();
    let e1 = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(1, 3))
        .unwrap();
    let tup = ast
        .add_expr(Expr::Tuple(smallvec::smallvec![e1]), Span::new(0, 5))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(tup);
    assert_eq!(ty, Ty::Tuple(vec![Ty::Int]));
}

#[test]
fn tuple_nested() {
    // ((1, 2), "hello")
    let mut ast = Ast::new();
    let e1 = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(2, 3))
        .unwrap();
    let e2 = ast
        .add_expr(Expr::Literal(Literal::Int(2)), Span::new(5, 6))
        .unwrap();
    let inner = ast
        .add_expr(Expr::Tuple(smallvec::smallvec![e1, e2]), Span::new(1, 7))
        .unwrap();
    let e3 = ast
        .add_expr(
            Expr::Literal(Literal::String("hello".into())),
            Span::new(9, 16),
        )
        .unwrap();
    let outer = ast
        .add_expr(
            Expr::Tuple(smallvec::smallvec![inner, e3]),
            Span::new(0, 17),
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(outer);
    assert_eq!(
        ty,
        Ty::Tuple(vec![Ty::Tuple(vec![Ty::Int, Ty::Int]), Ty::String])
    );
}

// Objects

#[test]
fn object_single_field() {
    let mut ast = Ast::new();
    let val = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(7, 9))
        .unwrap();
    let obj = ast
        .add_expr(Expr::Object(vec![("age".into(), val)]), Span::new(0, 11))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(obj);
    match ty {
        Ty::Object(fields) => {
            assert_eq!(fields.len(), 1);
            // Look up by getting the field value
            let field_ty = fields.values().next().unwrap();
            assert_eq!(*field_ty, Ty::Int);
        }
        _ => panic!("expected Object type, got {ty:?}"),
    }
}

#[test]
fn object_multiple_fields() {
    let mut ast = Ast::new();
    let v1 = ast
        .add_expr(
            Expr::Literal(Literal::String("Alice".into())),
            Span::new(8, 15),
        )
        .unwrap();
    let v2 = ast
        .add_expr(Expr::Literal(Literal::Int(30)), Span::new(23, 25))
        .unwrap();
    let obj = ast
        .add_expr(
            Expr::Object(vec![("name".into(), v1), ("age".into(), v2)]),
            Span::new(0, 27),
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(obj);
    match ty {
        Ty::Object(fields) => {
            assert_eq!(fields.len(), 2);
            let tys: Vec<_> = fields.values().collect();
            assert_eq!(*tys[0], Ty::String);
            assert_eq!(*tys[1], Ty::Int);
        }
        _ => panic!("expected Object type, got {ty:?}"),
    }
}

#[test]
fn object_nested() {
    // { outer: { inner: 123 } }
    let mut ast = Ast::new();
    let inner_val = ast
        .add_expr(Expr::Literal(Literal::Int(123)), Span::new(18, 21))
        .unwrap();
    let inner_obj = ast
        .add_expr(
            Expr::Object(vec![("inner".into(), inner_val)]),
            Span::new(9, 23),
        )
        .unwrap();
    let outer = ast
        .add_expr(
            Expr::Object(vec![("outer".into(), inner_obj)]),
            Span::new(0, 25),
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(outer);
    match ty {
        Ty::Object(fields) => {
            assert_eq!(fields.len(), 1);
            let outer_ty = fields.values().next().unwrap();
            match outer_ty {
                Ty::Object(inner_fields) => {
                    assert_eq!(inner_fields.len(), 1);
                    let inner_ty = inner_fields.values().next().unwrap();
                    assert_eq!(*inner_ty, Ty::Int);
                }
                _ => panic!("expected nested Object type"),
            }
        }
        _ => panic!("expected Object type, got {ty:?}"),
    }
}

#[test]
fn object_empty() {
    let (ast, id) = ast_with_expr(Expr::Object(vec![]));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    match ty {
        Ty::Object(fields) => assert!(fields.is_empty()),
        _ => panic!("expected Object type, got {ty:?}"),
    }
}

// Map literals

#[test]
fn infer_map_empty() {
    let (ast, id) = ast_with_expr(Expr::MapLit(smallvec::smallvec![]));
    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(id);
    match ty {
        Ty::Map(k, v) => match (*k, *v) {
            (Ty::Var(_), Ty::Var(_)) => {}
            _ => panic!("expected type variables for empty map"),
        },
        _ => panic!("expected Map type, got {ty:?}"),
    }
}

#[test]
fn infer_map_single_entry() {
    let mut ast = Ast::new();
    let k = ast
        .add_expr(
            Expr::Literal(Literal::String("key".into())),
            Span::new(2, 7),
        )
        .unwrap();
    let v = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(11, 13))
        .unwrap();
    let map = ast
        .add_expr(Expr::MapLit(smallvec::smallvec![(k, v)]), Span::new(0, 15))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(map);
    assert_eq!(ty, Ty::Map(Box::new(Ty::String), Box::new(Ty::Int)));
}

#[test]
fn infer_map_multiple_entries() {
    let mut ast = Ast::new();
    let k1 = ast
        .add_expr(Expr::Literal(Literal::String("a".into())), Span::new(2, 5))
        .unwrap();
    let v1 = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(9, 10))
        .unwrap();
    let k2 = ast
        .add_expr(
            Expr::Literal(Literal::String("b".into())),
            Span::new(13, 16),
        )
        .unwrap();
    let v2 = ast
        .add_expr(Expr::Literal(Literal::Int(2)), Span::new(20, 21))
        .unwrap();
    let map = ast
        .add_expr(
            Expr::MapLit(smallvec::smallvec![(k1, v1), (k2, v2)]),
            Span::new(0, 23),
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(map);
    assert_eq!(ty, Ty::Map(Box::new(Ty::String), Box::new(Ty::Int)));
    // Should have Eq constraints unifying keys and values
    let eq_constraints: Vec<_> = ctx
        .constraints()
        .iter()
        .filter(|c| matches!(c, Constraint::Eq(_, _, _)))
        .collect();
    // 2 constraints: one for keys (String ~ String), one for values (Int ~ Int)
    assert_eq!(eq_constraints.len(), 2);
}

#[test]
fn infer_map_mixed_values_adds_constraints() {
    // { "a" => 1, "b" => 2.0 } should unify Int with Float
    let mut ast = Ast::new();
    let k1 = ast
        .add_expr(Expr::Literal(Literal::String("a".into())), Span::new(2, 5))
        .unwrap();
    let v1 = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(9, 10))
        .unwrap();
    let k2 = ast
        .add_expr(
            Expr::Literal(Literal::String("b".into())),
            Span::new(13, 16),
        )
        .unwrap();
    let v2 = ast
        .add_expr(Expr::Literal(Literal::Float(2.0)), Span::new(20, 23))
        .unwrap();
    let map = ast
        .add_expr(
            Expr::MapLit(smallvec::smallvec![(k1, v1), (k2, v2)]),
            Span::new(0, 25),
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(map);
    // Result type is Map[String, Int] (first entry's types)
    assert_eq!(ty, Ty::Map(Box::new(Ty::String), Box::new(Ty::Int)));
    // but there's an Eq constraint to unify Int with Float
    assert!(ctx
        .constraints()
        .iter()
        .any(|c| matches!(c, Constraint::Eq(Ty::Int, Ty::Float, _))));
}

#[test]
fn infer_map_int_keys() {
    let mut ast = Ast::new();
    let k1 = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(2, 3))
        .unwrap();
    let v1 = ast
        .add_expr(
            Expr::Literal(Literal::String("one".into())),
            Span::new(7, 12),
        )
        .unwrap();
    let k2 = ast
        .add_expr(Expr::Literal(Literal::Int(2)), Span::new(15, 16))
        .unwrap();
    let v2 = ast
        .add_expr(
            Expr::Literal(Literal::String("two".into())),
            Span::new(20, 25),
        )
        .unwrap();
    let map = ast
        .add_expr(
            Expr::MapLit(smallvec::smallvec![(k1, v1), (k2, v2)]),
            Span::new(0, 27),
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(map);
    assert_eq!(ty, Ty::Map(Box::new(Ty::Int), Box::new(Ty::String)));
}

// Field access

#[test]
fn field_access_object() {
    // { name: "Alice" }.name
    let mut ast = Ast::new();
    let val = ast
        .add_expr(
            Expr::Literal(Literal::String("Alice".into())),
            Span::new(8, 15),
        )
        .unwrap();
    let obj = ast
        .add_expr(Expr::Object(vec![("name".into(), val)]), Span::new(0, 17))
        .unwrap();
    let field = ast
        .add_expr(Expr::Field(obj, "name".into()), Span::new(0, 22))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(field);
    assert_eq!(ty, Ty::String);
    assert!(!ctx.has_errors());
}

#[test]
fn field_access_object_nested() {
    // { user: { name: "Alice" } }.user.name
    let mut ast = Ast::new();
    let name_val = ast
        .add_expr(
            Expr::Literal(Literal::String("Alice".into())),
            Span::new(16, 23),
        )
        .unwrap();
    let inner_obj = ast
        .add_expr(
            Expr::Object(vec![("name".into(), name_val)]),
            Span::new(8, 25),
        )
        .unwrap();
    let outer_obj = ast
        .add_expr(
            Expr::Object(vec![("user".into(), inner_obj)]),
            Span::new(0, 27),
        )
        .unwrap();
    let user_field = ast
        .add_expr(Expr::Field(outer_obj, "user".into()), Span::new(0, 32))
        .unwrap();
    let name_field = ast
        .add_expr(Expr::Field(user_field, "name".into()), Span::new(0, 37))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(name_field);
    assert_eq!(ty, Ty::String);
    assert!(!ctx.has_errors());
}

#[test]
fn field_access_missing_field() {
    // { name: "Alice" }.age
    let mut ast = Ast::new();
    let val = ast
        .add_expr(
            Expr::Literal(Literal::String("Alice".into())),
            Span::new(8, 15),
        )
        .unwrap();
    let obj = ast
        .add_expr(Expr::Object(vec![("name".into(), val)]), Span::new(0, 17))
        .unwrap();
    let field = ast
        .add_expr(Expr::Field(obj, "age".into()), Span::new(0, 21))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(field);
    assert_eq!(ty, Ty::Error);
    assert!(ctx.has_errors());
    match &ctx.errors()[0] {
        TypeError::FieldNotFound { field, .. } => {
            assert_eq!(field, "age");
        }
        e => panic!("expected FieldNotFound, got {e:?}"),
    }
}

#[test]
fn field_access_on_int() {
    // 42.field
    let mut ast = Ast::new();
    let n = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2))
        .unwrap();
    let field = ast
        .add_expr(Expr::Field(n, "field".into()), Span::new(0, 8))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(field);
    assert_eq!(ty, Ty::Error);
    assert!(ctx.has_errors());
    match &ctx.errors()[0] {
        TypeError::NotAnObject(_, _) => {}
        e => panic!("expected NotAnObject, got {e:?}"),
    }
}

#[test]
fn field_access_var_creates_has_field_constraint() {
    // x.name where x is a type variable
    let mut ast = Ast::new();
    let var = ast
        .add_expr(Expr::Var("x".into()), Span::new(0, 1))
        .unwrap();
    let field = ast
        .add_expr(Expr::Field(var, "name".into()), Span::new(0, 6))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    // Bind x to fresh type variable
    let a = TyVar::new(1000);
    ctx.env_mut().bind(
        "x",
        Scheme {
            vars: vec![a],
            ty: Ty::Var(a),
        },
    );
    let ty = ctx.expr(field);
    // Result should be a fresh type variable
    match ty {
        Ty::Var(_) => {
            // Should have HasField constraint
            assert!(ctx.constraints().iter().any(|c| matches!(
                c,
                Constraint::HasField {
                    base: Ty::Var(_),
                    ..
                }
            )));
        }
        _ => panic!("expected type variable, got {ty:?}"),
    }
}

// Tuple indexing

#[test]
fn tuple_index_first() {
    // (1, "hello").0
    let mut ast = Ast::new();
    let e1 = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(1, 2))
        .unwrap();
    let e2 = ast
        .add_expr(
            Expr::Literal(Literal::String("hello".into())),
            Span::new(4, 11),
        )
        .unwrap();
    let tup = ast
        .add_expr(Expr::Tuple(smallvec::smallvec![e1, e2]), Span::new(0, 12))
        .unwrap();
    let idx = ast
        .add_expr(Expr::TupleIndex(tup, 0), Span::new(0, 14))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(idx);
    assert_eq!(ty, Ty::Int);
    assert!(!ctx.has_errors());
}

#[test]
fn tuple_index_second() {
    // (1, "hello").1
    let mut ast = Ast::new();
    let e1 = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(1, 2))
        .unwrap();
    let e2 = ast
        .add_expr(
            Expr::Literal(Literal::String("hello".into())),
            Span::new(4, 11),
        )
        .unwrap();
    let tup = ast
        .add_expr(Expr::Tuple(smallvec::smallvec![e1, e2]), Span::new(0, 12))
        .unwrap();
    let idx = ast
        .add_expr(Expr::TupleIndex(tup, 1), Span::new(0, 14))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(idx);
    assert_eq!(ty, Ty::String);
    assert!(!ctx.has_errors());
}

#[test]
fn tuple_index_out_of_bounds() {
    // (1, "hello").5
    let mut ast = Ast::new();
    let e1 = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(1, 2))
        .unwrap();
    let e2 = ast
        .add_expr(
            Expr::Literal(Literal::String("hello".into())),
            Span::new(4, 11),
        )
        .unwrap();
    let tup = ast
        .add_expr(Expr::Tuple(smallvec::smallvec![e1, e2]), Span::new(0, 12))
        .unwrap();
    let idx = ast
        .add_expr(Expr::TupleIndex(tup, 5), Span::new(0, 14))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(idx);
    assert_eq!(ty, Ty::Error);
    assert!(ctx.has_errors());
    match &ctx.errors()[0] {
        TypeError::TupleIndexOutOfBounds { idx, len, .. } => {
            assert_eq!(*idx, 5);
            assert_eq!(*len, 2);
        }
        e => panic!("expected TupleIndexOutOfBounds, got {e:?}"),
    }
}

#[test]
fn tuple_index_on_non_tuple() {
    // "hello".0
    let mut ast = Ast::new();
    let s = ast
        .add_expr(
            Expr::Literal(Literal::String("hello".into())),
            Span::new(0, 7),
        )
        .unwrap();
    let idx = ast
        .add_expr(Expr::TupleIndex(s, 0), Span::new(0, 9))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(idx);
    assert_eq!(ty, Ty::Error);
    assert!(ctx.has_errors());
    match &ctx.errors()[0] {
        TypeError::NotATuple(_, _) => {}
        e => panic!("expected NotATuple, got {e:?}"),
    }
}

// Index access

#[test]
fn index_array() {
    // [1, 2, 3][0]
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
    let idx_val = ast
        .add_expr(Expr::Literal(Literal::Int(0)), Span::new(10, 11))
        .unwrap();
    let idx = ast
        .add_expr(Expr::Index(arr, idx_val), Span::new(0, 12))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(idx);
    assert_eq!(ty, Ty::Int);
    // Index should be unified with Int
    assert!(ctx
        .constraints()
        .iter()
        .any(|c| matches!(c, Constraint::Eq(Ty::Int, Ty::Int, _))));
}

#[test]
fn index_map() {
    // { "a" => 1 }["a"]
    let mut ast = Ast::new();
    let k = ast
        .add_expr(Expr::Literal(Literal::String("a".into())), Span::new(2, 5))
        .unwrap();
    let v = ast
        .add_expr(Expr::Literal(Literal::Int(1)), Span::new(9, 10))
        .unwrap();
    let map = ast
        .add_expr(Expr::MapLit(smallvec::smallvec![(k, v)]), Span::new(0, 12))
        .unwrap();
    let idx_val = ast
        .add_expr(
            Expr::Literal(Literal::String("a".into())),
            Span::new(13, 16),
        )
        .unwrap();
    let idx = ast
        .add_expr(Expr::Index(map, idx_val), Span::new(0, 17))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(idx);
    assert_eq!(ty, Ty::Int);
}

#[test]
fn index_string() {
    // "hello"[0]
    let mut ast = Ast::new();
    let s = ast
        .add_expr(
            Expr::Literal(Literal::String("hello".into())),
            Span::new(0, 7),
        )
        .unwrap();
    let idx_val = ast
        .add_expr(Expr::Literal(Literal::Int(0)), Span::new(8, 9))
        .unwrap();
    let idx = ast
        .add_expr(Expr::Index(s, idx_val), Span::new(0, 10))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(idx);
    assert_eq!(ty, Ty::Char);
}

#[test]
fn index_on_non_indexable() {
    // true[0]
    let mut ast = Ast::new();
    let b = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), Span::new(0, 4))
        .unwrap();
    let idx_val = ast
        .add_expr(Expr::Literal(Literal::Int(0)), Span::new(5, 6))
        .unwrap();
    let idx = ast
        .add_expr(Expr::Index(b, idx_val), Span::new(0, 7))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(idx);
    assert_eq!(ty, Ty::Error);
    assert!(ctx.has_errors());
    match &ctx.errors()[0] {
        TypeError::NotIndexable(_, _) => {}
        e => panic!("expected NotIndexable, got {e:?}"),
    }
}

// JSON access

#[test]
fn json_access_field() {
    // data.name (where data is Json)
    use crate::ast::{JsonAccessKey, JsonAccessKind};
    let mut ast = Ast::new();
    let json = ast
        .add_expr(Expr::Literal(Literal::Null), Span::new(0, 4))
        .unwrap();
    let access = ast
        .add_expr(
            Expr::JsonAccess(
                json,
                JsonAccessKind::Json,
                JsonAccessKey::Field("name".into()),
            ),
            Span::new(0, 9),
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(access);
    assert_eq!(ty, Ty::Json);
}

#[test]
fn json_access_scalar() {
    // data..name (where data is Json)
    use crate::ast::{JsonAccessKey, JsonAccessKind};
    let mut ast = Ast::new();
    let json = ast
        .add_expr(Expr::Literal(Literal::Null), Span::new(0, 4))
        .unwrap();
    let access = ast
        .add_expr(
            Expr::JsonAccess(
                json,
                JsonAccessKind::Scalar,
                JsonAccessKey::Field("name".into()),
            ),
            Span::new(0, 10),
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(access);
    // Returns Option[Scalar]
    match ty {
        Ty::Option(inner) => {
            assert_eq!(*inner, Ty::Named(TypeId::SCALAR, vec![]))
        }
        _ => panic!("expected Option, got {ty:?}"),
    }
}

#[test]
fn json_access_dynamic_key() {
    // data->(key) where key is String
    use crate::ast::{JsonAccessKey, JsonAccessKind};
    let mut ast = Ast::new();
    let json = ast
        .add_expr(Expr::Literal(Literal::Null), Span::new(0, 4))
        .unwrap();
    let key = ast
        .add_expr(
            Expr::Literal(Literal::String("name".into())),
            Span::new(7, 13),
        )
        .unwrap();
    let access = ast
        .add_expr(
            Expr::JsonAccess(
                json,
                JsonAccessKind::Json,
                JsonAccessKey::Expr(key),
            ),
            Span::new(0, 14),
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(access);
    assert_eq!(ty, Ty::Json);
    // Key should be unified with String
    assert!(ctx
        .constraints()
        .iter()
        .any(|c| matches!(c, Constraint::Eq(Ty::String, Ty::String, _))));
}

#[test]
fn json_access_on_non_json() {
    // 42.name (where 42 is Int)
    use crate::ast::{JsonAccessKey, JsonAccessKind};
    let mut ast = Ast::new();
    let n = ast
        .add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2))
        .unwrap();
    let access = ast
        .add_expr(
            Expr::JsonAccess(
                n,
                JsonAccessKind::Json,
                JsonAccessKey::Field("name".into()),
            ),
            Span::new(0, 7),
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(access);
    assert_eq!(ty, Ty::Error);
    assert!(ctx.has_errors());
    match &ctx.errors()[0] {
        TypeError::NotJson(_, _) => {}
        e => panic!("expected NotJson, got {e:?}"),
    }
}

#[test]
fn infer_json_literal() {
    // { "name": "Alice" } as JSON
    let mut ast = Ast::new();
    let val = ast
        .add_expr(
            Expr::Literal(Literal::String("Alice".into())),
            Span::new(10, 17),
        )
        .unwrap();
    let json = ast
        .add_expr(Expr::Json(vec![("name".into(), val)]), Span::new(0, 19))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(json);
    assert_eq!(ty, Ty::Json);
}

// Optional field access

#[test]
fn optional_field_on_option() {
    // opt?.name where opt is Option[{ name: String }]
    let mut ast = Ast::new();
    let var = ast
        .add_expr(Expr::Var("opt".into()), Span::new(0, 3))
        .unwrap();
    let field = ast
        .add_expr(Expr::OptionalField(var, "name".into()), Span::new(0, 9))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    // Create an Option type with a structural object inside
    let name_id = ctx.env_mut().intern("name");
    let inner = Ty::Object(std::iter::once((name_id, Ty::String)).collect());
    ctx.env_mut()
        .bind("opt", Scheme::mono(Ty::Option(Box::new(inner))));
    let ty = ctx.expr(field);
    // Should return Option[String]
    match ty {
        Ty::Option(inner) => assert_eq!(*inner, Ty::String),
        _ => panic!("expected Option, got {ty:?}"),
    }
}

#[test]
fn optional_field_on_non_option() {
    // obj?.name where obj is { name: String }
    // Should return Option[String] (wraps field access in Option)
    let mut ast = Ast::new();
    let var = ast
        .add_expr(Expr::Var("obj".into()), Span::new(0, 3))
        .unwrap();
    let field = ast
        .add_expr(Expr::OptionalField(var, "name".into()), Span::new(0, 9))
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let name_id = ctx.env_mut().intern("name");
    let obj_ty = Ty::Object(std::iter::once((name_id, Ty::String)).collect());
    ctx.env_mut().bind("obj", Scheme::mono(obj_ty));
    let ty = ctx.expr(field);
    // Should return Option[String]
    match ty {
        Ty::Option(inner) => assert_eq!(*inner, Ty::String),
        _ => panic!("expected Option[String], got {ty:?}"),
    }
    assert!(!ctx.has_errors());
}

// --- Union type tests ---

#[test]
fn expand_union_members_anonymous() {
    let ast = Ast::new();
    let ctx = test_ctx(&ast);
    let u = Ty::Union(vec![Ty::Int, Ty::String, Ty::Bool]);
    let members = ctx.expand_union_members(&u);
    assert_eq!(members, Some(vec![Ty::Int, Ty::String, Ty::Bool]));
}

#[test]
fn expand_union_members_storable() {
    let ast = Ast::new();
    let ctx = test_ctx(&ast);
    let storable = Ty::Named(TypeId::STORABLE, vec![]);
    let members = ctx.expand_union_members(&storable);
    assert!(members.is_some());
    let members = members.unwrap();
    assert!(members.contains(&Ty::Bool));
    assert!(members.contains(&Ty::Int));
    assert!(members.contains(&Ty::Float));
    assert!(members.contains(&Ty::Char));
    assert!(members.contains(&Ty::String));
    assert!(members.contains(&Ty::Json));
    assert_eq!(members.len(), 6);
}

#[test]
fn expand_union_members_scalar() {
    let ast = Ast::new();
    let ctx = test_ctx(&ast);
    let scalar = Ty::Named(TypeId::SCALAR, vec![]);
    let members = ctx.expand_union_members(&scalar);
    assert!(members.is_some());
    let members = members.unwrap();
    assert!(members.contains(&Ty::Bool));
    assert!(members.contains(&Ty::Int));
    assert!(members.contains(&Ty::Float));
    assert!(members.contains(&Ty::String));
    assert_eq!(members.len(), 4);
}

#[test]
fn expand_union_members_non_union() {
    let ast = Ast::new();
    let ctx = test_ctx(&ast);
    // Primitive types are not unions
    assert!(ctx.expand_union_members(&Ty::Int).is_none());
    assert!(ctx.expand_union_members(&Ty::String).is_none());
    // Array is not a union
    assert!(ctx
        .expand_union_members(&Ty::Array(Box::new(Ty::Int)))
        .is_none());
}

#[test]
fn is_union_member_anonymous() {
    let ast = Ast::new();
    let ctx = test_ctx(&ast);
    let u = Ty::Union(vec![Ty::Int, Ty::String]);
    assert!(ctx.is_union_member(&u, &Ty::Int));
    assert!(ctx.is_union_member(&u, &Ty::String));
    assert!(!ctx.is_union_member(&u, &Ty::Bool));
}

#[test]
fn is_union_member_storable() {
    let ast = Ast::new();
    let ctx = test_ctx(&ast);
    let storable = Ty::Named(TypeId::STORABLE, vec![]);
    assert!(ctx.is_union_member(&storable, &Ty::Int));
    assert!(ctx.is_union_member(&storable, &Ty::String));
    assert!(ctx.is_union_member(&storable, &Ty::Json));
    // Array is not in Storable
    assert!(!ctx.is_union_member(&storable, &Ty::Array(Box::new(Ty::Int))));
}

#[test]
fn is_union_member_non_union_returns_false() {
    let ast = Ast::new();
    let ctx = test_ctx(&ast);
    // Non-union type should return false for any member check
    assert!(!ctx.is_union_member(&Ty::Int, &Ty::Int));
}

#[test]
fn ast_type_to_ty_anonymous_union() {
    let mut ast = Ast::new();
    let span = Span::new(0, 10);

    // Build `Int | String | Bool` as AstTypeExpr::Union
    let int_id = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), span)
        .unwrap();
    let str_id = ast
        .add_type_expr(AstTypeExpr::Named("String".into()), span)
        .unwrap();
    let bool_id = ast
        .add_type_expr(AstTypeExpr::Named("Bool".into()), span)
        .unwrap();
    let union_id = ast
        .add_type_expr(
            AstTypeExpr::Union(smallvec::smallvec![int_id, str_id, bool_id]),
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.ast_type_to_ty(union_id, &HashMap::new());

    assert_eq!(ty, Ty::Union(vec![Ty::Int, Ty::String, Ty::Bool]));
}

#[test]
fn ast_type_to_ty_empty_union_is_error() {
    let mut ast = Ast::new();
    let span = Span::new(0, 10);

    // Empty union
    let union_id = ast
        .add_type_expr(AstTypeExpr::Union(smallvec::smallvec![]), span)
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.ast_type_to_ty(union_id, &HashMap::new());

    assert_eq!(ty, Ty::Error);
    assert!(ctx.has_errors());
    match &ctx.errors()[0] {
        TypeError::EmptyUnion(_) => {}
        e => panic!("expected EmptyUnion, got {e:?}"),
    }
}

// --- Closure inference tests ---

#[test]
fn closure_zero_params() {
    // Build: () => 42
    let mut ast = Ast::new();
    let span = Span::new(0, 10);
    let body = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let closure = ast
        .add_expr(
            Expr::Closure {
                params: smallvec::smallvec![],
                ret: None,
                body,
            },
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(closure);

    // Should be Fn([], Int)
    assert_eq!(ty, Ty::Fn(vec![], Box::new(Ty::Int)));
}

#[test]
fn closure_no_annotations() {
    // Build: x => x
    let mut ast = Ast::new();
    let span = Span::new(0, 10);
    let x = ast.add_expr(Expr::Var("x".into()), span).unwrap();
    let closure = ast
        .add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body: x,
            },
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(closure);

    // Should be Fn([?0], ?0) since x has fresh type and body returns x
    match ty {
        Ty::Fn(params, ret) => {
            assert_eq!(params.len(), 1);
            match (&params[0], ret.as_ref()) {
                (Ty::Var(p), Ty::Var(r)) => {
                    // Body returns x, so param var and return var should match
                    assert_eq!(p, r);
                }
                _ => panic!("expected type variables"),
            }
        }
        _ => panic!("expected Fn type, got {ty:?}"),
    }
}

#[test]
fn closure_with_param_annotations() {
    // Build: (x: Int) => x
    let mut ast = Ast::new();
    let span = Span::new(0, 10);
    let int_ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), span)
        .unwrap();
    let x = ast.add_expr(Expr::Var("x".into()), span).unwrap();
    let closure = ast
        .add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), Some(int_ty))],
                ret: None,
                body: x,
            },
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(closure);

    // Should be Fn([Int], Int)
    assert_eq!(ty, Ty::Fn(vec![Ty::Int], Box::new(Ty::Int)));
}

#[test]
fn closure_with_return_annotation() {
    // Build: (x: Int) -> String => ...
    // Body returns Int, so there should be a unification constraint
    let mut ast = Ast::new();
    let span = Span::new(0, 10);
    let int_ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), span)
        .unwrap();
    let str_ty = ast
        .add_type_expr(AstTypeExpr::Named("String".into()), span)
        .unwrap();
    let x = ast.add_expr(Expr::Var("x".into()), span).unwrap();
    let closure = ast
        .add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), Some(int_ty))],
                ret: Some(str_ty),
                body: x,
            },
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(closure);

    // Return type should be String (from annotation)
    assert_eq!(ty, Ty::Fn(vec![Ty::Int], Box::new(Ty::String)));

    // Should have Eq constraint unifying body (Int) with return (String)
    let eq_constraints: Vec<_> = ctx
        .constraints()
        .iter()
        .filter(|c| matches!(c, Constraint::Eq(_, _, _)))
        .collect();
    assert!(!eq_constraints.is_empty());
}

#[test]
fn closure_multi_param() {
    // Build: (a: Int, b: Float) => 42
    let mut ast = Ast::new();
    let span = Span::new(0, 10);
    let int_ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), span)
        .unwrap();
    let float_ty = ast
        .add_type_expr(AstTypeExpr::Named("Float".into()), span)
        .unwrap();
    let body = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let closure = ast
        .add_expr(
            Expr::Closure {
                params: smallvec::smallvec![
                    ("a".into(), Some(int_ty)),
                    ("b".into(), Some(float_ty))
                ],
                ret: None,
                body,
            },
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(closure);

    // Should be Fn([Int, Float], Int)
    assert_eq!(ty, Ty::Fn(vec![Ty::Int, Ty::Float], Box::new(Ty::Int)));
}

#[test]
fn closure_body_uses_params() {
    // Build: (a, b) => a + b
    let mut ast = Ast::new();
    let span = Span::new(0, 10);
    let a = ast.add_expr(Expr::Var("a".into()), span).unwrap();
    let b = ast.add_expr(Expr::Var("b".into()), span).unwrap();
    let add = ast.add_expr(Expr::Binary(a, BinOp::Add, b), span).unwrap();
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
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(closure);

    // Should be Fn([?a, ?b], ?result) with Numeric constraints
    match ty {
        Ty::Fn(params, ret) => {
            assert_eq!(params.len(), 2);
            // Both params should be type vars, and result should be type var
            assert!(matches!(&params[0], Ty::Var(_)));
            assert!(matches!(&params[1], Ty::Var(_)));
            assert!(matches!(ret.as_ref(), Ty::Var(_)));
        }
        _ => panic!("expected Fn type, got {ty:?}"),
    }

    // Should have Numeric constraints for operands
    let numerics: Vec<_> = ctx
        .constraints()
        .iter()
        .filter(|c| matches!(c, Constraint::Numeric(_, _)))
        .collect();
    assert!(!numerics.is_empty());
}

// --- Call inference tests ---

#[test]
fn call_no_args() {
    // Build: f()
    let mut ast = Ast::new();
    let span = Span::new(0, 10);
    let f = ast.add_expr(Expr::Var("f".into()), span).unwrap();
    let call = ast
        .add_expr(Expr::Call(f, smallvec::smallvec![]), span)
        .unwrap();

    let mut ctx = test_ctx(&ast);
    // Bind f to () -> Int
    ctx.env_mut()
        .bind("f", Scheme::mono(Ty::Fn(vec![], Box::new(Ty::Int))));

    let ty = ctx.expr(call);

    // Result is fresh type var
    assert!(matches!(ty, Ty::Var(_)));

    // Should have Callable constraint
    let callables: Vec<_> = ctx
        .constraints()
        .iter()
        .filter(|c| matches!(c, Constraint::Callable { .. }))
        .collect();
    assert_eq!(callables.len(), 1);
}

#[test]
fn call_with_args() {
    // Build: f(1, "hello")
    let mut ast = Ast::new();
    let span = Span::new(0, 10);
    let f = ast.add_expr(Expr::Var("f".into()), span).unwrap();
    let arg1 = ast.add_expr(Expr::Literal(Literal::Int(1)), span).unwrap();
    let arg2 = ast
        .add_expr(Expr::Literal(Literal::String("hello".into())), span)
        .unwrap();
    let call = ast
        .add_expr(Expr::Call(f, smallvec::smallvec![arg1, arg2]), span)
        .unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.env_mut().bind(
        "f",
        Scheme::mono(Ty::Fn(vec![Ty::Int, Ty::String], Box::new(Ty::Bool))),
    );

    let ty = ctx.expr(call);

    // Result is fresh type var
    assert!(matches!(ty, Ty::Var(_)));

    // Check Callable constraint has correct arg types
    let callable = ctx.constraints().iter().find_map(|c| match c {
        Constraint::Callable { args, .. } => Some(args.clone()),
        _ => None,
    });
    assert!(callable.is_some());
    let args = callable.unwrap();
    assert_eq!(args.len(), 2);
    assert_eq!(args[0], Ty::Int);
    assert_eq!(args[1], Ty::String);
}

#[test]
fn call_on_closure() {
    // Build: (x => x)(42)
    let mut ast = Ast::new();
    let span = Span::new(0, 10);
    let x = ast.add_expr(Expr::Var("x".into()), span).unwrap();
    let closure = ast
        .add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body: x,
            },
            span,
        )
        .unwrap();
    let arg = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let call = ast
        .add_expr(Expr::Call(closure, smallvec::smallvec![arg]), span)
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(call);

    // Result is fresh type var
    assert!(matches!(ty, Ty::Var(_)));

    // Should have Callable constraint with the closure type
    let callables: Vec<_> = ctx
        .constraints()
        .iter()
        .filter(|c| matches!(c, Constraint::Callable { .. }))
        .collect();
    assert_eq!(callables.len(), 1);
}

// --- Named function inference tests ---

/// Helper to create an AST with a function statement.
fn ast_with_fun_stmt(
    name: &str,
    params: SmallVec<[(String, Option<AstTypeExprId>); 4]>,
    ret: Option<AstTypeExprId>,
    body: ExprId,
    ast: &mut Ast,
    span: Span,
) -> StmtId {
    ast.add_stmt(
        Stmt::Fun {
            name: name.into(),
            params,
            ret,
            body,
        },
        span,
    )
    .unwrap()
}

#[test]
fn fun_no_annotations() {
    // Build: FUN id(x) { x }
    let mut ast = Ast::new();
    let span = Span::new(0, 20);
    let x = ast.add_expr(Expr::Var("x".into()), span).unwrap();
    let stmt_id = ast_with_fun_stmt(
        "id",
        smallvec::smallvec![("x".into(), None)],
        None,
        x,
        &mut ast,
        span,
    );

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    // Function should be bound in environment
    let scheme = ctx.env().lookup("id");
    assert!(scheme.is_some());

    let scheme = scheme.unwrap();
    // Should be polymorphic: forall a. (a) -> a
    assert!(!scheme.vars.is_empty());
    match &scheme.ty {
        Ty::Fn(params, ret) => {
            assert_eq!(params.len(), 1);
            // Param and return should be the same type var
            assert_eq!(&params[0], ret.as_ref());
        }
        _ => panic!("expected Fn type, got {:?}", scheme.ty),
    }
}

#[test]
fn fun_with_annotations() {
    // Build: FUN inc(x: Int) -> Int { x + 1 }
    let mut ast = Ast::new();
    let span = Span::new(0, 30);
    let int_ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), span)
        .unwrap();
    let x = ast.add_expr(Expr::Var("x".into()), span).unwrap();
    let one = ast.add_expr(Expr::Literal(Literal::Int(1)), span).unwrap();
    let add = ast
        .add_expr(Expr::Binary(x, BinOp::Add, one), span)
        .unwrap();
    let stmt_id = ast_with_fun_stmt(
        "inc",
        smallvec::smallvec![("x".into(), Some(int_ty))],
        Some(int_ty),
        add,
        &mut ast,
        span,
    );

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    // Function should be bound in environment
    let scheme = ctx.env().lookup("inc");
    assert!(scheme.is_some());

    let scheme = scheme.unwrap();
    // Should be monomorphic: (Int) -> Int (no quantified vars for concrete types)
    assert!(scheme.vars.is_empty());
    assert_eq!(scheme.ty, Ty::Fn(vec![Ty::Int], Box::new(Ty::Int)));
}

#[test]
fn fun_recursive() {
    // Build: FUN factorial(n: Int) -> Int { n * factorial(n - 1) }
    // (simplified: just testing that recursive call works)
    let mut ast = Ast::new();
    let span = Span::new(0, 50);
    let int_ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), span)
        .unwrap();

    // n - 1
    let n = ast.add_expr(Expr::Var("n".into()), span).unwrap();
    let one = ast.add_expr(Expr::Literal(Literal::Int(1)), span).unwrap();
    let n_minus_1 = ast
        .add_expr(Expr::Binary(n, BinOp::Sub, one), span)
        .unwrap();

    // factorial(n - 1)
    let factorial_var =
        ast.add_expr(Expr::Var("factorial".into()), span).unwrap();
    let rec_call = ast
        .add_expr(
            Expr::Call(factorial_var, smallvec::smallvec![n_minus_1]),
            span,
        )
        .unwrap();

    // n * factorial(n - 1)
    let n2 = ast.add_expr(Expr::Var("n".into()), span).unwrap();
    let body = ast
        .add_expr(Expr::Binary(n2, BinOp::Mul, rec_call), span)
        .unwrap();

    let stmt_id = ast_with_fun_stmt(
        "factorial",
        smallvec::smallvec![("n".into(), Some(int_ty))],
        Some(int_ty),
        body,
        &mut ast,
        span,
    );

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    // Should not have errors about undefined variable "factorial"
    // (function name is bound before body is inferred)
    let undef_errors: Vec<_> = ctx
            .errors()
            .iter()
            .filter(|e| matches!(e, TypeError::UndefinedVar(name, _) if name == "factorial"))
            .collect();
    assert!(undef_errors.is_empty());

    // Function should be bound
    let scheme = ctx.env().lookup("factorial");
    assert!(scheme.is_some());
    assert_eq!(scheme.unwrap().ty, Ty::Fn(vec![Ty::Int], Box::new(Ty::Int)));
}

#[test]
fn fun_polymorphic_identity() {
    // Build: FUN id(x) { x }
    // Should generalize to: forall a. (a) -> a
    let mut ast = Ast::new();
    let span = Span::new(0, 20);
    let x = ast.add_expr(Expr::Var("x".into()), span).unwrap();
    let stmt_id = ast_with_fun_stmt(
        "id",
        smallvec::smallvec![("x".into(), None)],
        None,
        x,
        &mut ast,
        span,
    );

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    let scheme = ctx.env().lookup("id").unwrap();

    // Should have exactly one quantified variable
    assert_eq!(scheme.vars.len(), 1);

    // Type should be Fn([?a], ?a) where ?a is the quantified var
    match &scheme.ty {
        Ty::Fn(params, ret) => {
            assert_eq!(params.len(), 1);
            match (&params[0], ret.as_ref()) {
                (Ty::Var(p), Ty::Var(r)) => {
                    assert_eq!(p, r);
                    assert!(scheme.vars.contains(p));
                }
                _ => panic!("expected type vars"),
            }
        }
        _ => panic!("expected Fn"),
    }
}

#[test]
fn fun_multi_params() {
    // Build: FUN add(a: Int, b: Int) -> Int { a + b }
    let mut ast = Ast::new();
    let span = Span::new(0, 40);
    let int_ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), span)
        .unwrap();
    let a = ast.add_expr(Expr::Var("a".into()), span).unwrap();
    let b = ast.add_expr(Expr::Var("b".into()), span).unwrap();
    let add = ast.add_expr(Expr::Binary(a, BinOp::Add, b), span).unwrap();
    let stmt_id = ast_with_fun_stmt(
        "add",
        smallvec::smallvec![
            ("a".into(), Some(int_ty)),
            ("b".into(), Some(int_ty))
        ],
        Some(int_ty),
        add,
        &mut ast,
        span,
    );

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    let scheme = ctx.env().lookup("add");
    assert!(scheme.is_some());
    assert_eq!(
        scheme.unwrap().ty,
        Ty::Fn(vec![Ty::Int, Ty::Int], Box::new(Ty::Int))
    );
}

// Control Flow Tests (Phase 4.8)

#[test]
fn if_expr_same_branch_types() {
    // IF true { 1 } ELSE { 2 } -> Int
    let mut ast = Ast::new();
    let span = Span::new(0, 30);

    let cond = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), span)
        .unwrap();
    let then_br = ast.add_expr(Expr::Literal(Literal::Int(1)), span).unwrap();
    let else_br = ast.add_expr(Expr::Literal(Literal::Int(2)), span).unwrap();
    let if_expr = ast
        .add_expr(Expr::If(cond, then_br, Some(else_br)), span)
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(if_expr);

    assert_eq!(ty, Ty::Int);
    assert!(ctx.errors.is_empty());
}

#[test]
fn if_expr_with_storable_branches_creates_union() {
    // IF true { 1 } ELSE { "hello" } -> Int | String (anonymous union)
    let mut ast = Ast::new();
    let span = Span::new(0, 40);

    let cond = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), span)
        .unwrap();
    let then_br = ast.add_expr(Expr::Literal(Literal::Int(1)), span).unwrap();
    let else_br = ast
        .add_expr(Expr::Literal(Literal::String("hello".into())), span)
        .unwrap();
    let if_expr = ast
        .add_expr(Expr::If(cond, then_br, Some(else_br)), span)
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(if_expr);

    // Should produce anonymous union of storable primitives
    assert!(matches!(ty, Ty::Union(members) if members.len() == 2));
    assert!(ctx.errors.is_empty());
}

#[test]
fn if_single_arm_requires_unit() {
    // IF true { 42 } -> should unify body with Unit
    let mut ast = Ast::new();
    let span = Span::new(0, 20);

    let cond = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), span)
        .unwrap();
    let then_br = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let if_expr = ast.add_expr(Expr::If(cond, then_br, None), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(if_expr);

    // Single-arm IF always returns Unit
    assert_eq!(ty, Ty::Unit);
    // Should have constraint unifying body with Unit
    assert!(ctx.constraints().iter().any(|c| {
        matches!(
            c,
            Constraint::Eq(Ty::Int, Ty::Unit, _)
                | Constraint::Eq(Ty::Unit, Ty::Int, _)
        )
    }));
}

#[test]
fn if_condition_must_be_bool() {
    // IF 42 { 1 } ELSE { 2 } -> should unify condition with Bool
    let mut ast = Ast::new();
    let span = Span::new(0, 25);

    let cond = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let then_br = ast.add_expr(Expr::Literal(Literal::Int(1)), span).unwrap();
    let else_br = ast.add_expr(Expr::Literal(Literal::Int(2)), span).unwrap();
    let if_expr = ast
        .add_expr(Expr::If(cond, then_br, Some(else_br)), span)
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let _ty = ctx.expr(if_expr);

    // Should have constraint unifying Int with Bool
    assert!(ctx.constraints().iter().any(|c| {
        matches!(
            c,
            Constraint::Eq(Ty::Int, Ty::Bool, _)
                | Constraint::Eq(Ty::Bool, Ty::Int, _)
        )
    }));
}

#[test]
fn block_empty_returns_unit() {
    // { } -> Unit
    let mut ast = Ast::new();
    let span = Span::new(0, 5);

    let block = ast.add_expr(Expr::Block(vec![], None), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(block);

    assert_eq!(ty, Ty::Unit);
}

#[test]
fn block_with_tail_returns_tail_type() {
    // { 42 } -> Int
    let mut ast = Ast::new();
    let span = Span::new(0, 10);

    let tail = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let block = ast.add_expr(Expr::Block(vec![], Some(tail)), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(block);

    assert_eq!(ty, Ty::Int);
}

#[test]
fn block_with_string_tail() {
    // { "hello" } -> String
    let mut ast = Ast::new();
    let span = Span::new(0, 15);

    let tail = ast
        .add_expr(Expr::Literal(Literal::String("hello".into())), span)
        .unwrap();
    let block = ast.add_expr(Expr::Block(vec![], Some(tail)), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(block);

    assert_eq!(ty, Ty::String);
}

// MATCH Expression Tests

#[test]
fn match_option_some_extracts_inner_type() {
    // MATCH opt { Option.Some(x) => x, Option.None => 0 }
    // With opt : Option[Int], x should be Int
    let mut ast = Ast::new();
    let span = Span::new(0, 50);

    // Create scrutinee: Option.Some(42)
    let arg = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let scrutinee = ast
        .add_expr(
            Expr::Variant("Option".into(), "Some".into(), smallvec![arg]),
            span,
        )
        .unwrap();

    // Create patterns
    let x_pat = ast.add_pattern(MatchPattern::Var("x".into())).unwrap();
    let some_pat = ast
        .add_pattern(MatchPattern::Variant(
            "Option".into(),
            "Some".into(),
            smallvec![x_pat],
        ))
        .unwrap();
    let none_pat = ast
        .add_pattern(MatchPattern::Variant(
            "Option".into(),
            "None".into(),
            smallvec![],
        ))
        .unwrap();

    // Create arm bodies
    let x_var = ast.add_expr(Expr::Var("x".into()), span).unwrap();
    let zero = ast.add_expr(Expr::Literal(Literal::Int(0)), span).unwrap();

    // Create match expression
    let arms = vec![
        MatchArm {
            pattern: some_pat,
            guard: None,
            body: x_var,
        },
        MatchArm {
            pattern: none_pat,
            guard: None,
            body: zero,
        },
    ];
    let match_expr = ast.add_expr(Expr::Match(scrutinee, arms), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(match_expr);

    // Debug: print errors if any
    if !ctx.errors.is_empty() {
        eprintln!("Errors: {:?}", ctx.errors);
    }

    // Result should be Int (from both arms)
    assert_eq!(ty, Ty::Int);
    assert!(ctx.errors.is_empty());
}

#[test]
fn match_result_ok_and_err_extract_types() {
    // MATCH res { Result.Ok(v) => v, Result.Err(e) => 0 }
    let mut ast = Ast::new();
    let span = Span::new(0, 60);

    // Create scrutinee: Result.Ok(42)
    let arg = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let scrutinee = ast
        .add_expr(
            Expr::Variant("Result".into(), "Ok".into(), smallvec![arg]),
            span,
        )
        .unwrap();

    // Patterns
    let v_pat = ast.add_pattern(MatchPattern::Var("v".into())).unwrap();
    let ok_pat = ast
        .add_pattern(MatchPattern::Variant(
            "Result".into(),
            "Ok".into(),
            smallvec![v_pat],
        ))
        .unwrap();

    let e_pat = ast.add_pattern(MatchPattern::Var("e".into())).unwrap();
    let err_pat = ast
        .add_pattern(MatchPattern::Variant(
            "Result".into(),
            "Err".into(),
            smallvec![e_pat],
        ))
        .unwrap();

    // Bodies
    let v_var = ast.add_expr(Expr::Var("v".into()), span).unwrap();
    let zero = ast.add_expr(Expr::Literal(Literal::Int(0)), span).unwrap();

    let arms = vec![
        MatchArm {
            pattern: ok_pat,
            guard: None,
            body: v_var,
        },
        MatchArm {
            pattern: err_pat,
            guard: None,
            body: zero,
        },
    ];
    let match_expr = ast.add_expr(Expr::Match(scrutinee, arms), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(match_expr);

    assert_eq!(ty, Ty::Int);
    assert!(ctx.errors.is_empty());
}

#[test]
fn match_non_exhaustive_option_emits_error() {
    // MATCH opt { Option.Some(x) => x } -- missing None
    let mut ast = Ast::new();
    let span = Span::new(0, 40);

    let arg = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let scrutinee = ast
        .add_expr(
            Expr::Variant("Option".into(), "Some".into(), smallvec![arg]),
            span,
        )
        .unwrap();

    let x_pat = ast.add_pattern(MatchPattern::Var("x".into())).unwrap();
    let some_pat = ast
        .add_pattern(MatchPattern::Variant(
            "Option".into(),
            "Some".into(),
            smallvec![x_pat],
        ))
        .unwrap();

    let x_var = ast.add_expr(Expr::Var("x".into()), span).unwrap();

    let arms = vec![MatchArm {
        pattern: some_pat,
        guard: None,
        body: x_var,
    }];
    let match_expr = ast.add_expr(Expr::Match(scrutinee, arms), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let _ty = ctx.expr(match_expr);

    // Should have NonExhaustiveMatch error
    assert!(ctx
        .errors
        .iter()
        .any(|e| matches!(e, TypeError::NonExhaustiveMatch(_))));
}

#[test]
fn match_wildcard_makes_exhaustive() {
    // MATCH opt { Option.Some(x) => x, _ => 0 } -- wildcard covers None
    let mut ast = Ast::new();
    let span = Span::new(0, 50);

    let arg = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let scrutinee = ast
        .add_expr(
            Expr::Variant("Option".into(), "Some".into(), smallvec![arg]),
            span,
        )
        .unwrap();

    let x_pat = ast.add_pattern(MatchPattern::Var("x".into())).unwrap();
    let some_pat = ast
        .add_pattern(MatchPattern::Variant(
            "Option".into(),
            "Some".into(),
            smallvec![x_pat],
        ))
        .unwrap();
    let wildcard_pat = ast.add_pattern(MatchPattern::Wildcard).unwrap();

    let x_var = ast.add_expr(Expr::Var("x".into()), span).unwrap();
    let zero = ast.add_expr(Expr::Literal(Literal::Int(0)), span).unwrap();

    let arms = vec![
        MatchArm {
            pattern: some_pat,
            guard: None,
            body: x_var,
        },
        MatchArm {
            pattern: wildcard_pat,
            guard: None,
            body: zero,
        },
    ];
    let match_expr = ast.add_expr(Expr::Match(scrutinee, arms), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(match_expr);

    assert_eq!(ty, Ty::Int);
    // No NonExhaustiveMatch error
    assert!(!ctx
        .errors
        .iter()
        .any(|e| matches!(e, TypeError::NonExhaustiveMatch(_))));
}

#[test]
fn match_guarded_wildcard_not_exhaustive() {
    // MATCH opt { Option.Some(x) => x, _ IF false => 0 }
    // Guarded wildcard does NOT count for exhaustiveness (guard might fail)
    let mut ast = Ast::new();
    let span = Span::new(0, 50);

    let arg = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let scrutinee = ast
        .add_expr(
            Expr::Variant("Option".into(), "Some".into(), smallvec![arg]),
            span,
        )
        .unwrap();

    let x_pat = ast.add_pattern(MatchPattern::Var("x".into())).unwrap();
    let some_pat = ast
        .add_pattern(MatchPattern::Variant(
            "Option".into(),
            "Some".into(),
            smallvec![x_pat],
        ))
        .unwrap();
    let wildcard_pat = ast.add_pattern(MatchPattern::Wildcard).unwrap();

    let x_var = ast.add_expr(Expr::Var("x".into()), span).unwrap();
    let zero = ast.add_expr(Expr::Literal(Literal::Int(0)), span).unwrap();
    let guard = ast
        .add_expr(Expr::Literal(Literal::Bool(false)), span)
        .unwrap();

    let arms = vec![
        MatchArm {
            pattern: some_pat,
            guard: None,
            body: x_var,
        },
        MatchArm {
            pattern: wildcard_pat,
            guard: Some(guard), // Guard makes this arm not count!
            body: zero,
        },
    ];
    let match_expr = ast.add_expr(Expr::Match(scrutinee, arms), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let _ty = ctx.expr(match_expr);

    // Should have NonExhaustiveMatch because guarded wildcard doesn't cover
    assert!(ctx
        .errors
        .iter()
        .any(|e| matches!(e, TypeError::NonExhaustiveMatch(_))));
}

#[test]
fn match_arms_with_storable_types_creates_union() {
    // MATCH opt { Option.Some(x) => x, Option.None => "hello" }
    // Arms have Int and String; now creates anonymous union
    let mut ast = Ast::new();
    let span = Span::new(0, 60);

    let arg = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let scrutinee = ast
        .add_expr(
            Expr::Variant("Option".into(), "Some".into(), smallvec![arg]),
            span,
        )
        .unwrap();

    let x_pat = ast.add_pattern(MatchPattern::Var("x".into())).unwrap();
    let some_pat = ast
        .add_pattern(MatchPattern::Variant(
            "Option".into(),
            "Some".into(),
            smallvec![x_pat],
        ))
        .unwrap();
    let none_pat = ast
        .add_pattern(MatchPattern::Variant(
            "Option".into(),
            "None".into(),
            smallvec![],
        ))
        .unwrap();

    let x_var = ast.add_expr(Expr::Var("x".into()), span).unwrap();
    let hello = ast
        .add_expr(Expr::Literal(Literal::String("hello".into())), span)
        .unwrap();

    let arms = vec![
        MatchArm {
            pattern: some_pat,
            guard: None,
            body: x_var,
        },
        MatchArm {
            pattern: none_pat,
            guard: None,
            body: hello,
        },
    ];
    let match_expr = ast.add_expr(Expr::Match(scrutinee, arms), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(match_expr);

    // With union types, storable primitives create anonymous unions
    // Arms are Int (from x) and String; result is Int | String
    assert!(
        matches!(&ty, Ty::Union(members) if members.len() == 2)
            || matches!(&ty, Ty::Var(_)) // May remain as var if not yet resolved
    );
}

// Union type exhaustiveness tests

#[test]
fn match_union_exhaustive_with_is_patterns() {
    // MATCH val { x IS Int => x, x IS String => 0 }
    // where val : Int | String
    let mut ast = Ast::new();
    let span = Span::new(0, 50);

    // Create type expressions for the union members
    let int_ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), span)
        .unwrap();
    let string_ty = ast
        .add_type_expr(AstTypeExpr::Named("String".into()), span)
        .unwrap();

    // Create scrutinee as an Int literal (will have type Int, but we'll
    // test exhaustiveness against the union conceptually)
    let scrutinee =
        ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();

    // IS patterns
    let int_is = ast
        .add_pattern(MatchPattern::Is("x".into(), int_ty))
        .unwrap();
    let str_is = ast
        .add_pattern(MatchPattern::Is("y".into(), string_ty))
        .unwrap();

    // Bodies
    let x_var = ast.add_expr(Expr::Var("x".into()), span).unwrap();
    let zero = ast.add_expr(Expr::Literal(Literal::Int(0)), span).unwrap();

    let arms = vec![
        MatchArm {
            pattern: int_is,
            guard: None,
            body: x_var,
        },
        MatchArm {
            pattern: str_is,
            guard: None,
            body: zero,
        },
    ];
    let match_expr = ast.add_expr(Expr::Match(scrutinee, arms), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(match_expr);

    // Both arms return Int
    assert_eq!(ty, Ty::Int);
}

#[test]
fn variant_option_none_fresh_type() {
    // Option.None -> Option[?t] (fresh type variable for inner)
    let mut ast = Ast::new();
    let span = Span::new(0, 15);

    let none = ast
        .add_expr(
            Expr::Variant("Option".into(), "None".into(), smallvec![]),
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(none);

    // Should be Option[?t] where ?t is a fresh type variable
    match ty {
        Ty::Option(inner) => {
            assert!(
                matches!(*inner, Ty::Var(_)),
                "Option.None inner should be fresh type var, got {inner:?}"
            );
        }
        _ => panic!("expected Option type, got {ty:?}"),
    }
    assert!(ctx.errors.is_empty());
}

#[test]
fn variant_option_some_infers_inner() {
    // Option.Some(42) -> Option[Int]
    let mut ast = Ast::new();
    let span = Span::new(0, 20);

    let arg = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let some = ast
        .add_expr(
            Expr::Variant("Option".into(), "Some".into(), smallvec![arg]),
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(some);

    assert_eq!(ty, Ty::Option(Box::new(Ty::Int)));
    assert!(ctx.errors.is_empty());
}

#[test]
fn variant_option_some_with_string() {
    // Option.Some("hello") -> Option[String]
    let mut ast = Ast::new();
    let span = Span::new(0, 25);

    let arg = ast
        .add_expr(Expr::Literal(Literal::String("hello".into())), span)
        .unwrap();
    let some = ast
        .add_expr(
            Expr::Variant("Option".into(), "Some".into(), smallvec![arg]),
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(some);

    assert_eq!(ty, Ty::Option(Box::new(Ty::String)));
    assert!(ctx.errors.is_empty());
}

#[test]
fn variant_option_some_nested() {
    // Option.Some(Option.Some(42)) -> Option[Option[Int]]
    let mut ast = Ast::new();
    let span = Span::new(0, 30);

    let inner_arg =
        ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let inner_some = ast
        .add_expr(
            Expr::Variant("Option".into(), "Some".into(), smallvec![inner_arg]),
            span,
        )
        .unwrap();
    let outer_some = ast
        .add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec![inner_some],
            ),
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(outer_some);

    assert_eq!(ty, Ty::Option(Box::new(Ty::Option(Box::new(Ty::Int)))));
    assert!(ctx.errors.is_empty());
}

#[test]
fn variant_result_ok_infers_ok_type() {
    // Result.Ok(42) -> Result[Int, ?e] (fresh error type)
    let mut ast = Ast::new();
    let span = Span::new(0, 20);

    let arg = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let ok = ast
        .add_expr(
            Expr::Variant("Result".into(), "Ok".into(), smallvec![arg]),
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(ok);

    match ty {
        Ty::Result(ok_ty, err_ty) => {
            assert_eq!(*ok_ty, Ty::Int);
            assert!(
                matches!(*err_ty, Ty::Var(_)),
                "Result.Ok error type should be fresh var, got {err_ty:?}"
            );
        }
        _ => panic!("expected Result type, got {ty:?}"),
    }
    assert!(ctx.errors.is_empty());
}

#[test]
fn variant_result_err_infers_err_type() {
    // Result.Err("error") -> Result[?t, String] (fresh ok type)
    let mut ast = Ast::new();
    let span = Span::new(0, 25);

    let arg = ast
        .add_expr(Expr::Literal(Literal::String("error".into())), span)
        .unwrap();
    let err = ast
        .add_expr(
            Expr::Variant("Result".into(), "Err".into(), smallvec![arg]),
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(err);

    match ty {
        Ty::Result(ok_ty, err_ty) => {
            assert!(
                matches!(*ok_ty, Ty::Var(_)),
                "Result.Err ok type should be fresh var, got {ok_ty:?}"
            );
            assert_eq!(*err_ty, Ty::String);
        }
        _ => panic!("expected Result type, got {ty:?}"),
    }
    assert!(ctx.errors.is_empty());
}

#[test]
fn variant_result_ok_with_array() {
    // Result.Ok([1, 2, 3]) -> Result[Array[Int], ?e]
    let mut ast = Ast::new();
    let span = Span::new(0, 30);

    let e1 = ast.add_expr(Expr::Literal(Literal::Int(1)), span).unwrap();
    let e2 = ast.add_expr(Expr::Literal(Literal::Int(2)), span).unwrap();
    let e3 = ast.add_expr(Expr::Literal(Literal::Int(3)), span).unwrap();
    let arr = ast.add_expr(Expr::Array(vec![e1, e2, e3]), span).unwrap();
    let ok = ast
        .add_expr(
            Expr::Variant("Result".into(), "Ok".into(), smallvec![arr]),
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(ok);

    match ty {
        Ty::Result(ok_ty, err_ty) => {
            assert_eq!(*ok_ty, Ty::Array(Box::new(Ty::Int)));
            assert!(matches!(*err_ty, Ty::Var(_)));
        }
        _ => panic!("expected Result type, got {ty:?}"),
    }
    assert!(ctx.errors.is_empty());
}

#[test]
fn variant_arity_mismatch_some_no_args() {
    // Option.Some() -> arity error (expected 1, got 0)
    let mut ast = Ast::new();
    let span = Span::new(0, 15);

    let some = ast
        .add_expr(
            Expr::Variant("Option".into(), "Some".into(), smallvec![]),
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let _ty = ctx.expr(some);

    assert!(ctx.errors.iter().any(|e| matches!(
        e,
        TypeError::ArityMismatch {
            expected: 1,
            got: 0,
            ..
        }
    )));
}

#[test]
fn variant_arity_mismatch_none_with_args() {
    // Option.None(42) -> arity error (expected 0, got 1)
    let mut ast = Ast::new();
    let span = Span::new(0, 20);

    let arg = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let none = ast
        .add_expr(
            Expr::Variant("Option".into(), "None".into(), smallvec![arg]),
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let _ty = ctx.expr(none);

    assert!(ctx.errors.iter().any(|e| matches!(
        e,
        TypeError::ArityMismatch {
            expected: 0,
            got: 1,
            ..
        }
    )));
}

#[test]
fn variant_arity_mismatch_ok_no_args() {
    // Result.Ok() -> arity error (expected 1, got 0)
    let mut ast = Ast::new();
    let span = Span::new(0, 15);

    let ok = ast
        .add_expr(
            Expr::Variant("Result".into(), "Ok".into(), smallvec![]),
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let _ty = ctx.expr(ok);

    assert!(ctx.errors.iter().any(|e| matches!(
        e,
        TypeError::ArityMismatch {
            expected: 1,
            got: 0,
            ..
        }
    )));
}

#[test]
fn variant_arity_mismatch_err_too_many_args() {
    // Result.Err("a", "b") -> arity error (expected 1, got 2)
    let mut ast = Ast::new();
    let span = Span::new(0, 25);

    let a = ast
        .add_expr(Expr::Literal(Literal::String("a".into())), span)
        .unwrap();
    let b = ast
        .add_expr(Expr::Literal(Literal::String("b".into())), span)
        .unwrap();
    let err = ast
        .add_expr(
            Expr::Variant("Result".into(), "Err".into(), smallvec![a, b]),
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let _ty = ctx.expr(err);

    assert!(ctx.errors.iter().any(|e| matches!(
        e,
        TypeError::ArityMismatch {
            expected: 1,
            got: 2,
            ..
        }
    )));
}

#[test]
fn variant_unknown_type() {
    // Unknown.Foo() -> unknown type error
    let mut ast = Ast::new();
    let span = Span::new(0, 15);

    let var = ast
        .add_expr(
            Expr::Variant("Unknown".into(), "Foo".into(), smallvec![]),
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(var);

    assert_eq!(ty, Ty::Error);
    assert!(ctx
            .errors
            .iter()
            .any(|e| matches!(e, TypeError::UnknownType(name, _) if name == "Unknown.Foo")));
}

#[test]
fn variant_unknown_variant_name() {
    // Option.Unknown() -> unknown type error
    let mut ast = Ast::new();
    let span = Span::new(0, 20);

    let var = ast
        .add_expr(
            Expr::Variant("Option".into(), "Unknown".into(), smallvec![]),
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(var);

    assert_eq!(ty, Ty::Error);
    assert!(ctx
            .errors
            .iter()
            .any(|e| matches!(e, TypeError::UnknownType(name, _) if name == "Option.Unknown")));
}

#[test]
fn variant_pattern_extracts_option_some_payload() {
    // In a MATCH, Option.Some(x) pattern should bind x with inner type
    let mut ast = Ast::new();
    let span = Span::new(0, 50);

    // Scrutinee: Option.Some("hello")
    let arg = ast
        .add_expr(Expr::Literal(Literal::String("hello".into())), span)
        .unwrap();
    let scrutinee = ast
        .add_expr(
            Expr::Variant("Option".into(), "Some".into(), smallvec![arg]),
            span,
        )
        .unwrap();

    // Pattern: Option.Some(x)
    let x_pat = ast.add_pattern(MatchPattern::Var("x".into())).unwrap();
    let some_pat = ast
        .add_pattern(MatchPattern::Variant(
            "Option".into(),
            "Some".into(),
            smallvec![x_pat],
        ))
        .unwrap();
    let none_pat = ast
        .add_pattern(MatchPattern::Variant(
            "Option".into(),
            "None".into(),
            smallvec![],
        ))
        .unwrap();

    // Body uses x (should be String)
    let x_var = ast.add_expr(Expr::Var("x".into()), span).unwrap();
    let empty = ast
        .add_expr(Expr::Literal(Literal::String("".into())), span)
        .unwrap();

    let arms = vec![
        MatchArm {
            pattern: some_pat,
            guard: None,
            body: x_var,
        },
        MatchArm {
            pattern: none_pat,
            guard: None,
            body: empty,
        },
    ];
    let match_expr = ast.add_expr(Expr::Match(scrutinee, arms), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(match_expr);

    // Result should be String (from both arms)
    assert_eq!(ty, Ty::String);
    assert!(ctx.errors.is_empty());
}

#[test]
fn variant_pattern_extracts_result_ok_payload() {
    // Result.Ok(v) pattern should bind v with ok type
    let mut ast = Ast::new();
    let span = Span::new(0, 60);

    // Scrutinee: Result.Ok(3.14)
    let arg = ast
        .add_expr(Expr::Literal(Literal::Float(3.14)), span)
        .unwrap();
    let scrutinee = ast
        .add_expr(
            Expr::Variant("Result".into(), "Ok".into(), smallvec![arg]),
            span,
        )
        .unwrap();

    // Patterns
    let v_pat = ast.add_pattern(MatchPattern::Var("v".into())).unwrap();
    let ok_pat = ast
        .add_pattern(MatchPattern::Variant(
            "Result".into(),
            "Ok".into(),
            smallvec![v_pat],
        ))
        .unwrap();

    let e_pat = ast.add_pattern(MatchPattern::Var("e".into())).unwrap();
    let err_pat = ast
        .add_pattern(MatchPattern::Variant(
            "Result".into(),
            "Err".into(),
            smallvec![e_pat],
        ))
        .unwrap();

    // Bodies
    let v_var = ast.add_expr(Expr::Var("v".into()), span).unwrap();
    let zero = ast
        .add_expr(Expr::Literal(Literal::Float(0.0)), span)
        .unwrap();

    let arms = vec![
        MatchArm {
            pattern: ok_pat,
            guard: None,
            body: v_var,
        },
        MatchArm {
            pattern: err_pat,
            guard: None,
            body: zero,
        },
    ];
    let match_expr = ast.add_expr(Expr::Match(scrutinee, arms), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(match_expr);

    // Result should be Float
    assert_eq!(ty, Ty::Float);
    assert!(ctx.errors.is_empty());
}

#[test]
fn variant_pattern_extracts_result_err_payload() {
    // Result.Err(e) pattern should bind e with err type
    let mut ast = Ast::new();
    let span = Span::new(0, 60);

    // Scrutinee: Result.Err("oops")
    let arg = ast
        .add_expr(Expr::Literal(Literal::String("oops".into())), span)
        .unwrap();
    let scrutinee = ast
        .add_expr(
            Expr::Variant("Result".into(), "Err".into(), smallvec![arg]),
            span,
        )
        .unwrap();

    // Patterns
    let v_pat = ast.add_pattern(MatchPattern::Var("v".into())).unwrap();
    let ok_pat = ast
        .add_pattern(MatchPattern::Variant(
            "Result".into(),
            "Ok".into(),
            smallvec![v_pat],
        ))
        .unwrap();

    let e_pat = ast.add_pattern(MatchPattern::Var("e".into())).unwrap();
    let err_pat = ast
        .add_pattern(MatchPattern::Variant(
            "Result".into(),
            "Err".into(),
            smallvec![e_pat],
        ))
        .unwrap();

    // Bodies: Ok arm returns "", Err arm returns e (String)
    let empty = ast
        .add_expr(Expr::Literal(Literal::String("".into())), span)
        .unwrap();
    let e_var = ast.add_expr(Expr::Var("e".into()), span).unwrap();

    let arms = vec![
        MatchArm {
            pattern: ok_pat,
            guard: None,
            body: empty,
        },
        MatchArm {
            pattern: err_pat,
            guard: None,
            body: e_var,
        },
    ];
    let match_expr = ast.add_expr(Expr::Match(scrutinee, arms), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(match_expr);

    // Result should be String (both arms return String)
    assert_eq!(ty, Ty::String);
    assert!(ctx.errors.is_empty());
}

#[test]
fn variant_non_exhaustive_result_missing_err() {
    // MATCH res { Result.Ok(v) => v } -- missing Err arm
    let mut ast = Ast::new();
    let span = Span::new(0, 40);

    let arg = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let scrutinee = ast
        .add_expr(
            Expr::Variant("Result".into(), "Ok".into(), smallvec![arg]),
            span,
        )
        .unwrap();

    let v_pat = ast.add_pattern(MatchPattern::Var("v".into())).unwrap();
    let ok_pat = ast
        .add_pattern(MatchPattern::Variant(
            "Result".into(),
            "Ok".into(),
            smallvec![v_pat],
        ))
        .unwrap();

    let v_var = ast.add_expr(Expr::Var("v".into()), span).unwrap();

    let arms = vec![MatchArm {
        pattern: ok_pat,
        guard: None,
        body: v_var,
    }];
    let match_expr = ast.add_expr(Expr::Match(scrutinee, arms), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let _ty = ctx.expr(match_expr);

    // Should have NonExhaustiveMatch error
    assert!(ctx
        .errors
        .iter()
        .any(|e| matches!(e, TypeError::NonExhaustiveMatch(_))));
}

#[test]
fn variant_non_exhaustive_result_missing_ok() {
    // MATCH res { Result.Err(e) => e } -- missing Ok arm
    let mut ast = Ast::new();
    let span = Span::new(0, 40);

    let arg = ast
        .add_expr(Expr::Literal(Literal::String("err".into())), span)
        .unwrap();
    let scrutinee = ast
        .add_expr(
            Expr::Variant("Result".into(), "Err".into(), smallvec![arg]),
            span,
        )
        .unwrap();

    let e_pat = ast.add_pattern(MatchPattern::Var("e".into())).unwrap();
    let err_pat = ast
        .add_pattern(MatchPattern::Variant(
            "Result".into(),
            "Err".into(),
            smallvec![e_pat],
        ))
        .unwrap();

    let e_var = ast.add_expr(Expr::Var("e".into()), span).unwrap();

    let arms = vec![MatchArm {
        pattern: err_pat,
        guard: None,
        body: e_var,
    }];
    let match_expr = ast.add_expr(Expr::Match(scrutinee, arms), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let _ty = ctx.expr(match_expr);

    // Should have NonExhaustiveMatch error
    assert!(ctx
        .errors
        .iter()
        .any(|e| matches!(e, TypeError::NonExhaustiveMatch(_))));
}

#[test]
fn variant_bool_exhaustive() {
    // MATCH b { true => 1, false => 0 }
    let mut ast = Ast::new();
    let span = Span::new(0, 40);

    let scrutinee = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), span)
        .unwrap();

    let true_pat = ast
        .add_pattern(MatchPattern::Literal(Literal::Bool(true)))
        .unwrap();
    let false_pat = ast
        .add_pattern(MatchPattern::Literal(Literal::Bool(false)))
        .unwrap();

    let one = ast.add_expr(Expr::Literal(Literal::Int(1)), span).unwrap();
    let zero = ast.add_expr(Expr::Literal(Literal::Int(0)), span).unwrap();

    let arms = vec![
        MatchArm {
            pattern: true_pat,
            guard: None,
            body: one,
        },
        MatchArm {
            pattern: false_pat,
            guard: None,
            body: zero,
        },
    ];
    let match_expr = ast.add_expr(Expr::Match(scrutinee, arms), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(match_expr);

    assert_eq!(ty, Ty::Int);
    // No NonExhaustiveMatch error
    assert!(!ctx
        .errors
        .iter()
        .any(|e| matches!(e, TypeError::NonExhaustiveMatch(_))));
}

#[test]
fn variant_bool_non_exhaustive_missing_false() {
    // MATCH b { true => 1 } -- missing false
    let mut ast = Ast::new();
    let span = Span::new(0, 30);

    let scrutinee = ast
        .add_expr(Expr::Literal(Literal::Bool(true)), span)
        .unwrap();

    let true_pat = ast
        .add_pattern(MatchPattern::Literal(Literal::Bool(true)))
        .unwrap();

    let one = ast.add_expr(Expr::Literal(Literal::Int(1)), span).unwrap();

    let arms = vec![MatchArm {
        pattern: true_pat,
        guard: None,
        body: one,
    }];
    let match_expr = ast.add_expr(Expr::Match(scrutinee, arms), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let _ty = ctx.expr(match_expr);

    // Should have NonExhaustiveMatch error
    assert!(ctx
        .errors
        .iter()
        .any(|e| matches!(e, TypeError::NonExhaustiveMatch(_))));
}

#[test]
fn variant_int_requires_wildcard() {
    // MATCH n { 1 => "one" } -- non-exhaustive without wildcard
    let mut ast = Ast::new();
    let span = Span::new(0, 30);

    let scrutinee =
        ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();

    let one_pat = ast
        .add_pattern(MatchPattern::Literal(Literal::Int(1)))
        .unwrap();

    let one_str = ast
        .add_expr(Expr::Literal(Literal::String("one".into())), span)
        .unwrap();

    let arms = vec![MatchArm {
        pattern: one_pat,
        guard: None,
        body: one_str,
    }];
    let match_expr = ast.add_expr(Expr::Match(scrutinee, arms), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let _ty = ctx.expr(match_expr);

    // Should have NonExhaustiveMatch error (Int requires wildcard)
    assert!(ctx
        .errors
        .iter()
        .any(|e| matches!(e, TypeError::NonExhaustiveMatch(_))));
}

#[test]
fn variant_int_with_wildcard_exhaustive() {
    // MATCH n { 1 => "one", _ => "other" }
    let mut ast = Ast::new();
    let span = Span::new(0, 40);

    let scrutinee =
        ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();

    let one_pat = ast
        .add_pattern(MatchPattern::Literal(Literal::Int(1)))
        .unwrap();
    let wild_pat = ast.add_pattern(MatchPattern::Wildcard).unwrap();

    let one_str = ast
        .add_expr(Expr::Literal(Literal::String("one".into())), span)
        .unwrap();
    let other_str = ast
        .add_expr(Expr::Literal(Literal::String("other".into())), span)
        .unwrap();

    let arms = vec![
        MatchArm {
            pattern: one_pat,
            guard: None,
            body: one_str,
        },
        MatchArm {
            pattern: wild_pat,
            guard: None,
            body: other_str,
        },
    ];
    let match_expr = ast.add_expr(Expr::Match(scrutinee, arms), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(match_expr);

    assert_eq!(ty, Ty::String);
    // No NonExhaustiveMatch error
    assert!(!ctx
        .errors
        .iter()
        .any(|e| matches!(e, TypeError::NonExhaustiveMatch(_))));
}

// Phase 4.10: Special Expressions Tests

#[test]
fn unwrap_creates_unwrappable_constraint() {
    // opt! where opt is Option[Int]
    let mut ast = Ast::new();
    let span = Span::new(0, 10);

    // Create a variable that we'll type as Option[Int]
    let opt_var = ast.add_expr(Expr::Var("opt".into()), span).unwrap();
    let unwrap_expr = ast.add_expr(Expr::Unwrap(opt_var), span).unwrap();

    let mut ctx = test_ctx(&ast);
    // Bind opt to Option[Int]
    ctx.env_mut()
        .bind("opt", Scheme::mono(Ty::Option(Box::new(Ty::Int))));

    let ty = ctx.expr(unwrap_expr);

    // Should be a fresh type variable (unification will resolve to Int)
    assert!(matches!(ty, Ty::Var(_)));

    // Should have an Unwrappable constraint
    assert!(ctx
        .constraints()
        .iter()
        .any(|c| matches!(c, Constraint::Unwrappable { .. })));
}

#[test]
fn is_check_returns_bool() {
    // x IS Int
    let mut ast = Ast::new();
    let span = Span::new(0, 10);

    let x_var = ast.add_expr(Expr::Var("x".into()), span).unwrap();
    let ty_id = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), span)
        .unwrap();
    let is_expr = ast
        .add_expr(Expr::Is(x_var, TypePattern::Type(ty_id)), span)
        .unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.env_mut().bind("x", Scheme::mono(Ty::Unknown));

    let ty = ctx.expr(is_expr);
    assert_eq!(ty, Ty::Bool);
}

#[test]
fn is_check_variant() {
    // opt IS Option.Some(x)
    let mut ast = Ast::new();
    let span = Span::new(0, 20);

    let opt_var = ast.add_expr(Expr::Var("opt".into()), span).unwrap();
    let is_expr = ast
        .add_expr(
            Expr::Is(
                opt_var,
                TypePattern::VariantBind(
                    "Option".into(),
                    "Some".into(),
                    smallvec!["val".into()],
                ),
            ),
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.env_mut()
        .bind("opt", Scheme::mono(Ty::Option(Box::new(Ty::Int))));

    let ty = ctx.expr(is_expr);
    assert_eq!(ty, Ty::Bool);
    assert!(!ctx.has_errors());
}

#[test]
fn is_check_variant_arity_mismatch() {
    // opt IS Option.Some(a, b) -- wrong arity (should be 1)
    let mut ast = Ast::new();
    let span = Span::new(0, 25);

    let opt_var = ast.add_expr(Expr::Var("opt".into()), span).unwrap();
    let is_expr = ast
        .add_expr(
            Expr::Is(
                opt_var,
                TypePattern::VariantBind(
                    "Option".into(),
                    "Some".into(),
                    smallvec!["a".into(), "b".into()],
                ),
            ),
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.env_mut()
        .bind("opt", Scheme::mono(Ty::Option(Box::new(Ty::Int))));

    let ty = ctx.expr(is_expr);
    assert_eq!(ty, Ty::Bool);
    assert!(ctx.has_errors());
    assert!(ctx
        .errors()
        .iter()
        .any(|e| matches!(e, TypeError::ArityMismatch { .. })));
}

#[test]
fn as_cast_int_to_float() {
    // 42 AS Float
    let mut ast = Ast::new();
    let span = Span::new(0, 12);

    let int_lit = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let float_ty = ast
        .add_type_expr(AstTypeExpr::Named("Float".into()), span)
        .unwrap();
    let as_expr = ast.add_expr(Expr::As(int_lit, float_ty), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(as_expr);

    assert_eq!(ty, Ty::Float);
    assert!(!ctx.has_errors());
}

#[test]
fn as_cast_float_to_int() {
    // 3.14 AS Int
    let mut ast = Ast::new();
    let span = Span::new(0, 12);

    let float_lit = ast
        .add_expr(Expr::Literal(Literal::Float(3.14)), span)
        .unwrap();
    let int_ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), span)
        .unwrap();
    let as_expr = ast.add_expr(Expr::As(float_lit, int_ty), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(as_expr);

    assert_eq!(ty, Ty::Int);
    assert!(!ctx.has_errors());
}

#[test]
fn as_cast_to_string() {
    // 42 AS String
    let mut ast = Ast::new();
    let span = Span::new(0, 14);

    let int_lit = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let string_ty = ast
        .add_type_expr(AstTypeExpr::Named("String".into()), span)
        .unwrap();
    let as_expr = ast.add_expr(Expr::As(int_lit, string_ty), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(as_expr);

    assert_eq!(ty, Ty::String);
    assert!(!ctx.has_errors());

    // Should have Stringable constraint
    assert!(ctx
        .constraints()
        .iter()
        .any(|c| matches!(c, Constraint::Stringable(_, _))));
}

#[test]
fn as_cast_to_json() {
    // { a: 1 } AS Json
    let mut ast = Ast::new();
    let span = Span::new(0, 18);

    let one_lit = ast.add_expr(Expr::Literal(Literal::Int(1)), span).unwrap();
    let obj = ast
        .add_expr(Expr::Object(vec![("a".into(), one_lit)]), span)
        .unwrap();
    let json_ty = ast
        .add_type_expr(AstTypeExpr::Named("Json".into()), span)
        .unwrap();
    let as_expr = ast.add_expr(Expr::As(obj, json_ty), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(as_expr);

    assert_eq!(ty, Ty::Json);
    assert!(!ctx.has_errors());

    // Should have Jsonable constraint
    assert!(ctx
        .constraints()
        .iter()
        .any(|c| matches!(c, Constraint::Jsonable(_, _))));
}

#[test]
fn as_cast_invalid() {
    // [1, 2] AS Int -- invalid
    let mut ast = Ast::new();
    let span = Span::new(0, 12);

    let one = ast.add_expr(Expr::Literal(Literal::Int(1)), span).unwrap();
    let two = ast.add_expr(Expr::Literal(Literal::Int(2)), span).unwrap();
    let arr = ast.add_expr(Expr::Array(vec![one, two]), span).unwrap();
    let int_ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), span)
        .unwrap();
    let as_expr = ast.add_expr(Expr::As(arr, int_ty), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(as_expr);

    // Still returns target type for error recovery
    assert_eq!(ty, Ty::Int);
    assert!(ctx.has_errors());
    assert!(ctx
        .errors()
        .iter()
        .any(|e| matches!(e, TypeError::InvalidCast { .. })));
}

#[test]
fn as_cast_storable_to_non_member() {
    // x AS Array[Int] where x: Storable -- invalid (Array is not a Storable member)
    let mut ast = Ast::new();
    let span = Span::new(0, 20);

    let x_var = ast.add_expr(Expr::Var("x".into()), span).unwrap();
    let int_ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), span)
        .unwrap();
    let arr_ty = ast
        .add_type_expr(
            AstTypeExpr::App("Array".into(), smallvec![int_ty]),
            span,
        )
        .unwrap();
    let as_expr = ast.add_expr(Expr::As(x_var, arr_ty), span).unwrap();

    let mut ctx = test_ctx(&ast);
    // Bind x to Storable union type
    ctx.env_mut()
        .bind("x", Scheme::mono(Ty::Named(TypeId::STORABLE, vec![])));

    let ty = ctx.expr(as_expr);

    // Returns target type for error recovery
    assert_eq!(ty, Ty::Array(Box::new(Ty::Int)));
    // Should emit InvalidCast error
    assert!(ctx.has_errors());
    assert!(ctx
        .errors()
        .iter()
        .any(|e| matches!(e, TypeError::InvalidCast { .. })));
}

#[test]
fn read_returns_result() {
    // "42" READ Int
    let mut ast = Ast::new();
    let span = Span::new(0, 14);

    let str_lit = ast
        .add_expr(Expr::Literal(Literal::String("42".into())), span)
        .unwrap();
    let int_ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), span)
        .unwrap();
    let read_expr = ast.add_expr(Expr::Read(str_lit, int_ty), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(read_expr);

    assert_eq!(ty, Ty::Result(Box::new(Ty::Int), Box::new(Ty::String)));
    assert!(!ctx.has_errors());
}

#[test]
fn read_array_type() {
    // json READ Array[Int]
    let mut ast = Ast::new();
    let span = Span::new(0, 20);

    let json_var = ast.add_expr(Expr::Var("json".into()), span).unwrap();
    let int_ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), span)
        .unwrap();
    let arr_ty = ast
        .add_type_expr(
            AstTypeExpr::App("Array".into(), smallvec![int_ty]),
            span,
        )
        .unwrap();
    let read_expr = ast.add_expr(Expr::Read(json_var, arr_ty), span).unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.env_mut().bind("json", Scheme::mono(Ty::Json));

    let ty = ctx.expr(read_expr);

    assert_eq!(
        ty,
        Ty::Result(
            Box::new(Ty::Array(Box::new(Ty::Int))),
            Box::new(Ty::String)
        )
    );
    assert!(!ctx.has_errors());
}

#[test]
fn get_returns_storable() {
    // GET local("key")
    let mut ast = Ast::new();
    let span = Span::new(0, 15);

    let key_lit = ast
        .add_expr(Expr::Literal(Literal::String("key".into())), span)
        .unwrap();
    let local = ast
        .add_expr(Expr::Local("test".into(), smallvec![key_lit]), span)
        .unwrap();
    let get_expr = ast.add_expr(Expr::Get(local), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(get_expr);

    assert_eq!(ty, Ty::Named(TypeId::STORABLE, vec![]));
    assert!(!ctx.has_errors());

    // Should have Subscript constraint for the key
    assert!(ctx
        .constraints()
        .iter()
        .any(|c| matches!(c, Constraint::Subscript(_, _))));
}

#[test]
fn annotate_unifies_types() {
    // (42) : Int
    let mut ast = Ast::new();
    let span = Span::new(0, 10);

    let int_lit = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let int_ty = ast
        .add_type_expr(AstTypeExpr::Named("Int".into()), span)
        .unwrap();
    let ann_expr = ast.add_expr(Expr::Annotate(int_lit, int_ty), span).unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(ann_expr);

    assert_eq!(ty, Ty::Int);
    assert!(!ctx.has_errors());
}

#[test]
fn annotate_mismatch() {
    // (42) : String -- mismatch
    let mut ast = Ast::new();
    let span = Span::new(0, 14);

    let int_lit = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let string_ty = ast
        .add_type_expr(AstTypeExpr::Named("String".into()), span)
        .unwrap();
    let ann_expr = ast
        .add_expr(Expr::Annotate(int_lit, string_ty), span)
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(ann_expr);

    // Returns annotation type
    assert_eq!(ty, Ty::String);

    // Should have Eq constraint that will fail during solving
    assert!(ctx
        .constraints()
        .iter()
        .any(|c| matches!(c, Constraint::Eq(Ty::Int, Ty::String, _))));
}

#[test]
fn annotate_option_none() {
    // (Option.None) : Option[String]
    let mut ast = Ast::new();
    let span = Span::new(0, 25);

    let none_expr = ast
        .add_expr(
            Expr::Variant("Option".into(), "None".into(), smallvec![]),
            span,
        )
        .unwrap();
    let string_ty = ast
        .add_type_expr(AstTypeExpr::Named("String".into()), span)
        .unwrap();
    let opt_ty = ast
        .add_type_expr(
            AstTypeExpr::App("Option".into(), smallvec![string_ty]),
            span,
        )
        .unwrap();
    let ann_expr = ast
        .add_expr(Expr::Annotate(none_expr, opt_ty), span)
        .unwrap();

    let mut ctx = test_ctx(&ast);
    let ty = ctx.expr(ann_expr);

    assert_eq!(ty, Ty::Option(Box::new(Ty::String)));
    // Unification will handle type variable reconciliation
}

// --- Statement inference tests ---

#[test]
fn let_simple_var() {
    // LET x = 42
    let mut ast = Ast::new();
    let span = Span::new(0, 10);
    let lit = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let stmt_id = ast
        .add_stmt(Stmt::Let(BindingPattern::Var("x".into()), None, lit), span)
        .unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    let scheme = ctx.env().lookup("x");
    assert!(scheme.is_some());
    assert_eq!(scheme.unwrap().ty, Ty::Int);
}

#[test]
fn let_with_annotation() {
    // LET x: Float = 42 (should unify Int with Float)
    let mut ast = Ast::new();
    let span = Span::new(0, 15);
    let float_ty = ast
        .add_type_expr(AstTypeExpr::Named("Float".into()), span)
        .unwrap();
    let lit = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let stmt_id = ast
        .add_stmt(
            Stmt::Let(BindingPattern::Var("x".into()), Some(float_ty), lit),
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    // Variable bound with annotation type
    let scheme = ctx.env().lookup("x");
    assert!(scheme.is_some());
    assert_eq!(scheme.unwrap().ty, Ty::Float);

    // Should have Eq constraint for unification
    assert!(ctx
        .constraints()
        .iter()
        .any(|c| matches!(c, Constraint::Eq(Ty::Int, Ty::Float, _))));
}

#[test]
fn let_tuple_destructure() {
    // LET (a, b) = (1, "hello")
    let mut ast = Ast::new();
    let span = Span::new(0, 25);
    let int_lit = ast.add_expr(Expr::Literal(Literal::Int(1)), span).unwrap();
    let str_lit = ast
        .add_expr(Expr::Literal(Literal::String("hello".into())), span)
        .unwrap();
    let tuple = ast
        .add_expr(Expr::Tuple(smallvec![int_lit, str_lit]), span)
        .unwrap();

    let pattern = BindingPattern::Tuple(vec![
        BindingPattern::Var("a".into()),
        BindingPattern::Var("b".into()),
    ]);
    let stmt_id = ast.add_stmt(Stmt::Let(pattern, None, tuple), span).unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    // Both bindings should be in env
    assert_eq!(ctx.env().lookup("a").map(|s| &s.ty), Some(&Ty::Int));
    assert_eq!(ctx.env().lookup("b").map(|s| &s.ty), Some(&Ty::String));
}

#[test]
fn let_object_destructure() {
    // LET { name } = { name: "Alice" }
    let mut ast = Ast::new();
    let span = Span::new(0, 30);
    let str_lit = ast
        .add_expr(Expr::Literal(Literal::String("Alice".into())), span)
        .unwrap();
    let obj = ast
        .add_expr(Expr::Object(vec![("name".into(), str_lit)]), span)
        .unwrap();

    let pattern = BindingPattern::Object(vec![(
        "name".into(),
        BindingPattern::Var("name".into()),
    )]);
    let stmt_id = ast.add_stmt(Stmt::Let(pattern, None, obj), span).unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    assert_eq!(ctx.env().lookup("name").map(|s| &s.ty), Some(&Ty::String));
}

#[test]
fn let_array_destructure() {
    // LET [a, b] = [1, 2]
    let mut ast = Ast::new();
    let span = Span::new(0, 20);
    let e1 = ast.add_expr(Expr::Literal(Literal::Int(1)), span).unwrap();
    let e2 = ast.add_expr(Expr::Literal(Literal::Int(2)), span).unwrap();
    let arr = ast.add_expr(Expr::Array(vec![e1, e2]), span).unwrap();

    let pattern = BindingPattern::Array(
        vec![
            BindingPattern::Var("a".into()),
            BindingPattern::Var("b".into()),
        ],
        None,
    );
    let stmt_id = ast.add_stmt(Stmt::Let(pattern, None, arr), span).unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    // Both should have element type (Int)
    assert_eq!(ctx.env().lookup("a").map(|s| &s.ty), Some(&Ty::Int));
    assert_eq!(ctx.env().lookup("b").map(|s| &s.ty), Some(&Ty::Int));
}

#[test]
fn let_array_rest_pattern() {
    // LET [head, ...rest] = [1, 2, 3]
    let mut ast = Ast::new();
    let span = Span::new(0, 25);
    let e1 = ast.add_expr(Expr::Literal(Literal::Int(1)), span).unwrap();
    let e2 = ast.add_expr(Expr::Literal(Literal::Int(2)), span).unwrap();
    let e3 = ast.add_expr(Expr::Literal(Literal::Int(3)), span).unwrap();
    let arr = ast.add_expr(Expr::Array(vec![e1, e2, e3]), span).unwrap();

    let pattern = BindingPattern::Array(
        vec![BindingPattern::Var("head".into())],
        Some(RestPattern::Bind("rest".into())),
    );
    let stmt_id = ast.add_stmt(Stmt::Let(pattern, None, arr), span).unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    // head should have element type
    assert_eq!(ctx.env().lookup("head").map(|s| &s.ty), Some(&Ty::Int));
    // rest should have array type
    assert_eq!(
        ctx.env().lookup("rest").map(|s| &s.ty),
        Some(&Ty::Array(Box::new(Ty::Int)))
    );
}

#[test]
fn let_wildcard() {
    // LET _ = 42 (no binding)
    let mut ast = Ast::new();
    let span = Span::new(0, 10);
    let lit = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let stmt_id = ast
        .add_stmt(Stmt::Let(BindingPattern::Wildcard, None, lit), span)
        .unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    // No bindings added
    assert!(!ctx.has_errors());
}

#[test]
fn let_nested_destructure() {
    // LET (a, (b, c)) = (1, (2, 3))
    let mut ast = Ast::new();
    let span = Span::new(0, 30);
    let e1 = ast.add_expr(Expr::Literal(Literal::Int(1)), span).unwrap();
    let e2 = ast.add_expr(Expr::Literal(Literal::Int(2)), span).unwrap();
    let e3 = ast.add_expr(Expr::Literal(Literal::Int(3)), span).unwrap();
    let inner = ast.add_expr(Expr::Tuple(smallvec![e2, e3]), span).unwrap();
    let outer = ast
        .add_expr(Expr::Tuple(smallvec![e1, inner]), span)
        .unwrap();

    let pattern = BindingPattern::Tuple(vec![
        BindingPattern::Var("a".into()),
        BindingPattern::Tuple(vec![
            BindingPattern::Var("b".into()),
            BindingPattern::Var("c".into()),
        ]),
    ]);
    let stmt_id = ast.add_stmt(Stmt::Let(pattern, None, outer), span).unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    assert_eq!(ctx.env().lookup("a").map(|s| &s.ty), Some(&Ty::Int));
    assert_eq!(ctx.env().lookup("b").map(|s| &s.ty), Some(&Ty::Int));
    assert_eq!(ctx.env().lookup("c").map(|s| &s.ty), Some(&Ty::Int));
}

#[test]
fn output_adds_stringable_constraint() {
    // OUTPUT 42
    let mut ast = Ast::new();
    let span = Span::new(0, 10);
    let lit = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let stmt_id = ast.add_stmt(Stmt::Output(lit), span).unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    // Should have Stringable constraint
    assert!(ctx
        .constraints()
        .iter()
        .any(|c| matches!(c, Constraint::Stringable(Ty::Int, _))));
}

#[test]
fn expr_stmt_infers_expression() {
    // 1 + 2 (expression statement)
    let mut ast = Ast::new();
    let span = Span::new(0, 5);
    let lhs = ast.add_expr(Expr::Literal(Literal::Int(1)), span).unwrap();
    let rhs = ast.add_expr(Expr::Literal(Literal::Int(2)), span).unwrap();
    let add = ast
        .add_expr(Expr::Binary(lhs, BinOp::Add, rhs), span)
        .unwrap();
    let stmt_id = ast.add_stmt(Stmt::Expr(add), span).unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    // Expression should be typed
    assert_eq!(ctx.get_type(add), Some(&Ty::Int));
}

#[test]
fn set_adds_storable_constraint() {
    // SET local("key") = 42
    let mut ast = Ast::new();
    let span = Span::new(0, 20);
    let key = ast
        .add_expr(Expr::Literal(Literal::String("key".into())), span)
        .unwrap();
    let target = ast
        .add_expr(Expr::Local("local".into(), smallvec![key]), span)
        .unwrap();
    let val = ast.add_expr(Expr::Literal(Literal::Int(42)), span).unwrap();
    let stmt_id = ast.add_stmt(Stmt::Set(target, val), span).unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    // Should have Subscript constraint for the key
    assert!(ctx
        .constraints()
        .iter()
        .any(|c| matches!(c, Constraint::Subscript(Ty::String, _))));

    // Should have Storable constraint for the value
    assert!(ctx
        .constraints()
        .iter()
        .any(|c| matches!(c, Constraint::Storable(Ty::Int, _))));
}

#[test]
fn kill_adds_subscript_constraints() {
    // KILL local("key", 123)
    let mut ast = Ast::new();
    let span = Span::new(0, 25);
    let k1 = ast
        .add_expr(Expr::Literal(Literal::String("key".into())), span)
        .unwrap();
    let k2 = ast
        .add_expr(Expr::Literal(Literal::Int(123)), span)
        .unwrap();
    let target = ast
        .add_expr(Expr::Local("local".into(), smallvec![k1, k2]), span)
        .unwrap();
    let stmt_id = ast.add_stmt(Stmt::Kill(target), span).unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    // Should have Subscript constraints for both keys
    let subs: Vec<_> = ctx
        .constraints()
        .iter()
        .filter(|c| matches!(c, Constraint::Subscript(_, _)))
        .collect();
    assert_eq!(subs.len(), 2);
}

#[test]
fn type_stmt_no_error() {
    // TYPE Foo = { x: Int }
    // Type declarations are processed by registry; stmt just ignores them
    let mut ast = Ast::new();
    let span = Span::new(0, 20);
    let stmt_id = ast
        .add_stmt(
            Stmt::Type {
                name: "Foo".into(),
                type_params: SmallVec::new(),
                def: crate::ast::TypeDefAst::Struct(vec![]),
            },
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    // No errors, no changes to env
    assert!(!ctx.has_errors());
}

#[test]
fn union_stmt_no_error() {
    // UNION Bar = Int | String
    // Union declarations are processed by registry; stmt just ignores them
    let mut ast = Ast::new();
    let span = Span::new(0, 25);
    let stmt_id = ast
        .add_stmt(
            Stmt::Union {
                name: "Bar".into(),
                type_params: SmallVec::new(),
                members: SmallVec::new(),
            },
            span,
        )
        .unwrap();

    let mut ctx = test_ctx(&ast);
    ctx.stmt(stmt_id);

    // No errors, no changes to env
    assert!(!ctx.has_errors());
}

/// Test parametric struct with Option[T] field.
///
/// This mirrors what happens in script 56_struct_types.rumps:
/// ```rumps
/// TYPE Maybe[T] = { inner: Option[T] }
/// LET some-val: Maybe[Int] = { inner: Option.Some(123) }
/// ```
#[test]
fn parametric_struct_with_option_field() {
    use crate::parser::Parser;
    use crate::resolve::resolve;
    use crate::value::{TypeExprArena, ValueArena};

    // Reduced test case - find the minimal failing case
    let src = r#"
TYPE Box[T] = { value: T }
LET int-box: Box[Int] = { value: 42 }
OUTPUT int-box.value

TYPE Pair[L, R] = { left: L, right: R }
LET nested: Box[Pair[Int, Bool]] = { value: { left: 99, right: true } }
OUTPUT nested.value.left
OUTPUT nested.value.right

TYPE Maybe[T] = { inner: Option[T] }
LET some-val: Maybe[Int] = { inner: Option.Some(123) }
LET none-val: Maybe[Int] = { inner: Option.None }
OUTPUT some-val.inner!
        "#;

    let mut result = Parser::parse(src).expect("parse failed");
    let ast = &mut result.ast;
    let stmts = &result.stmts;

    let mut arena = ValueArena::new();
    let mut type_exprs = TypeExprArena::new();
    let mut registry = TypeRegistry::new(&mut arena, &mut type_exprs).unwrap();

    // Register user types (like interpreter does)
    registry
        .register_from_ast(ast, stmts, &mut arena, &mut type_exprs)
        .expect("register failed");

    // Resolve (like interpreter does)
    resolve(ast, &mut arena, &registry);

    let env = crate::env::Environment::new();
    let strings = arena.interner();

    // Type check
    let mut ctx = InferCtx::new(ast, &registry, &type_exprs, &env, strings);
    stmts.iter().for_each(|id| ctx.stmt(*id));

    let subst = ctx.solve_constraints();
    ctx.apply_subst(&subst);
    ctx.check_remaining_unknowns();

    // Should have no errors
    if ctx.has_errors() {
        panic!(
            "unexpected type errors: {:?}",
            ctx.errors
                .iter()
                .map(|e| format!("{:?}", e))
                .collect::<Vec<_>>()
        );
    }
}
