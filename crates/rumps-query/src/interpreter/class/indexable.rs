use super::*;

/// Indexing for `Array`, `Map`, `String`.
pub(crate) struct Indexable;

impl Class for Indexable {
    const ID: ClassId = ClassId::INDEXABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "index", MethodFn::Binary(Self::index));
        Self::register(methods, i, "get", MethodFn::Binary(Self::get));
    }
}

impl Indexable {
    pub(crate) fn index(
        ctx: &mut ClassCtx<'_>,
        base: &Payload,
        idx: &Payload,
    ) -> Result<Payload> {
        match (base, idx) {
            (Payload::Array(elems), Payload::Int(i)) => {
                let index = if *i < 0 {
                    elems.len().checked_sub((-*i) as usize)
                } else {
                    Some(*i as usize)
                };
                index
                    .and_then(|idx| elems.get(idx))
                    .and_then(|id| ctx.arena.payload(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(
                            ctx.span,
                            format!("array index {i} out of bounds"),
                        )
                    })
            }
            (Payload::Map(_), _) => {
                typechecked!("index", "async Map index")
            }
            (Payload::String(sid), Payload::Int(i)) => {
                let s = ctx.arena.get_str(*sid).unwrap_or("");
                let len = s.chars().count() as i64;
                let index = if *i < 0 { len + *i } else { *i };
                s.chars().nth(index as usize).map(Payload::Char).ok_or_else(
                    || {
                        Error::runtime(
                            ctx.span,
                            format!("string index {i} out of bounds"),
                        )
                    },
                )
            }
            _ => typechecked!("index", "Indexable"),
        }
    }

    pub(crate) fn get(
        ctx: &mut ClassCtx<'_>,
        base: &Payload,
        idx: &Payload,
    ) -> Result<Payload> {
        match (base, idx) {
            (Payload::Array(elems), Payload::Int(i)) => {
                let index = if *i < 0 {
                    elems.len().checked_sub((-*i) as usize)
                } else {
                    Some(*i as usize)
                };
                Ok(index
                    .and_then(|idx| elems.get(idx))
                    .map(|id| Payload::some(*id))
                    .unwrap_or_else(Payload::none))
            }
            (Payload::Map(_), _) => {
                typechecked!("get", "async Map get")
            }
            (Payload::String(sid), Payload::Int(i)) => {
                let s = ctx.arena.get_str(*sid).unwrap_or("");
                let len = s.chars().count() as i64;
                let index = if *i < 0 { len + *i } else { *i };
                Ok(s.chars()
                    .nth(index as usize)
                    .map(|c| {
                        let id = ctx.arena.add_typed(
                            Payload::Char(c),
                            ctx.runtime_types.meta_char(),
                            ctx.span,
                        );
                        Payload::some(id)
                    })
                    .unwrap_or_else(Payload::none))
            }
            _ => typechecked!("get", "Indexable"),
        }
    }
}
