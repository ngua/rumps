use super::*;

/// Display formatting: produces valid RUMPS syntax (for `write`).
pub(crate) struct Display;

impl Class for Display {
    const ID: ClassId = ClassId::DISPLAY;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "display", MethodFn::Unary(Self::display));
    }
}

impl Display {
    /// Format a value as valid RUMPS syntax (strings quoted).
    ///
    /// Unlike `Into[String]` which produces raw string content, this produces
    /// output suitable for display (e.g., `write` statements) where strings
    /// are quoted and complex types are formatted for readability.
    pub(crate) fn display(
        ctx: &mut ClassCtx<'_>,
        v: &Payload,
    ) -> Result<Payload> {
        let s = Self::format(ctx, v);
        let id = ctx.arena.intern(&s);
        Ok(Payload::String(id))
    }

    pub(crate) fn display_value(ctx: &mut ClassCtx<'_>, v: &Value) -> Payload {
        let s = Self::format_value(ctx, v);
        let id = ctx.arena.intern(&s);
        Payload::String(id)
    }

    /// Format a value as valid RUMPS syntax.
    ///
    /// Returns the string directly; for the class method wrapper that returns
    /// `Payload::String`, see [`display`](Self::display).
    pub(crate) fn format(ctx: &ClassCtx<'_>, v: &Payload) -> String {
        match v {
            Payload::Unit => "Unit".into(),
            Payload::Bool(b) => b.to_string(),
            Payload::Int(n) => n.to_string(),
            Payload::Word(n) => n.to_string(),
            Payload::Float(f) => {
                let s = f.to_string();
                if s.contains('.') || s.contains('e') || s.contains('E') {
                    s
                } else {
                    format!("{s}.0")
                }
            }
            Payload::Char(c) => format!("'{c}'"),
            Payload::String(id) | Payload::FilePath(id) => {
                let s = ctx.arena.get_str(*id).unwrap_or("");
                format!("\"{s}\"")
            }
            Payload::Regex(idx) => {
                let re =
                    ctx.regex_cache.get(*idx as usize).unwrap_or_else(|| {
                        typechecked!("Display", "valid Regex cache index")
                    });
                format!("/{}/", re.as_str())
            }
            Payload::Array(elems) => {
                let items = elems
                    .iter()
                    .filter_map(|id| ctx.arena.value(*id))
                    .map(|v| Self::format_value(ctx, v))
                    .join(", ");
                format!("[ {items} ]")
            }
            Payload::Tuple(elems) => {
                let items = elems
                    .iter()
                    .filter_map(|id| ctx.arena.value(*id))
                    .map(|v| Self::format_value(ctx, v))
                    .join(", ");
                let trail = if elems.len() == 1 { "," } else { "" };
                format!("({items}{trail})")
            }
            Payload::Object(obj) => {
                let fields = obj
                    .iter()
                    .map(|(k, vid)| {
                        let key = ctx.arena.get_str(*k).unwrap_or("?");
                        let val = ctx
                            .arena
                            .value(*vid)
                            .map(|v| Self::format_value(ctx, v))
                            .unwrap_or_else(|| "?".to_owned());
                        format!("{key}: {val}")
                    })
                    .join(", ");
                format!("{{ {fields} }}")
            }
            Payload::Map(entries) => {
                let items = entries
                    .entries()
                    .into_iter()
                    .map(|(k, vid)| {
                        let key = ctx
                            .arena
                            .value(k)
                            .map(|v| Self::format_value(ctx, v))
                            .unwrap_or_else(|| "?".to_owned());
                        let val = ctx
                            .arena
                            .value(vid)
                            .map(|v| Self::format_value(ctx, v))
                            .unwrap_or_else(|| "?".to_owned());
                        format!("{key} => {val}")
                    })
                    .join(", ");
                format!("{{ {items} }}")
            }
            Payload::Time(t) => t.to_rfc3339(),
            Payload::Json(j) => j.to_string(),
            Payload::Variant { tag, vals } => {
                Self::format_variant(ctx, None, *tag, vals)
            }
            Payload::VariantCtor { .. } => {
                typechecked!("Display", "Display (not VariantCtor)")
            }
            Payload::Closure { .. } => {
                typechecked!("Display", "Display (not Closure)")
            }
            Payload::Function { .. } => {
                typechecked!("Display", "Display (not Function)")
            }
            Payload::ModuleFn { .. } => {
                typechecked!("Display", "Display (not ModuleFn)")
            }
            Payload::ClassMethodFn { .. } => {
                typechecked!("Display", "Display (not ClassMethodFn)")
            }
            Payload::PartialApp { .. } => {
                typechecked!("Display", "Display (not PartialApp)")
            }
            Payload::ModuleConst { path } => {
                let path_str: String = path
                    .iter()
                    .filter_map(|id| ctx.arena.get_str(*id))
                    .join(".");
                format!("<{path_str}>")
            }
            Payload::Range {
                start,
                end,
                inclusive,
            } => {
                if *inclusive {
                    format!("{start} ..= {end}")
                } else {
                    format!("{start} .. {end}")
                }
            }
            Payload::LoopContinuation => "<continuation>".into(),
            Payload::LoopContinue(_) => "<loop-continue>".into(),
            Payload::Ref(is_global, name_id, sub_ids) => {
                let prefix = if *is_global { "^" } else { "" };
                let name = ctx.arena.get_str(*name_id).unwrap_or("?");
                let subs = sub_ids
                    .iter()
                    .filter_map(|id| ctx.arena.value(*id))
                    .map(|v| Self::format_value(ctx, v))
                    .join(", ");
                format!("{prefix}{name}{{{subs}}}")
            }
        }
    }

    pub(crate) fn format_value(ctx: &ClassCtx<'_>, v: &Value) -> String {
        match &v.payload {
            Payload::Variant { tag, vals } => Self::format_variant(
                ctx,
                ctx.value_variant_base_type(v),
                *tag,
                vals,
            ),
            payload => Self::format(ctx, payload),
        }
    }

    fn format_variant(
        ctx: &ClassCtx<'_>,
        type_id: Option<TypeId>,
        tag: u8,
        vals: &[ValueId],
    ) -> String {
        let ty_name = type_id
            .and_then(|type_id| ctx.registry.type_name(type_id, ctx.arena))
            .unwrap_or("Variant");
        let var_name = type_id
            .and_then(|type_id| {
                ctx.registry.variant_name(type_id, tag, ctx.arena)
            })
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| tag.to_string());

        if vals.is_empty() {
            format!("{ty_name}.{var_name}")
        } else {
            let args = vals
                .iter()
                .filter_map(|id| ctx.arena.value(*id))
                .map(|v| Self::format_value(ctx, v))
                .join(", ");
            format!("{ty_name}.{var_name}({args})")
        }
    }
}
