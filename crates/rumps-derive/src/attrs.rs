//! Attribute parsing for RUMPS derive macros.
//!
//! Handles parsing of `#[rumps(...)]` attributes on structs, enums, and fields.

use proc_macro2::{Span, TokenStream};
use quote::ToTokens;
use syn::spanned::Spanned;
use syn::{Attribute, Expr, Ident, LitInt, LitStr, Token};

/// Case convention for renaming identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenameAll {
    /// Keep original identifier name (default).
    #[default]
    None,
    /// `snake_case`
    SnakeCase,
    /// `camelCase`
    CamelCase,
    /// `PascalCase`
    PascalCase,
    /// `train-case` (aka kebab-case)
    TrainCase,
    /// `lowercase`
    Lowercase,
    /// `UPPERCASE`
    Uppercase,
    /// `SCREAMING_SNAKE_CASE`
    ScreamingSnakeCase,
}

impl RenameAll {
    /// Parse from attribute literal (e.g., `"snake-case"`).
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "snake-case" | "snake_case" => Some(Self::SnakeCase),
            "camel-case" | "camelCase" => Some(Self::CamelCase),
            "pascal-case" | "PascalCase" => Some(Self::PascalCase),
            "train-case" | "kebab-case" => Some(Self::TrainCase),
            "lowercase" => Some(Self::Lowercase),
            "uppercase" | "UPPERCASE" => Some(Self::Uppercase),
            "screaming-snake-case" | "SCREAMING_SNAKE_CASE" => {
                Some(Self::ScreamingSnakeCase)
            }
            _ => None,
        }
    }

    /// Apply this renaming convention to an identifier.
    pub fn apply(&self, s: &str) -> String {
        match self {
            Self::None => s.to_owned(),
            Self::SnakeCase => to_snake_case(s),
            Self::CamelCase => to_camel_case(s),
            Self::PascalCase => to_pascal_case(s),
            Self::TrainCase => to_train_case(s),
            Self::Lowercase => s.to_lowercase(),
            Self::Uppercase => s.to_uppercase(),
            Self::ScreamingSnakeCase => to_screaming_snake_case(s),
        }
    }
}

/// Convert to `snake_case`.
fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    s.chars().enumerate().for_each(|(i, c)| {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    });
    out
}

/// Convert to `camelCase`.
fn to_camel_case(s: &str) -> String {
    let snake = to_snake_case(s);
    let mut cap_next = false;
    snake
        .chars()
        .filter_map(|c| {
            if c == '_' {
                cap_next = true;
                None
            } else if cap_next {
                cap_next = false;
                Some(c.to_ascii_uppercase())
            } else {
                Some(c)
            }
        })
        .collect()
}

/// Convert to `PascalCase`.
fn to_pascal_case(s: &str) -> String {
    let camel = to_camel_case(s);
    let mut chars = camel.chars();
    chars
        .next()
        .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or_default()
}

/// Convert to `train-case` (kebab-case).
fn to_train_case(s: &str) -> String {
    to_snake_case(s).replace('_', "-")
}

/// Convert to `SCREAMING_SNAKE_CASE`.
fn to_screaming_snake_case(s: &str) -> String {
    to_snake_case(s).to_uppercase()
}

/// Container-level attributes (on struct or enum).
#[derive(Debug, Default)]
pub struct ContainerAttrs {
    /// The global name for storage (e.g., `"patient"` for `^patient`).
    pub global: Option<LitStr>,
    /// For enums: tag field name for adjacently-tagged representation.
    pub tag: Option<LitStr>,
    /// For enums: content field name (used with `tag` for adjacent tagging).
    pub content: Option<LitStr>,
    /// Case convention for variant/field names.
    pub rename_all: RenameAll,
    /// For enums: use untagged representation (no variant discriminator).
    pub untagged: bool,
}

