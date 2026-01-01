//! Transaction block evaluation.

use super::Interpreter;
use crate::ast::{ExprId, StmtId, TransactionExpr};
use crate::io::IoContext;
use crate::value::Value;
use crate::{Error, Result, Span};

/// Wrapper to distinguish body errors from commit errors.
enum TxnError {
    Body(Error),
    Commit(rumps_storage::StorageError),
}

impl From<rumps_storage::StorageError> for TxnError {
    fn from(e: rumps_storage::StorageError) -> Self {
        Self::Commit(e)
    }
}

impl<I: IoContext> Interpreter<'_, I> {
    /// Evaluate a transaction block expression.
    ///
    /// Returns `Result[T, String]` where `T` is the trailing expression type.
    /// Nested transactions are a runtime error that propagates up to rollback
    /// the outer transaction.
    pub(super) async fn transaction(
        &mut self,
        txn: &TransactionExpr,
        span: Span,
    ) -> Result<Value> {
        // Nested transactions are an error (causes outer transaction to rollback)
        if self.txn.is_some() {
            Err(Error::runtime(
                span,
                "nested transactions are not supported",
            ))
        } else {
            // Build transaction with modifiers
            let mut builder = self.db.build_transaction();
            let mut timeout_ms: Option<u64> = None;

            if let Some(conflict) = txn.modifiers.conflict {
                builder = builder.conflict(conflict);
            }
            if let Some(timeout_id) = txn.modifiers.timeout {
                let timeout_val = self.eval(timeout_id).await?;
                let ms = match timeout_val {
                    Value::Int(n) => n as u64,
                    _ => typechecked!("timeout", "Int"),
                };
                timeout_ms = Some(ms);
                builder = builder.timeout(ms);
            }
            if let Some(priority) = txn.modifiers.priority {
                builder = builder.priority(priority);
            }
            if let Some(isolation) = txn.modifiers.isolation {
                builder = builder.isolation(isolation);
            }

            // Execute transaction (with timeout wrapper if specified)
            let result = self
                .execute_txn_body(
                    builder, &txn.stmts, txn.expr, timeout_ms, span,
                )
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
        ms: Option<u64>,
        span: Span,
    ) -> Result<Value> {
        // Start the transaction
        let txn = builder.start().await.map_err(|e| {
            Error::runtime(span, format!("failed to start transaction: {e}"))
        })?;

        // Set transaction context
        self.txn = Some(txn.clone());

        // Enter new scope for local bindings
        self.env.scopes.push();

        // Execute body (with timeout if specified); timeout -> Err
        let body_result =
            txn.timed(ms, self.execute_txn_stmts(stmts, expr)).await;

        // Pop scope
        self.env.scopes.pop();

        // Finish transaction: commit on Ok, rollback on Err
        self.txn
            .take()
            .ok_or_else(|| {
                Error::runtime(span, "transaction unexpectedly missing")
            })?
            .finish(body_result.map_err(TxnError::Body))
            .await
            .map_err(|e| match e {
                TxnError::Body(e) => e,
                TxnError::Commit(e) => {
                    Error::runtime(span, format!("commit failed: {e}"))
                }
            })
    }

    /// Execute transaction statements and trailing expression.
    async fn execute_txn_stmts(
        &mut self,
        stmts: &[StmtId],
        expr: Option<ExprId>,
    ) -> Result<Value> {
        self.stmts(stmts).await?;
        match expr {
            None => Ok(Value::Unit),
            Some(id) => self.eval(id).await,
        }
    }
}
