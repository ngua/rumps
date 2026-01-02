//! Transaction block evaluation.

use super::Interpreter;
use crate::ast::{ExprId, StmtId, TransactionExpr, TxnId};
use crate::io::IoContext;
use crate::value::Value;
use crate::{Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Evaluate a transaction block expression.
    ///
    /// Returns `Result[T, String]` where `T` is the trailing expression type.
    ///
    /// Nested transactions are rejected at compile time by the type checker;
    /// see `InferCtx::transaction` in `typecheck/infer/expr.rs`.
    pub(super) async fn transaction(
        &mut self,
        txn_expr: &TransactionExpr,
        span: Span,
    ) -> Result<Value> {
        // Get the unique ID assigned during typecheck
        let id = txn_expr
            .id
            .unwrap_or_else(|| typechecked!("transaction", "TxnId assigned"));

        // Build transaction with modifiers
        let mut builder = self.db.build_transaction();
        let mut timeout_ms: Option<u64> = None;

        if let Some(conflict) = txn_expr.modifiers.conflict {
            builder = builder.conflict(conflict);
        }
        if let Some(timeout_id) = txn_expr.modifiers.timeout {
            let timeout_val = self.eval(timeout_id).await?;
            let ms = match timeout_val {
                Value::Int(n) => n as u64,
                _ => typechecked!("timeout", "Int"),
            };
            timeout_ms = Some(ms);
            builder = builder.timeout(ms);
        }
        if let Some(retries) = txn_expr.modifiers.retries {
            builder = builder.retries(retries);
        }
        if let Some(isolation) = txn_expr.modifiers.isolation {
            builder = builder.isolation(isolation);
        }

        // Execute transaction (with timeout wrapper if specified)
        // Returns Result[T, String] directly
        self.execute_txn_body(
            id,
            builder,
            &txn_expr.stmts,
            txn_expr.expr,
            timeout_ms,
            span,
        )
        .await
    }

    /// Execute transaction body. Returns `Result[T, String]` directly.
    async fn execute_txn_body(
        &mut self,
        id: TxnId,
        builder: rumps_storage::TransactionBuilder,
        stmts: &[StmtId],
        expr: Option<ExprId>,
        ms: Option<u64>,
        span: Span,
    ) -> Result<Value> {
        // Start the transaction
        let txn = match builder.start().await {
            Ok(t) => t,
            Err(e) => {
                let msg = format!("failed to start transaction: {e}");
                return Ok(self.make_result_err(&msg, span));
            }
        };

        // Get retry count before cloning
        let retries = txn.retry_count();

        // Insert into the transaction map
        self.txns.insert(id, txn.clone());

        // Enter new scope for local bindings
        self.env.scopes.push();

        // Execute body (with timeout if specified); timeout -> Err
        let body_result =
            txn.timed(ms, self.execute_txn_stmts(stmts, expr)).await;

        // Pop scope
        self.env.scopes.pop();

        // Remove from the transaction map and finish
        let txn = self
            .txns
            .remove(&id)
            .unwrap_or_else(|| typechecked!("transaction", "TxnId in map"));

        // Commit (or rollback on body error)
        match txn.finish_with_retry(body_result, retries).await {
            Ok(val) => Ok(self.make_result_ok(val, span)),
            Err(e) => Ok(self.make_result_err(&e.to_string(), span)),
        }
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
