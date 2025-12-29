//! Database primitives: GET, SET, KILL, DATA, and key construction.

use async_recursion::async_recursion;
use rumps_types::{DataStatus, Key, Name, Subscript};
use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{Expr, ExprId};
use crate::io::IoContext;
use crate::value::{TypeId, Value};
use crate::{Error, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// `GET` primitive; reads a value from a B-tree variable.
    ///
    /// The inner expression must be a `Local` or `Global`. Uses the active
    /// transaction if one exists, otherwise reads directly from the database.
    #[async_recursion]
    pub(super) async fn get(
        &mut self,
        inner: ExprId,
        span: Span,
    ) -> Result<Value> {
        let inner_span = self.ast.expr_span(inner).unwrap_or(span);
        let inner_expr = self
            .ast
            .get_expr(inner)
            .ok_or_else(|| Error::runtime(span, "invalid expression id"))?
            .clone();

        let (name, subs) = match inner_expr {
            Expr::Local(n, s) => Ok((Name::local(&n), s)),
            Expr::Global(n, s) => Ok((Name::global(&n), s)),
            _ => Err(Error::runtime(
                inner_span,
                "GET requires a local or global",
            )),
        }?;

        let key = self.build_key(&subs).await?;

        let opt_val = match &self.txn {
            Some(txn) => txn.get(&name, &key).await,
            None => self.db.get(&name, &key).await,
        }
        .map_err(|e| Error::runtime(span, format!("GET failed: {e}")))?;

        Ok(match opt_val {
            None => self.make_none_storable(),
            Some(sv) => {
                let v = self.load(sv);
                let vid = self.arena.add(v, span);
                self.make_some_storable(vid)
            }
        })
    }

    /// `SET` primitive; writes a value to a B-tree variable.
    ///
    /// The target expression must be a `Local` or `Global`. Dispatches based
    /// on the name type: globals require an active transaction, locals can
    /// be set outside transactions.
    #[async_recursion]
    pub(super) async fn set(
        &mut self,
        target: ExprId,
        expr_id: ExprId,
        span: Span,
    ) -> Result<()> {
        let target_span = self.ast.expr_span(target).unwrap_or(span);
        let target_expr = self
            .ast
            .get_expr(target)
            .ok_or_else(|| Error::runtime(span, "invalid expression id"))?
            .clone();

        let (name, subs) = match target_expr {
            Expr::Local(n, s) => Ok((Name::local(&n), s)),
            Expr::Global(n, s) => Ok((Name::global(&n), s)),
            _ => Err(Error::runtime(
                target_span,
                "SET requires a local or global",
            )),
        }?;

        let key = self.build_key(&subs).await?;
        let val = self.eval(expr_id).await?;
        let storage_val = self.store(&val)?;

        if name.is_global() {
            match self.txn.as_ref() {
                Some(txn) => {
                    txn.set(&name, &key, storage_val).await.map_err(|e| {
                        Error::runtime(span, format!("SET failed: {e}"))
                    })
                }
                None => Err(Error::runtime(
                    span,
                    "global SET requires a transaction",
                )),
            }
        } else {
            self.db
                .set(&name, &key, storage_val)
                .await
                .map_err(|e| Error::runtime(span, format!("SET failed: {e}")))
        }
    }

    /// `KILL` primitive; deletes a variable and its descendants.
    ///
    /// The target expression must be a `Local` or `Global`. For globals,
    /// requires an active transaction. For locals, operates directly on
    /// the database.
    #[async_recursion]
    pub(super) async fn kill(
        &mut self,
        target: ExprId,
        span: Span,
    ) -> Result<()> {
        let target_span = self.ast.expr_span(target).unwrap_or(span);
        let target_expr = self
            .ast
            .get_expr(target)
            .ok_or_else(|| Error::runtime(span, "invalid expression id"))?
            .clone();

        let (name, subs) = match target_expr {
            Expr::Local(n, s) => Ok((Name::local(&n), s)),
            Expr::Global(n, s) => Ok((Name::global(&n), s)),
            _ => Err(Error::runtime(
                target_span,
                "KILL requires a local or global",
            )),
        }?;

        let key = self.build_key(&subs).await?;

        if name.is_global() {
            match self.txn.as_ref() {
                Some(txn) => txn.kill(&name, &key).await.map_err(|e| {
                    Error::runtime(span, format!("KILL failed: {e}"))
                }),
                None => Err(Error::runtime(
                    span,
                    "global KILL requires a transaction",
                )),
            }
        } else {
            self.db
                .kill(&name, &key)
                .await
                .map_err(|e| Error::runtime(span, format!("KILL failed: {e}")))
        }
    }

    /// `DATA` primitive; queries existence status of a B-tree node.
    ///
    /// The inner expression must be a `Local` or `Global`. Uses the active
    /// transaction if one exists, otherwise reads directly from the database.
    /// Returns a `DataStatus` enum value (tagged variant).
    #[async_recursion]
    pub(super) async fn data(
        &mut self,
        inner: ExprId,
        span: Span,
    ) -> Result<Value> {
        let inner_span = self.ast.expr_span(inner).unwrap_or(span);
        let inner_expr = self
            .ast
            .get_expr(inner)
            .ok_or_else(|| Error::runtime(span, "invalid expression id"))?
            .clone();

        let (name, subs) = match inner_expr {
            Expr::Local(n, s) => Ok((Name::local(&n), s)),
            Expr::Global(n, s) => Ok((Name::global(&n), s)),
            _ => Err(Error::runtime(
                inner_span,
                "DATA requires a local or global",
            )),
        }?;

        let key = self.build_key(&subs).await?;

        let status = match &self.txn {
            Some(txn) => txn.data(&name, &key).await,
            None => self.db.data(&name, &key).await,
        }
        .map_err(|e| Error::runtime(span, format!("DATA failed: {e}")))?;

        // Convert DataStatus to Tagged variant
        let type_expr_id = self.type_exprs.named(TypeId::DATA_STATUS);
        let variant_idx = match status {
            DataStatus::NoData => 0,
            DataStatus::HasValue => 1,
            DataStatus::HasDescendants => 2,
            DataStatus::Both => 3,
        };
        Ok(Value::Tagged(type_expr_id, variant_idx, SmallVec::new()))
    }

    /// Evaluate subscript expressions and build a `Key`.
    #[async_recursion]
    pub(super) async fn build_key(&mut self, subs: &[ExprId]) -> Result<Key> {
        self.build_key_acc(subs, Vec::with_capacity(subs.len()))
            .await
    }

    /// Recursive helper for building a key from subscript expressions.
    #[async_recursion]
    async fn build_key_acc(
        &mut self,
        subs: &[ExprId],
        mut acc: Vec<Subscript>,
    ) -> Result<Key> {
        match subs.split_first() {
            None => Ok(Key::from(acc)),
            Some((head, tail)) => {
                let val = self.eval(*head).await?;
                let sub = self.subscript(&val)?;
                acc.push(sub);
                self.build_key_acc(tail, acc).await
            }
        }
    }
}