impl ContainerAttrs {
    pub fn from_attrs(attrs: &[Attribute]) -> syn::Result<Self> {
        attrs
            .iter()
            .filter(|a| a.path().is_ident("rumps"))
            .try_fold(Self::default(), |mut acc, attr| {
                attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("global") {
                        meta.input.parse::<Token![=]>()?;
                        acc.global = Some(meta.input.parse()?);
                        Ok(())
                    } else if meta.path.is_ident("tag") {
                        meta.input.parse::<Token![=]>()?;
                        acc.tag = Some(meta.input.parse()?);
                        Ok(())
                    } else if meta.path.is_ident("content") {
                        meta.input.parse::<Token![=]>()?;
                        acc.content = Some(meta.input.parse()?);
                        Ok(())
                    } else if meta.path.is_ident("rename_all") {
                        meta.input.parse::<Token![=]>()?;
                        let lit: LitStr = meta.input.parse()?;
                        acc.rename_all = RenameAll::from_str(&lit.value())
                            .ok_or_else(|| meta.error(format!(
                                "unknown rename_all value: `{}` (expected one of: \
                                 snake-case, camel-case, pascal-case, train-case, \
                                 lowercase, uppercase, screaming-snake-case)",
                                lit.value()
                            )))?;
                        Ok(())
                    } else if meta.path.is_ident("untagged") {
                        acc.untagged = true;
                        Ok(())
                    } else {
                        Err(meta.error(format!(
                            "unknown rumps container attribute: `{}`",
                            meta.path.to_token_stream()
                        )))
                    }
                })?;
                Ok(acc)
            })
    }

    pub fn global_or_err(&self, span: Span) -> syn::Result<&LitStr> {
        self.global.as_ref().ok_or_else(|| {
            syn::Error::new(
                span,
                "missing `#[rumps(global = \"...\")]` attribute",
            )
        })
    }
}

/// Field-level attributes.
#[derive(Debug, Default, Clone)]
pub struct FieldAttrs {
    /// Field is part of the key path.
    pub key: bool,
    /// Explicit ordering for composite keys.
    pub order: Option<usize>,
    /// Inline nested struct fields at current level.
    pub flatten: bool,
    /// Store nested struct as subtree.
    pub subtree: bool,
    /// Custom name for the subscript.
    pub rename: Option<String>,
    /// Don't persist this field.
    pub skip: bool,
    /// Use default value on read.
    pub default: Option<DefaultValue>,
}

/// Default value specification.
#[derive(Debug, Clone)]
pub enum DefaultValue {
    /// Use `Default::default()`.
    Trait,
    /// Use a specific expression.
    Expr(TokenStream),
}

impl FieldAttrs {
    pub fn from_attrs(attrs: &[Attribute]) -> syn::Result<Self> {
        attrs
            .iter()
            .filter(|a| a.path().is_ident("rumps"))
            .try_fold(Self::default(), |mut acc, attr| {
                attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("key") {
                        acc.key = true;
                        Ok(())
                    } else if meta.path.is_ident("order") {
                        meta.input.parse::<Token![=]>()?;
                        let lit: LitInt = meta.input.parse()?;
                        acc.order = Some(lit.base10_parse()?);
                        Ok(())
                    } else if meta.path.is_ident("flatten") {
                        acc.flatten = true;
                        Ok(())
                    } else if meta.path.is_ident("subtree") {
                        acc.subtree = true;
                        Ok(())
                    } else if meta.path.is_ident("rename") {
                        meta.input.parse::<Token![=]>()?;
                        let lit: LitStr = meta.input.parse()?;
                        acc.rename = Some(lit.value());
                        Ok(())
                    } else if meta.path.is_ident("skip") {
                        acc.skip = true;
                        Ok(())
                    } else if meta.path.is_ident("default") {
                        acc.default = Some(
                            meta.input
                                .peek(Token![=])
                                .then(|| {
                                    meta.input.parse::<Token![=]>()?;
                                    let expr: Expr = meta.input.parse()?;
                                    Ok::<_, syn::Error>(DefaultValue::Expr(
                                        expr.to_token_stream(),
                                    ))
                                })
                                .transpose()?
                                .unwrap_or(DefaultValue::Trait),
                        );
                        Ok(())
                    } else {
                        Err(meta.error(format!(
                            "unknown rumps field attribute: `{}`",
                            meta.path.to_token_stream()
                        )))
                    }
                })?;
                Ok(acc)
            })
    }

    /// Validate that there are no conflicting attribute combinations.
    pub fn validate(&self, span: Span) -> syn::Result<()> {
        let conflicts: &[(&str, bool, &str, bool)] = &[
            ("key", self.key, "skip", self.skip),
            ("key", self.key, "flatten", self.flatten),
            ("key", self.key, "subtree", self.subtree),
            ("flatten", self.flatten, "subtree", self.subtree),
            ("skip", self.skip, "flatten", self.flatten),
            ("skip", self.skip, "subtree", self.subtree),
        ];

        conflicts
            .iter()
            .find(|(_, a, _, b)| *a && *b)
            .map(|(na, _, nb, _)| {
                Err(syn::Error::new(
                    span,
                    format!(
                        "conflicting attributes: `#[rumps({})]` and `#[rumps({})]` \
                         cannot be used together",
                        na, nb
                    ),
                ))
            })
            .unwrap_or(Ok(()))
    }
}

