//! Proc macros for `rumps-query`.
//!
//! Provides the `scheme!` macro for declaring polymorphic type schemes
//! with a readable syntax like `forall T U. (Array[T], (T) -> U) -> Array[U]`.

use std::collections::HashMap;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use smallvec::SmallVec;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, Result, Token};

/// Parse a type scheme with optional universal quantification and constraints.
///
/// # Syntax
///
/// ```text
/// scheme!(forall T U. (Array[T], (T) -> U) -> Array[U])
/// scheme!(forall I: Iterable[T], T. (I) -> Int)  // constrained type variable
/// scheme!(Int -> Bool)  // monomorphic, no forall
/// scheme!(ctx; { src: FilePath, dest: FilePath } -> Unit)  // with context for objects
/// ```
///
/// ## Class syntax
///
/// Type variables can have simple or parameterized class constraints:
///
/// Simple classes (no type argument):
/// - `T: Numeric` ; `T` must be `Int`, `Float`, or `Word`
/// - `T: Negatable` ; `T` must be `Int` or `Float`
/// - `T: BitLike` ; `T` must be `Bool`, `Int`, or `Word`
/// - `T: Monoid` ; `T` must be `String`, `Array[_]`, `Map[_, _]`, or `Option[_]`
/// - `T: Storable` ; `T` must be storable in the database
/// - `T: Subscriptable` ; `T` must be usable as a subscript key
/// - `T: Indexable` ; `T` must support indexing (`Array`, `Map`, `String`)
/// - `T: Ord` ; `T` must support ordering (`Bool`, `Int`, `Word`, `Float`, `Char`, `String`)
/// - `T: Display` ; `T` can be displayed as RUMPS syntax
///
/// Parameterized classes (require a type argument in brackets):
/// - `I: Iterable[T]` ; `I` must be iterable with element type `T`
/// - `F: Fallible[T]` ; `F` must be a fallible type (`Option[T]` or `Result[T, _]`)
/// - `T: Into[U]` ; type `T` is convertible to type `U`
/// - `T: TryInto[U]` ; type `T` is fallibly convertible to type `U`
/// - `F: Mappable[T]` ; `F` is a functor with element type `T`
/// - `F: Foldable[T]` ; `F` supports fold/reduce with element type `T`
/// - `F: Filterable[T]` ; `F` supports filter with element type `T`
///
/// ## Type syntax
///
/// - Primitives: `Int`, `Float`, `Bool`, `String`, `Char`, `Unit`, `Time`, `Range`, `Json`,
///   `Word`, `DataStatus`, `Regex`, `RuntimeError`, `Local`, `Global`, `Ordering`,
///   `FilePath`, `Path`, `Error`, `Unknown`
/// - Named builtins: `Storable`, `Subscript`, `Scalar`, `Ref` (maps to `Ty::Named`)
/// - Type variables: any identifier bound by `forall`
/// - Parameterized: `Array[T]`, `Option[T]`, `Result[T, E]`, `Map[K, V]`
/// - Functions: `(A, B) -> C` or `A -> B`
/// - Tuples: `(A, B)` (without `->` following), `(A,)` for 1-tuple
/// - Grouping: `(A)` is just `A`
/// - Objects: `{ field1: Type1, field2: Type2 }` (requires context; use `ctx; ...`)
#[proc_macro]
pub fn scheme(input: TokenStream) -> TokenStream {
    syn::parse_macro_input!(input as SchemeInput)
        .to_tokens()
        .into()
}

/// A class bound on a type variable.
#[derive(Debug, Clone)]
enum VarClass {
    /// Simple class (no type arguments): `Numeric`, `Storable`, etc.
    Simple(String),
    /// Parameterized class with type arguments: `Iterable[T]`, `Into[T, U]`, etc.
    Parameterized(String, SmallVec<[String; 2]>),
}

/// Parsed scheme input: optional context + optional `forall` + type.
struct SchemeInput {
    /// Optional context identifier for interning object field names.
    ctx: Option<Ident>,
    /// Type variables with optional class bounds: `(var_name, class)`
    vars: Vec<(Ident, Option<VarClass>)>,
    ty: TyExpr,
}

