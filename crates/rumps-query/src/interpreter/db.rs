//! Database primitives: GET, SET, KILL, DATA, and key construction.

use async_recursion::async_recursion;
use rumps_types::{DataStatus, Key, Name, Subscript};
use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{DbRef, ExprId, Intrinsic, RefTarget, SubscriptElem, TxnId};
use crate::io::IoContext;
use crate::value::{TypeId, Value};
use crate::{Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Evaluate a `DbRef` literal to a `Value::Ref`.
    ///
    /// Evaluates all subscript expressions and creates a first-class `Ref` value.
    #[async_recursion]
    pub(super) async fn ref_lit(
        &mut self,
        dbref: &DbRef,
        span: Span,
    ) -> Result<Value> {
        let (is_global, name) = match dbref {
            DbRef::Local(n, _) => (false, n.as_str()),
            DbRef::Global(n, _) => (true, n.as_str()),
        };
        let name_id = self.arena.intern(name);
        let (_, subs) = dbref.split();

        // Evaluate subscripts and store them
        let sub_ids = self.eval_subscripts(subs, span).await?;

        Ok(Value::Ref(is_global, name_id, sub_ids))
    }

    /// Dispatcher for all DB intrinsics (`@GET`, `@SET`, `@KILL`, `@DATA`, `@ORDER`, `@QUERY`).
    #[async_recursion]
    pub(super) async fn intrinsic(
        &mut self,
        op: Intrinsic,
        rt: &RefTarget,
        val: Option<ExprId>,
        txn_id: Option<TxnId>,
        span: Span,
    ) -> Result<Value> {
        match op {
            Intrinsic::Get => self.get(rt, txn_id, span).await,
            Intrinsic::Set => {
                let v = val.unwrap_or_else(|| invariant!("SET has value"));
                self.set(rt, v, txn_id, span).await
            }
            Intrinsic::Kill => self.kill(rt, txn_id, span).await,
            Intrinsic::Data => self.data(rt, txn_id).await,
            Intrinsic::Order => self.order(rt, txn_id, span).await,
            Intrinsic::Query => self.query(rt, txn_id, span).await,
        }
    }

    /// Evaluate subscript elements and return a `SmallVec` of `ValueId`s.
    #[async_recursion]
    async fn eval_subscripts(
        &mut self,
        subs: &[SubscriptElem],
        span: Span,
    ) -> Result<SmallVec<[crate::value::ValueId; 4]>> {
        let mut acc = SmallVec::with_capacity(subs.len());
        self.eval_subscripts_acc(subs, &mut acc, span).await?;
        Ok(acc)
    }

    /// Recursive helper for evaluating subscripts.
    #[async_recursion]
    async fn eval_subscripts_acc(
        &mut self,
        subs: &[SubscriptElem],
        acc: &mut SmallVec<[crate::value::ValueId; 4]>,
        span: Span,
    ) -> Result<()> {
        match subs.split_first() {
            None => Ok(()),
            Some((head, tail)) => {
                match head {
                    SubscriptElem::Elem(id) => {
                        let val = self.eval(*id).await?;
                        let val_id = self.arena.add(val, span);
                        acc.push(val_id);
                    }
                    SubscriptElem::Spread(id) => {
                        let val = self.eval(*id).await?;
                        match &val {
                            Value::Array(_, elems) => {
                                elems.iter().for_each(|elem_id| {
                                    acc.push(*elem_id);
                                });
                            }
                            _ => typechecked!("...spread", "Array[Subscript]"),
                        }
                    }
                }
                self.eval_subscripts_acc(tail, acc, span).await
            }
        }
    }

    /// Resolve a `RefTarget` to `(Name, Key)`.
    ///
    /// - `Inline(DbRef)`: extracts name and evaluates subscript expressions
    /// - `Expr(ExprId)`: evaluates to `Value::Ref` with pre-evaluated subscripts
    #[async_recursion]
    async fn resolve_ref_target(
        &mut self,
        rt: &RefTarget,
    ) -> Result<(Name, Key)> {
        match rt {
            RefTarget::Inline(dbref) => {
                let (name, subs) = dbref.split();
                let key = self.build_key(subs).await?;
                Ok((name, key))
            }
            RefTarget::Expr(e) => {
                let val = self.eval(*e).await?;
                // Unwrap Union/Newtype to find the inner Ref
                let unwrapped = self.unwrap_value_recursive(&val);
                let v = unwrapped.as_ref().unwrap_or(&val);
                match v {
                    Value::Ref(is_global, name_id, sub_ids) => {
                        let name_str = self
                            .arena
                            .get_str(*name_id)
                            .unwrap_or_else(|| invariant!("Ref name in arena"));
                        let name = if *is_global {
                            Name::global(name_str)
                        } else {
                            Name::local(name_str)
                        };
                        // Build key from pre-evaluated subscripts
                        let key = sub_ids
                            .iter()
                            .map(|vid| {
                                let v =
                                    self.arena.get(*vid).unwrap_or_else(|| {
                                        invariant!("Ref subscript in arena")
                                    });
                                self.subscript(v)
                            })
                            .collect::<Vec<_>>();
                        Ok((name, Key::from(key)))
                    }
                    _ => typechecked!("RefTarget::Expr", "Value::Ref"),
                }
            }
        }
    }

    /// `@GET` primitive; reads a value from a B-tree variable.
    ///
    /// Uses the specified transaction if `txn_id` is `Some`, otherwise reads
    /// directly from the database.
    #[async_recursion]
    pub(super) async fn get(
        &mut self,
        rt: &RefTarget,
        txn_id: Option<TxnId>,
        span: Span,
    ) -> Result<Value> {
        let (name, key) = self.resolve_ref_target(rt).await?;

        let opt_val = match txn_id.and_then(|id| self.txns.get(&id)) {
            Some(txn) => txn.get(&name, &key).await.ok().flatten(),
            None => self.db.get(&name, &key).await.ok().flatten(),
        };

        Ok(match opt_val {
            None => self.make_none_storable(),
            Some(sv) => {
                let v = self.load(sv);
                let vid = self.arena.add(v, span);
                self.make_some_storable(vid)
            }
        })
    }

    /// `@SET` primitive; writes a value to a B-tree variable.
    ///
    /// Returns `Result[Unit, String]`. Globals require an active transaction
    /// (enforced by typechecker). Locals can be set outside transactions.
    #[async_recursion]
    pub(super) async fn set(
        &mut self,
        rt: &RefTarget,
        expr_id: ExprId,
        txn_id: Option<TxnId>,
        span: Span,
    ) -> Result<Value> {
        let (name, key) = self.resolve_ref_target(rt).await?;
        let val = self.eval(expr_id).await?;
        let storage_val = self.store(&val);

        // Global writes require transaction (typechecked); locals go direct
        let res = if name.is_global() {
            txn_id
                .and_then(|id| self.txns.get(&id))
                .unwrap_or_else(|| {
                    typechecked!("global SET", "transaction context")
                })
                .set(&name, &key, storage_val)
                .await
                .map_err(|e| e.to_string())
        } else {
            self.db
                .set(&name, &key, storage_val)
                .await
                .map_err(|e| e.to_string())
        };

        Ok(match res {
            Ok(()) => self.make_result_ok(Value::Unit, span),
            Err(e) => self.make_result_err(&format!("SET failed: {e}"), span),
        })
    }

    /// `@KILL` primitive; deletes a variable and its descendants.
    ///
    /// Returns `Result[Unit, String]`. Globals require an active transaction
    /// (enforced by typechecker). Locals can be killed outside transactions.
    #[async_recursion]
    pub(super) async fn kill(
        &mut self,
        rt: &RefTarget,
        txn_id: Option<TxnId>,
        span: Span,
    ) -> Result<Value> {
        let (name, key) = self.resolve_ref_target(rt).await?;

        // Global writes require transaction (typechecked); locals go direct
        let res = if name.is_global() {
            txn_id
                .and_then(|id| self.txns.get(&id))
                .unwrap_or_else(|| {
                    typechecked!("global KILL", "transaction context")
                })
                .kill(&name, &key)
                .await
                .map_err(|e| e.to_string())
        } else {
            self.db.kill(&name, &key).await.map_err(|e| e.to_string())
        };

        Ok(match res {
            Ok(()) => self.make_result_ok(Value::Unit, span),
            Err(e) => self.make_result_err(&format!("KILL failed: {e}"), span),
        })
    }

    /// `@DATA` primitive; queries existence status of a B-tree node.
    ///
    /// Uses the specified transaction if `txn_id` is `Some`, otherwise reads
    /// directly from the database. Returns a `DataStatus` enum value.
    #[async_recursion]
    pub(super) async fn data(
        &mut self,
        rt: &RefTarget,
        txn_id: Option<TxnId>,
    ) -> Result<Value> {
        let (name, key) = self.resolve_ref_target(rt).await?;

        let status = match txn_id.and_then(|id| self.txns.get(&id)) {
            Some(txn) => txn.data(&name, &key).await,
            None => self.db.data(&name, &key).await,
        }
        .unwrap_or(DataStatus::NoData);

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

    /// `@ORDER` primitive; returns the next subscript at a given level.
    ///
    /// Uses the specified transaction if `txn_id` is `Some`, otherwise reads
    /// directly from the database. Returns `Option[Subscript]`.
    #[async_recursion]
    pub(super) async fn order(
        &mut self,
        rt: &RefTarget,
        txn_id: Option<TxnId>,
        span: Span,
    ) -> Result<Value> {
        let (name, key) = self.resolve_ref_target(rt).await?;

        // `ORDER items(1)` means "find next subscript after `1` at root level",
        // so we split the key: prefix = [] (root), after = `Some(1)`.
        let (prefix, after): (Key, Option<Subscript>) =
            match key.as_slice().split_last() {
                None => (Key::new(), None),
                Some((last, init)) => {
                    (Key::from(init.to_vec()), Some(last.clone()))
                }
            };

        let opt_sub = match txn_id.and_then(|id| self.txns.get(&id)) {
            Some(txn) => txn
                .order(&name, &prefix, after.as_ref())
                .await
                .ok()
                .flatten(),
            None => self
                .db
                .order(&name, &prefix, after.as_ref())
                .await
                .ok()
                .flatten(),
        };

        // Convert Option<Subscript> to Option[Subscript] value
        match opt_sub {
            None => Ok(self.make_none()),
            Some(sub) => {
                let val = self.value_from_subscript(sub);
                let val_id = self.arena.add(val, span);
                Ok(self.make_some(val_id))
            }
        }
    }

    /// `@QUERY` primitive; returns the full key path to the next node.
    ///
    /// Uses the specified transaction if `txn_id` is `Some`, otherwise reads
    /// directly from the database. Returns `Option[Array[Subscript]]`.
    #[async_recursion]
    pub(super) async fn query(
        &mut self,
        rt: &RefTarget,
        txn_id: Option<TxnId>,
        span: Span,
    ) -> Result<Value> {
        let (name, key) = self.resolve_ref_target(rt).await?;

        // The `query` API takes `Option<&Key>` for the "after" position.
        let after = if key.is_empty() { None } else { Some(&key) };

        let opt_key = match txn_id.and_then(|id| self.txns.get(&id)) {
            Some(txn) => txn.query(&name, after).await.ok().flatten(),
            None => self.db.query(&name, after).await.ok().flatten(),
        };

        // Convert Option<Key> to Option[Array[Subscript]] value
        match opt_key {
            None => Ok(self.make_none()),
            Some(k) => {
                let arr = self.key_to_array(k, span);
                let arr_id = self.arena.add(arr, span);
                Ok(self.make_some(arr_id))
            }
        }
    }

    /// Convert a `Key` to an `Array[Subscript]` value.
    fn key_to_array(&mut self, key: Key, span: Span) -> Value {
        let elem_ids = key
            .into_iter()
            .map(|sub| {
                let v = self.value_from_subscript(sub);
                self.arena.add(v, span)
            })
            .collect();
        let type_expr_id = self.type_exprs.named(TypeId::SUBSCRIPT);
        Value::Array(type_expr_id, elem_ids)
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
                        let sub = self.subscript(&val);
                        acc.push(sub);
                    }
                    SubscriptElem::Spread(id) => {
                        let val = self.eval(*id).await?;
                        // Extract subscripts from the array
                        match &val {
                            Value::Array(_, elems) => {
                                elems.iter().for_each(|elem_id| {
                                    let elem = self
                                        .arena
                                        .get(*elem_id)
                                        .unwrap_or_else(|| {
                                            invariant!("ValueId in arena")
                                        });
                                    let sub = self.subscript(elem);
                                    acc.push(sub);
                                });
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
