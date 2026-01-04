//! Expression type inference.
//!
//! Contains methods for inferring types of expressions: literals, variables,
//! operators, collections, function calls, control flow, etc.

use std::collections::HashMap;

use indexmap::IndexMap;
use smallvec::SmallVec;

use super::{Constraint, InferCtx};
use crate::ast::{
    ArrayElem, AstTypeExpr, AstTypeExprId, BinOp, DbRef, Expr, ExprId,
    JsonAccessKey, JsonAccessKind, Literal, MatchArm, ObjectEntry, StmtId,
    SubscriptElem, TransactionExpr, TxnId, TypeParam, TypePattern, UnOp,
    UserConstraint,
};
use crate::intern::StringId;
use crate::typecheck::error::TypeError;
use crate::typecheck::ty::{Scheme, Ty};
use crate::value::{TypeDef, TypeId};
use crate::Span;

impl InferCtx<'_> {
    /// Infer the type of an expression.
    ///
    /// Records the inferred type in `expr_types` and returns it. For undefined
    /// variables, records an error and returns `Ty::Error` for recovery.
    ///
    /// Currently handles Phase 4.3 expressions (literals and variables).
    /// Other expression types will be added in subsequent phases.
    pub(crate) fn expr(&mut self, id: ExprId) -> Ty {
        let span = self.ast.expr_span(id).unwrap_or(Span::new(0, 0));
        // Clone the expression to avoid borrow issues with mutable ast reference
        let ty = match self.ast.get_expr(id).cloned() {
            None => Ty::Error,
            Some(expr) => self.expr_inner(id, &expr, span),
        };
        self.record_type(id, ty.clone());
        ty
    }

    /// Inner expression inference; dispatches on expression variant.
    fn expr_inner(&mut self, id: ExprId, expr: &Expr, span: Span) -> Ty {
        match expr {
            // Literals
            Expr::Literal(lit) => self.literal(lit),

            // Unit: empty tuple
            Expr::Tuple(elems) if elems.is_empty() => Ty::Unit,

            // Variable reference
            Expr::Var(name) => self.var(name, span),

            // Binary operations
            Expr::Binary(lhs, op, rhs) => self.binary(*lhs, *op, *rhs, span),

            // Unary operations
            Expr::Unary(op, operand) => self.unary(*op, *operand, span),

            // Range expressions
            Expr::Range(start, end, _inclusive) => {
                let start_ty = self.expr(*start);
                let end_ty = self.expr(*end);
                self.unify(start_ty, Ty::Int, span);
                self.unify(end_ty, Ty::Int, span);
                Ty::Range
            }

            // Arrays
            Expr::Array(elems) => self.array(elems, span),

            // Non-empty tuples (empty handled above as Unit)
            Expr::Tuple(elems) => self.tuple(elems),

            // Structural objects with potential spreads
            Expr::Object(entries) => self.object(entries, span),

            // Map literals
            Expr::MapLit(entries) => self.map_lit(entries, span),

            // Field access: obj.field
            Expr::Field(base, field) => self.field(*base, field, span),

            // Optional field access: obj?.field
            Expr::OptionalField(base, field) => {
                self.optional_field(*base, field, span)
            }

            // Tuple index: tuple.0, tuple.1, etc.
            Expr::TupleIndex(base, idx) => self.tuple_index(*base, *idx, span),

            // Index access: arr[i] or map[k]
            Expr::Index(base, idx) => self.index(*base, *idx, span),

            // JSON access: data.field, data..field, data->"key", data->>"key"
            Expr::JsonAccess(base, kind, key) => {
                self.json_access(*base, kind, key, span)
            }

            // JSON literals
            Expr::Json(_) => Ty::Json,

            // Closures: (x, y) => body or [T](x: T) -> T => body
            Expr::Closure {
                type_params,
                params,
                ret,
                body,
            } => self.closure(type_params, params, ret.as_ref(), *body, span),

            // Function calls: f(args...)
            Expr::Call(callee, args) => self.call(*callee, args, span),

            // Control flow: IF
            Expr::If(cond, then_br, else_br) => {
                self.r#if(*cond, *then_br, else_br.as_ref().copied(), span)
            }

            // Control flow: blocks
            Expr::Block(stmts, tail) => {
                self.block(stmts, tail.as_ref().copied(), span)
            }

            // Control flow: MATCH
            Expr::Match(scrutinee, arms) => {
                self.r#match(*scrutinee, arms, span)
            }

            // Variant constructors
            Expr::Variant(ty_name, var_name, args) => {
                self.variant(ty_name, var_name, args, span)
            }

            // Unwrap: postfix `!`
            Expr::Unwrap(inner) => self.unwrap(*inner, span),

            // Type check: `expr IS Pattern`
            Expr::Is(scrutinee, pattern) => {
                self.is_check(*scrutinee, pattern, span)
            }

            // Type cast: `expr AS Type`
            Expr::As(inner, ty_id) => self.as_cast(*inner, *ty_id, span),

            // Fallible conversion: `expr READ Type`
            Expr::Read(inner, ty_id) => self.read_conv(*inner, *ty_id, span),

            // Database read: `GET local(...)` or `GET ^global(...)`
            Expr::Get(ref dbref, _) => self.get(id, dbref, span),

            // Type annotation: `(expr) : Type`
            Expr::Annotate(inner, ty_id) => self.annotate(*inner, *ty_id, span),

            // Module path: `Module.function` or `Module.constant`
            Expr::Path(segments) => {
                // Look up the type from the runtime environment;
                // first check constants, then functions
                let path: SmallVec<[&str; 4]> =
                    segments.iter().map(String::as_str).collect();

                // Check builtin module constants first (no constraints)
                if let Some(ty) = self.runtime_env.get_module_const_type(&path)
                {
                    ty.clone()
                } else if let Some(scheme) =
                    self.runtime_env.get_module_fn_type(&path)
                {
                    // Builtin module functions (no user constraints)
                    let (ty, constraints) =
                        scheme.instantiate(&mut self.next_var);
                    self.emit_user_constraints(constraints, span);
                    ty
                } else if let Some(scheme) =
                    self.env.lookup_user_module_member(&path)
                {
                    // User-defined module members may have constraints
                    let (ty, constraints) =
                        scheme.instantiate(&mut self.next_var);
                    self.emit_user_constraints(constraints, span);
                    ty
                } else {
                    Ty::Unknown
                }
            }

            // Regex literal: `/pattern/`
            Expr::Regex(pattern, _) => {
                // Compile and cache the regex pattern; invalid patterns
                // produce a type error during compile_regex
                self.compile_regex(pattern, span)
                    .map(|idx| self.regex_indices.insert(id, idx));
                Ty::Regex
            }

            // Regex match: `expr MATCHES regex`
            Expr::Matches(lhs, rhs) => {
                let lhs_ty = self.expr(*lhs);
                let rhs_ty = self.expr(*rhs);

                // LHS must be Stringable
                self.constrain(Constraint::Stringable(lhs_ty, span));

                // RHS must be Regex
                self.unify(rhs_ty, Ty::Regex, span);

                Ty::Bool
            }

            // Catch expression: `expr CATCH handler`
            Expr::Catch(expr_id, handler_id) => {
                let expr_ty = self.expr(*expr_id);
                let handler_ty = self.expr(*handler_id);

                // Handler must be `(Error) -> T` where `T` matches expr type
                let expected =
                    Ty::Fn(vec![Ty::RuntimeError], Box::new(expr_ty.clone()));
                self.unify(handler_ty, expected, span);

                expr_ty
            }

            // Data query: `DATA local(...)` or `DATA ^global(...)`
            Expr::Data(ref dbref, _) => self.data(id, dbref, span),

            // Order query: `ORDER local(...)` or `ORDER ^global(...)`
            Expr::Order(ref dbref, _) => self.order(id, dbref, span),

            // Query: `@QUERY local(...)` or `@QUERY ^global(...)`
            Expr::Query(ref dbref, _) => self.query(id, dbref, span),

            // Output expression: `@OUTPUT expr [JSON] [TO target]`
            // Same typing as statement version, but returns `Unit`
            Expr::Output(output) => {
                self.output(output, span);
                Ty::Unit
            }

            // Set expression: `@SET target = value`
            // Returns `Result[Unit, String]`
            Expr::Set(ref dbref, value, _) => {
                self.set_expr(id, dbref, *value, span);
                Ty::Result(Box::new(Ty::Unit), Box::new(Ty::String))
            }

            // Kill expression: `@KILL target`
            // Returns `Result[Unit, String]`
            Expr::Kill(ref dbref, _) => {
                self.kill_expr(id, dbref, span);
                Ty::Result(Box::new(Ty::Unit), Box::new(Ty::String))
            }

            // Raise expression: `@RAISE expr`
            // Never returns; can unify with any expected type.
            Expr::Raise(inner) => {
                let ty = self.expr(*inner);
                self.constrain(Constraint::Stringable(ty, span));
                self.fresh()
            }

            // Forever loop: `FOREVER seed (state, cont) => body`
            Expr::Forever {
                seed,
                state_param,
                cont_param,
                body,
            } => self.forever(*seed, state_param, cont_param, *body, span),

            // Transaction block: `TRANSACTION { ... }`
            Expr::Transaction(ref txn) => self.transaction(id, txn, span),
        }
    }

    /// Infer type of a literal.
    pub(super) fn literal(&self, lit: &Literal) -> Ty {
        match lit {
            Literal::Bool(_) => Ty::Bool,
            Literal::Int(_) => Ty::Int,
            Literal::Float(_) => Ty::Float,
            Literal::Char(_) => Ty::Char,
            Literal::String(_) => Ty::String,
            Literal::Null => Ty::Json,
            Literal::Unit => Ty::Unit,
        }
    }

    /// Infer type of a variable reference.
    ///
    /// Looks up the variable in the type environment and instantiates its
    /// scheme with fresh type variables. If undefined, records an error
    /// and returns `Ty::Error`.
    fn var(&mut self, name: &str, span: Span) -> Ty {
        match self.env.lookup(name) {
            Some(scheme) => {
                let (ty, constraints) = scheme.instantiate(&mut self.next_var);
                self.emit_user_constraints(constraints, span);
                ty
            }
            None => {
                self.error(TypeError::UndefinedVar(name.to_string(), span));
                Ty::Error
            }
        }
    }

    /// Infer type of a binary operation.
    ///
    /// Generates appropriate constraints based on the operator and returns
    /// the result type.
    fn binary(
        &mut self,
        lhs_id: ExprId,
        op: BinOp,
        rhs_id: ExprId,
        span: Span,
    ) -> Ty {
        let lhs_ty = self.expr(lhs_id);
        let rhs_ty = self.expr(rhs_id);

        match op {
            // Arithmetic: both numeric, result depends on operand types
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Mod | BinOp::Pow => {
                // If either is concrete Int/Float/Word, determine result type
                // Word op Word => Word; mixed Word/Int => Int; any Float => Float
                match (&lhs_ty, &rhs_ty) {
                    (Ty::Float, _) | (_, Ty::Float) => {
                        self.constrain(Constraint::Numeric(lhs_ty, span));
                        self.constrain(Constraint::Numeric(rhs_ty, span));
                        Ty::Float
                    }
                    (Ty::Word, Ty::Word) => Ty::Word,
                    (Ty::Int, _) | (Ty::Word, _) => {
                        self.unify(rhs_ty, Ty::Int, span);
                        Ty::Int
                    }
                    (_, Ty::Int) | (_, Ty::Word) => {
                        self.unify(lhs_ty, Ty::Int, span);
                        Ty::Int
                    }
                    _ => {
                        // Both are type vars or unknown; add Numeric constraints
                        self.constrain(Constraint::Numeric(lhs_ty, span));
                        self.constrain(Constraint::Numeric(rhs_ty, span));
                        let result = self.fresh();
                        self.constrain(Constraint::Numeric(
                            result.clone(),
                            span,
                        ));
                        result
                    }
                }
            }

            // Division always returns Float
            BinOp::Div => {
                self.constrain(Constraint::Numeric(lhs_ty, span));
                self.constrain(Constraint::Numeric(rhs_ty, span));
                Ty::Float
            }

            // Floor division: Word // Word => Word, otherwise Int
            BinOp::FloorDiv => {
                self.constrain(Constraint::Numeric(lhs_ty.clone(), span));
                self.constrain(Constraint::Numeric(rhs_ty.clone(), span));
                if matches!((&lhs_ty, &rhs_ty), (Ty::Word, Ty::Word)) {
                    Ty::Word
                } else {
                    Ty::Int
                }
            }

            // Comparison: operands must unify, result is Bool
            BinOp::Eq
            | BinOp::Ne
            | BinOp::Lt
            | BinOp::Gt
            | BinOp::Le
            | BinOp::Ge => {
                self.unify(lhs_ty, rhs_ty, span);
                Ty::Bool
            }

            // Logical: both must be Bool, result is Bool
            BinOp::And | BinOp::Or => {
                self.unify(lhs_ty, Ty::Bool, span);
                self.unify(rhs_ty, Ty::Bool, span);
                Ty::Bool
            }

            // Monoid concatenation: both operands are Monoid, same type
            BinOp::Concat => {
                self.constrain(Constraint::Monoid(lhs_ty.clone(), span));
                self.constrain(Constraint::Monoid(rhs_ty.clone(), span));
                self.unify(lhs_ty.clone(), rhs_ty, span);
                lhs_ty
            }

            // Coalesce: lhs is Option[T] or Result[T, E], rhs unifies with T
            BinOp::Coalesce => {
                let inner = self.fresh();
                self.constrain(Constraint::Unwrappable {
                    ty: lhs_ty,
                    inner: inner.clone(),
                    span,
                });
                self.unify(rhs_ty, inner.clone(), span);
                inner
            }

            // Pipe: rhs is callable with lhs as argument
            BinOp::Pipe => {
                let result = self.fresh();
                self.constrain(Constraint::Callable {
                    callee: rhs_ty,
                    args: smallvec::smallvec![lhs_ty],
                    ret: result.clone(),
                    span,
                });
                result
            }
        }
    }

    /// Infer type of a unary operation.
    ///
    /// Generates appropriate constraints based on the operator and returns
    /// the result type.
    fn unary(&mut self, op: UnOp, operand_id: ExprId, span: Span) -> Ty {
        let operand_ty = self.expr(operand_id);

        match op {
            // Negation: operand must be numeric, result is same type
            UnOp::Neg => {
                self.constrain(Constraint::Numeric(operand_ty.clone(), span));
                operand_ty
            }

            // Logical not: operand must be Bool, result is Bool
            UnOp::Not => {
                self.unify(operand_ty, Ty::Bool, span);
                Ty::Bool
            }

            // Wrap: `?e` where `e : T` produces `Option[T]`
            UnOp::Wrap => Ty::Option(Box::new(operand_ty)),
        }
    }

    /// Infer type of an array literal with potential spread elements.
    ///
    /// Empty arrays get a fresh element type. Homogeneous arrays get
    /// `Array[T]`. Heterogeneous arrays (mixed types) become `Json`.
    /// Spreads contribute their element type to the overall array type.
    fn array(&mut self, elems: &[ArrayElem], span: Span) -> Ty {
        // Collect element types (for regular elements) and array element types (for spreads)
        let elem_tys: Vec<Ty> = elems
            .iter()
            .map(|elem| match elem {
                ArrayElem::Elem(id) => self.expr(*id),
                ArrayElem::Spread(id) => {
                    let spread_ty = self.expr(*id);
                    match spread_ty {
                        Ty::Array(inner) => *inner,
                        Ty::Var(_) => {
                            // Create constraint: spread must be an array
                            let elem_ty = self.fresh();
                            self.unify(
                                spread_ty,
                                Ty::Array(Box::new(elem_ty.clone())),
                                span,
                            );
                            elem_ty
                        }
                        Ty::Error => Ty::Error,
                        _ => {
                            self.error(TypeError::NotAnArray(spread_ty, span));
                            Ty::Error
                        }
                    }
                }
            })
            .collect();

        if let Some((first_ty, rest_tys)) = elem_tys.split_first() {
            // Check for errors
            if first_ty == &Ty::Error {
                Ty::Error
            } else {
                // Check if all elements can unify with the first
                let heterogeneous = rest_tys.iter().any(|ty| {
                    ty != &Ty::Error && !self.types_compatible(first_ty, ty)
                });

                if heterogeneous {
                    Ty::Json
                } else {
                    // Homogeneous: unify all elements
                    rest_tys.iter().filter(|ty| **ty != Ty::Error).for_each(
                        |ty| {
                            self.unify(first_ty.clone(), ty.clone(), span);
                        },
                    );
                    Ty::Array(Box::new(first_ty.clone()))
                }
            }
        } else {
            Ty::Array(Box::new(self.fresh()))
        }
    }

    /// Infer type of a tuple literal.
    ///
    /// Infers each element independently; the tuple type contains all element
    /// types in order. Empty tuples are handled as `Unit` in `expr_inner`.
    fn tuple(&mut self, elems: &SmallVec<[ExprId; 4]>) -> Ty {
        Ty::Tuple(elems.iter().map(|id| self.expr(*id)).collect())
    }

    /// Infer type of an object literal with potential spread entries.
    ///
    /// Spreads merge fields from the spread object; later fields override earlier.
    /// For struct preservation (Option 2): if we spread a struct and the result
    /// still has all required fields, we preserve the struct type.
    fn object(&mut self, entries: &[ObjectEntry], span: Span) -> Ty {
        // Track accumulated fields; later entries override earlier
        let mut acc: IndexMap<StringId, Ty> = IndexMap::new();
        // Track if we're spreading exactly one struct (for potential preservation)
        let mut spread_struct: Option<TypeId> = None;
        let mut has_error = false;

        entries.iter().for_each(|entry| match entry {
            ObjectEntry::Field(name, expr_id) => {
                let field_ty = self.expr(*expr_id);
                let field_id = self.env.intern(name);
                // Override or add field
                acc.insert(field_id, field_ty);
            }
            ObjectEntry::Spread(expr_id) => {
                let spread_ty = self.expr(*expr_id);
                match &spread_ty {
                    Ty::Object(fields) => {
                        // Merge fields from spread object
                        fields.iter().for_each(|(k, t)| {
                            acc.insert(*k, t.clone());
                        });
                    }
                    Ty::Named(ty_id, _args) => {
                        // Spreading an alias to object: get its fields
                        let spread_ok =
                            self.registry.get_def(*ty_id).and_then(|def| {
                                match def {
                                    TypeDef::Alias { target, .. } => self
                                        .ast
                                        .get_type_expr(*target)
                                        .and_then(|te| match te {
                                            AstTypeExpr::Object(fields) => {
                                                Some(fields.clone())
                                            }
                                            _ => None,
                                        }),
                                    _ => None,
                                }
                            });
                        if let Some(fields) = spread_ok {
                            // Remember we spread this (for potential preservation)
                            if spread_struct.is_none() && acc.is_empty() {
                                spread_struct = Some(*ty_id);
                            } else {
                                // Multiple spreads or fields before; no preservation
                                spread_struct = None;
                            }
                            // Merge fields (convert AstTypeExprId -> Ty)
                            let empty_subst = HashMap::new();
                            fields.iter().for_each(|(name, ast_ty_id)| {
                                let k = self.env.intern(name);
                                let field_ty = self
                                    .ast_type_to_ty(*ast_ty_id, &empty_subst);
                                acc.insert(k, field_ty);
                            });
                        } else {
                            self.error(TypeError::NotAnObjectSpread(
                                spread_ty.clone(),
                                span,
                            ));
                            has_error = true;
                        }
                    }
                    Ty::Var(_) => {
                        // Create constraint: spread must be an object
                        let fresh_obj = Ty::Object(IndexMap::new());
                        self.unify(spread_ty, fresh_obj, span);
                        // Can't know fields statically; no struct preservation
                        spread_struct = None;
                    }
                    Ty::Error => has_error = true,
                    _ => {
                        self.error(TypeError::NotAnObjectSpread(
                            spread_ty, span,
                        ));
                        has_error = true;
                    }
                }
            }
        });

        if has_error {
            Ty::Error
        } else if let Some(struct_id) = spread_struct {
            // Check if we can preserve the struct type (all required fields present)
            // Since spreading can only add fields, never remove them, the struct is valid
            // Extensible record semantics: struct + extra fields is still that struct
            Ty::Named(struct_id, vec![])
        } else {
            Ty::Object(acc)
        }
    }

    /// Infer type of a map literal.
    ///
    /// Keys are unified to a common type; values are unified to a common type.
    /// Empty maps get fresh type variables for both.
    fn map_lit(
        &mut self,
        entries: &SmallVec<[(ExprId, ExprId); 8]>,
        span: Span,
    ) -> Ty {
        if let Some(((first_k, first_v), rest)) = entries.split_first() {
            let k_ty = self.expr(*first_k);
            let v_ty = self.expr(*first_v);
            rest.iter().for_each(|(k, v)| {
                let k = self.expr(*k);
                let v = self.expr(*v);
                self.unify(k_ty.clone(), k, span);
                self.unify(v_ty.clone(), v, span);
            });
            Ty::Map(Box::new(k_ty), Box::new(v_ty))
        } else {
            Ty::Map(Box::new(self.fresh()), Box::new(self.fresh()))
        }
    }

    /// Infer type of field access: `base.field`.
    ///
    /// Works for structural objects (`Ty::Object`) and named struct types
    /// (`Ty::Named` with `TypeDef::Struct`). For type variables, we cannot
    /// yet infer the field type without row polymorphism, so we create a
    /// structural object constraint.
    fn field(&mut self, base_id: ExprId, field: &str, span: Span) -> Ty {
        let base_ty = self.expr(base_id);
        self.field_type(&base_ty, field, span)
    }

    /// Infer type of optional field access: `base?.field`.
    ///
    /// Works on any type that has the field; always returns `Option[FieldType]`.
    /// - If base is `Option[T]`, unwraps and accesses field on `T`
    /// - If base is an object/struct with the field, accesses it directly
    /// - Either way, result is wrapped in `Option`
    fn optional_field(
        &mut self,
        base_id: ExprId,
        field: &str,
        span: Span,
    ) -> Ty {
        let base_ty = self.expr(base_id);

        match &base_ty {
            // Option[T]: unwrap, access field on T, rewrap
            Ty::Option(inner) => {
                let field_ty = self.optional_field_type(inner, field, span);
                Ty::Option(Box::new(field_ty))
            }

            // Object: field may or may not exist; missing → Unknown (no error)
            Ty::Object(fields) => {
                let field_id = self.env.intern(field);
                let field_ty =
                    fields.get(&field_id).cloned().unwrap_or(Ty::Unknown);
                Ty::Option(Box::new(field_ty))
            }

            // Named struct: use strict field lookup (structs have defined schema)
            Ty::Named(_, _) => {
                let field_ty = self.field_type(&base_ty, field, span);
                Ty::Option(Box::new(field_ty))
            }

            // Type variable: create object constraint, wrap result in Option
            Ty::Var(_) => {
                let field_ty = self.field_type(&base_ty, field, span);
                Ty::Option(Box::new(field_ty))
            }

            Ty::Error => Ty::Error,

            _ => {
                self.error(TypeError::NotAnObject(base_ty, span));
                Ty::Error
            }
        }
    }

    /// Infer type of tuple index: `tuple.0`, `tuple.1`, etc.
    fn tuple_index(&mut self, base_id: ExprId, idx: u32, span: Span) -> Ty {
        let base_ty = self.expr(base_id);

        match &base_ty {
            Ty::Tuple(elems) => {
                elems.get(idx as usize).cloned().unwrap_or_else(|| {
                    self.error(TypeError::TupleIndexOutOfBounds {
                        idx,
                        len: elems.len(),
                        span,
                    });
                    Ty::Error
                })
            }

            Ty::Var(_) => {
                // Cannot infer tuple structure from index access alone;
                // the constraint solver would need tuple row polymorphism.
                // For now, return fresh var and hope it unifies later.
                self.fresh()
            }

            Ty::Error => Ty::Error,

            _ => {
                self.error(TypeError::NotATuple(base_ty, span));
                Ty::Error
            }
        }
    }

    /// Infer type of index access: `base[idx]`.
    ///
    /// Works for `Array[T]` (index must be `Int`, returns `T`) and
    /// `Map[K, V]` (index unifies with `K`, returns `V`).
    fn index(&mut self, base_id: ExprId, idx_id: ExprId, span: Span) -> Ty {
        let base_ty = self.expr(base_id);
        let idx_ty = self.expr(idx_id);

        match &base_ty {
            Ty::Array(elem) => {
                self.unify(idx_ty, Ty::Int, span);
                elem.as_ref().clone()
            }

            Ty::Map(key, val) => {
                self.unify(idx_ty, key.as_ref().clone(), span);
                Ty::Option(val.clone())
            }

            Ty::Var(_) => {
                // Base is type variable; could be Array or Map.
                // We can't know which, so return fresh and let unification handle it.
                self.fresh()
            }

            Ty::Error => Ty::Error,

            Ty::String => {
                // String indexing returns Char
                self.unify(idx_ty, Ty::Int, span);
                Ty::Char
            }

            _ => {
                self.error(TypeError::NotIndexable(base_ty, span));
                Ty::Error
            }
        }
    }

    /// Infer type of JSON access operators.
    ///
    /// | Operator | Returns                           |
    /// |----------|-----------------------------------|
    /// | `.`      | `Json`                            |
    /// | `..`     | `Option[Scalar]` (union)          |
    /// | `->`     | `Json`                            |
    /// | `->>`    | `Option[Scalar]` (union)          |
    fn json_access(
        &mut self,
        base_id: ExprId,
        kind: &JsonAccessKind,
        key: &JsonAccessKey,
        span: Span,
    ) -> Ty {
        let base_ty = self.expr(base_id);

        // Infer the key expression type if dynamic
        if let JsonAccessKey::Expr(key_id) = key {
            let key_ty = self.expr(*key_id);
            // Dynamic key must be String
            self.unify(key_ty, Ty::String, span);
        }

        // Base must be Json
        match &base_ty {
            Ty::Json => match kind {
                JsonAccessKind::Json => Ty::Json,
                JsonAccessKind::Scalar => {
                    Ty::Option(Box::new(Ty::Named(TypeId::SCALAR, vec![])))
                }
            },

            Ty::Var(_) => {
                // Constrain base to be Json
                self.unify(base_ty, Ty::Json, span);
                match kind {
                    JsonAccessKind::Json => Ty::Json,
                    JsonAccessKind::Scalar => {
                        Ty::Option(Box::new(Ty::Named(TypeId::SCALAR, vec![])))
                    }
                }
            }

            Ty::Error => Ty::Error,

            _ => {
                self.error(TypeError::NotJson(base_ty, span));
                Ty::Error
            }
        }
    }

    /// Infer types for function/closure parameters.
    ///
    /// For each parameter: uses annotation if present, otherwise fresh type var.
    pub(super) fn param_tys(
        &mut self,
        params: &SmallVec<[(String, Option<AstTypeExprId>); 4]>,
    ) -> Vec<Ty> {
        self.param_tys_with_subst(params, &HashMap::new())
    }

    /// Infer types for function/closure parameters with type param substitution.
    ///
    /// For generic functions, the `subst` map provides fresh type variables for
    /// explicit type parameters (e.g., `T` in `fn foo[T](x: T)`).
    pub(super) fn param_tys_with_subst(
        &mut self,
        params: &SmallVec<[(String, Option<AstTypeExprId>); 4]>,
        subst: &HashMap<StringId, Ty>,
    ) -> Vec<Ty> {
        params
            .iter()
            .map(|(_, ann)| match ann {
                Some(id) => self.ast_type_to_ty(*id, subst),
                None => self.fresh(),
            })
            .collect()
    }

    /// Bind parameters in the current scope with their inferred types.
    pub(super) fn bind_params(
        &mut self,
        params: &SmallVec<[(String, Option<AstTypeExprId>); 4]>,
        tys: &[Ty],
    ) {
        params.iter().zip(tys.iter()).for_each(|((name, _), ty)| {
            self.env.bind(name, Scheme::mono(ty.clone()));
        });
    }

    /// Infer type of a closure expression.
    ///
    /// For each parameter: uses annotation if present, otherwise fresh type var.
    /// Binds parameters in a new scope, infers body, then pops scope.
    /// If return annotation present, unifies body type with it.
    ///
    /// For generic closures (`[T](x: T) -> T => x`), type parameters are bound
    /// as fresh type variables before inferring parameter/return types.
    fn closure(
        &mut self,
        type_params: &SmallVec<[TypeParam; 2]>,
        params: &SmallVec<[(String, Option<AstTypeExprId>); 4]>,
        ret: Option<&AstTypeExprId>,
        body: ExprId,
        span: Span,
    ) -> Ty {
        // Two-pass approach: first create all type variables, then emit
        // constraints (needed for Iterable[T] where T references another param)
        let type_param_subst: HashMap<_, _> = type_params
            .iter()
            .map(|tp| {
                let id = self.env.intern(&tp.name);
                let tv = self.fresh();
                (id, tv)
            })
            .collect();

        // Build name -> Ty map for element type lookup
        let name_to_ty: HashMap<&str, Ty> = type_params
            .iter()
            .map(|tp| {
                let id = self.env.intern(&tp.name);
                let ty =
                    type_param_subst.get(&id).cloned().unwrap_or_else(|| {
                        self.error(TypeError::Custom {
                            msg: format!(
                                "internal: type param `{}` not in subst",
                                tp.name
                            ),
                            span,
                        });
                        self.fresh()
                    });
                (tp.name.as_str(), ty)
            })
            .collect();

        // Emit constraints for each user-specified bound
        type_params.iter().for_each(|tp| {
            let id = self.env.intern(&tp.name);
            if let Some(tv) = type_param_subst.get(&id) {
                tp.constraints.iter().for_each(|c| {
                    let constraint = match c {
                        UserConstraint::Numeric => {
                            Constraint::Numeric(tv.clone(), span)
                        }
                        UserConstraint::Stringable => {
                            Constraint::Stringable(tv.clone(), span)
                        }
                        UserConstraint::Jsonable => {
                            Constraint::Jsonable(tv.clone(), span)
                        }
                        UserConstraint::Subscriptable => {
                            Constraint::Subscriptable(tv.clone(), span)
                        }
                        UserConstraint::Storable => {
                            Constraint::Storable(tv.clone(), span)
                        }
                        UserConstraint::Iterable(elem_name) => {
                            let elem = elem_name
                                .as_ref()
                                .map(|el| {
                                    name_to_ty.get(el.as_str()).cloned().unwrap_or_else(
                                        || {
                                            self.error(TypeError::Custom {
                                                msg: format!(
                                                    "unknown type parameter `{el}` in \
                                                     constraint `Iterable[{el}]`"
                                                ),
                                                span,
                                            });
                                            self.fresh()
                                        },
                                    )
                                })
                                .unwrap_or_else(|| self.fresh());
                            Constraint::Iterable {
                                coll: tv.clone(),
                                elem,
                                span,
                            }
                        }
                        UserConstraint::Monoid => {
                            Constraint::Monoid(tv.clone(), span)
                        }
                    };
                    self.constrain(constraint);
                });
            } else {
                self.error(TypeError::Custom {
                    msg: format!("internal: type param `{}` not in subst", tp.name),
                    span,
                });
            }
        });

        let param_tys = self.param_tys_with_subst(params, &type_param_subst);

        self.env.push_scope();
        self.bind_params(params, &param_tys);

        let body_ty = self.expr(body);
        self.env.pop_scope();

        // If return annotation present, unify body with it
        let ret_ty = match ret {
            Some(ret_id) => {
                let expected = self.ast_type_to_ty(*ret_id, &type_param_subst);
                self.unify(body_ty.clone(), expected.clone(), span);
                expected
            }
            None => body_ty,
        };

        Ty::Fn(param_tys, Box::new(ret_ty))
    }

    /// Infer type of a function call expression.
    ///
    /// Infers callee and argument types, then adds a `Callable` constraint.
    /// Returns a fresh type variable that will be unified with the return type.
    fn call(
        &mut self,
        callee_id: ExprId,
        args: &SmallVec<[ExprId; 4]>,
        span: Span,
    ) -> Ty {
        let callee_ty = self.expr(callee_id);
        let arg_tys: SmallVec<[Ty; 4]> =
            args.iter().map(|id| self.expr(*id)).collect();

        let ret = self.fresh();
        self.constrain(Constraint::Callable {
            callee: callee_ty,
            args: arg_tys,
            ret: ret.clone(),
            span,
        });
        ret
    }

    /// Infer type of an IF expression.
    ///
    /// # Type Checking Rules
    ///
    /// - Condition must be `Bool`
    /// - IF/ELSE: both branches must have the same type
    /// - Single-arm IF (no ELSE): body must be `Unit`, whole expression is `Unit`
    ///
    /// # IS with Bindings
    ///
    /// If the condition is `expr IS Pattern(bindings)`, the bindings are only
    /// visible in the then branch, not the else branch. The type checker
    /// extracts these bindings and adds them to the then-branch scope.
    fn r#if(
        &mut self,
        cond_id: ExprId,
        then_id: ExprId,
        else_id: Option<ExprId>,
        span: Span,
    ) -> Ty {
        // Check if condition is an IS expression with variant bindings
        let cond_expr = self.ast.get_expr(cond_id).cloned();

        let then_ty = match cond_expr {
            Some(Expr::Is(
                scrutinee_id,
                TypePattern::VariantBind(ty_name, var_name, names),
            )) => {
                // IS with variant bindings: bindings only visible in then branch
                let scrutinee_ty = self.expr(scrutinee_id);
                let payload_tys = self.variant_payload_types(
                    &ty_name,
                    &var_name,
                    &scrutinee_ty,
                    span,
                );

                if payload_tys.len() != names.len() {
                    self.error(TypeError::ArityMismatch {
                        expected: payload_tys.len(),
                        got: names.len(),
                        span,
                    });
                }

                self.env.push_scope();
                names.iter().zip(payload_tys.iter()).for_each(|(name, ty)| {
                    self.env.bind(name, Scheme::mono(ty.clone()));
                });
                let ty = self.expr(then_id);
                self.env.pop_scope();
                ty
            }
            _ => {
                // Regular condition: infer and unify with Bool
                let cond_ty = self.expr(cond_id);
                self.unify(cond_ty, Ty::Bool, span);
                self.expr(then_id)
            }
        };

        // Unify branches (or find common union type)
        if let Some(else_id) = else_id {
            let else_ty = self.expr(else_id);
            self.join_types(&[then_ty, else_ty], span)
        } else {
            self.unify(then_ty, Ty::Unit, span);
            Ty::Unit
        }
    }

    /// Infer type of a block expression.
    ///
    /// Executes statements for side effects, then evaluates to the trailing
    /// expression. Returns `Unit` if no trailing expression.
    fn block(
        &mut self,
        stmts: &[StmtId],
        tail: Option<ExprId>,
        _span: Span,
    ) -> Ty {
        self.env.push_scope();
        stmts.iter().for_each(|id| self.stmt(*id));
        let result_ty = tail.map_or(Ty::Unit, |id| self.expr(id));
        self.env.pop_scope();
        result_ty
    }

    /// Infer type of a MATCH expression.
    ///
    /// Evaluates the scrutinee once, then checks each arm. All arm bodies must
    /// have the same type (or be members of a common union). Also performs
    /// exhaustiveness checking.
    fn r#match(
        &mut self,
        scrutinee_id: ExprId,
        arms: &[MatchArm],
        span: Span,
    ) -> Ty {
        let scrutinee_ty = self.expr(scrutinee_id);

        if arms.is_empty() {
            self.error(TypeError::NonExhaustiveMatch(span));
            Ty::Error
        } else {
            // Infer all arm body types
            let arm_tys: Vec<Ty> = arms
                .iter()
                .map(|arm| self.match_arm(arm, &scrutinee_ty, span))
                .collect();

            // Try to find a common type for all arms
            let result_ty = self.join_types(&arm_tys, span);

            // Exhaustiveness check
            self.check_exhaustiveness(arms, &scrutinee_ty, span);

            result_ty
        }
    }

    /// Find a common type for a list of types.
    ///
    /// If all types are the same, returns that type. If they differ and contain
    /// type variables, unifies them (standard HM behavior). If all are primitive
    /// storable types, creates an anonymous union. Otherwise, unifies normally.
    fn join_types(&mut self, tys: &[Ty], span: Span) -> Ty {
        let first = tys.first().cloned().unwrap_or(Ty::Error);
        let all_same = tys.iter().skip(1).all(|t| *t == first);

        // Only create anonymous unions for primitive storable types
        // (Bool, Int, Float, Char, String, Json). This supports common
        // patterns like `IF cond { 42 } ELSE { "string" }` -> Int | String.
        // For other types (Option, Result, user structs), unify normally.
        let all_storable = || {
            tys.iter().all(|t| {
                matches!(
                    t,
                    Ty::Bool
                        | Ty::Int
                        | Ty::Float
                        | Ty::Char
                        | Ty::String
                        | Ty::Json
                )
            })
        };

        if all_same {
            first
        } else if all_storable() {
            // Deduplicate members (O(n²) but n is small for match/if arms)
            let members = tys.iter().fold(Vec::new(), |mut acc, t| {
                if !acc.contains(t) {
                    acc.push(t.clone());
                }
                acc
            });
            Ty::Union(members)
        } else {
            // Unify normally; mismatches will error
            tys.iter().skip(1).for_each(|ty| {
                self.unify(first.clone(), ty.clone(), span);
            });
            first
        }
    }

    /// Infer type of a single match arm.
    ///
    /// Checks the pattern, binds variables, evaluates guard (if any),
    /// and infers the body type.
    fn match_arm(
        &mut self,
        arm: &MatchArm,
        scrutinee_ty: &Ty,
        span: Span,
    ) -> Ty {
        self.env.push_scope();

        // Check pattern and collect bindings
        self.pattern_bindings(arm.pattern, scrutinee_ty, span);

        // Check guard if present
        if let Some(guard_id) = arm.guard {
            let guard_ty = self.expr(guard_id);
            self.unify(guard_ty, Ty::Bool, span);
        }

        // Infer body
        let body_ty = self.expr(arm.body);
        self.env.pop_scope();
        body_ty
    }

    /// Infer type of a variant constructor: `Type.Variant(args)`.
    fn variant(
        &mut self,
        ty_name: &str,
        var_name: &str,
        args: &SmallVec<[ExprId; 4]>,
        span: Span,
    ) -> Ty {
        let arg_tys: Vec<Ty> = args.iter().map(|id| self.expr(*id)).collect();

        // Look up type and variant
        let var_name_id = self.env.intern(var_name);
        let lookup = self
            .env
            .lookup_str(ty_name)
            .and_then(|id| self.registry.lookup(id))
            .and_then(|type_id| {
                self.registry
                    .lookup_variant(type_id, var_name_id)
                    .map(|var_def| (type_id, var_def))
            });

        match lookup {
            None => {
                self.error(TypeError::UnknownType(
                    format!("{ty_name}.{var_name}"),
                    span,
                ));
                Ty::Error
            }
            Some((type_id, var_def)) => {
                if var_def.arity as usize != arg_tys.len() {
                    self.error(TypeError::ArityMismatch {
                        expected: var_def.arity as usize,
                        got: arg_tys.len(),
                        span,
                    });
                }

                if type_id == TypeId::OPTION {
                    let inner = arg_tys
                        .first()
                        .cloned()
                        .unwrap_or_else(|| self.fresh());
                    Ty::Option(Box::new(inner))
                } else if type_id == TypeId::RESULT {
                    match var_def.idx {
                        0 => {
                            let ok = arg_tys
                                .first()
                                .cloned()
                                .unwrap_or_else(|| self.fresh());
                            Ty::Result(Box::new(ok), Box::new(self.fresh()))
                        }
                        1 => {
                            let err = arg_tys
                                .first()
                                .cloned()
                                .unwrap_or_else(|| self.fresh());
                            Ty::Result(Box::new(self.fresh()), Box::new(err))
                        }
                        _ => Ty::Error,
                    }
                } else if type_id == TypeId::ORDERING {
                    // Ordering has no type parameters; all variants are nullary
                    Ty::Ordering
                } else {
                    match self.registry.get_def(type_id) {
                        Some(TypeDef::Sum { type_params, .. }) => {
                            let type_args: Vec<Ty> = type_params
                                .iter()
                                .map(|_| self.fresh())
                                .collect();
                            let subst: HashMap<crate::intern::StringId, Ty> =
                                type_params
                                    .iter()
                                    .zip(type_args.iter())
                                    .map(|(p, a)| (*p, a.clone()))
                                    .collect();

                            var_def
                                .payloads
                                .iter()
                                .zip(arg_tys.iter())
                                .for_each(|(expected_id, got)| {
                                    let expected = self
                                        .ast_type_to_ty(*expected_id, &subst);
                                    self.unify(expected, got.clone(), span);
                                });

                            Ty::Named(type_id, type_args)
                        }
                        _ => {
                            self.error(TypeError::UnknownType(
                                format!("{ty_name}.{var_name}"),
                                span,
                            ));
                            Ty::Error
                        }
                    }
                }
            }
        }
    }

    /// Infer type of postfix unwrap `!`.
    ///
    /// The operand must be `Option[T]` or `Result[T, E]`. Returns `T`.
    /// Adds an `Unwrappable` constraint that the solver will check.
    fn unwrap(&mut self, inner_id: ExprId, span: Span) -> Ty {
        let inner_ty = self.expr(inner_id);
        let result = self.fresh();
        self.constrain(Constraint::Unwrappable {
            ty: inner_ty,
            inner: result.clone(),
            span,
        });
        result
    }

    /// Infer type of `IS` expression.
    ///
    /// Always returns `Bool`. Pattern bindings are extracted by `Expr::If`
    /// and added to the then-branch scope; they are not bound here.
    ///
    /// # Pattern Handling
    ///
    /// - `Type`: runtime type check
    /// - `Variant(ty, var)`: zero-arity variant check
    /// - `VariantWildcard(ty, var)`: variant check ignoring payload
    /// - `VariantBind(ty, var, names)`: variant check with payload bindings
    ///   (bindings handled by enclosing `IF`)
    /// - `Object(fields)`: structural object check
    fn is_check(
        &mut self,
        scrutinee_id: ExprId,
        pattern: &TypePattern,
        span: Span,
    ) -> Ty {
        let scrutinee_ty = self.expr(scrutinee_id);

        match pattern {
            TypePattern::Type(ty_id) => {
                let target_ty = self.ast_type_to_ty(*ty_id, &HashMap::new());
                // If scrutinee is a union, verify target is a member
                // Skip check if target is the union type itself (e.g., `x IS Storable`)
                // or if target is also a union that contains the scrutinee members
                if let Some(members) = self.expand_union_members(&scrutinee_ty)
                {
                    let target_is_same_union = scrutinee_ty == target_ty;
                    let target_is_member = members.contains(&target_ty)
                        || target_ty == Ty::Unknown;
                    if !target_is_same_union && !target_is_member {
                        self.error(TypeError::NotAUnionMember {
                            member: target_ty,
                            union_ty: scrutinee_ty,
                            span,
                        });
                    }
                }
            }
            TypePattern::Variant(ty_name, var_name)
            | TypePattern::VariantWildcard(ty_name, var_name) => {
                // Just validate that the variant exists
                let var_name_id = self.env.intern(var_name);
                let exists = self
                    .env
                    .lookup_str(ty_name)
                    .and_then(|id| self.registry.lookup(id))
                    .and_then(|type_id| {
                        self.registry.lookup_variant(type_id, var_name_id)
                    })
                    .is_some();
                if !exists {
                    self.error(TypeError::UnknownType(
                        format!("{ty_name}.{var_name}"),
                        span,
                    ));
                }
            }
            TypePattern::VariantBind(ty_name, var_name, names) => {
                // Validate variant and arity; bindings are handled by IF
                let var_name_id = self.env.intern(var_name);
                let lookup = self
                    .env
                    .lookup_str(ty_name)
                    .and_then(|id| self.registry.lookup(id))
                    .and_then(|type_id| {
                        self.registry
                            .lookup_variant(type_id, var_name_id)
                            .map(|v| (type_id, v))
                    });
                match lookup {
                    None => {
                        self.error(TypeError::UnknownType(
                            format!("{ty_name}.{var_name}"),
                            span,
                        ));
                    }
                    Some((_, var_def)) => {
                        if var_def.arity as usize != names.len() {
                            self.error(TypeError::ArityMismatch {
                                expected: var_def.arity as usize,
                                got: names.len(),
                                span,
                            });
                        }
                    }
                }
            }
            TypePattern::Object(fields) => {
                // Validate that scrutinee could be an object with these fields
                match &scrutinee_ty {
                    Ty::Object(_) | Ty::Var(_) | Ty::Unknown | Ty::Error => {}
                    Ty::Named(type_id, _) => {
                        // Check it's an alias to object
                        let is_obj_alias = self
                            .registry
                            .get_def(*type_id)
                            .is_some_and(|def| match def {
                                TypeDef::Alias { target, .. } => self
                                    .ast
                                    .get_type_expr(*target)
                                    .is_some_and(|te| {
                                        matches!(te, AstTypeExpr::Object(_))
                                    }),
                                _ => false,
                            });
                        if !is_obj_alias {
                            self.error(TypeError::NotAnObject(
                                scrutinee_ty.clone(),
                                span,
                            ));
                        }
                    }
                    _ => {
                        self.error(TypeError::NotAnObject(scrutinee_ty, span));
                    }
                }
                // Resolve field types (validates type expressions)
                fields.iter().for_each(|(_, ty_id)| {
                    self.ast_type_to_ty(*ty_id, &HashMap::new());
                });
            }
        }

        Ty::Bool
    }

    /// Infer type of `AS` cast expression.
    ///
    /// Handles several cases:
    /// - `T AS Json`: add `Jsonable` constraint, return `Json`
    /// - `Storable AS T` (where `T` is a `Storable` member): return `T` (infallible)
    /// - `T AS String`: all types can stringify, return `String`
    /// - `Int AS Float` / `Float AS Int`: numeric coercion
    /// - Otherwise: emit `InvalidCast` error
    fn as_cast(
        &mut self,
        inner_id: ExprId,
        ty_id: AstTypeExprId,
        span: Span,
    ) -> Ty {
        let inner_ty = self.expr(inner_id);
        let target_ty = self.ast_type_to_ty(ty_id, &HashMap::new());

        // If target is Json, add Jsonable constraint
        if target_ty == Ty::Json {
            self.constrain(Constraint::Jsonable(inner_ty, span));
            Ty::Json
        } else if target_ty == Ty::String {
            // All types can be cast to String
            self.constrain(Constraint::Stringable(inner_ty, span));
            Ty::String
        } else {
            // Check for valid conversions
            match (&inner_ty, &target_ty) {
                // Numeric coercions
                (Ty::Int, Ty::Float) | (Ty::Float, Ty::Int) => target_ty,

                // Word -> Int (always safe)
                (Ty::Word, Ty::Int) => target_ty,

                // Word -> Float (widen)
                (Ty::Word, Ty::Float) => target_ty,

                // Bool <-> Int
                (Ty::Bool, Ty::Int) | (Ty::Int, Ty::Bool) => target_ty,

                // DataStatus -> Int (infallible; variant idx to MUMPS value)
                (Ty::DataStatus, Ty::Int) => Ty::Int,

                // String -> FilePath
                (Ty::String, Ty::FilePath) => Ty::FilePath,

                // Path -> FilePath (extract filepath from File or Dir variant)
                (Ty::Path, Ty::FilePath) => Ty::FilePath,
                (Ty::Named(id, _), Ty::FilePath) if *id == TypeId::PATH => {
                    Ty::FilePath
                }

                // Same type is always valid
                (a, b) if a == b => target_ty,

                // Type variable: defer to unification
                (Ty::Var(_), _) | (_, Ty::Var(_)) => {
                    self.unify(inner_ty.clone(), target_ty.clone(), span);
                    target_ty
                }

                // Error recovery
                (Ty::Error, _) | (_, Ty::Error) => Ty::Error,

                // Unknown can be cast to anything (database reads)
                (Ty::Unknown, _) => target_ty,

                // Storable to member type (special case: infallible at compile
                // time but may fail at runtime with RuntimeType error)
                (Ty::Named(id, _), _) if *id == TypeId::STORABLE => {
                    if Ty::STORABLE_MEMBERS.contains(&target_ty) {
                        target_ty
                    } else {
                        self.error(TypeError::InvalidCast {
                            from: inner_ty,
                            to: target_ty.clone(),
                            span,
                        });
                        target_ty
                    }
                }

                // Member type to union: valid if source is a member
                (_, Ty::Named(id, _)) => {
                    let is_member = self
                        .expand_union_members(&target_ty)
                        .is_some_and(|members| members.contains(&inner_ty));
                    if is_member
                        || *id == TypeId::STORABLE
                        || *id == TypeId::SCALAR
                    {
                        // For Storable/Scalar, always allow casting from members
                        // The runtime will handle the actual type tag
                        target_ty
                    } else {
                        self.error(TypeError::InvalidCast {
                            from: inner_ty,
                            to: target_ty.clone(),
                            span,
                        });
                        target_ty
                    }
                }

                // Invalid cast
                _ => {
                    self.error(TypeError::InvalidCast {
                        from: inner_ty,
                        to: target_ty.clone(),
                        span,
                    });
                    target_ty
                }
            }
        }
    }

    /// Infer type of `READ` conversion expression.
    ///
    /// `expr READ T` returns `Result[T, String]`. The conversion is fallible;
    /// if the value cannot be converted to `T`, an error message is returned.
    ///
    /// For `READ Json`, adds a `Jsonable` constraint on the input type to
    /// catch known-impossible conversions at compile time.
    fn read_conv(
        &mut self,
        inner_id: ExprId,
        ty_id: AstTypeExprId,
        span: Span,
    ) -> Ty {
        let inner_ty = self.expr(inner_id);
        let target_ty = self.ast_type_to_ty(ty_id, &HashMap::new());

        // Validate the target type is usable for READ
        match &target_ty {
            Ty::Bool
            | Ty::Int
            | Ty::Word
            | Ty::Float
            | Ty::Char
            | Ty::String
            | Ty::DataStatus
            | Ty::Array(_)
            | Ty::Option(_)
            | Ty::Object(_)
            | Ty::Named(_, _) => {}
            // READ Json requires the input to be Jsonable
            Ty::Json => {
                self.constrain(Constraint::Jsonable(inner_ty, span));
            }
            Ty::Fn(_, _) => {
                self.error(TypeError::Mismatch {
                    expected: Ty::String, // placeholder
                    got: target_ty.clone(),
                    span,
                });
            }
            Ty::Var(_) | Ty::Unknown | Ty::Error => {}
            _ => {}
        }

        Ty::Result(Box::new(target_ty), Box::new(Ty::String))
    }

    /// Infer type of `GET` expression.
    ///
    /// Database reads return `Option[Storable]`; the value may not exist at the
    /// given path. Usage may narrow via `IS`/`AS` checks or arithmetic operations.
    /// Populates the `TxnId` field in the AST based on current transaction context.
    fn get(&mut self, id: ExprId, dbref: &DbRef, span: Span) -> Ty {
        let subs = match dbref {
            DbRef::Local(_, s) | DbRef::Global(_, s) => s,
        };
        self.check_subscript_elems(subs, span);
        self.ast
            .set_expr(id, Expr::Get(dbref.clone(), self.in_transaction));
        Ty::Option(Box::new(Ty::Named(TypeId::STORABLE, vec![])))
    }

    /// Type-check subscript elements.
    ///
    /// For `Elem`, adds a `Subscriptable` constraint.
    /// For `Spread`, constrains to `Array[Subscript]`.
    pub(super) fn check_subscript_elems(
        &mut self,
        subs: &[SubscriptElem],
        span: Span,
    ) {
        subs.iter().for_each(|elem| match elem {
            SubscriptElem::Elem(id) => {
                let ty = self.expr(*id);
                self.constrain(Constraint::Subscriptable(ty, span));
            }
            SubscriptElem::Spread(id) => {
                let ty = self.expr(*id);
                // Spread must be Array[Subscript]
                let expected =
                    Ty::Array(Box::new(Ty::Named(TypeId::SUBSCRIPT, vec![])));
                self.unify(ty, expected, span);
            }
        });
    }

    /// Infer type of `DATA` expression.
    ///
    /// Queries the existence status of a node. Returns `DataStatus` enum.
    /// Populates the `TxnId` field in the AST based on current transaction context.
    fn data(&mut self, id: ExprId, dbref: &DbRef, span: Span) -> Ty {
        let subs = match dbref {
            DbRef::Local(_, s) | DbRef::Global(_, s) => s,
        };
        self.check_subscript_elems(subs, span);
        self.ast
            .set_expr(id, Expr::Data(dbref.clone(), self.in_transaction));
        Ty::DataStatus
    }

    /// Infer type of `ORDER` expression.
    ///
    /// Returns the next subscript at a given level. Returns `Option[Subscript]`.
    /// Populates the `TxnId` field in the AST based on current transaction context.
    fn order(&mut self, id: ExprId, dbref: &DbRef, span: Span) -> Ty {
        let subs = match dbref {
            DbRef::Local(_, s) | DbRef::Global(_, s) => s,
        };
        self.check_subscript_elems(subs, span);
        self.ast
            .set_expr(id, Expr::Order(dbref.clone(), self.in_transaction));
        Ty::Option(Box::new(Ty::Named(TypeId::SUBSCRIPT, vec![])))
    }

    /// Infer type of `@QUERY` expression.
    ///
    /// Returns the full key path to the next node with a value.
    /// Returns `Option[Array[Subscript]]`.
    /// Populates the `TxnId` field in the AST based on current transaction context.
    fn query(&mut self, id: ExprId, dbref: &DbRef, span: Span) -> Ty {
        let subs = match dbref {
            DbRef::Local(_, s) | DbRef::Global(_, s) => s,
        };
        self.check_subscript_elems(subs, span);
        self.ast
            .set_expr(id, Expr::Query(dbref.clone(), self.in_transaction));
        let subscript = Ty::Named(TypeId::SUBSCRIPT, vec![]);
        Ty::Option(Box::new(Ty::Array(Box::new(subscript))))
    }

    /// Infer type of type annotation expression `(expr) : Type`.
    ///
    /// Infers the inner expression type, parses the annotation, and unifies
    /// them. Returns the annotation type (which is the expected type).
    ///
    /// Special case: when the inner expression is an array literal and the
    /// annotation is `Array[UnionType]`, uses bidirectional typing to check
    /// each element against the union instead of inferring then unifying.
    /// This allows `[1, "a"]: Array[Int | String]` to produce a union-typed
    /// array rather than falling back to `Json`.
    fn annotate(
        &mut self,
        inner_id: ExprId,
        ty_id: AstTypeExprId,
        span: Span,
    ) -> Ty {
        let ann_ty = self.ast_type_to_ty(ty_id, &HashMap::new());

        // Clone inner expression to avoid borrow issues
        let inner_expr = self.ast.get_expr(inner_id).cloned();

        // Reject negative literals for Word type
        if let (Ty::Word, Some(Expr::Unary(UnOp::Neg, _))) =
            (&ann_ty, inner_expr.as_ref())
        {
            self.error(TypeError::NegativeWord(span));
            self.expr(inner_id);
            Ty::Word
        }
        // Try special case: array literal with `Array[UnionType]`
        else if let (Ty::Array(elem_ty), Some(Expr::Array(elems))) =
            (&ann_ty, inner_expr.as_ref())
        {
            if self.expand_union_members(elem_ty).is_some() {
                let result = self.array_with_expected(elems, elem_ty, span);
                self.record_type(inner_id, result.clone());
                result
            } else {
                // Default: infer then unify
                let inner_ty = self.expr(inner_id);
                self.unify(inner_ty, ann_ty.clone(), span);
                ann_ty
            }
        } else {
            // Default: infer then unify
            let inner_ty = self.expr(inner_id);
            self.unify(inner_ty, ann_ty.clone(), span);
            ann_ty
        }
    }

    /// Infer type of array literal with expected union element type.
    ///
    /// When annotating an array with `Array[UnionType]`, each element must be
    /// a member of the union. Returns `Array[expected_elem]` if all elements
    /// match, or `Ty::Error` if any element fails.
    pub(super) fn array_with_expected(
        &mut self,
        elems: &[ArrayElem],
        expected_elem: &Ty,
        span: Span,
    ) -> Ty {
        let members = self.expand_union_members(expected_elem);

        elems.iter().for_each(|elem| match elem {
            ArrayElem::Elem(id) => {
                let elem_ty = self.expr(*id);
                // Check element is a member of the expected union
                if elem_ty != Ty::Error {
                    let is_member = members.as_ref().is_some_and(|ms| {
                        ms.iter().any(|m| self.types_compatible(&elem_ty, m))
                    });
                    if !is_member {
                        self.error(TypeError::Mismatch {
                            expected: expected_elem.clone(),
                            got: elem_ty,
                            span,
                        });
                    }
                }
            }
            ArrayElem::Spread(id) => {
                let spread_ty = self.expr(*id);
                // Spread must be Array[T] where T is compatible with expected
                match spread_ty {
                    Ty::Array(inner) => {
                        let is_member = members.as_ref().is_some_and(|ms| {
                            ms.iter().any(|m| self.types_compatible(&inner, m))
                        });
                        if !is_member {
                            self.error(TypeError::Mismatch {
                                expected: Ty::Array(Box::new(
                                    expected_elem.clone(),
                                )),
                                got: Ty::Array(inner),
                                span,
                            });
                        }
                    }
                    Ty::Var(_) => {
                        self.unify(
                            spread_ty,
                            Ty::Array(Box::new(expected_elem.clone())),
                            span,
                        );
                    }
                    Ty::Error => {}
                    _ => {
                        self.error(TypeError::NotAnArray(spread_ty, span));
                    }
                }
            }
        });

        Ty::Array(Box::new(expected_elem.clone()))
    }

    /// Infer type of `FOREVER` expression.
    ///
    /// `FOREVER seed (state, cont) => body` is a continuation-passing loop:
    /// - `seed` is the initial state value
    /// - `state` is bound to the current state in each iteration
    /// - `cont` is a pseudo-function that, when called with a new state,
    ///   continues the loop; not calling it exits and returns the body value
    ///
    /// Typing rules:
    /// - `state` has the same type as `seed` (or its annotation)
    /// - `cont` has type `(StateType) -> BodyType`
    /// - The overall expression returns `BodyType`
    pub(super) fn forever(
        &mut self,
        seed: ExprId,
        state_param: &(String, Option<AstTypeExprId>),
        cont_param: &(String, Option<AstTypeExprId>),
        body: ExprId,
        span: Span,
    ) -> Ty {
        // Infer seed type
        let seed_ty = self.expr(seed);

        // State parameter type: annotation or unify with seed
        let state_ty = state_param
            .1
            .map(|id| self.ast_type_to_ty(id, &HashMap::new()))
            .unwrap_or_else(|| seed_ty.clone());

        // Unify seed with state type
        self.unify(seed_ty, state_ty.clone(), span);

        // Create fresh type variable for body/result type
        let body_ty = self.fresh();

        // Continuation type: (StateType) -> BodyType
        let cont_ty = Ty::Fn(vec![state_ty.clone()], Box::new(body_ty.clone()));

        // Check cont_param annotation if present
        if let Some(ann_id) = cont_param.1 {
            let ann_ty = self.ast_type_to_ty(ann_id, &HashMap::new());
            self.unify(cont_ty.clone(), ann_ty, span);
        }

        // Push scope, bind parameters, infer body
        self.env.push_scope();
        self.env.bind(&state_param.0, Scheme::mono(state_ty));
        self.env.bind(&cont_param.0, Scheme::mono(cont_ty));
        let inferred_body_ty = self.expr(body);
        self.env.pop_scope();

        // Unify inferred body type with result type
        self.unify(inferred_body_ty, body_ty.clone(), span);

        body_ty
    }

    /// Typecheck a transaction block expression; assigns a unique `TxnId`.
    ///
    /// Returns `Result[T, String]` where `T` is the trailing expression type
    /// (or `Unit` if no trailing expression).
    ///
    /// Nested transactions are rejected at compile time (not runtime).
    pub(super) fn transaction(
        &mut self,
        id: ExprId,
        txn: &TransactionExpr,
        span: Span,
    ) -> Ty {
        // Nested transactions rejected at compile time
        if self.in_transaction.is_some() {
            self.error(TypeError::Custom {
                msg: "nested transactions are not supported".to_string(),
                span,
            });
            // Continue with a fresh ID anyway to allow further inference
        }

        // Assign unique ID
        let txn_id = TxnId::new(self.next_txn_id);
        self.next_txn_id += 1;

        // Set transaction context
        let prev = self.in_transaction.replace(txn_id);

        // Enter new scope for transaction body
        self.env.push_scope();

        // Typecheck all statements
        txn.stmts.iter().for_each(|&stmt_id| {
            self.stmt(stmt_id);
        });

        // Typecheck trailing expression or default to Unit
        let inner_ty = txn.expr.map_or(Ty::Unit, |expr_id| self.expr(expr_id));

        // Typecheck timeout modifier if present
        txn.modifiers.timeout.iter().for_each(|&timeout_id| {
            let timeout_ty = self.expr(timeout_id);
            self.unify(timeout_ty, Ty::Int, span);
        });

        self.env.pop_scope();

        // Restore previous transaction context
        self.in_transaction = prev;

        // Update AST with assigned ID
        let updated = TransactionExpr {
            id: Some(txn_id),
            stmts: txn.stmts.clone(),
            expr: txn.expr,
            modifiers: txn.modifiers,
        };
        self.ast.set_expr(id, Expr::Transaction(updated));

        // Return Result[T, String]
        Ty::Result(Box::new(inner_ty), Box::new(Ty::String))
    }
}
