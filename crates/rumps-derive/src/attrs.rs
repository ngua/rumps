//! Attribute parsing for RUMPS derive macros.
//!
//! Handles parsing of `#[rumps(...)]` attributes on structs, enums, and fields.

use proc_macro2::{Span, TokenStream};
use quote::ToTokens;
use syn::spanned::Spanned;
use syn::{Attribute, Expr, Ident, LitInt, LitStr, Token};

/// Container-level attributes (on struct or enum).
#[derive(Debug, Default)]
pub struct ContainerAttrs {
    /// The global name for storage (e.g., `"patient"` for `^patient`).
    pub global: Option<LitStr>,
    /// For enums: tag field name for adjacently-tagged representation.
    pub tag: Option<LitStr>,
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
    /// Subscript name for this field (renamed or field name).
    pub fn subscript_name(&self) -> String {
        self.attrs
            .rename
            .clone()
            .unwrap_or_else(|| self.ident.to_string())
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