/// Variant-level attributes (on enum variants).
#[derive(Debug, Default, Clone)]
pub struct VariantAttrs {
    /// Custom name for the variant tag subscript.
    pub rename: Option<String>,
    /// Case convention for fields within this variant (overrides container).
    pub rename_all: Option<RenameAll>,
}

impl VariantAttrs {
    pub fn from_attrs(attrs: &[Attribute]) -> syn::Result<Self> {
        attrs
            .iter()
            .filter(|a| a.path().is_ident("rumps"))
            .try_fold(Self::default(), |mut acc, attr| {
                attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("rename") {
                        meta.input.parse::<Token![=]>()?;
                        let lit: LitStr = meta.input.parse()?;
                        acc.rename = Some(lit.value());
                        Ok(())
                    } else if meta.path.is_ident("rename_all") {
                        meta.input.parse::<Token![=]>()?;
                        let lit: LitStr = meta.input.parse()?;
                        acc.rename_all = Some(
                            RenameAll::from_str(&lit.value()).ok_or_else(|| {
                                meta.error(format!(
                                    "unknown rename_all value: `{}` (expected one of: \
                                     snake-case, camel-case, pascal-case, train-case, \
                                     lowercase, uppercase, screaming-snake-case)",
                                    lit.value()
                                ))
                            })?,
                        );
                        Ok(())
                    } else {
                        Err(meta.error(format!(
                            "unknown rumps variant attribute: `{}`",
                            meta.path.to_token_stream()
                        )))
                    }
                })?;
                Ok(acc)
            })
    }
}

/// Parsed field information.
#[derive(Debug)]
pub struct FieldInfo {
    pub ident: Ident,
    pub ty: syn::Type,
    pub attrs: FieldAttrs,
    /// Original position in struct definition.
    pub position: usize,
}

impl FieldInfo {
    /// Subscript name for this field.
    ///
    /// If `#[rumps(rename = "...")]` is set:
    /// - If it's a case convention (e.g., `"snake-case"`), apply that to the field name
    /// - Otherwise, use it as a literal string
    ///
    /// If no rename is set, apply `rename_all` to the field name.
    pub fn subscript_name(&self, rename_all: RenameAll) -> String {
        match &self.attrs.rename {
            Some(r) => RenameAll::from_str(r)
                .map(|case| case.apply(&self.ident.to_string()))
                .unwrap_or_else(|| r.clone()),
            None => rename_all.apply(&self.ident.to_string()),
        }
    }

    /// Key order (explicit or positional).
    pub fn key_order(&self) -> usize {
        self.attrs.order.unwrap_or(self.position)
    }
}

/// Categorized fields for code generation.
#[derive(Debug)]
pub struct ParsedFields {
    /// Fields that are part of the key (sorted by order).
    pub key_fields: Vec<FieldInfo>,
    /// Fields stored as values.
    pub value_fields: Vec<FieldInfo>,
    /// Fields that are flattened.
    pub flatten_fields: Vec<FieldInfo>,
    /// Fields stored as subtrees.
    pub subtree_fields: Vec<FieldInfo>,
    /// Fields that are skipped (not persisted).
    pub skip_fields: Vec<FieldInfo>,
}