/// Simple class names (no type argument).
const SIMPLE_CLASSES: &[&str] = &[
    "Numeric",
    "Negatable",
    "BitLike",
    "Monoid",
    "Storable",
    "Subscriptable",
    "Indexable",
    "Ord",
    "Display",
];

/// Parameterized class names (require `[T, ...]` arguments).
const PARAMETERIZED_CLASSES: &[&str] = &[
    "Iterable",
    "Fallible",
    "Into",
    "TryInto",
    "Mappable",
    "Foldable",
    "Filterable",
];

/// Parse a single type variable with optional class bound.
///
/// Syntax:
/// - `T` (no class)
/// - `T: Numeric` (simple class)
/// - `I: Iterable[T]` (parameterized class)
fn parse_type_var(input: ParseStream) -> Result<(Ident, Option<VarClass>)> {
    let name: Ident = input.parse()?;

    // Check for class: `: Class` or `: Class[T]`
    let class = if input.peek(Token![:]) {
        input.parse::<Token![:]>()?;
        let class_name: Ident = input.parse()?;
        let cname = class_name.to_string();

        if SIMPLE_CLASSES.contains(&cname.as_str()) {
            Some(VarClass::Simple(cname))
        } else if PARAMETERIZED_CLASSES.contains(&cname.as_str()) {
            // Parameterized class: parse bracketed, comma-separated type args
            let content;
            syn::bracketed!(content in input);
            let args: syn::punctuated::Punctuated<Ident, Token![,]> =
                syn::punctuated::Punctuated::parse_terminated(&content)?;
            let args: SmallVec<[String; 2]> =
                args.into_iter().map(|id| id.to_string()).collect();
            Some(VarClass::Parameterized(cname, args))
        } else {
            panic!(
                "unknown class: `{cname}`; use one of {:?} or {:?}",
                SIMPLE_CLASSES, PARAMETERIZED_CLASSES
            )
        }
    } else {
        None
    };

    Ok((name, class))
}

impl Parse for SchemeInput {
    fn parse(input: ParseStream) -> Result<Self> {
        // Check for optional context: `ctx; ...`
        let ctx = input
            .peek(Ident)
            .then(|| {
                input.peek2(Token![;]).then(|| {
                    let ctx: Ident = input.parse().ok()?;
                    input.parse::<Token![;]>().ok()?;
                    Some(ctx)
                })?
            })
            .flatten();

        // Check for `forall` keyword
        if input.peek(Ident) && input.peek2(Ident) {
            let kw: Ident = input.parse()?;
            if kw == "forall" {
                // Parse type variables (with optional constraints), comma-separated, until `.`
                let mut vars = Vec::new();
                while !input.peek(Token![.]) {
                    vars.push(parse_type_var(input)?);
                    // Consume optional comma between variables
                    let _ = input.parse::<Token![,]>();
                }
                input.parse::<Token![.]>()?;
                let ty = parse_ty(input)?;
                Ok(Self { ctx, vars, ty })
            } else {
                // Not `forall`; the ident we parsed is part of the type
                let ty = parse_ty_starting_with(input, kw)?;
                Ok(Self {
                    ctx,
                    vars: vec![],
                    ty,
                })
            }
        } else {
            let ty = parse_ty(input)?;
            Ok(Self {
                ctx,
                vars: vec![],
                ty,
            })
        }
    }
}

