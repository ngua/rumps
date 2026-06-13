//! Statement parsing for RUMPS.

use chumsky::prelude::{choice, just, recursive, select};
use chumsky::Parser as _;
use smallvec::SmallVec;

use super::{ParseErr, Parser};
use crate::intern::StringInterner;
use crate::parser::cst;
use crate::{Span, Token};

impl Parser {
    pub(super) fn stmt(
        interner: &mut StringInterner,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        let import_stmt = Self::import();
        let type_stmt = Self::type_stmt(interner);
        let newtype_stmt = Self::newtype_stmt(interner);
        let union_stmt = Self::union_stmt(interner);
        let pragma_stmt = Self::pragma_stmt(interner);

        recursive(move |stmt| {
            let let_stmt = Self::let_stmt(interner, stmt.clone());
            let output_stmt = Self::output_stmt(interner, stmt.clone());
            let fun_stmt = Self::fun_stmt(interner, stmt.clone());
            let class_stmt = Self::class_stmt(interner, stmt.clone());
            let module_stmt = Self::module_stmt(stmt.clone());
            let expr_stmt = Self::expr_stmt(interner, stmt);

            choice((
                import_stmt.clone(),
                let_stmt,
                output_stmt,
                fun_stmt,
                type_stmt.clone(),
                newtype_stmt.clone(),
                union_stmt.clone(),
                class_stmt,
                module_stmt,
                pragma_stmt.clone(),
                expr_stmt,
            ))
        })
    }

    fn let_stmt(
        interner: &mut StringInterner,
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        let type_ann = just(Token::Colon)
            .ignore_then(Self::type_expr(interner))
            .or_not();

        let pat = Self::binding_pattern(interner);

        // Optional `+` visibility prefix
        let vis = just(Token::Plus)
            .to(cst::Visibility::Public)
            .or_not()
            .map(|v| v.unwrap_or_default());

        vis.then_ignore(just(Token::Let))
            .then(pat)
            .then(type_ann)
            .then_ignore(just(Token::Assign))
            .then_ignore(Self::opt_newlines())
            .then(Self::expr(interner, stmt))
            .map_with_span(|(((vis, pat), ty_ann), val), span| {
                cst::Stmt::new(cst::StmtKind::Let(pat, ty_ann, val, vis), span)
            })
    }

    fn output_stmt(
        interner: &mut StringInterner,
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
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
            .ignore_then(Self::expr(interner, stmt.clone()))
            .map(|e| cst::OutputTarget::File(Box::new(e)));

        let target =
            to_error.or(to_file).or_not().map(|t| t.unwrap_or_default());

        just(Token::Write)
            .ignore_then(Self::expr(interner, stmt))
            .then(format)
            .then(target)
            .map_with_span(|((expr, format), target), span| {
                let output = cst::WriteStmt {
                    expr,
                    format,
                    target,
                };
                cst::Stmt::new(cst::StmtKind::Write(output), span)
            })
    }