impl ParsedFields {
    pub fn from_named(fields: &syn::FieldsNamed) -> syn::Result<Self> {
        let all_fields: Vec<FieldInfo> = fields
            .named
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let attrs = FieldAttrs::from_attrs(&f.attrs)?;
                attrs.validate(f.span())?;
                Ok(FieldInfo {
                    ident: f.ident.clone().ok_or_else(|| {
                        syn::Error::new(f.span(), "expected named field")
                    })?,
                    ty: f.ty.clone(),
                    attrs,
                    position: i,
                })
            })
            .collect::<syn::Result<_>>()?;

        // Partition fields by type
        let (skip_fields, rest): (Vec<_>, Vec<_>) =
            all_fields.into_iter().partition(|f| f.attrs.skip);

        let (key_fields, rest): (Vec<_>, Vec<_>) =
            rest.into_iter().partition(|f| f.attrs.key);

        let (flatten_fields, rest): (Vec<_>, Vec<_>) =
            rest.into_iter().partition(|f| f.attrs.flatten);

        let (subtree_fields, value_fields): (Vec<_>, Vec<_>) =
            rest.into_iter().partition(|f| f.attrs.subtree);

        // Sort key fields by order
        let mut key_fields = key_fields;
        key_fields.sort_by_key(|f| f.key_order());

        Ok(Self {
            key_fields,
            value_fields,
            flatten_fields,
            subtree_fields,
            skip_fields,
        })
    }
}

/// Variant field types.
#[derive(Debug)]
pub enum VariantFields {
    /// Unit variant: `Active`.
    Unit,
    /// Tuple variant: `Error(String)` or `Point(i32, i32)`.
    Tuple(Vec<TupleFieldInfo>),
    /// Struct variant: `User { name: String, age: u32 }`.
    Struct(ParsedFields),
}

/// Tuple field information (unnamed fields).
#[derive(Debug)]
pub struct TupleFieldInfo {
    pub ty: syn::Type,
    pub attrs: FieldAttrs,
    pub index: usize,
}

/// Parsed variant information.
#[derive(Debug)]
pub struct VariantInfo {
    pub ident: Ident,
    pub attrs: VariantAttrs,
    pub fields: VariantFields,
}

impl VariantInfo {
    /// Tag name for this variant.
    ///
    /// If `#[rumps(rename = "...")]` is set:
    /// - If it's a case convention (e.g., `"snake-case"`), apply that to the variant name
    /// - Otherwise, use it as a literal string
    ///
    /// If no rename is set, apply `rename_all` to the variant name.
    pub fn tag_name(&self, rename_all: RenameAll) -> String {
        match &self.attrs.rename {
            Some(r) => RenameAll::from_str(r)
                .map(|case| case.apply(&self.ident.to_string()))
                .unwrap_or_else(|| r.clone()),
            None => rename_all.apply(&self.ident.to_string()),
        }
    }
}

/// Parse all variants of an enum.
pub fn parse_variants(
    variants: &syn::punctuated::Punctuated<syn::Variant, Token![,]>,
) -> syn::Result<Vec<VariantInfo>> {
    variants
        .iter()
        .map(|v| {
            let attrs = VariantAttrs::from_attrs(&v.attrs)?;
            let fields = match &v.fields {
                syn::Fields::Unit => VariantFields::Unit,
                syn::Fields::Unnamed(uf) => {
                    let tuple_fields = uf
                        .unnamed
                        .iter()
                        .enumerate()
                        .map(|(i, f)| {
                            let attrs = FieldAttrs::from_attrs(&f.attrs)?;
                            attrs.validate(f.span())?;
                            Ok(TupleFieldInfo {
                                ty: f.ty.clone(),
                                attrs,
                                index: i,
                            })
                        })
                        .collect::<syn::Result<Vec<_>>>()?;
                    VariantFields::Tuple(tuple_fields)
                }
                syn::Fields::Named(nf) => {
                    VariantFields::Struct(ParsedFields::from_named(nf)?)
                }
            };
            Ok(VariantInfo {
                ident: v.ident.clone(),
                attrs,
                fields,
            })
        })
        .collect()
}
