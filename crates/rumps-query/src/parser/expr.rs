//! Expression parsing for RUMPS.

use chumsky::prelude::{choice, just, recursive, select};
use chumsky::Parser as _;
use ordered_float::OrderedFloat;
use smallvec::SmallVec;

use super::{ParseErr, Parser};
use crate::ast::{BinOp, Intrinsic, Literal, NumericLit, UnOp};
use crate::intern::{StringId, StringInterner};
use crate::parser::cst::{self, TypePattern};
use crate::{Span, Token};

impl Parser {
    fn write_expr(
        interner: &mut StringInterner,
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let json = interner.intern("json");
        let raw = interner.intern("raw");
        let to = interner.intern("to");
        let error = interner.intern("error");
        let file = interner.intern("file");

        let format = Self::ctx_ident(json)
            .to(cst::OutputFormat::Json)
            .or(Self::ctx_ident(raw).to(cst::OutputFormat::Raw))
            .or_not()
            .map(|f| f.unwrap_or_default());

        let to_error = Self::ctx_ident(to)
            .ignore_then(Self::ctx_ident(error))
            .to(cst::OutputTarget::Stderr);

        let to_file = Self::ctx_ident(to)
            .ignore_then(Self::ctx_ident(file))
            .ignore_then(expr.clone())
            .map(|e| cst::OutputTarget::File(Box::new(e)));

        let target =
            to_error.or(to_file).or_not().map(|t| t.unwrap_or_default());

        just(Token::Write)
            .ignore_then(expr)
            .then(format)
            .then(target)
            .map_with_span(|((inner, format), target), span| {
                let output = cst::WriteStmt {
                    expr: inner,
                    format,
                    target,
                };
                cst::Expr::new(cst::ExprKind::Write(Box::new(output)), span)
            })
    }

    fn set_expr(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        just(Token::Set)
            .ignore_then(Self::ref_expr(expr.clone()))
            .then_ignore(Self::opt_newlines())
            .then(expr)
            .map_with_span(|(r, val), span| {
                cst::Expr::new(
                    cst::ExprKind::Intrinsic(
                        Intrinsic::Set,
                        Box::new(r),
                        Some(Box::new(val)),
                    ),
                    span,
                )
            })
    }

    fn kill_expr(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        just(Token::Kill)
            .ignore_then(Self::ref_expr(expr))
            .map_with_span(|r, span| {
                cst::Expr::new(
                    cst::ExprKind::Intrinsic(
                        Intrinsic::Kill,
                        Box::new(r),
                        None,
                    ),
                    span,
                )
            })
    }

    fn raise_expr(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        just(Token::Raise)
            .ignore_then(expr)
            .map_with_span(|e, span| {
                cst::Expr::new(cst::ExprKind::Raise(Box::new(e)), span)
            })
    }

