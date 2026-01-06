//! Declaration hoisting for the interpreter.
//!
//! Implements Pass 1 of function/module hoisting: pre-register all function
//! and module definitions before executing any statements. This enables
//! forward references and mutual recursion at runtime.

use async_recursion::async_recursion;

use crate::ast::{Stmt, StmtId};
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
}
