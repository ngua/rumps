//! Database primitives: GET, SET, KILL, DATA, and key construction.

use async_recursion::async_recursion;
use rumps_types::{DataStatus, Key, Subscript};
use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{DbRef, ExprId, SubscriptElem};
use crate::io::IoContext;
use crate::value::{TypeId, Value};
use crate::{Error, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// `$GET` primitive; reads a value from a B-tree variable.
    ///
    /// Uses the active transaction if one exists, otherwise reads directly
    /// from the database.
    #[async_recursion]
    pub(super) async fn get(
        &mut self,
        dbref: &DbRef,
        span: Span,
    ) -> Result<Value> {
        let (name, subs) = dbref.split();
        let key = self.build_key(subs).await?;

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

    /// `$SET` primitive; writes a value to a B-tree variable.
    ///
    /// Dispatches based on the name type: globals require an active
    /// transaction, locals can be set outside transactions.
    #[async_recursion]
    pub(super) async fn set(
        &mut self,
        dbref: &DbRef,
        expr_id: ExprId,
        span: Span,
    ) -> Result<()> {
        let (name, subs) = dbref.split();
        let key = self.build_key(subs).await?;
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

    /// `$KILL` primitive; deletes a variable and its descendants.
    ///
    /// For globals, requires an active transaction. For locals, operates
    /// directly on the database.
    #[async_recursion]
    pub(super) async fn kill(
        &mut self,
        dbref: &DbRef,
        span: Span,
    ) -> Result<()> {
        let (name, subs) = dbref.split();
        let key = self.build_key(subs).await?;

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

    /// `$DATA` primitive; queries existence status of a B-tree node.
    ///
    /// Uses the active transaction if one exists, otherwise reads directly
    /// from the database. Returns a `DataStatus` enum value (tagged variant).
    #[async_recursion]
    pub(super) async fn data(
        &mut self,
        dbref: &DbRef,
        span: Span,
    ) -> Result<Value> {
        let (name, subs) = dbref.split();
        let key = self.build_key(subs).await?;

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

    /// `$ORDER` primitive; returns the next subscript at a given level.
    ///
    /// Uses the active transaction if one exists, otherwise reads directly
    /// from the database. Returns `Option[Subscript]`.
    #[async_recursion]
    pub(super) async fn order(
        &mut self,
        dbref: &DbRef,
        span: Span,
    ) -> Result<Value> {
        let (name, subs) = dbref.split();
        let key = self.build_key(subs).await?;

        // `ORDER items(1)` means "find next subscript after `1` at root level",
        // so we split the key: prefix = [] (root), after = `Some(1)`.
        let (prefix, after): (Key, Option<Subscript>) =
            match key.as_slice().split_last() {
                None => (Key::new(), None),
                Some((last, init)) => {
                    (Key::from(init.to_vec()), Some(last.clone()))
                }
            };

        let opt_sub = match &self.txn {
            Some(txn) => txn.order(&name, &prefix, after.as_ref()).await,
            None => self.db.order(&name, &prefix, after.as_ref()).await,
        }
        .map_err(|e| Error::runtime(span, format!("ORDER failed: {e}")))?;

        // Convert Option<Subscript> to Option[Subscript] value
        match opt_sub {
            None => Ok(self.make_none()),
            Some(sub) => {
                let val = self.value_from_subscript(sub)?;
                let val_id = self.arena.add(val, span);
                Ok(self.make_some(val_id))
            }
        }
    }

    /// Evaluate subscript elements and build a `Key`.
    ///
    /// Handles both regular subscripts (`Elem`) and spread syntax (`Spread`).
    /// Spreads flatten an `Array[Subscript]` into the key.
    #[async_recursion]
    pub(super) async fn build_key(
        &mut self,
        subs: &[SubscriptElem],
    ) -> Result<Key> {
        self.build_key_acc(subs, Vec::with_capacity(subs.len()))
            .await
    }

    /// Recursive helper for building a key from subscript elements.
    #[async_recursion]
    async fn build_key_acc(
        &mut self,
        subs: &[SubscriptElem],
        mut acc: Vec<Subscript>,
    ) -> Result<Key> {
        match subs.split_first() {
            None => Ok(Key::from(acc)),
            Some((head, tail)) => {
                match head {
                    SubscriptElem::Elem(id) => {
                        let val = self.eval(*id).await?;
                        let sub = self.subscript(&val)?;
                        acc.push(sub);
                    }
                    SubscriptElem::Spread(id) => {
                        let val = self.eval(*id).await?;
                        let span = self.ast.expr_span(*id).unwrap_or_default();
                        // Extract subscripts from the array
                        match &val {
                            Value::Array(_, elems) => {
                                elems.iter().try_for_each(|elem_id| {
                                    let elem = self
                                        .arena
                                        .get(*elem_id)
                                        .ok_or_else(|| {
                                            Error::runtime(
                                                span,
                                                "invalid value id",
                                            )
                                        })?;
                                    let sub = self.subscript(elem)?;
                                    acc.push(sub);
                                    Ok::<_, Error>(())
                                })?;
                            }
                            _ => typechecked!("...spread", "Array[Subscript]"),
                        }
                    }
                }
                self.build_key_acc(tail, acc).await
            }
        }
    }
}
