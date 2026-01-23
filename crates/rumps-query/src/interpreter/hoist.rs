//! Declaration hoisting for the interpreter.
//!
//! Implements Pass 1 of function/module hoisting: pre-register all function
//! and module definitions before executing any statements. This enables
//! forward references and mutual recursion at runtime.
//!
//! Also hoists `CLASS` instance declarations, registering the generated methods
//! as functions and populating `user_instances` for runtime dispatch.

use std::collections::HashMap;

use async_recursion::async_recursion;

use crate::ast::{InstanceMethodDef, Stmt, StmtId};
use crate::interpreter::instance::RuntimeInstance;
use crate::interpreter::Interpreter;
use crate::io::IoContext;
use crate::{Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Pass 1: Hoist all function and module declarations.
    ///
    /// Pre-registers functions and modules so they can be referenced before
    /// their lexical definition point. This enables forward references like:
    /// ```text
    /// id(10)
    /// FUN id[T](x: T) -> T { x }
    /// ```
    pub(crate) async fn hoist_declarations(
        &mut self,
        stmts: &[StmtId],
    ) -> Result<()> {
        self.hoist_stmts(stmts).await
    }

    /// Hoist a sequence of statements (recursive helper).
    #[async_recursion]
    async fn hoist_stmts(&mut self, stmts: &[StmtId]) -> Result<()> {
        match stmts.split_first() {
            None => Ok(()),
            Some((&head, tail)) => {
                self.hoist_stmt(head).await?;
                self.hoist_stmts(tail).await
            }
        }
    }

    /// Hoist a single statement's declarations.
    async fn hoist_stmt(&mut self, id: StmtId) -> Result<()> {
        let span = self.ast.stmt_span(id).unwrap_or_default();
        let stmt = self.ast.get_stmt(id).cloned();

        match stmt {
            Some(Stmt::Fun {
                name,
                params,
                ret,
                body,
                ..
            }) => self.hoist_fun(&name, &params, ret, body, span),

            Some(Stmt::Module { name, body }) => {
                self.hoist_module(&name, &body, span).await
            }

            Some(Stmt::ClassInstance {
                for_type, methods, ..
            }) => self.hoist_class_instance(id, for_type, &methods, span),

            // Other statements don't introduce hoistable bindings
            _ => Ok(()),
        }
    }

    /// Hoist a function declaration.
    ///
    /// Registers the function in the `functions` map so it can be called
    /// before its definition is executed.
    fn hoist_fun(
        &mut self,
        name: &str,
        params: &[(String, Option<crate::ast::AstTypeExprId>)],
        ret: Option<crate::ast::AstTypeExprId>,
        body: crate::ast::ExprId,
        span: Span,
    ) -> Result<()> {
        // Delegate to the existing `fun` method which handles registration
        self.fun(name, params, ret, body, span)
    }

    /// Hoist a module declaration and its members.
    async fn hoist_module(
        &mut self,
        name: &str,
        body: &[StmtId],
        span: Span,
    ) -> Result<()> {
        // Delegate to the existing `user_module` method
        self.user_module(name, body, span).await
    }

    /// Hoist a class instance declaration.
    ///
    /// Registers the instance methods as functions with generated internal names
    /// and populates `user_instances` for runtime dispatch.
    pub(crate) fn hoist_class_instance(
        &mut self,
        id: StmtId,
        _for_type: crate::ast::AstTypeExprId,
        methods: &[InstanceMethodDef],
        span: Span,
    ) -> Result<()> {
        // Extract data from resolved instance (clone to release borrow).
        // If resolution didn't produce info (e.g., invalid class name),
        // skip; typechecking will report the error.
        match self.resolved_instances.get(&id) {
            None => Ok(()),
            Some(r) => {
                let class = r.class;
                let type_name = r.type_name.clone();
                let mappings = r.methods.clone();

                // Look up the TypeId for the implementing type.
                // If type doesn't exist, skip; typechecking will report.
                let type_name_id = self.arena.intern(&type_name);
                match self.registry.lookup(type_name_id) {
                    None => Ok(()),
                    Some(type_id) => {
                        // Build method lookup map for O(1) access
                        let method_map: HashMap<&str, &InstanceMethodDef> =
                            methods
                                .iter()
                                .map(|m| (m.name.as_str(), m))
                                .collect();

                        // Register each method as a function
                        let mut runtime_inst = RuntimeInstance::default();

                        let result: Result<()> = mappings.iter().try_for_each(
                            |(method_name, fn_name)| {
                                let method_def = method_map
                                    .get(method_name.as_str())
                                    .unwrap_or_else(|| {
                                        invariant!("resolved method not in AST")
                                    });

                                self.fun(
                                    fn_name,
                                    &method_def.params,
                                    method_def.ret,
                                    method_def.body,
                                    span,
                                )?;

                                let method_id = self.arena.intern(method_name);
                                let fn_id = self.arena.intern(fn_name);
                                runtime_inst.methods.insert(method_id, fn_id);

                                Ok(())
                            },
                        );

                        self.user_instances.register(
                            class,
                            type_id,
                            runtime_inst,
                        );

                        result
                    }
                }
            }
        }
    }
}
