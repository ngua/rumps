//! Expression type inference.
//!
//! Contains methods for inferring types of expressions: literals, variables,
//! operators, collections, function calls, control flow, etc.

use std::borrow::Cow;
use std::collections::HashMap;

use indexmap::IndexMap;
use smallvec::SmallVec;

use super::{Constraint, InferCtx};
use crate::ast::{
    ArrayElem, AstTypeExpr, AstTypeExprId, BinOp, DbRef, Expr, ExprId,
    Intrinsic, JsonAccessKey, JsonAccessKind, Literal, MatchArm, ObjectEntry,
    ParamConstraint, PostfixOp, RefTarget, StmtId, SubscriptElem,
    TransactionExpr, TxnId, TypeParam, TypePattern, UnOp, Visibility,
};
use crate::env::TxnReq;
use crate::intern::StringId;
use crate::typecheck::error::{ConstraintKind, TypeError};
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

            // String interpolation: all parts must be Into[String]
            Expr::Interpolation(parts) => self.interpolation(parts, span),

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

            // Optional index access: arr?[i] or str?[i] (safe, returns Option)
            Expr::OptionalIndex(base, idx) => {
                self.optional_index(*base, *idx, span)
            }

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
            } => {
                self.closure(id, type_params, params, ret.as_ref(), *body, span)
            }

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

            // Postfix operators: `!`
            Expr::Postfix(op, inner) => self.postfix(*op, *inner, span),

            // Type check: `expr IS Pattern`
            Expr::Is(scrutinee, pattern) => {
                self.is_check(*scrutinee, pattern, span)
            }

            // Type cast: `expr AS Type`
            Expr::As(inner, ty_id) => self.as_cast(*inner, *ty_id, span),

            // Fallible conversion: `expr READ Type`
            Expr::Read(inner, ty_id) => self.read_conv(*inner, *ty_id, span),

            // Database intrinsics: `@GET`, `@SET`, `@KILL`, `@DATA`, `@ORDER`, `@QUERY`
            Expr::Intrinsic(op, ref rt, val, _) => {
                self.intrinsic(id, *op, rt, val.as_ref().copied(), span)
            }

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
                } else if let Some(member) =
                    self.env.lookup_user_module_member(&path)
                {
                    // Check visibility; private members cannot be accessed
                    // from outside the module
                    if member.vis == Visibility::Private {
                        let module = path
                            .iter()
                            .take(path.len().saturating_sub(1))
                            .copied()
                            .collect::<Vec<_>>()
                            .join(".");
                        let name =
                            path.last().copied().unwrap_or("").to_string();
                        self.error(TypeError::PrivateAccess {
                            module,
                            name,
                            span,
                        });
                        Ty::Error
                    } else {
                        // Public member; instantiate and use
                        let (ty, constraints) =
                            member.scheme.instantiate(&mut self.next_var);
                        self.emit_user_constraints(constraints, span);
                        ty
                    }
                } else {
                    // Path resolved as module but member not found
                    let module = path
                        .iter()
                        .take(path.len().saturating_sub(1))
                        .copied()
                        .collect::<Vec<_>>()
                        .join(".");
                    let name = path.last().copied().unwrap_or("").to_string();
                    self.error(TypeError::NotFoundInModule {
                        module,
                        name,
                        span,
                    });
                    Ty::Error
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

                // LHS must be convertible to String
                self.constrain(Constraint::Into {
                    from: lhs_ty,
                    to: Ty::String,
                    span,
                });

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

            // Write expression: `WRITE expr [JSON] [TO target]`
            // Same typing as statement version, but returns `Unit`
            Expr::Write(output) => {
                self.write(output, span);
                Ty::Unit
            }

            // Raise expression: `RAISE expr`
            // Never returns; can unify with any expected type.
            Expr::Raise(inner) => {
                let ty = self.expr(*inner);
                // Error message must be convertible to String
                self.constrain(Constraint::Into {
                    from: ty,
                    to: Ty::String,
                    span,
                });
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

            // Mempty: `_` (monoid identity)
            //
            // Creates a fresh type variable with `Monoid` constraint.
            // The concrete type is inferred from context (e.g., `_ ++ [1]` infers `Array[Int]`).
            // Store the type variable for later resolution.
            Expr::Mempty => {
                let tv = Ty::Var(self.fresh_var());
                self.constrain(Constraint::Monoid(tv.clone(), span));
                self.mempty_types.insert(id, tv.clone());
                tv
            }

            // Ref literal: `data{1, 2}` or `^global{key}`
            // Creates a first-class `Local` or `Global` type.
            Expr::Ref(ref dbref) => match dbref {
                DbRef::Local(_, subs) => {
                    self.check_subscript_elems(subs, span);
                    Ty::Local
                }
                DbRef::Global(_, subs) => {
                    self.check_subscript_elems(subs, span);
                    Ty::Global
                }
            },
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

    /// Infer type of string interpolation.
    ///
    /// All expression parts (odd indices) must be convertible to `String`.
    /// Literal parts (even indices) are already strings. Returns `String`.
    fn interpolation(&mut self, parts: &[ExprId], span: Span) -> Ty {
        parts.iter().enumerate().for_each(|(i, &part_id)| {
            let part_ty = self.expr(part_id);
            // Odd indices are expressions; they must be convertible to String
            // Even indices are string literals; no constraint needed
            if i % 2 == 1 {
                self.constrain(Constraint::Into {
                    from: part_ty,
                    to: Ty::String,
                    span,
                });
            }
        });
        Ty::String
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

    /// Apply an operator's type scheme to operands.
    ///
    /// Instantiates the scheme, unifies operands with parameter types,
    /// emits constraints from the scheme, and returns the result type.
    fn apply_op_scheme(
        &mut self,
        scheme: &Scheme,
        args: &[Ty],
        span: Span,
    ) -> Ty {
        let (fn_ty, constraints) = scheme.instantiate(&mut self.next_var);
        self.emit_user_constraints(constraints, span);

        match fn_ty {
            Ty::Fn(params, ret) => {
                params.iter().zip(args.iter()).for_each(|(param, arg)| {
                    self.unify(arg.clone(), param.clone(), span);
                });
                *ret
            }
            _ => unreachable!("operator scheme must be function type"),
        }
    }

    /// Infer type of a binary operation.
    ///
    /// Uses the operator's type scheme to generate constraints and determine
    /// the result type. Operands must satisfy the scheme's constraints.
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
            // Equality allows comparing refs of different types
            BinOp::Eq | BinOp::Ne => {
                let both_refs = lhs_ty.is_ref() && rhs_ty.is_ref();
                if !both_refs {
                    self.unify(lhs_ty, rhs_ty, span);
                }
                Ty::Bool
            }

            // Pipe needs Callable constraint for polymorphic callables
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

            // All other operators use their type schemes
            _ => self.apply_op_scheme(&op.def().ty, &[lhs_ty, rhs_ty], span),
        }
    }

    /// Infer type of a unary operation.
    ///
    /// Uses the operator's type scheme to generate constraints and determine
    /// the result type.
    fn unary(&mut self, op: UnOp, operand_id: ExprId, span: Span) -> Ty {
        let operand_ty = self.expr(operand_id);
        self.apply_op_scheme(&op.def().ty, &[operand_ty], span)
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

            // Object: field may or may not exist; missing -> Unknown (no error)
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
    /// Works for `Array[T]` (index must be `Int`, returns `T`),
    /// `Map[K, V]` (index unifies with `K`, returns `Option[V]`),
    /// and `String` (index must be `Int`, returns `Char`).
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
                // Base is type variable; generate Indexable constraint
                let elem = self.fresh();
                self.constrain(Constraint::Indexable {
                    base: base_ty,
                    idx: idx_ty,
                    elem: elem.clone(),
                    span,
                });
                elem
            }

            Ty::Error => Ty::Error,

            Ty::String => {
                // String indexing returns Char
                self.unify(idx_ty, Ty::Int, span);
                Ty::Char
            }

            _ => {
                self.error(TypeError::UnsatisfiedConstraint(
                    ConstraintKind::Indexable,
                    base_ty,
                    span,
                ));
                Ty::Error
            }
        }
    }

    /// Infer type of optional index access: `base?[idx]`.
    ///
    /// Safe indexing that returns `Option[T]` instead of panicking:
    /// - `Array[T]?[Int]` returns `Option[T]`
    /// - `Map[K, V]?[K]` returns `Option[V]` (map lookup already returns Option)
    /// - `String?[Int]` returns `Option[Char]`
    fn optional_index(
        &mut self,
        base_id: ExprId,
        idx_id: ExprId,
        span: Span,
    ) -> Ty {
        let base_ty = self.expr(base_id);
        let idx_ty = self.expr(idx_id);

        match &base_ty {
            Ty::Array(elem) => {
                self.unify(idx_ty, Ty::Int, span);
                Ty::Option(elem.clone())
            }

            Ty::Map(key, val) => {
                self.unify(idx_ty, key.as_ref().clone(), span);
                // Map?[k] is the same as Map[k] since both return Option[V]
                Ty::Option(val.clone())
            }

            Ty::Var(_) => {
                // Generate Indexable constraint with elem wrapped in Option
                let inner = self.fresh();
                self.constrain(Constraint::Indexable {
                    base: base_ty,
                    idx: idx_ty,
                    elem: inner.clone(),
                    span,
                });
                Ty::Option(Box::new(inner))
            }

            Ty::Error => Ty::Error,

            Ty::String => {
                self.unify(idx_ty, Ty::Int, span);
                Ty::Option(Box::new(Ty::Char))
            }

            _ => {
                self.error(TypeError::UnsatisfiedConstraint(
                    ConstraintKind::Indexable,
                    base_ty,
                    span,
                ));
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
    /// as fresh type variables before inferring parameter/return types. The full
    /// type scheme (with quantified vars and constraints) is stored in
    /// `closure_schemes` for proper generalization when bound via `LET`.
    fn closure(
        &mut self,
        expr_id: ExprId,
        type_params: &SmallVec<[TypeParam; 2]>,
        params: &SmallVec<[(String, Option<AstTypeExprId>); 4]>,
        ret: Option<&AstTypeExprId>,
        body: ExprId,
        span: Span,
    ) -> Ty {
        use crate::typecheck::ty::TyVar;

        // Two-pass approach: first create all type variables, then emit
        // constraints (needed for Iterable[T] where T references another param)
        // Keep track of name -> TyVar for scheme building
        let name_to_tv: HashMap<&str, TyVar> = type_params
            .iter()
            .map(|tp| (tp.name.as_str(), self.fresh_var()))
            .collect();

        let type_param_subst: HashMap<_, _> = type_params
            .iter()
            .map(|tp| {
                let id = self.env.intern(&tp.name);
                let tv = name_to_tv[tp.name.as_str()];
                (id, Ty::Var(tv))
            })
            .collect();

        // Build scheme constraints (for storing in closure_schemes)
        let mut scheme_constraints: SmallVec<
            [(TyVar, ParamConstraint, Option<Ty>); 2],
        > = SmallVec::new();

        // Emit constraints for each user-specified bound
        type_params.iter().for_each(|tp| {
            let tv = name_to_tv[tp.name.as_str()];
            let ty = Ty::Var(tv);

            tp.constraints.iter().for_each(|c| {
                // Resolve element/inner/target type for parameterized constraints
                let elem_ty = match c {
                    ParamConstraint::Iterable(ty_id)
                    | ParamConstraint::Fallible(ty_id)
                    | ParamConstraint::Into(ty_id)
                    | ParamConstraint::TryInto(ty_id) => {
                        Some(self.ast_type_to_ty(*ty_id, &type_param_subst))
                    }
                    _ => None,
                };
                scheme_constraints.push((tv, c.clone(), elem_ty.clone()));

                // Emit constraint for body inference
                let constraint = match c {
                    ParamConstraint::Numeric => {
                        Constraint::Numeric(ty.clone(), span)
                    }
                    ParamConstraint::Subscriptable => {
                        Constraint::Subscriptable(ty.clone(), span)
                    }
                    ParamConstraint::Storable => {
                        Constraint::Storable(ty.clone(), span)
                    }
                    ParamConstraint::Iterable(_) => {
                        let elem =
                            elem_ty.clone().unwrap_or_else(|| self.fresh());
                        Constraint::Iterable {
                            coll: ty.clone(),
                            elem,
                            span,
                        }
                    }
                    ParamConstraint::Monoid => {
                        Constraint::Monoid(ty.clone(), span)
                    }
                    ParamConstraint::BitLike => {
                        Constraint::BitLike(ty.clone(), span)
                    }
                    ParamConstraint::Fallible(_) => {
                        let inner =
                            elem_ty.clone().unwrap_or_else(|| self.fresh());
                        Constraint::Fallible {
                            ty: ty.clone(),
                            inner,
                            span,
                        }
                    }
                    ParamConstraint::Into(_) => {
                        let to =
                            elem_ty.clone().unwrap_or_else(|| self.fresh());
                        Constraint::Into {
                            from: ty.clone(),
                            to,
                            span,
                        }
                    }
                    ParamConstraint::TryInto(_) => {
                        let to =
                            elem_ty.clone().unwrap_or_else(|| self.fresh());
                        Constraint::TryInto {
                            from: ty.clone(),
                            to,
                            span,
                        }
                    }
                };
                self.constrain(constraint);
            });
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

        let fn_ty = Ty::Fn(param_tys, Box::new(ret_ty));

        // If there are type params, store the scheme for LET binding generalization
        if !type_params.is_empty() {
            let vars: Vec<_> = name_to_tv.values().copied().collect();
            let scheme = Scheme {
                vars,
                ty: fn_ty.clone(),
                constraints: scheme_constraints,
            };
            self.closure_schemes.insert(expr_id, scheme);
        }

        fn_ty
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
        self.hoist_declarations(stmts);
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

    /// Infer type of postfix operators.
    ///
    /// Uses the operator's type scheme to generate constraints and determine
    /// the result type.
    fn postfix(&mut self, op: PostfixOp, inner_id: ExprId, span: Span) -> Ty {
        let inner_ty = self.expr(inner_id);
        self.apply_op_scheme(&op.def().ty, &[inner_ty], span)
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
    /// Emits an `Into` constraint to verify the conversion is valid.
    /// The actual validation happens in `check_into` during constraint solving.
    ///
    /// Special case: when casting a type variable to a numeric type, also
    /// emit a `Numeric` constraint to ensure polymorphic expressions like
    /// `(-2.9) AS Int` are properly constrained.
    fn as_cast(
        &mut self,
        inner_id: ExprId,
        ty_id: AstTypeExprId,
        span: Span,
    ) -> Ty {
        let inner_ty = self.expr(inner_id);
        let target_ty = self.ast_type_to_ty(ty_id, &HashMap::new());

        // Emit Into constraint for validation
        self.constrain(Constraint::Into {
            from: inner_ty.clone(),
            to: target_ty.clone(),
            span,
        });

        // Special case: type variable cast to numeric requires Numeric constraint
        // This allows `(-2.9) AS Int` where `-2.9` has polymorphic Numeric type
        match (&inner_ty, &target_ty) {
            (Ty::Var(_), Ty::Int | Ty::Float | Ty::Word) => {
                self.constrain(Constraint::Numeric(inner_ty.clone(), span));
            }
            _ => {}
        }

        // Error recovery: return Error type if either side is Error
        if matches!(inner_ty, Ty::Error) || matches!(target_ty, Ty::Error) {
            Ty::Error
        } else {
            target_ty
        }
    }

    /// Infer type of `READ` conversion expression.
    ///
    /// `expr READ T` returns `Result[T, String]`. The conversion is fallible;
    /// if the value cannot be converted to `T`, an error message is returned.
    ///
    /// Emits a `TryInto` constraint to validate that the conversion is possible
    /// at compile time; function types, regex, and refs cannot be used with `READ`.
    fn read_conv(
        &mut self,
        inner_id: ExprId,
        ty_id: AstTypeExprId,
        span: Span,
    ) -> Ty {
        let inner_ty = self.expr(inner_id);
        let target_ty = self.ast_type_to_ty(ty_id, &HashMap::new());

        // Emit TryInto constraint for validation
        self.constrain(Constraint::TryInto {
            from: inner_ty,
            to: target_ty.clone(),
            span,
        });

        Ty::Result(Box::new(target_ty), Box::new(Ty::String))
    }

    /// Infer type of a database intrinsic (`@GET`, `@SET`, `@KILL`, etc.).
    ///
    /// Validates transaction requirements for mutating intrinsics, typechecks
    /// the value expression for `@SET`, and populates the `TxnId` field in the
    /// AST based on current transaction context.
    ///
    /// When the target is `RefTarget::Inline(DbRef::Local(name, []))` and `name`
    /// is a variable of type `Ref`, rewrites to `RefTarget::Expr`.
    fn intrinsic(
        &mut self,
        id: ExprId,
        op: Intrinsic,
        rt: &RefTarget,
        val: Option<ExprId>,
        span: Span,
    ) -> Ty {
        let def = op.def();
        let resolved_rt = self.resolve_ref_target(rt, span).into_owned();

        // Validate transaction requirement for mutating intrinsics
        if def.txn == TxnReq::Globals {
            match op {
                Intrinsic::Set => self.set_validate(&resolved_rt, span),
                Intrinsic::Kill => self.kill_validate(&resolved_rt, span),
                _ => {}
            }
        }

        // For Set, also typecheck the value expression
        if let Some(v) = val {
            let v_ty = self.expr(v);
            self.constrain(Constraint::Storable(v_ty, span));
        }

        self.ast.set_expr(
            id,
            Expr::Intrinsic(op, resolved_rt, val, self.in_transaction),
        );

        def.ty.return_ty().cloned().unwrap_or(Ty::Error)
    }

    /// Resolve a `RefTarget`, checking for variable references.
    ///
    /// When the target is `RefTarget::Inline(DbRef::Local(name, []))` and `name`
    /// is a variable of type `Ref`, rewrites to `RefTarget::Expr` referencing
    /// that variable. Otherwise, type-checks subscripts and returns as-is.
    ///
    /// Returns `Cow::Borrowed` when unchanged, `Cow::Owned` when rewritten.
    pub(super) fn resolve_ref_target<'a>(
        &mut self,
        rt: &'a RefTarget,
        span: Span,
    ) -> Cow<'a, RefTarget> {
        match rt {
            RefTarget::Inline(dbref) => {
                match dbref {
                    DbRef::Local(name, subs) if subs.is_empty() => {
                        // Check if name is a variable of type Ref
                        let ref_ty = self.env.lookup(name).and_then(|s| {
                            let (ty, _) = s.instantiate(&mut self.next_var);
                            ty.is_ref().then_some(ty)
                        });
                        ref_ty.map_or_else(
                            // Not a Ref variable; treat as DB local
                            || Cow::Borrowed(rt),
                            |ty| {
                                // Create a Var expression and rewrite to Expr
                                match self
                                    .ast
                                    .add_expr(Expr::Var(name.clone()), span)
                                {
                                    Ok(var_id) => {
                                        self.record_type(var_id, ty);
                                        Cow::Owned(RefTarget::Expr(var_id))
                                    }
                                    Err(e) => {
                                        // Arena overflow; report error and fall back
                                        self.error(TypeError::Custom {
                                            msg: e.to_string(),
                                            span,
                                        });
                                        Cow::Borrowed(rt)
                                    }
                                }
                            },
                        )
                    }
                    DbRef::Local(_, subs) | DbRef::Global(_, subs) => {
                        // Has subscripts or is global; check subscripts
                        self.check_subscript_elems(subs, span);
                        Cow::Borrowed(rt)
                    }
                }
            }
            RefTarget::Expr(e) => {
                let ty = self.expr(*e);
                // Ensure expression is a Ref type (Local, Global, or Ref union)
                if !ty.is_ref() && !matches!(ty, Ty::Var(_) | Ty::Error) {
                    self.error(TypeError::Mismatch {
                        // Display hint: use Ref union (Local | Global)
                        expected: Ty::Named(TypeId::REF, vec![]),
                        got: ty,
                        span,
                    });
                }
                Cow::Borrowed(rt)
            }
        }
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