    fn loop_expr(
        interner: &mut StringInterner,
        primary: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        // Parameter: `name` or `name: Type`
        let param = Self::ident().then(
            just(Token::Colon)
                .ignore_then(Self::opt_newlines())
                .ignore_then(Self::type_expr(interner))
                .or_not(),
        );

        // Two parameters: `(state, cont)`
        let params = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(param.clone())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::Comma))
            .then_ignore(Self::opt_newlines())
            .then(param)
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen));

        // Use `primary` for seed (no postfix ops) to avoid parsing (state, cont) as call
        just(Token::Loop)
            .ignore_then(Self::opt_newlines())
            .ignore_then(primary)
            .then_ignore(Self::opt_newlines())
            .then(params)
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::FatArrow))
            .then_ignore(Self::opt_newlines())
            .then(expr)
            .map_with_span(|((seed, (state_param, cont_param)), body), span| {
                cst::Expr::new(
                    cst::ExprKind::Loop {
                        seed: Box::new(seed),
                        state_param,
                        cont_param,
                        body: Box::new(body),
                    },
                    span,
                )
            })
    }

    /// Parse transaction modifiers (contextual identifiers).
    ///
    /// Syntax: `[on conflict ...] [with timeout expr] [with retries n] [with isolation ...]`
    pub(super) fn transaction_modifiers(
        interner: &mut StringInterner,
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::TransactionModifiers, Error = ParseErr>
           + Clone {
        let on = interner.intern("on");
        let conflict = interner.intern("conflict");
        let abort = interner.intern("abort");
        let overwrite = interner.intern("overwrite");
        let with = interner.intern("with");
        let timeout = interner.intern("timeout");
        let retries = interner.intern("retries");
        let isolation = interner.intern("isolation");
        let snapshot = interner.intern("snapshot");

        // on conflict (abort | overwrite)
        let conflict_mod = Self::ctx_ident(on)
            .ignore_then(Self::ctx_ident(conflict))
            .ignore_then(choice((
                Self::ctx_ident(abort).to(cst::ConflictModifier::Abort),
                Self::ctx_ident(overwrite).to(cst::ConflictModifier::Overwrite),
            )));

        // with timeout expr
        let timeout_mod = Self::ctx_ident(with)
            .ignore_then(Self::ctx_ident(timeout))
            .ignore_then(expr)
            .map(Box::new);

        // with retries n
        let retries_mod = Self::ctx_ident(with)
            .ignore_then(Self::ctx_ident(retries))
            .ignore_then(select! { Token::Int(n) => n as u32 });

        // with isolation snapshot
        let isolation_mod = Self::ctx_ident(with)
            .ignore_then(Self::ctx_ident(isolation))
            .ignore_then(
                Self::ctx_ident(snapshot).to(cst::IsolationModifier::Snapshot),
            );

        // Modifiers must appear in this fixed order: conflict, timeout, retries,
        // isolation. Each modifier can appear at most once. Out-of-order or
        // duplicate modifiers will produce a parse error.
        conflict_mod
            .or_not()
            .then(timeout_mod.or_not())
            .then(retries_mod.or_not())
            .then(isolation_mod.or_not())
            .map(|(((conflict, timeout), retries), isolation)| {
                cst::TransactionModifiers {
                    conflict,
                    timeout,
                    retries,
                    isolation,
                }
            })
    }

    fn transaction_expr(
        interner: &mut StringInterner,
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        just(Token::Transaction)
            .ignore_then(Self::block(stmt))
            .then(Self::transaction_modifiers(interner, expr))
            .map_with_span(|((stmts, _blk_span), modifiers), span| {
                // Split trailing expr statement from regular statements
                let has_tail = stmts
                    .last()
                    .map(|s| matches!(&s.kind, cst::StmtKind::Expr(_)))
                    .unwrap_or(false);

                let (block_stmts, tail) = if has_tail {
                    let n = stmts.len().saturating_sub(1);
                    let mut iter = stmts.into_iter();
                    let ss: Vec<_> = iter.by_ref().take(n).collect();
                    let t = iter.next().and_then(|s| match s.kind {
                        cst::StmtKind::Expr(e) => Some(Box::new(e)),
                        _ => None,
                    });
                    (ss, t)
                } else {
                    (stmts, None)
                };

                cst::Expr::new(
                    cst::ExprKind::Transaction(Box::new(
                        cst::TransactionExpr {
                            stmts: block_stmts,
                            expr: tail,
                            modifiers,
                        },
                    )),
                    span,
                )
            })
    }

    pub(super) fn expr(
        interner: &mut StringInterner,
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        // Construct type_expr and type_pattern once BEFORE the recursive block.
        // Parser construction inside recursive() can cause stack overflow.
        let ty = Self::type_expr(interner);
        let ty_pat = Self::type_pattern(interner);

        recursive(move |expr| {
            // Define `pipe` (expr without CATCH) using nested recursive.
            // Intrinsics use `pipe` for operands so they don't consume CATCH.
            let pipe = recursive({
                let expr = expr.clone();
                let stmt = stmt.clone();
                let ty = ty.clone();
                let ty_pat = ty_pat.clone();
                move |pipe| {
                    let primary = Self::primary_expr(
                        interner,
                        expr.clone(),
                        stmt.clone(),
                    );
                    let postfix =
                        Self::postfix_expr(expr.clone(), primary.clone())
                            .boxed();
                    // Pass `pipe` to `unary_expr` for intrinsic operands
                    let unary =
                        Self::unary_expr(interner, pipe, primary, postfix);
                    let annotate = Self::annotate_expr(unary, ty.clone());
                    let pow = Self::pow_expr(annotate);
                    let mul = Self::mul_expr(pow).boxed();
                    let shift = Self::shift_expr(mul);
                    let add = Self::add_expr(shift);
                    let range = Self::range_expr(add);
                    let cmp = Self::cmp_expr(range).boxed();
                    let is = Self::is_expr(cmp, ty_pat.clone());
                    let matches = Self::matches_expr(is);
                    let as_cast = Self::as_expr(matches, ty.clone());
                    let read = Self::read_expr(as_cast, ty.clone()).boxed();
                    let bitand = Self::bitand_expr(read);
                    let bitor = Self::bitor_expr(bitand);
                    let and = Self::and_expr(bitor);
                    let or = Self::or_expr(and);
                    let coalesce = Self::coalesce_expr(or);
                    Self::pipe_expr(coalesce)
                }
            });

            Self::catch_expr(pipe)
        })
    }

    fn pipe_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = just(Token::Pipe).to(BinOp::Pipe);
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), span| Self::fold_pipe(first, rest, span),
        )
    }

    fn catch_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let catch_rhs = Self::opt_newlines()
            .ignore_then(just(Token::Catch))
            .then_ignore(Self::opt_newlines())
            .ignore_then(operand.clone());

        operand.clone().then(catch_rhs.or_not()).map_with_span(
            |(expr, handler), span| match handler {
                Some(h) => cst::Expr::new(
                    cst::ExprKind::Catch(Box::new(expr), Box::new(h)),
                    span,
                ),
                None => expr,
            },
        )
    }

    fn coalesce_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = just(Token::QuestionQuestion).to(BinOp::Coalesce);
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), span| Self::fold_binary(first, rest, span),
        )
    }

    fn or_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = choice((just(Token::PipePipe), just(Token::Or))).to(BinOp::Or);
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), span| Self::fold_binary(first, rest, span),
        )
    }

    fn and_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = choice((just(Token::AmpAmp), just(Token::And))).to(BinOp::And);
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), span| Self::fold_binary(first, rest, span),
        )
    }

    fn bitor_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = just(Token::SinglePipe).to(BinOp::BitOr);
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), span| Self::fold_binary(first, rest, span),
        )
    }

    fn bitand_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = just(Token::Amp).to(BinOp::BitAnd);
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), span| Self::fold_binary(first, rest, span),
        )
    }

    fn cmp_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = choice((
            just(Token::Eq).to(BinOp::Eq),
            just(Token::Ne).to(BinOp::Ne),
            just(Token::Le).to(BinOp::Le),
            just(Token::Ge).to(BinOp::Ge),
            just(Token::Lt).to(BinOp::Lt),
            just(Token::Gt).to(BinOp::Gt),
        ));
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), span| Self::fold_binary(first, rest, span),
        )
    }

    fn is_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
        type_pattern: impl chumsky::Parser<Token, TypePattern, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let is_rhs = Self::opt_newlines()
            .ignore_then(just(Token::Is))
            .then_ignore(Self::opt_newlines())
            .ignore_then(type_pattern);

        operand.clone().then(is_rhs.or_not()).map_with_span(
            |(expr, pattern), span| match pattern {
                Some(pat) => {
                    cst::Expr::new(cst::ExprKind::Is(Box::new(expr), pat), span)
                }
                None => expr,
            },
        )
    }

    fn matches_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let matches_rhs = Self::opt_newlines()
            .ignore_then(just(Token::Matches))
            .then_ignore(Self::opt_newlines())
            .ignore_then(operand.clone());

        operand.clone().then(matches_rhs.or_not()).map_with_span(
            |(lhs, rhs), span| match rhs {
                Some(r) => cst::Expr::new(
                    cst::ExprKind::Matches(Box::new(lhs), Box::new(r)),
                    span,
                ),
                None => lhs,
            },
        )
    }

    fn as_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
        ty: impl chumsky::Parser<Token, cst::TypeExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let as_rhs = Self::opt_newlines()
            .ignore_then(just(Token::As))
            .then_ignore(Self::opt_newlines())
            .ignore_then(ty);

        operand.clone().then(as_rhs.or_not()).map_with_span(
            |(expr, ty), span| match ty {
                Some(ty_expr) => cst::Expr::new(
                    cst::ExprKind::As(Box::new(expr), ty_expr),
                    span,
                ),
                None => expr,
            },
        )
    }

    fn read_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
        ty: impl chumsky::Parser<Token, cst::TypeExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let read_rhs = Self::opt_newlines()
            .ignore_then(just(Token::Read))
            .then_ignore(Self::opt_newlines())
            .ignore_then(ty);

        operand.clone().then(read_rhs.or_not()).map_with_span(
            |(expr, ty), span| match ty {
                Some(ty_expr) => cst::Expr::new(
                    cst::ExprKind::Read(Box::new(expr), ty_expr),
                    span,
                ),
                None => expr,
            },
        )
    }

    fn add_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = choice((
            just(Token::Plus).to(BinOp::Add),
            just(Token::Minus).to(BinOp::Sub),
            just(Token::Concat).to(BinOp::Concat),
        ));
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), span| Self::fold_binary(first, rest, span),
        )
    }

    fn range_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        // Parse the range operator and whether it's inclusive
        let range_op = choice((
            just(Token::DotDotEquals).to(true), // ..= inclusive
            just(Token::DotDot).to(false),      // .. exclusive
        ));

        let range_rhs = Self::opt_newlines()
            .ignore_then(range_op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());

        operand.clone().then(range_rhs.or_not()).map_with_span(
            |(start, suffix), span| match suffix {
                Some((inclusive, end)) => {
                    let range_span = Span::new(start.span.start, end.span.end);
                    cst::Expr::new(
                        cst::ExprKind::Range(
                            Box::new(start),
                            Box::new(end),
                            inclusive,
                        ),
                        range_span,
                    )
                }
                None => start.with_span(span),
            },
        )
    }

    fn pow_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op_rhs = Self::opt_newlines()
            .ignore_then(just(Token::StarStar))
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), _span| Self::fold_binary_right(first, rest),
        )
    }

    fn fold_binary_right(
        first: cst::Expr,
        rest: Vec<(Token, cst::Expr)>,
    ) -> cst::Expr {
        rest.into_iter()
            .rfold(None, |acc: Option<cst::Expr>, (_tok, expr)| match acc {
                None => Some(expr),
                Some(rhs) => {
                    let span = Span::new(expr.span.start, rhs.span.end);
                    Some(cst::Expr::new(
                        cst::ExprKind::Binary(
                            Box::new(expr),
                            BinOp::Pow,
                            Box::new(rhs),
                        ),
                        span,
                    ))
                }
            })
            .map_or(first.clone(), |rhs| {
                let span = Span::new(first.span.start, rhs.span.end);
                cst::Expr::new(
                    cst::ExprKind::Binary(
                        Box::new(first),
                        BinOp::Pow,
                        Box::new(rhs),
                    ),
                    span,
                )
            })
    }

    fn mul_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = choice((
            just(Token::Mul).to(BinOp::Mul),
            just(Token::FloorDiv).to(BinOp::FloorDiv),
            just(Token::Div).to(BinOp::Div),
            just(Token::Modulo).to(BinOp::Mod),
        ));
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), span| Self::fold_binary(first, rest, span),
        )
    }

    fn shift_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = choice((
            just(Token::Shl).to(BinOp::Shl),
            just(Token::Shr).to(BinOp::Shr),
        ));
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), span| Self::fold_binary(first, rest, span),
        )
    }

    fn fold_binary(
        first: cst::Expr,
        rest: Vec<(BinOp, cst::Expr)>,
        _outer_span: Span,
    ) -> cst::Expr {
        rest.into_iter().fold(first, |lhs, (op, rhs)| {
            let span = Span::new(lhs.span.start, rhs.span.end);
            cst::Expr::new(
                cst::ExprKind::Binary(Box::new(lhs), op, Box::new(rhs)),
                span,
            )
        })
    }

    fn fold_pipe(
        first: cst::Expr,
        rest: Vec<(BinOp, cst::Expr)>,
        _outer_span: Span,
    ) -> cst::Expr {
        rest.into_iter().fold(first, |lhs, (op, rhs)| {
            let span = Span::new(lhs.span.start, rhs.span.end);

            // Check if RHS is a call with placeholder arguments
            match &rhs.kind {
                cst::ExprKind::Call(callee, args) => {
                    // Check if any argument is a placeholder
                    let has_placeholder = args.iter().any(|arg| {
                        matches!(arg.kind, cst::ExprKind::PipePlaceholder)
                    });

                    if has_placeholder {
                        // Replace all placeholders with the LHS value
                        let new_args = args
                            .iter()
                            .map(|arg| {
                                if matches!(
                                    arg.kind,
                                    cst::ExprKind::PipePlaceholder
                                ) {
                                    lhs.clone()
                                } else {
                                    arg.clone()
                                }
                            })
                            .collect();

                        // Return the call directly; no pipe operator
                        cst::Expr::new(
                            cst::ExprKind::Call(callee.clone(), new_args),
                            span,
                        )
                    } else {
                        // No placeholder; use normal pipe binary op
                        cst::Expr::new(
                            cst::ExprKind::Binary(
                                Box::new(lhs),
                                op,
                                Box::new(rhs),
                            ),
                            span,
                        )
                    }
                }
                _ => {
                    // Not a call; use normal pipe binary op
                    cst::Expr::new(
                        cst::ExprKind::Binary(Box::new(lhs), op, Box::new(rhs)),
                        span,
                    )
                }
            }
        })
    }

    fn unary_expr(
        interner: &mut StringInterner,
        intrinsic_op: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
        primary: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = choice((
            just(Token::Not).to(UnOp::Not),
            just(Token::Bang).to(UnOp::Not),
            just(Token::Minus).to(UnOp::Neg),
            just(Token::Question).to(UnOp::Wrap),
        ));

        // Postfix operators for intrinsic expressions (e.g., `@get d(1)!`).
        let postfix_ops = Self::postfix_ops(intrinsic_op.clone());

        // OUTPUT expr [JSON] [TO target]
        let output = Self::write_expr(interner, intrinsic_op.clone());

        // `loop seed (state, cont) => body`
        // Use primary for seed (no postfix ops) to avoid parsing (state, cont) as a call
        let loop_expr =
            Self::loop_expr(interner, primary, intrinsic_op.clone());

        recursive(move |unary| {
            let with_op = op.clone().then(unary.clone()).map_with_span(
                |(op, inner), span| {
                    cst::Expr::new(
                        cst::ExprKind::Unary(op, Box::new(inner)),
                        span,
                    )
                },
            );

            // Read intrinsics: @get, @data, @order, @query (with optional postfix ops)
            let read_intrinsic = choice((
                just(Token::Get).to(Intrinsic::Get),
                just(Token::Data).to(Intrinsic::Data),
                just(Token::Order).to(Intrinsic::Order),
                just(Token::Query).to(Intrinsic::Query),
            ))
            .then(Self::ref_expr(intrinsic_op.clone()))
            .map_with_span(|(op, r), span| {
                cst::Expr::new(
                    cst::ExprKind::Intrinsic(op, Box::new(r), None),
                    span,
                )
            })
            .then(postfix_ops.clone())
            .map_with_span(|(base, ops), span| {
                Self::fold_postfix(base, ops).unwrap_or_else(|| {
                    cst::Expr::new(
                        cst::ExprKind::Error("postfix fold failed".into()),
                        span,
                    )
                })
            });

            // SET target = value
            let set = Self::set_expr(intrinsic_op.clone());

            // kill target
            let kill = Self::kill_expr(intrinsic_op.clone());

            // RAISE expr
            let raise = Self::raise_expr(intrinsic_op.clone());

            choice((
                with_op,
                read_intrinsic,
                output.clone(),
                set,
                kill,
                raise,
                loop_expr.clone(),
            ))
            .or(operand.clone())
        })
    }

    fn annotate_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
        ty: impl chumsky::Parser<Token, cst::TypeExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let annotate = just(Token::Colon)
            .ignore_then(Self::opt_newlines())
            .ignore_then(ty);

        operand.clone().then(annotate.or_not()).map_with_span(
            |(expr, anno), span| match anno {
                Some(ty) => cst::Expr::new(
                    cst::ExprKind::Annotate(Box::new(expr), ty),
                    span,
                ),
                None => expr,
            },
        )
    }

    fn primary_expr(
        interner: &mut StringInterner,
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let unit = interner.intern("Unit");
        let underscore = interner.intern("_");
        let ty = Self::type_expr(interner);
        let ty_params = Self::type_params(interner);

        // Literals
        let int_lit =
            select! { Token::Int(n) => Literal::Numeric(NumericLit::Int(n)) };
        let float_lit = select! { Token::Float(OrderedFloat(n)) => Literal::Numeric(NumericLit::Float(n)) };
        let char_lit = select! { Token::Char(c) => Literal::Char(c) };
        let str_lit = select! { Token::String(s) => Literal::String(s) };
        let bool_lit = choice((
            just(Token::True).to(Literal::Bool(true)),
            just(Token::False).to(Literal::Bool(false)),
        ));
        let null_lit = just(Token::Null).to(Literal::Null);
        let unit_lit =
            select! { Token::Ident(s) if s == unit => Literal::Unit };

        let literal = choice((
            int_lit, float_lit, char_lit, str_lit, bool_lit, null_lit, unit_lit,
        ))
        .map_with_span(|lit, span| {
            cst::Expr::new(cst::ExprKind::Literal(lit), span)
        });

        // Regex literal: `/pattern/`
        let regex_lit =
            select! { Token::Regex(s) => s }.map_with_span(|pattern, span| {
                cst::Expr::new(cst::ExprKind::Regex(pattern), span)
            });

        // Interpolated string: `"Hello {name}!"`
        let interpolation = select! { Token::Interpolation(parts) => parts }
            .map_with_span(|parts, span| {
                cst::Expr::new(cst::ExprKind::Interpolation(parts), span)
            });

        // Database reference literals: `name{...}` or `^global{...}`.
        //
        // Creates a first-class `Ref` value. Uses `IdentBrace`/`GlobalBrace`
        // tokens which only form when there's NO space between name and `{`.
        // This allows `if cond { ... }` to work (space means block, not ref).
        let ref_local = select! { Token::IdentBrace(name) => name }
            .then(Self::subscript_contents(expr.clone()))
            .map_with_span(|(name, subs), span| {
                cst::Expr::new(
                    cst::ExprKind::RefLit(cst::DbRef::Local(name, subs)),
                    span,
                )
            });
        let ref_global = select! { Token::GlobalBrace(name) => name }
            .then(Self::subscript_contents(expr.clone()))
            .map_with_span(|(name, subs), span| {
                cst::Expr::new(
                    cst::ExprKind::RefLit(cst::DbRef::Global(name, subs)),
                    span,
                )
            });

        // Pipe placeholder: `.`
        //
        // Only valid as argument in function call on RHS of `|>`. Transformed
        // during pipe expression parsing; any remaining placeholders are errors.
        let pipe_placeholder = just(Token::Dot).map_with_span(|_, span| {
            cst::Expr::new(cst::ExprKind::PipePlaceholder, span)
        });

        // Class method call: `Class:method(args)`.
        //
        // Dispatches to a typeclass method. Uses `ColonNoSpace` to require
        // no space around the colon, distinguishing from type annotations.
        // Examples:
        // - `Numeric:add(a, b)` (binary method)
        // - `Fallible:unwrap(opt)` (unary method)
        // - `Mappable:map(fn, arr)` (higher-order method)
        let class_method_sep =
            just(Token::Comma).then_ignore(Self::opt_newlines());
        let class_method = Self::ident()
            .then_ignore(just(Token::ColonNoSpace))
            .then(Self::ident())
            .then_ignore(just(Token::LParen))
            .then_ignore(Self::opt_newlines())
            .then(
                expr.clone()
                    .separated_by(class_method_sep.clone())
                    .allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen))
            .map_with_span(|((class, method), args), span| {
                cst::Expr::new(
                    cst::ExprKind::ClassMethod(class, method, args),
                    span,
                )
            });

        // Class method reference: `Class:method` or `Class[T, ...]:method`.
        //
        // A first-class function value. Uses `ColonNoSpace` to require no
        // space around the colon. This is ordered after `class_method` in the
        // choice, so `class_method` (with parens) is tried first.
        //
        // The optional type arguments are required for convert methods
        // (`Fallible`, `Into`, `TryInto`) when used as first-class values.
        let class_type_args = ty
            .clone()
            .separated_by(just(Token::Comma))
            .at_least(1)
            .delimited_by(just(Token::LBracket), just(Token::RBracket));
        let class_method_ref = Self::ident()
            .then(class_type_args.or_not())
            .then_ignore(just(Token::ColonNoSpace))
            .then(Self::ident())
            .map_with_span(|((class, type_args), method), span| {
                cst::Expr::new(
                    cst::ExprKind::ClassMethodRef(
                        class,
                        type_args.unwrap_or_default(),
                        method,
                    ),
                    span,
                )
            });

        // Naked class method call: `:method(args)`.
        //
        // Like `class_method` but without the class name prefix. Resolved
        // during type checking; ambiguous method names are errors.
        let naked_class_method = just(Token::ColonNoSpace)
            .ignore_then(Self::ident())
            .then_ignore(just(Token::LParen))
            .then_ignore(Self::opt_newlines())
            .then(
                expr.clone()
                    .separated_by(class_method_sep.clone())
                    .allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen))
            .map_with_span(|(method, args), span| {
                cst::Expr::new(
                    cst::ExprKind::NakedClassMethod(method, args),
                    span,
                )
            });

        // Naked class method reference: `:method`.
        //
        // Like `class_method_ref` but without the class name prefix.
        let naked_class_method_ref = just(Token::ColonNoSpace)
            .ignore_then(Self::ident())
            .map_with_span(|method, span| {
                cst::Expr::new(cst::ExprKind::NakedClassMethodRef(method), span)
            });

        // Naked variant constructor call: `.Variant(args)`.
        let naked_variant = select! { Token::DotIdent(name) => name };
        let naked_variant_call = naked_variant
            .clone()
            .then_ignore(just(Token::LParen))
            .then_ignore(Self::opt_newlines())
            .then(
                expr.clone()
                    .separated_by(class_method_sep)
                    .at_least(1)
                    .allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen))
            .map_with_span(|(var, args), span| {
                cst::Expr::new(cst::ExprKind::NakedVariant(var, args), span)
            });

        // Naked variant constructor value: `.Variant`.
        let naked_variant_ref = naked_variant.map_with_span(|var, span| {
            cst::Expr::new(cst::ExprKind::NakedVariant(var, Vec::new()), span)
        });

        // Lexical variable or mempty (`_`)
        let var = Self::ident().map_with_span(move |name, span| {
            if name == underscore {
                cst::Expr::new(cst::ExprKind::Mempty, span)
            } else {
                cst::Expr::new(cst::ExprKind::Var(name), span)
            }
        });

        // Parenthesized expression or tuple literal
        // - `(expr)` -> parenthesized expression (unwrapped)
        // - `(expr,)` -> single-element tuple
        // - `(expr, expr, ...)` -> multi-element tuple
        // - `()` -> empty tuple
        //
        // Type annotations are handled as postfix operators: `expr: Type`.
        // This works uniformly for `(expr): T`, `(expr: T)`, `10: Int`, etc.
        //
        // We manually detect trailing comma instead of using `allow_trailing()`
        // so we can distinguish `(x)` from `(x,)`.
        let paren_or_tuple = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                // Empty: `()`
                just(Token::RParen).to(ParenContents::Empty).or(
                    // First element
                    expr.clone()
                        .then(
                            // More elements or trailing comma
                            just(Token::Comma)
                                .ignore_then(Self::opt_newlines())
                                .ignore_then(
                                    expr.clone()
                                        .separated_by(
                                            just(Token::Comma).then_ignore(
                                                Self::opt_newlines(),
                                            ),
                                        )
                                        .allow_trailing(),
                                )
                                .or_not(),
                        )
                        .then_ignore(Self::opt_newlines())
                        .then_ignore(just(Token::RParen))
                        .map(|(first, rest)| match rest {
                            None => {
                                // `(x)` - single element, no comma
                                ParenContents::Elements(vec![first], false)
                            }
                            Some(mut more) => {
                                // `(x,)` or `(x, y, ...)` - has comma
                                let mut elems = vec![first];
                                elems.append(&mut more);
                                ParenContents::Elements(elems, true)
                            }
                        }),
                ),
            )
            .map_with_span(|contents, span| match contents {
                ParenContents::Empty => {
                    cst::Expr::new(cst::ExprKind::Tuple(vec![]), span)
                }
                ParenContents::Elements(mut elems, has_comma) => {
                    if elems.len() == 1 && !has_comma {
                        // Single element without comma: unwrap parens
                        // SAFETY: len checked above; `unwrap` is OK
                        #[allow(clippy::unwrap_used)]
                        elems.pop().unwrap()
                    } else {
                        cst::Expr::new(cst::ExprKind::Tuple(elems), span)
                    }
                }
            });
        let paren = paren_or_tuple;

        // Array literal with spread support
        let arr_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let spread_elem = just(Token::DotDotDot)
            .ignore_then(expr.clone())
            .map(cst::ArrayElem::Spread);
        let single_elem = expr.clone().map(cst::ArrayElem::Elem);
        let arr_elem = spread_elem.or(single_elem);
        let array = just(Token::LBracket)
            .ignore_then(Self::opt_newlines())
            .ignore_then(arr_elem.separated_by(arr_sep).allow_trailing())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBracket))
            .map_with_span(|elems, span| {
                cst::Expr::new(cst::ExprKind::Array(elems), span)
            });

        // Object/JSON literal with spread support:
        // - `{ field: expr, ... }` (unquoted keys -> Object)
        // - `{ "field": expr, ... }` (quoted keys -> JSON)
        // - `{ ...expr, field: value }` (spread + fields -> Object)
        // Mixed quoted/unquoted keys produce a parse error.
        // Spreads are only valid in Object context (not JSON).

        // Entry kind: Spread, UnquotedField, or QuotedField
        #[derive(Clone)]
        enum ObjEntryKind {
            UnquotedField(StringId, cst::Expr),
            QuotedField(String, cst::Expr),
            Spread(cst::Expr),
        }

        let unquoted_field = Self::ident()
            .then_ignore(just(Token::Colon))
            .then(expr.clone())
            .map(|(key, value)| ObjEntryKind::UnquotedField(key, value));

        let quoted_field = select! { Token::String(s) => s }
            .then_ignore(just(Token::Colon))
            .then(expr.clone())
            .map(|(key, value)| ObjEntryKind::QuotedField(key, value));

        let obj_spread = just(Token::DotDotDot)
            .ignore_then(expr.clone())
            .map(ObjEntryKind::Spread);

        let obj_entry = obj_spread.or(quoted_field).or(unquoted_field);

        let obj_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let object_or_json = just(Token::LBrace)
            .ignore_then(Self::opt_newlines())
            .ignore_then(obj_entry.separated_by(obj_sep).allow_trailing())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBrace))
            .map_with_span(|entries: Vec<ObjEntryKind>, span| {
                let has_spread = entries
                    .iter()
                    .any(|e| matches!(e, ObjEntryKind::Spread(_)));
                let has_quoted = entries
                    .iter()
                    .any(|e| matches!(e, ObjEntryKind::QuotedField(..)));
                let has_unquoted = entries
                    .iter()
                    .any(|e| matches!(e, ObjEntryKind::UnquotedField(..)));
                let has_mixed = has_quoted && has_unquoted;

                if has_spread && has_quoted {
                    cst::Expr::new(
                        cst::ExprKind::Error(
                            "cannot use spread in JSON object (quoted keys)"
                                .into(),
                        ),
                        span,
                    )
                } else if has_mixed {
                    cst::Expr::new(
                        cst::ExprKind::Error(
                            "cannot mix quoted and unquoted keys in object"
                                .into(),
                        ),
                        span,
                    )
                } else if has_quoted {
                    // JSON (all quoted, no spreads)
                    let json_fields: Vec<(String, cst::Expr)> = entries
                        .into_iter()
                        .filter_map(|e| match e {
                            ObjEntryKind::QuotedField(k, v) => Some((k, v)),
                            _ => None,
                        })
                        .collect();
                    cst::Expr::new(cst::ExprKind::Json(json_fields), span)
                } else {
                    // Object (unquoted keys, possibly with spreads)
                    let obj_entries: Vec<cst::ObjectEntry> = entries
                        .into_iter()
                        .map(|e| match e {
                            ObjEntryKind::UnquotedField(k, v) => {
                                cst::ObjectEntry::Field(k, v)
                            }
                            ObjEntryKind::Spread(e) => {
                                cst::ObjectEntry::Spread(e)
                            }
                            ObjEntryKind::QuotedField(..) => unreachable!(),
                        })
                        .collect();
                    cst::Expr::new(cst::ExprKind::Object(obj_entries), span)
                }
            });

        // Map literal: { key => value, ... }
        let map_entry = expr
            .clone()
            .then_ignore(just(Token::FatArrow))
            .then(expr.clone());

        let map_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let map_lit = just(Token::LBrace)
            .ignore_then(Self::opt_newlines())
            .ignore_then(map_entry.separated_by(map_sep).allow_trailing())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBrace))
            .map_with_span(|entries, span| {
                cst::Expr::new(cst::ExprKind::MapLit(entries), span)
            });

        // Try object/json first (key + `:`), then fall back to map (expr + `=>`).
        //
        // NOTE: `{}` is ambiguous and parses as an empty object, not an empty map.
        // Use `Map.empty()` for empty maps.
        let object_or_map = object_or_json.or(map_lit);

        // Block expression
        let block_parser = Self::block(stmt.clone());
        let block_expr =
            block_parser
                .clone()
                .map_with_span(|(stmts, blk_span), span| {
                    Self::stmts_to_block(stmts, blk_span).with_span(span)
                });

        // Transaction expression
        let txn_expr = Self::transaction_expr(interner, stmt, expr.clone());

        // IF expression
        let if_expr = just(Token::If)
            .ignore_then(expr.clone())
            .then(block_parser.clone())
            .then(
                just(Token::Else)
                    .ignore_then(Self::opt_newlines())
                    .ignore_then(block_parser)
                    .or_not(),
            )
            .map_with_span(
                |((cond, (then_stmts, then_span)), else_block), span| {
                    let then_expr = Self::stmts_to_block(then_stmts, then_span);
                    let else_expr = else_block.map(|(stmts, blk_span)| {
                        Self::stmts_to_block(stmts, blk_span)
                    });
                    cst::Expr::new(
                        cst::ExprKind::If(
                            Box::new(cond),
                            Box::new(then_expr),
                            else_expr.map(Box::new),
                        ),
                        span,
                    )
                },
            );

        // Closure: single param `x => expr`
        let closure_single = Self::ident()
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::FatArrow))
            .then_ignore(Self::opt_newlines())
            .then(expr.clone())
            .map_with_span(|(name, body), span| {
                let params = smallvec::smallvec![(name, None)];
                cst::Expr::new(
                    cst::ExprKind::Closure {
                        type_params: vec![],
                        params,
                        ret: None,
                        body: Box::new(body),
                    },
                    span,
                )
            });

        // Closure param: `name` or `name: Type`
        let closure_param = Self::ident()
            .then(
                just(Token::Colon)
                    .ignore_then(Self::opt_newlines())
                    .ignore_then(ty.clone())
                    .or_not(),
            )
            .map(|(name, ty)| (name, ty));

        // Multi-param closure: `(params) => expr`, `(params) -> Type => expr`,
        // or with type params: `[T](params) -> Type => expr`
        let param_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let params_or_empty =
            just(Token::RParen).to(Vec::new()).or(closure_param
                .separated_by(param_sep)
                .allow_trailing()
                .then_ignore(Self::opt_newlines())
                .then_ignore(just(Token::RParen)));
        let closure_multi = ty_params
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::LParen))
            .then_ignore(Self::opt_newlines())
            .then(params_or_empty)
            .then_ignore(Self::opt_newlines())
            .then(
                just(Token::Arrow)
                    .ignore_then(Self::opt_newlines())
                    .ignore_then(ty)
                    .or_not(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::FatArrow))
            .then_ignore(Self::opt_newlines())
            .then(expr.clone())
            .map_with_span(|(((type_params, params_vec), ret), body), span| {
                let params = SmallVec::from_vec(params_vec);
                cst::Expr::new(
                    cst::ExprKind::Closure {
                        type_params,
                        params,
                        ret,
                        body: Box::new(body),
                    },
                    span,
                )
            });

        // Match expression
        let match_expr = Self::match_expr(interner, expr);

        // Order matters: ref literals before var (IdentBrace is distinct from
        // Ident so they won't conflict). Closures before var since both can
        // start with ident but closure needs `=>`. Pipe placeholder before
        // postfix operations (field access uses `.` too). Class method before
        // var since both start with ident but class method has `:` after.
        choice((
            literal,
            interpolation,
            regex_lit,
            closure_single,
            closure_multi,
            ref_local,
            ref_global,
            pipe_placeholder,
            class_method,
            class_method_ref,
            naked_class_method,
            naked_class_method_ref,
            naked_variant_call,
            naked_variant_ref,
            var,
            paren,
            array,
            object_or_map,
            block_expr,
            txn_expr,
            if_expr,
            match_expr,
        ))
    }

    fn match_expr(
        interner: &mut StringInterner,
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let arms = just(Token::LBrace)
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                Self::match_arm(interner, expr.clone())
                    .separated_by(Self::item_sep())
                    .allow_leading()
                    .allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBrace));

        just(Token::Match)
            .ignore_then(Self::opt_newlines())
            .ignore_then(expr)
            .then_ignore(Self::opt_newlines())
            .then(arms)
            .map_with_span(|(scrutinee, arms), span| {
                cst::Expr::new(
                    cst::ExprKind::Match(Box::new(scrutinee), arms),
                    span,
                )
            })
    }
}

/// Helper enum for parenthesized expressions vs tuples.
#[derive(Clone)]
pub(super) enum ParenContents {
    Empty,
    Elements(Vec<cst::Expr>, bool), // (elements, has_trailing_comma)
}
