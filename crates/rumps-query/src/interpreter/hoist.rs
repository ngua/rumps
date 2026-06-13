//! Declaration hoisting for the interpreter.
//!
//! Implements Pass 1 of function/module hoisting: pre-register all function
//! and module definitions before executing any statements. This enables
//! forward references and mutual recursion at runtime.
//!
//! Also hoists `class` instance declarations, registering the generated methods
//! as functions and populating `user_instances` for runtime dispatch.

use std::collections::HashMap;

use async_recursion::async_recursion;

use crate::ast::{AstClassMethod, InstanceMethodDef, Stmt, StmtId};
use crate::intern::StringId;
use crate::interpreter::instance::RuntimeInstance;
use crate::interpreter::Interpreter;
use crate::io::IoContext;
use crate::resolve::ResolvedMethod;
use crate::{ClassId, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Pass 1: Hoist all function and module declarations.
    ///
    /// Pre-registers functions and modules so they can be referenced before
    /// their lexical definition point. This enables forward references like:
    /// ```text
    /// id(10)
    /// fun id[T](x: T) -> T { x }
    /// ```
    pub(crate) async fn hoist_declarations(
        &mut self,
        stmts: &[StmtId],
    ) -> Result<()> {
        self.hoist_stmts(stmts).await
    }

    /// Hoist a sequence of statements (recursive helper).
    async fn hoist_stmts(&mut self, stmts: &[StmtId]) -> Result<()> {
        self.hoist_stmts_pre(stmts).await?;
        self.hoist_stmts_instances(stmts).await
    }

    #[async_recursion]
    async fn hoist_stmts_pre(&mut self, stmts: &[StmtId]) -> Result<()> {
        match stmts.split_first() {
            None => Ok(()),
            Some((&head, tail)) => {
                self.hoist_stmt_pre(head).await?;
                self.hoist_stmts_pre(tail).await
            }
        }
    }

    #[async_recursion]
    async fn hoist_stmts_instances(&mut self, stmts: &[StmtId]) -> Result<()> {
        match stmts.split_first() {
            None => Ok(()),
            Some((&head, tail)) => {
                self.hoist_stmt_instance(head)?;
                self.hoist_stmts_instances(tail).await
            }
        }
    }

    async fn hoist_stmt_pre(&mut self, id: StmtId) -> Result<()> {
        let span = self.ast.stmt_span(id).unwrap_or_default();
        let stmt = self.ast.get_stmt(id).cloned();

        match stmt {
            Some(Stmt::Fun {
                name, params, body, ..
            }) => self.fun(
                name,
                params.iter().map(|(name, _)| *name).collect(),
                body,
            ),

            Some(Stmt::Module { name, body }) => {
                self.hoist_module(name, &body, span).await
            }

            Some(Stmt::ClassDef { name, methods, .. }) => self
                .checked
                .class_registry
                .lookup_by_name(name)
                .into_iter()
                .try_for_each(|class| {
                    self.hoist_class_defaults(class, &methods)
                }),

            _ => Ok(()),
        }
    }

    fn hoist_stmt_instance(&mut self, id: StmtId) -> Result<()> {
        let stmt = self.ast.get_stmt(id).cloned();

        match stmt {
            Some(Stmt::ClassInstance { methods, .. }) => {
                self.hoist_class_instance(id, &methods)
            }

            _ => Ok(()),
        }
    }

    /// Hoist a module declaration and its members.
    async fn hoist_module(
        &mut self,
        name: StringId,
        body: &[StmtId],
        span: Span,
    ) -> Result<()> {
        // Delegate to the existing `user_module` method
        self.user_module(name, body, span).await
    }

    pub(crate) fn hoist_class_defaults(
        &mut self,
        class: ClassId,
        methods: &[AstClassMethod],
    ) -> Result<()> {
        methods.iter().try_for_each(|m| {
            m.default.as_ref().map_or(Ok(()), |default| {
                self.hoist_class_default_method(class, default)
            })
        })
    }

    fn hoist_class_default_method(
        &mut self,
        class: ClassId,
        default: &InstanceMethodDef,
    ) -> Result<()> {
        let cn_id = self.checked.class_registry.name(class);
        let cn = self.arena.strings.resolve(cn_id).to_owned();
        let mn = self.arena.strings.resolve(default.name).to_owned();
        let fn_name = RuntimeInstance::default_fn_name(&cn, &mn);
        let fn_id = self.arena.strings.intern(&fn_name);
        self.fun(
            fn_id,
            default.params.iter().map(|(name, _)| *name).collect(),
            default.body,
        )
    }

    /// Hoist a class instance declaration.
    ///
    /// Registers the instance methods as functions with generated internal names
    /// and populates `user_instances` for runtime dispatch.
    pub(crate) fn hoist_class_instance(
        &mut self,
        id: StmtId,
        methods: &[InstanceMethodDef],
    ) -> Result<()> {
        // Extract data from resolved instance (clone to release borrow).
        // If resolution didn't produce info (e.g., invalid class name),
        // skip; typechecking will report the error.
        match self.resolved_instances.get(&id) {
            None => Ok(()),
            Some(r) => {
                let class = r.class;
                let type_qn = r.type_name.clone();
                let tuple_arity = r.tuple_arity;
                let mappings = r.methods.clone();

                // Look up the TypeId for the implementing type.
                // If type doesn't exist, skip; typechecking will report.
                match self.registry.lookup(&type_qn) {
                    None => Ok(()),
                    Some(type_id) => {
                        // Build method lookup map for O(1) access
                        let method_map: HashMap<StringId, &InstanceMethodDef> =
                            methods.iter().map(|m| (m.name, m)).collect();

                        // Register each method as a function
                        let mut runtime_inst = RuntimeInstance::default();

                        let result: Result<()> =
                            mappings.iter().try_for_each(|m| match m {
                                ResolvedMethod::Instance {
                                    method: mid,
                                    fun: fn_id,
                                } => {
                                    let method_def = method_map
                                        .get(mid)
                                        .unwrap_or_else(|| {
                                            invariant!(
                                                "resolved method not in AST"
                                            )
                                        });

                                    self.fun(
                                        *fn_id,
                                        method_def
                                            .params
                                            .iter()
                                            .map(|(name, _)| *name)
                                            .collect(),
                                        method_def.body,
                                    )?;

                                    runtime_inst.methods.insert(*mid, *fn_id);

                                    Ok(())
                                }
                                ResolvedMethod::Default {
                                    method: mid,
                                    fun: fn_id,
                                } => {
                                    runtime_inst.methods.insert(*mid, *fn_id);
                                    Ok(())
                                }
                            });

                        self.user_instances.register_with_tuple_arity(
                            class,
                            type_id,
                            tuple_arity,
                            runtime_inst,
                        );

                        result
                    }
                }
            }
        }
    }
}
