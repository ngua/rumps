//! Transaction block evaluation.

use super::Interpreter;
use crate::ast::{ExprId, StmtId, TransactionExpr};
use crate::io::IoContext;
use crate::value::Value;
use crate::{Error, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Evaluate a transaction block expression.
    ///
    /// Returns `Result[T, String]` where `T` is the trailing expression type.
    /// Nested transactions return an error wrapped in `Result.Err`.
    pub(super) async fn transaction(
        &mut self,
        txn: &TransactionExpr,
        span: Span,
    ) -> Result<Value> {
        // Check for nested transaction
        if self.txn.is_some() {
            let msg = "nested transactions are not supported";
            Ok(self.make_result_err(msg, span))
        } else {
            // Build transaction with modifiers
            let mut builder = self.db.build_transaction();

            if let Some(conflict) = txn.modifiers.conflict {
                builder = builder.conflict(conflict);
            }
            if let Some(timeout_id) = txn.modifiers.timeout {
                let timeout_val = self.eval(timeout_id).await?;
                let timeout_ms = match timeout_val {
                    Value::Int(n) => n,
                    _ => typechecked!("timeout", "Int"),
                };
                builder = builder.timeout(timeout_ms as u64);
            }
            if let Some(priority) = txn.modifiers.priority {
                builder = builder.priority(priority);
            }
            if let Some(isolation) = txn.modifiers.isolation {
                builder = builder.isolation(isolation);
            }

            // Execute transaction
            let result = self
                .execute_txn_body(builder, &txn.stmts, txn.expr, span)
                .await;

            // Convert to Result[T, String]
            match result {
                Ok(val) => Ok(self.make_result_ok(val, span)),
                Err(e) => Ok(self.make_result_err(&e.to_string(), span)),
            }
        }
    }

    /// Execute transaction body and return the result value or error.
    async fn execute_txn_body(
        &mut self,
        builder: rumps_storage::TransactionBuilder,
        stmts: &[StmtId],
        expr: Option<ExprId>,
        span: Span,
    ) -> Result<Value> {
        // Start the transaction
        let txn = builder.start().await.map_err(|e| {
            Error::runtime(span, format!("failed to start transaction: {e}"))
        })?;

        // Set transaction context
        self.txn = Some(txn);

        // Enter new scope for local bindings
        self.env.scopes.push();

        // Execute statements
        let stmt_result = self.stmts(stmts).await;

        // Evaluate trailing expression if statements succeeded
        let body_result = match stmt_result {
            Err(e) => Err(e),
            Ok(()) => match expr {
                None => Ok(Value::Unit),
                Some(expr_id) => self.eval(expr_id).await,
            },
        };

        // Pop scope
        self.env.scopes.pop();

        // Take transaction (should always exist at this point)
        match self.txn.take() {
            Some(txn) => match body_result {
                Ok(val) => {
                    txn.commit().await.map_err(|e| {
                        Error::runtime(span, format!("commit failed: {e}"))
                    })?;
                    Ok(val)
                }
                Err(e) => {
                    // Rollback on error; ignore rollback errors
                    let _ = txn.rollback().await;
                    Err(e)
                }
            },
            None => {
                Err(Error::runtime(span, "transaction unexpectedly missing"))
            }
        }
    }
}
