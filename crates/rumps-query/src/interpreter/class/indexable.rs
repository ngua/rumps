use futures::future::BoxFuture;

use super::*;

/// Indexing for `Array`, `Map`, `String`.
pub(crate) struct Indexable;

impl Class for Indexable {
    const ID: ClassId = ClassId::INDEXABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "index",
            MethodAbi::Binary,
            Builtin::Selected(Self::select),
        );
        Self::register(
            methods,
            i,
            "get",
            MethodAbi::Binary,
            Builtin::Selected(Self::select),
        );
    }
}

impl Indexable {
    pub(crate) fn select(
        interp: &mut Interpreter<'_, '_>,
        d: &Dispatch,
    ) -> Result<builtins::Call> {
        let is_map = interp
            .arena
            .payload(d.args[0])
            .is_some_and(|v| matches!(v, Payload::Map(_)));
        let index = interp.arena.intern("index");
        let get = interp.arena.intern("get");
        let imp = if d.method == index {
            if is_map {
                Impl::Async(Self::map_index)
            } else {
                Impl::Sync(Self::index)
            }
        } else if d.method == get {
            if is_map {
                Impl::Async(Self::map_get)
            } else {
                Impl::Sync(Self::get)
            }
        } else {
            typechecked!("Indexable method", "index or get")
        };
        Ok(builtins::Call {
            imp,
            args: d.args.clone(),
            span: d.span,
            output: interp.class_output_meta(
                d.output_expr_id,
                d.dispatch_expr_id,
                d.output_ty,
                d.class,
            ),
            meta: None,
        })
    }

    pub(crate) fn index(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let base_id = args[0];
        let idx_id = args[1];
        let span = ctx.span();
        let (base, idx) = {
            let vals = ctx.vals();
            (
                vals.payload(base_id)?.clone(),
                vals.payload(idx_id)?.clone(),
            )
        };
        match (&base, &idx) {
            (Payload::Array(elems), Payload::Int(i)) => {
                let idx = if *i < 0 {
                    elems.len().checked_sub((-*i) as usize)
                } else {
                    Some(*i as usize)
                };
                idx.and_then(|idx| elems.get(idx)).copied().ok_or_else(|| {
                    Error::runtime(
                        span,
                        format!("array index {i} out of bounds"),
                    )
                })
            }
            (Payload::String(sid), Payload::Int(i)) => {
                let mut vals = ctx.vals();
                let s = vals.str(*sid)?;
                let len = s.chars().count() as i64;
                let idx = if *i < 0 { len + *i } else { *i };
                s.chars()
                    .nth(idx as usize)
                    .map(|c| {
                        vals.add_typed(
                            Payload::Char(c),
                            RuntimeTyId::from(TyArena::CHAR),
                        )
                    })
                    .ok_or_else(|| {
                        Error::runtime(
                            span,
                            format!("string index {i} out of bounds"),
                        )
                    })
            }
            _ => typechecked!("index", "Indexable instance"),
        }
    }

    pub(crate) fn map_index<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let base_id = args[0];
            let idx_id = args[1];
            let span = ctx.span();
            let base = ctx.vals().payload(base_id)?.clone();
            match base {
                Payload::Map(map) => {
                    ctx.maps().lookup(&map, idx_id).await?.ok_or_else(|| {
                        Error::runtime(span, "map key not found")
                    })
                }
                _ => typechecked!("index", "Map"),
            }
        })
    }

    pub(crate) fn get(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let base_id = args[0];
        let idx_id = args[1];
        let out = ctx.output_ty();
        let (base, idx) = {
            let vals = ctx.vals();
            (
                vals.payload(base_id)?.clone(),
                vals.payload(idx_id)?.clone(),
            )
        };
        match (&base, &idx) {
            (Payload::Array(elems), Payload::Int(i)) => {
                let idx = if *i < 0 {
                    elems.len().checked_sub((-*i) as usize)
                } else {
                    Some(*i as usize)
                };
                Ok(idx
                    .and_then(|idx| elems.get(idx))
                    .copied()
                    .map(|id| match out {
                        Some(ty) => ctx.vals().add_typed(Payload::some(id), ty),
                        None => ctx.vals().option_some(id),
                    })
                    .unwrap_or_else(|| match out {
                        Some(ty) => ctx.vals().add_typed(Payload::none(), ty),
                        None => ctx.vals().option_none(),
                    }))
            }
            (Payload::String(sid), Payload::Int(i)) => {
                let mut vals = ctx.vals();
                let s = vals.str(*sid)?.to_owned();
                let len = s.chars().count() as i64;
                let idx = if *i < 0 { len + *i } else { *i };
                Ok(s.chars()
                    .nth(idx as usize)
                    .map(|c| {
                        let id = vals.add_typed(
                            Payload::Char(c),
                            RuntimeTyId::from(TyArena::CHAR),
                        );
                        match out {
                            Some(ty) => vals.add_typed(Payload::some(id), ty),
                            None => vals.option_some(id),
                        }
                    })
                    .unwrap_or_else(|| match out {
                        Some(ty) => vals.add_typed(Payload::none(), ty),
                        None => vals.option_none(),
                    }))
            }
            _ => typechecked!("get", "Indexable instance"),
        }
    }

    pub(crate) fn map_get<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let base_id = args[0];
            let idx_id = args[1];
            let out = ctx.output_ty();
            let base = ctx.vals().payload(base_id)?.clone();
            match base {
                Payload::Map(map) => Ok(ctx
                    .maps()
                    .lookup(&map, idx_id)
                    .await?
                    .map(|id| match out {
                        Some(ty) => ctx.vals().add_typed(Payload::some(id), ty),
                        None => ctx.vals().option_some(id),
                    })
                    .unwrap_or_else(|| match out {
                        Some(ty) => ctx.vals().add_typed(Payload::none(), ty),
                        None => ctx.vals().option_none(),
                    })),
                _ => typechecked!("get", "Map"),
            }
        })
    }
}