    /// `fun name (params) { body }` or `fun name[T](params) -> Type { body }`
    fn fun_stmt(
        interner: &mut StringInterner,
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        // Parameter: `name` or `name: Type`
        let param = Self::ident()
            .then(
                just(Token::Colon)
                    .ignore_then(Self::opt_newlines())
                    .ignore_then(Self::type_expr(interner))
                    .or_not(),
            )
            .map(|(name, ty)| (name, ty));

        let param_sep = just(Token::Comma).then_ignore(Self::opt_newlines());

        // Parameter list: `(params...)`
        let params = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(param.separated_by(param_sep).allow_trailing())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen));

        // Optional return type: `-> Type`
        let ret_ty = Self::opt_newlines()
            .ignore_then(just(Token::Arrow))
            .ignore_then(Self::opt_newlines())
            .ignore_then(Self::type_expr(interner))
            .or_not();

        // Body block
        let body = Self::block(stmt);

        // Optional `+` visibility prefix
        let vis = just(Token::Plus)
            .to(cst::Visibility::Public)
            .or_not()
            .map(|v| v.unwrap_or_default());

        vis.then_ignore(just(Token::Fun))
            .then_ignore(Self::opt_newlines())
            .then(Self::ident())
            .then_ignore(Self::opt_newlines())
            .then(Self::type_params(interner))
            .then_ignore(Self::opt_newlines())
            .then(params)
            .then(ret_ty)
            .then(body)
            .map_with_span(
                |(
                    ((((vis, name), type_params), params_vec), ret),
                    (stmts, blk_span),
                ),
                 span| {
                    let params = SmallVec::from_vec(params_vec);
                    let body = Self::stmts_to_block(stmts, blk_span);
                    cst::Stmt::new(
                        cst::StmtKind::Fun {
                            name,
                            type_params,
                            params,
                            ret,
                            body,
                            vis,
                        },
                        span,
                    )
                },
            )
    }

    fn type_stmt(
        interner: &mut StringInterner,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> + Clone {
        // Variant: `Name` or `Name(Type, Type, ...)`
        let payload_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let payloads = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                Self::type_expr(interner)
                    .separated_by(payload_sep)
                    .allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen))
            .or_not()
            .map(|ps| ps.unwrap_or_default());

        let variant = Self::ident()
            .then(payloads)
            .map(|(name, payloads)| cst::VariantCst { name, payloads });

        // Variants separated by `|`, allowing newlines
        let variant_sep = Self::opt_newlines()
            .ignore_then(just(Token::SinglePipe))
            .then_ignore(Self::opt_newlines());

        let sum_def = variant
            .separated_by(variant_sep)
            .at_least(1)
            .allow_leading() // Allow leading `|` for multi-line formatting
            .map(cst::TypeDefCst::Sum);

        // Optional `+` visibility prefix
        let vis = just(Token::Plus)
            .to(cst::Visibility::Public)
            .or_not()
            .map(|v| v.unwrap_or_default());

        vis.then_ignore(just(Token::Variant))
            .then_ignore(Self::opt_newlines())
            .then(Self::ident())
            .then(Self::type_params(interner))
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::Assign))
            .then_ignore(Self::opt_newlines())
            .then(sum_def)
            .map_with_span(|(((vis, name), type_params), def), span| {
                cst::Stmt::new(
                    cst::StmtKind::Type {
                        name,
                        type_params,
                        def,
                        vis,
                    },
                    span,
                )
            })
    }

    fn newtype_stmt(
        interner: &mut StringInterner,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> + Clone {
        // Optional `+` visibility prefix
        let vis = just(Token::Plus)
            .to(cst::Visibility::Public)
            .or_not()
            .map(|v| v.unwrap_or_default());

        vis.then_ignore(just(Token::NewType))
            .then_ignore(Self::opt_newlines())
            .then(Self::ident())
            .then(Self::type_params(interner))
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::Assign))
            .then_ignore(Self::opt_newlines())
            .then(
                just(Token::Plus)
                    .to(cst::Visibility::Public)
                    .or_not()
                    .map(|v| v.unwrap_or_default()),
            )
            .then_ignore(Self::opt_newlines())
            .then(Self::type_expr(interner))
            .map_with_span(
                |((((vis, name), type_params), repr_vis), target), span| {
                    cst::Stmt::new(
                        cst::StmtKind::Newtype {
                            name,
                            type_params,
                            target,
                            vis,
                            repr_vis,
                        },
                        span,
                    )
                },
            )
    }

    fn union_stmt(
        interner: &mut StringInterner,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> + Clone {
        // Type members separated by `|`
        let member_sep = Self::opt_newlines()
            .ignore_then(just(Token::SinglePipe))
            .then_ignore(Self::opt_newlines());

        let members = Self::type_expr_atom(interner)
            .separated_by(member_sep)
            .at_least(2)
            .allow_leading();

        // Optional `+` visibility prefix
        let vis = just(Token::Plus)
            .to(cst::Visibility::Public)
            .or_not()
            .map(|v| v.unwrap_or_default());

        vis.then_ignore(just(Token::Union))
            .then_ignore(Self::opt_newlines())
            .then(Self::ident())
            .then(Self::type_params(interner))
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::Assign))
            .then_ignore(Self::opt_newlines())
            .then(members)
            .map_with_span(|(((vis, name), type_params), members), span| {
                cst::Stmt::new(
                    cst::StmtKind::Union {
                        name,
                        type_params,
                        members,
                        vis,
                    },
                    span,
                )
            })
    }

    /// Class definition or instance.
    ///
    /// Definition: `class Name[Params] SelfVar [: Supers] { sigs }`
    /// Instance: `class Name[Args] FOR Type [WHERE constraints] { methods }`
    ///
    /// Disambiguation: after `class Name [brackets]`, `FOR` selects the
    /// instance path; any other ident selects the definition path.
    fn class_stmt(
        interner: &mut StringInterner,
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        let class_def = Self::class_def_stmt(interner, stmt.clone());
        let class_inst = Self::class_instance_stmt(interner, stmt);
        class_def.or(class_inst)
    }

    /// Class definition: `class Name[Params] SelfVar [: Super1 + Super2] { body }`.
    ///
    /// Body items are method signatures (`fun`) and associated type
    /// declarations (`newtype`).
    fn class_def_stmt(
        interner: &mut StringInterner,
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        let for_ = interner.intern("for");
        let default = interner.intern("default");

        // Self var: any ident EXCEPT `for` (which disambiguates instances)
        let self_var = select! { Token::Ident(s) if s != for_ => s };

        // Superclass constraints: `: Super1 + Super2`
        let supers = just(Token::Colon)
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                Self::constraint(interner)
                    .separated_by(
                        Self::opt_newlines()
                            .ignore_then(just(Token::Plus))
                            .then_ignore(Self::opt_newlines()),
                    )
                    .at_least(1),
            )
            .or_not()
            .map(|cs| SmallVec::from_vec(cs.unwrap_or_default()));

        // Method signature param: `name: Type` or `name` (missing annotation)
        let sig_param = Self::ident().then(
            just(Token::Colon)
                .ignore_then(Self::opt_newlines())
                .ignore_then(Self::type_expr(interner))
                .or_not(),
        );
        let sig_param_sep =
            just(Token::Comma).then_ignore(Self::opt_newlines());
        let sig_params = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(sig_param.separated_by(sig_param_sep).allow_trailing())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen));
        let sig_ret = Self::opt_newlines()
            .ignore_then(just(Token::Arrow))
            .ignore_then(Self::opt_newlines())
            .ignore_then(Self::type_expr(interner))
            .or_not();

        // Method signature: `fun name[T, U](params) -> RetType` (no body)
        let method_sig = just(Token::Fun)
            .ignore_then(Self::opt_newlines())
            .ignore_then(Self::ident())
            .then_ignore(Self::opt_newlines())
            .then(Self::type_params(interner))
            .then_ignore(Self::opt_newlines())
            .then(sig_params)
            .then(sig_ret)
            .map_with_span(|(((name, type_params), params_vec), ret), span| {
                let params = SmallVec::from_vec(params_vec);
                cst::ClassMethodSig {
                    name,
                    type_params,
                    params,
                    ret,
                    span,
                }
            });

        // Associated type declaration: `newtype Name` (no `= Type`)
        let assoc_decl = just(Token::NewType)
            .ignore_then(Self::opt_newlines())
            .ignore_then(Self::ident())
            .map_with_span(|name, span| cst::ClassAssocTypeDecl { name, span });

        let default_pragma = just(Token::Hash)
            .ignore_then(just(Token::LParen))
            .ignore_then(Self::opt_newlines())
            .ignore_then(Self::ctx_ident(default))
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen))
            .map_with_span(|_, span| span);

        let method_def = Self::method_def(interner, stmt);

        // Definition body item: associated type, method sig, or default method
        // body.
        #[derive(Clone)]
        #[allow(clippy::large_enum_variant)]
        enum DefItem {
            AssocType(cst::ClassAssocTypeDecl),
            MethodSig(cst::ClassMethodSig),
            DefaultPragma(Span),
            MethodDef(cst::InstanceMethodDef),
        }
        let def_item = choice((
            default_pragma.map(DefItem::DefaultPragma),
            method_def.map(DefItem::MethodDef),
            assoc_decl.map(DefItem::AssocType),
            method_sig.map(DefItem::MethodSig),
        ));

        // Definition body: `{ items }`
        let def_body = just(Token::LBrace)
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                def_item.separated_by(Self::item_sep()).allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBrace))
            .try_map(move |items, _span| {
                let mut assoc_types = SmallVec::new();
                let mut methods: Vec<cst::ClassMethod> = Vec::new();
                let mut defaults = Vec::new();
                let mut default_errors = Vec::new();
                let mut pending = None;

                items.into_iter().try_for_each(|item| match item {
                    DefItem::AssocType(a) => {
                        pending.take().into_iter().for_each(|span| {
                            default_errors.push(
                                cst::ClassDefaultPlacementError::BeforeNonMethod {
                                    span,
                                },
                            );
                        });
                        assoc_types.push(a);
                        Ok(())
                    }
                    DefItem::MethodSig(sig) => {
                        pending.take().into_iter().for_each(|span| {
                            default_errors.push(
                                cst::ClassDefaultPlacementError::BeforeNonMethod {
                                    span,
                                },
                            );
                        });
                        methods.push(cst::ClassMethod {
                            sig,
                            default: None,
                        });
                        Ok(())
                    }
                    DefItem::DefaultPragma(span) => {
                        if pending.is_some() {
                            default_errors.push(
                                cst::ClassDefaultPlacementError::DuplicatePragma {
                                    span,
                                },
                            );
                        } else {
                            pending = Some(span);
                        }
                        Ok(())
                    }
                    DefItem::MethodDef(m) => {
                        if pending.take().is_some() {
                            defaults.push(m);
                        } else {
                            default_errors.push(
                                cst::ClassDefaultPlacementError::MethodWithoutDefault {
                                    span: m.span,
                                },
                            );
                        }
                        Ok(())
                    }
                })?;

                pending.take().into_iter().for_each(|span| {
                    default_errors
                        .push(cst::ClassDefaultPlacementError::Unattached {
                            span,
                        });
                });

                defaults.into_iter().for_each(|d| {
                    let method = methods
                        .iter_mut()
                        .find(|m| m.sig.name == d.name && m.default.is_none());
                    if let Some(m) = method {
                        m.default = Some(d);
                    } else {
                        let sig = cst::ClassMethodSig {
                            name: d.name,
                            type_params: d.type_params.clone(),
                            params: d.params.clone(),
                            ret: d.ret.clone(),
                            span: d.span,
                        };
                        methods.push(cst::ClassMethod {
                            sig,
                            default: Some(d),
                        });
                    }
                });

                Ok((assoc_types, methods, default_errors))
            });

        just(Token::Class)
            .ignore_then(Self::opt_newlines())
            .ignore_then(Self::ident())
            .then_ignore(Self::opt_newlines())
            .then(Self::type_params(interner))
            .then_ignore(Self::opt_newlines())
            .then(self_var)
            .then_ignore(Self::opt_newlines())
            .then(supers)
            .then_ignore(Self::opt_newlines())
            .then(def_body)
            .map_with_span(
                |(
                    (((name, class_params), self_var), supers),
                    (assoc_types, methods, default_errors),
                ),
                 span| {
                    cst::Stmt::new(
                        cst::StmtKind::ClassDef {
                            name,
                            class_params,
                            self_var,
                            supers,
                            assoc_types,
                            methods,
                            default_errors,
                        },
                        span,
                    )
                },
            )
    }

    /// Class instance: `class Name[Args] FOR Type [WHERE constraints] { methods }`.
    fn class_instance_stmt(
        interner: &mut StringInterner,
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        let where_ = interner.intern("where");
        let for_ = interner.intern("for");

        // Optional class type arguments: `[String]` for `Into[String]`
        let class_args = just(Token::LBracket)
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                Self::type_expr(interner)
                    .separated_by(
                        just(Token::Comma).then_ignore(Self::opt_newlines()),
                    )
                    .at_least(1)
                    .allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBracket))
            .or_not()
            .map(|ps| ps.unwrap_or_default());

        // WHERE clause constraint: `name: Class1 + Class2`
        let where_constraint = Self::ident()
            .then_ignore(just(Token::Colon))
            .then_ignore(Self::opt_newlines())
            .then(
                Self::constraint(interner)
                    .separated_by(
                        Self::opt_newlines()
                            .ignore_then(just(Token::Plus))
                            .then_ignore(Self::opt_newlines()),
                    )
                    .at_least(1),
            );

        // Optional WHERE clause: `WHERE A: Display, B: Display`
        let where_clause = Self::ctx_ident(where_)
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                where_constraint
                    .separated_by(
                        just(Token::Comma).then_ignore(Self::opt_newlines()),
                    )
                    .at_least(1)
                    .allow_trailing(),
            )
            .or_not()
            .map(|cs| cs.unwrap_or_default());

        let method = Self::method_def(interner, stmt);

        // Associated type: `newtype Index = Int` or `newtype Index: Ord = Int`
        let assoc_type_constraint = just(Token::Colon)
            .ignore_then(Self::opt_newlines())
            .ignore_then(Self::constraint(interner))
            .or_not();
        let assoc_type = just(Token::NewType)
            .ignore_then(Self::opt_newlines())
            .ignore_then(Self::ident())
            .then(assoc_type_constraint)
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::Assign))
            .then_ignore(Self::opt_newlines())
            .then(Self::type_expr(interner))
            .map_with_span(|((name, constraint), target), span| {
                cst::AssocTypeCst {
                    name,
                    constraint,
                    target,
                    span,
                }
            });

        // Instance body item: either newtype or fun
        #[derive(Clone)]
        #[allow(clippy::large_enum_variant)]
        enum InstanceItem {
            AssocType(cst::AssocTypeCst),
            Method(cst::InstanceMethodDef),
        }
        let instance_item = assoc_type
            .map(InstanceItem::AssocType)
            .or(method.map(InstanceItem::Method));

        // Instance body: `{ newtype ... fun ... }`
        let instance_body = just(Token::LBrace)
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                instance_item
                    .separated_by(Self::item_sep())
                    .allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBrace))
            .map(|items| {
                let mut assoc_types = Vec::new();
                let mut methods = Vec::new();
                items.into_iter().for_each(|item| match item {
                    InstanceItem::AssocType(a) => assoc_types.push(a),
                    InstanceItem::Method(m) => methods.push(m),
                });
                (assoc_types, methods)
            });

        // Full CLASS instance statement
        just(Token::Class)
            .ignore_then(Self::opt_newlines())
            .ignore_then(Self::ident())
            .then(class_args)
            .then_ignore(Self::opt_newlines())
            .then_ignore(Self::ctx_ident(for_))
            .then_ignore(Self::opt_newlines())
            .then(Self::for_type_expr(interner))
            .then_ignore(Self::opt_newlines())
            .then(where_clause)
            .then_ignore(Self::opt_newlines())
            .then(instance_body)
            .map_with_span(
                |(
                    (((class_name, class_args), for_type), constraints),
                    (assoc_types, methods),
                ),
                 span| {
                    cst::Stmt::new(
                        cst::StmtKind::ClassInstance {
                            class_name,
                            class_args,
                            type_params: vec![], // Derived during resolution
                            for_type,
                            constraints,
                            assoc_types,
                            methods,
                        },
                        span,
                    )
                },
            )
    }

    /// User-defined module declaration.
    ///
    /// Two forms are supported:
    /// - Inline: `module Name { ... }`
    fn module_stmt(
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        // Inline body: `{ ... }`
        let inline_body = just(Token::LBrace)
            .ignore_then(Self::opt_newlines())
            .ignore_then(stmt.separated_by(Self::item_sep()).allow_trailing())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBrace))
            .map(cst::ModuleSource::Inline);
        // File import: `FROM "path"`
        let file_import = just(Token::From)
            .ignore_then(Self::opt_newlines())
            .ignore_then(select! { Token::String(s) => s })
            .map(cst::ModuleSource::File);
        just(Token::Module)
            .ignore_then(Self::opt_newlines())
            .ignore_then(Self::ident())
            .then_ignore(Self::opt_newlines())
            .then(inline_body.or(file_import))
            .map_with_span(|(name, source), span| {
                cst::Stmt::new(cst::StmtKind::Module { name, source }, span)
            })
    }

    /// `import Module.{ member, ... }` or `import Module.{ ... }`.
    ///
    /// Imports members from a module into the current scope.
    fn import(
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> + Clone {
        // Module path: idents separated by `.`
        let path = Self::ident().separated_by(just(Token::Dot)).at_least(1);

        // Named with optional alias: `name` or `name as alias`
        let named = Self::ident()
            .then(just(Token::As).ignore_then(Self::ident()).or_not())
            .map(|(name, alias)| cst::ImportItem::Named { name, alias });

        // Wildcard: `...`
        let wildcard = just(Token::DotDotDot).to(cst::ImportItem::Wildcard);

        // Exclusion: `-name`
        let exclude = just(Token::Minus)
            .ignore_then(Self::ident())
            .map(cst::ImportItem::Exclude);

        let item = choice((wildcard, exclude, named));

        let items = item
            .separated_by(just(Token::Comma))
            .allow_trailing()
            .delimited_by(just(Token::LBrace), just(Token::RBrace));

        just(Token::Import)
            .ignore_then(Self::opt_newlines())
            .ignore_then(path)
            .then_ignore(just(Token::Dot))
            .then(items)
            .map_with_span(|(path, items), span| {
                cst::Stmt::new(
                    cst::StmtKind::Import(cst::ImportStmt {
                        path: SmallVec::from_vec(path),
                        items,
                    }),
                    span,
                )
            })
    }

    fn expr_stmt(
        interner: &mut StringInterner,
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        Self::expr(interner, stmt).map_with_span(|expr, span| {
            cst::Stmt::new(cst::StmtKind::Expr(expr), span)
        })
    }

    fn method_def(
        interner: &mut StringInterner,
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::InstanceMethodDef, Error = ParseErr> + Clone
    {
        let method_param = Self::ident().then(
            just(Token::Colon)
                .ignore_then(Self::opt_newlines())
                .ignore_then(Self::type_expr(interner))
                .or_not(),
        );
        let method_param_sep =
            just(Token::Comma).then_ignore(Self::opt_newlines());
        let method_params = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                method_param.separated_by(method_param_sep).allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen));
        let method_ret = Self::opt_newlines()
            .ignore_then(just(Token::Arrow))
            .ignore_then(Self::opt_newlines())
            .ignore_then(Self::type_expr(interner))
            .or_not();
        let method_body = Self::block(stmt);

        just(Token::Fun)
            .ignore_then(Self::opt_newlines())
            .ignore_then(Self::ident())
            .then_ignore(Self::opt_newlines())
            .then(Self::type_params(interner))
            .then_ignore(Self::opt_newlines())
            .then(method_params)
            .then(method_ret)
            .then(method_body)
            .map_with_span(
                |(
                    (((name, type_params), params_vec), ret),
                    (stmts, blk_span),
                ),
                 span| {
                    let params = SmallVec::from_vec(params_vec);
                    let body = Self::stmts_to_block(stmts, blk_span);
                    cst::InstanceMethodDef {
                        name,
                        type_params,
                        params,
                        ret,
                        body,
                        span,
                    }
                },
            )
    }
}