/// Generate tokens for a parameterized class.
///
/// For parameterized classes (`Iterable[T]`, `Fallible[T]`, `Into[T]`, etc.),
/// we generate a `ty::Class::Variant(Ty::Var(...))` with the inner type.
fn parameterized_class_tokens(
    name: &str,
    inner_ty: TokenStream2,
) -> TokenStream2 {
    match name {
        "Iterable" => {
            quote! { crate::typecheck::Class::Iterable(#inner_ty) }
        }
        "Fallible" => {
            quote! { crate::typecheck::Class::Fallible(#inner_ty) }
        }
        "Into" => {
            quote! { crate::typecheck::Class::Into(#inner_ty) }
        }
        "TryInto" => {
            quote! { crate::typecheck::Class::TryInto(#inner_ty) }
        }
        "Mappable" => {
            quote! { crate::typecheck::Class::Mappable(#inner_ty) }
        }
        "Foldable" => {
            quote! { crate::typecheck::Class::Foldable(#inner_ty) }
        }
        "Filterable" => {
            quote! { crate::typecheck::Class::Filterable(#inner_ty) }
        }
        _ => panic!("unknown parameterized class: `{name}`"),
    }
}

impl SchemeInput {
    fn to_tokens(&self) -> TokenStream2 {
        // Build var name -> index map
        let var_map: HashMap<String, u32> = self
            .vars
            .iter()
            .enumerate()
            .map(|(i, (v, _))| (v.to_string(), i as u32))
            .collect();

        let ty_tokens = self.ty.to_tokens(&var_map, self.ctx.as_ref());

        if self.vars.is_empty() {
            quote! {
                crate::typecheck::Scheme::mono(#ty_tokens)
            }
        } else {
            let var_indices: Vec<u32> = (0..self.vars.len() as u32).collect();

            // Generate class entries: (TyVar, Class) tuples
            let class_entries: Vec<TokenStream2> = self
                .vars
                .iter()
                .enumerate()
                .filter_map(|(i, (_, class))| {
                    class.as_ref().map(|c| {
                        let var_idx = i as u32;
                        match c {
                            VarClass::Simple(name) => {
                                let ident =
                                    Ident::new(name, proc_macro2::Span::call_site());
                                quote! {
                                    (
                                        crate::typecheck::TyVar::new(#var_idx),
                                        crate::typecheck::Class::#ident
                                    )
                                }
                            }
                            VarClass::Parameterized(name, args) => {
                                let arg_indices: Vec<u32> = args
                                    .iter()
                                    .map(|arg| {
                                        var_map.get(arg).copied().unwrap_or_else(|| {
                                            panic!(
                                                "unbound type variable in class: `{arg}`"
                                            )
                                        })
                                    })
                                    .collect();
                                let inner_ty = if arg_indices.len() == 1 {
                                    let idx = arg_indices[0];
                                    quote! {
                                        crate::typecheck::Ty::Var(
                                            crate::typecheck::TyVar::new(#idx)
                                        )
                                    }
                                } else {
                                    let var_tokens: Vec<_> = arg_indices
                                        .iter()
                                        .map(|idx| {
                                            quote! {
                                                crate::typecheck::Ty::Var(
                                                    crate::typecheck::TyVar::new(#idx)
                                                )
                                            }
                                        })
                                        .collect();
                                    quote! {
                                        crate::typecheck::Ty::Tuple(vec![#(#var_tokens),*])
                                    }
                                };
                                let class_tokens =
                                    parameterized_class_tokens(name, inner_ty);
                                quote! {
                                    (
                                        crate::typecheck::TyVar::new(#var_idx),
                                        #class_tokens
                                    )
                                }
                            }
                        }
                    })
                })
                .collect();

            quote! {
                crate::typecheck::Scheme {
                    vars: vec![#(crate::typecheck::TyVar::new(#var_indices)),*],
                    ty: #ty_tokens,
                    constraints: smallvec::smallvec![#(#class_entries),*],
                }
            }
        }
    }
}

/// A type expression in the scheme DSL.
#[derive(Debug)]
enum TyExpr {
    /// Primitive type: `Int`, `Bool`, etc.
    Prim(String),
    /// Type variable: `T`, `U`, etc.
    Var(String),
    /// Parameterized type: `Array[T]`, `Map[K, V]`, etc.
    App(String, Vec<Self>),
    /// Type variable application: `F[U]` where `F` is a bound type variable.
    ///
    /// Used for higher-kinded polymorphism; `F[U]` applies the type constructor
    /// bound to `F` to the argument `U`. For example, if `F: Fallible[T]` and
    /// `F` resolves to `Option[T]`, then `F[U]` becomes `Option[U]`.
    Apply(String, Vec<Self>),
    /// Function type: `(A, B) -> C`.
    Fn(Vec<Self>, Box<Self>),
    /// Tuple type: `(A, B)`.
    Tuple(Vec<Self>),
    /// Union type: `A | B`.
    Union(Vec<Self>),
    /// Object type: `{ field: Type, ... }`.
    Object(Vec<(String, Box<Self>)>),
    /// Named builtin type: `Storable`, `Subscript`, etc.
    Named(String),
}

impl TyExpr {
    fn to_tokens(
        &self,
        vars: &HashMap<String, u32>,
        ctx: Option<&Ident>,
    ) -> TokenStream2 {
        match self {
            Self::Prim(name) => {
                let ident = Ident::new(name, proc_macro2::Span::call_site());
                quote! { crate::typecheck::Ty::#ident }
            }
            Self::Var(name) => {
                let idx = vars.get(name).copied().unwrap_or_else(|| {
                    panic!("unbound type variable: `{name}`")
                });
                quote! { crate::typecheck::Ty::Var(crate::typecheck::TyVar::new(#idx)) }
            }
            Self::App(name, args) => {
                let arg_tokens: Vec<_> =
                    args.iter().map(|a| a.to_tokens(vars, ctx)).collect();
                match name.as_str() {
                    "Array" => {
                        let inner = &arg_tokens[0];
                        quote! { crate::typecheck::Ty::Array(Box::new(#inner)) }
                    }
                    "Option" => {
                        let inner = &arg_tokens[0];
                        quote! { crate::typecheck::Ty::Option(Box::new(#inner)) }
                    }
                    "Result" => {
                        let ok = &arg_tokens[0];
                        let err = &arg_tokens[1];
                        quote! { crate::typecheck::Ty::Result(Box::new(#ok), Box::new(#err)) }
                    }
                    "Map" => {
                        let k = &arg_tokens[0];
                        let v = &arg_tokens[1];
                        quote! { crate::typecheck::Ty::Map(Box::new(#k), Box::new(#v)) }
                    }
                    _ => panic!("unknown parameterized type: `{name}`"),
                }
            }
            Self::Fn(params, ret) => {
                let param_tokens: Vec<_> =
                    params.iter().map(|p| p.to_tokens(vars, ctx)).collect();
                let ret_tokens = ret.to_tokens(vars, ctx);
                quote! {
                    crate::typecheck::Ty::Fn(
                        vec![#(#param_tokens),*],
                        Box::new(#ret_tokens)
                    )
                }
            }
            Self::Tuple(elems) => {
                let elem_tokens: Vec<_> =
                    elems.iter().map(|e| e.to_tokens(vars, ctx)).collect();
                quote! {
                    crate::typecheck::Ty::Tuple(vec![#(#elem_tokens),*])
                }
            }
            Self::Union(members) => {
                let member_tokens: Vec<_> =
                    members.iter().map(|m| m.to_tokens(vars, ctx)).collect();
                quote! {
                    crate::typecheck::Ty::Union(vec![#(#member_tokens),*])
                }
            }
            Self::Object(fields) => {
                let ctx = ctx.unwrap_or_else(|| {
                    panic!("object types require a context; use `scheme!(ctx; ...)`")
                });
                let field_entries: Vec<_> = fields
                    .iter()
                    .map(|(name, ty)| {
                        let ty_tokens = ty.to_tokens(vars, Some(ctx));
                        quote! { #ctx.intern(#name) => #ty_tokens }
                    })
                    .collect();
                quote! {
                    crate::typecheck::Ty::Object(indexmap::indexmap! {
                        #(#field_entries),*
                    })
                }
            }
            Self::Named(type_id) => {
                let id = Ident::new(type_id, proc_macro2::Span::call_site());
                quote! {
                    crate::typecheck::Ty::Named(crate::value::TypeId::#id, vec![])
                }
            }
            Self::Apply(var_name, args) => {
                let idx = vars.get(var_name).copied().unwrap_or_else(|| {
                    panic!("unbound type variable in Apply: `{var_name}`")
                });
                let arg_tokens: Vec<_> =
                    args.iter().map(|a| a.to_tokens(vars, ctx)).collect();
                quote! {
                    crate::typecheck::Ty::Apply(
                        crate::typecheck::TyVar::new(#idx),
                        vec![#(#arg_tokens),*]
                    )
                }
            }
        }
    }
}

/// Primitive type names (directly variants of `Ty`).
const PRIMITIVES: &[&str] = &[
    "Bool",
    "Int",
    "Float",
    "Char",
    "String",
    "Unit",
    "Time",
    "Range",
    "Json",
    "Unknown",
    "Error",
    "Ordering",
    "FilePath",
    "Path",
    "Word",
    "DataStatus",
    "Regex",
    "RuntimeError",
    "Local",
    "Global",
];

/// Parameterized type names (require `[...]` args).
const PARAMETERIZED: &[&str] = &["Array", "Option", "Result", "Map"];

/// Named builtin types (represented as `Ty::Named(TypeId::XXX, vec![])`).
///
/// These are builtin unions like `Storable`, `Subscript`, etc.
const NAMED: &[(&str, &str)] = &[
    ("Storable", "STORABLE"),
    ("Subscript", "SUBSCRIPT"),
    ("Scalar", "SCALAR"),
    ("Ref", "REF"),
];

fn is_primitive(s: &str) -> bool {
    PRIMITIVES.contains(&s)
}

fn is_parameterized(s: &str) -> bool {
    PARAMETERIZED.contains(&s)
}

fn named_type_id(s: &str) -> Option<&'static str> {
    NAMED.iter().find(|(name, _)| *name == s).map(|(_, id)| *id)
}

/// Parse a type expression.
fn parse_ty(input: ParseStream) -> Result<TyExpr> {
    parse_ty_union(input)
}

/// Parse union types: `A | B | C`.
fn parse_ty_union(input: ParseStream) -> Result<TyExpr> {
    // Parse first, then collect any `| T` continuations
    let first = parse_ty_fn(input)?;
    let rest: Vec<_> = std::iter::from_fn(|| {
        input.peek(Token![|]).then(|| {
            input.parse::<Token![|]>().ok()?;
            parse_ty_fn(input).ok()
        })?
    })
    .collect();

    if rest.is_empty() {
        Ok(first)
    } else {
        Ok(TyExpr::Union(std::iter::once(first).chain(rest).collect()))
    }
}

/// Parse function types: `(A, B) -> C`.
///
/// Function args MUST be parenthesized:
/// - `() -> T` is zero-param function
/// - `(T) -> U` is single-param function
/// - `(T, U) -> V` is two-param function
/// - `(T,)` is a 1-tuple (trailing comma)
/// - `(T, U)` NOT followed by `->` is a 2-tuple
fn parse_ty_fn(input: ParseStream) -> Result<TyExpr> {
    let (lhs, was_paren) = parse_ty_atom_track_paren(input)?;

    if input.peek(Token![->]) {
        input.parse::<Token![->]>()?;
        let ret = parse_ty_fn(input)?;
        // Require parentheses for function params
        if !was_paren {
            panic!("function arguments must be parenthesized: use `(T) -> U` not `T -> U`");
        }
        // If lhs is a tuple, treat as multi-param function
        // If lhs is Unit (from `()`), treat as zero-param function
        // If lhs is anything else, treat as single-param function
        let params = match lhs {
            TyExpr::Tuple(elems) => elems,
            TyExpr::Prim(ref s) if s == "Unit" => vec![],
            other => vec![other],
        };
        Ok(TyExpr::Fn(params, Box::new(ret)))
    } else {
        Ok(lhs)
    }
}

/// Parse an atomic type, tracking whether it was parenthesized.
fn parse_ty_atom_track_paren(input: ParseStream) -> Result<(TyExpr, bool)> {
    if input.peek(syn::token::Paren) {
        parse_paren_or_tuple(input).map(|ty| (ty, true))
    } else if input.peek(syn::token::Brace) {
        parse_object(input).map(|ty| (ty, false))
    } else {
        parse_ty_ident(input).map(|ty| (ty, false))
    }
}

/// Parse `(...)`: could be grouping, tuple, or function params.
///
/// - `()` -> Unit (zero-param fn if followed by `->`)
/// - `(T)` -> grouping (just T)
/// - `(T,)` -> 1-tuple
/// - `(T, U)` -> 2-tuple (or fn params if followed by `->`, handled by caller)
fn parse_paren_or_tuple(input: ParseStream) -> Result<TyExpr> {
    let content;
    syn::parenthesized!(content in input);

    // Parse comma-separated types, tracking trailing comma
    let elems: syn::punctuated::Punctuated<TyExpr, Token![,]> =
        syn::punctuated::Punctuated::parse_terminated_with(&content, parse_ty)?;
    let trailing = elems.trailing_punct();
    let mut elems: Vec<_> = elems.into_iter().collect();

    // `()` -> Unit
    // `(T)` -> grouping (just T)
    // `(T,)` -> 1-tuple (trailing comma forces tuple)
    // `(T, U, ...)` -> n-tuple
    match (elems.len(), trailing) {
        (0, _) => Ok(TyExpr::Prim("Unit".to_string())),
        (1, false) => Ok(elems.pop().unwrap_or_else(|| unreachable!())),
        _ => Ok(TyExpr::Tuple(elems)),
    }
}

/// Parse `{ field: Type, ... }`: object type literal.
fn parse_object(input: ParseStream) -> Result<TyExpr> {
    let content;
    syn::braced!(content in input);

    // Parse comma-separated field definitions: `name: Type`
    let fields: syn::punctuated::Punctuated<(String, Box<TyExpr>), Token![,]> =
        syn::punctuated::Punctuated::parse_terminated_with(&content, |input| {
            let name: Ident = input.parse()?;
            input.parse::<Token![:]>()?;
            let ty = parse_ty(input)?;
            Ok((name.to_string(), Box::new(ty)))
        })?;

    Ok(TyExpr::Object(fields.into_iter().collect()))
}

/// Parse bracketed type arguments: `[T, U, ...]`.
fn parse_bracketed_args(input: ParseStream) -> Result<Vec<TyExpr>> {
    let content;
    syn::bracketed!(content in input);
    syn::punctuated::Punctuated::<TyExpr, Token![,]>::parse_terminated_with(
        &content, parse_ty,
    )
    .map(|p| p.into_iter().collect())
}

/// Parse an identifier-based type: primitive, named, type var, or parameterized.
fn parse_ty_ident(input: ParseStream) -> Result<TyExpr> {
    let name = input.parse::<Ident>()?.to_string();

    if is_parameterized(&name) {
        parse_bracketed_args(input).map(|args| TyExpr::App(name, args))
    } else if is_primitive(&name) {
        Ok(TyExpr::Prim(name))
    } else if let Some(type_id) = named_type_id(&name) {
        Ok(TyExpr::Named(type_id.to_string()))
    } else if input.peek(syn::token::Bracket) {
        // Type variable with args: `F[U]` (higher-kinded application)
        parse_bracketed_args(input).map(|args| TyExpr::Apply(name, args))
    } else {
        Ok(TyExpr::Var(name))
    }
}

/// Parse a type when we've already consumed an ident (for the `forall` lookahead case).
fn parse_ty_starting_with(input: ParseStream, ident: Ident) -> Result<TyExpr> {
    let name = ident.to_string();

    let lhs = if is_parameterized(&name) {
        TyExpr::App(name, parse_bracketed_args(input)?)
    } else if is_primitive(&name) {
        TyExpr::Prim(name)
    } else if let Some(type_id) = named_type_id(&name) {
        TyExpr::Named(type_id.to_string())
    } else if input.peek(syn::token::Bracket) {
        // Type variable with args: `F[U]` (higher-kinded application)
        TyExpr::Apply(name, parse_bracketed_args(input)?)
    } else {
        TyExpr::Var(name)
    };

    // Continue parsing for `->` or `|`
    if input.peek(Token![->]) {
        input.parse::<Token![->]>()?;
        let ret = parse_ty_fn(input)?;
        let params = match lhs {
            TyExpr::Tuple(elems) => elems,
            other => vec![other],
        };
        Ok(TyExpr::Fn(params, Box::new(ret)))
    } else if input.peek(Token![|]) {
        // Collect union members
        let rest: Vec<_> = std::iter::from_fn(|| {
            input.peek(Token![|]).then(|| {
                input.parse::<Token![|]>().ok()?;
                parse_ty_fn(input).ok()
            })?
        })
        .collect();
        Ok(TyExpr::Union(std::iter::once(lhs).chain(rest).collect()))
    } else {
        Ok(lhs)
    }
}
