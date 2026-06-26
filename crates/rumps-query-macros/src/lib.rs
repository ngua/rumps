//! Proc macros for `rumps-query`.
//!
//! Provides the `scheme!` macro for declaring polymorphic type schemes
//! with a readable syntax like `forall T U. (Array[T], (T) -> U) -> Array[U]`.

use std::collections::HashMap;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use smallvec::SmallVec;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, LitStr, Result, Token};

/// Parse a type scheme with optional universal quantification and constraints.
///
/// # Syntax
///
/// ```text
/// scheme!(a, forall T U. (Array[T], (T) -> U) -> Array[U])
/// scheme!(a, forall I: Iterable. (I) -> Int)
/// scheme!(a, (Int) -> Bool)
/// scheme!(a, intern, ({ src: FilePath, dest: FilePath }) -> Unit)
/// ```
///
/// ## Class syntax
///
/// Type variables can have simple, HKT, or multi-param class constraints:
///
/// Simple classes (kind `*`, no type argument):
/// `T: Numeric` ; `T` satisfies the full numeric marker stack
/// `T: Additive` ; `T` supports additive identity and addition
/// `T: Subtractive` ; `T` supports subtraction
/// `T: Multiplicative` ; `T` supports multiplicative identity and multiplication
/// `T: Divisible` ; `T` supports division
/// `T: FloorDivisible` ; `T` supports floor division and modulo
/// `T: Powerable` ; `T` supports exponentiation
/// `T: Negatable` ; `T` must be `Int` or `Float`
/// `T: BitLike` ; `T` must be `Bool`, `Int`, or `Word`
/// `T: Concatable` ; `T` must be `String`, `Array[_]`, `Map[_, _]`, or `Option[_]`
/// `T: Iterable` ; `T` must be `Array[_]` or `Range`
/// `T: Default` ; `T` has a default value
/// `T: Ord` ; `T` must support ordering (`Bool`, `Int`, `Word`, `Float`, `Char`, `String`)
/// `T: Eq` ; `T` must support equality (`==`, `!=`)
/// `T: Display` ; `T` can be displayed as RUMPS syntax
/// `T: Default + Concatable` ; `T` has all listed class constraints
///
/// HKT classes (kind `* -> *`, no type argument in constraint; element at usage):
/// - `F: Fallible` ; `F` is a fallible type constructor; use `F[T]` in type position
/// - `W: Wrappable` ; `W` supports wrapping a value (`wrap`); use `W[T]` in type position
/// - `C: Chainable` ; `C` supports monadic chaining (`chain`); use `C[T]` in type position
/// - `M: Mappable` ; `M` is a functor; use `M[T]` in type position
/// - `F: Foldable` ; `F` supports fold/reduce; use `F[T]` in type position
/// - `F: Filterable` ; `F` supports filter; use `F[T]` in type position
///
/// Multi-param classes (require a type argument in brackets):
/// - `T: Into[U]` ; type `T` is convertible to type `U`
/// - `T: TryInto[U]` ; type `T` is fallibly convertible to type `U`
/// - `B: Indexable[E]` ; `B` supports indexing with element type `E`
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
/// - Objects: `{ field1: Type1, field2: Type2 }` (requires an interner; use `a, intern, ...`)
/// - Associated types: `T:Class:Assoc` (e.g., `B:Indexable:Index` for the index type of `B`)
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
    /// Parameterized class with type arguments: `Into[T]`, `Indexable[T]`, etc.
    Parameterized(String, SmallVec<[String; 2]>),
}

/// Parsed scheme input: optional context + optional `forall` + type.
struct SchemeInput {
    /// `TyArena` identifier used to allocate compound types.
    arena: Ident,
    /// Optional callable identifier for interning object field names.
    ctx: Option<Ident>,
    /// Type variables with class bounds: `(var_name, classes)`
    vars: Vec<(Ident, Vec<VarClass>)>,
    ty: TyExpr,
}

/// Simple class names (no type argument).
const SIMPLE_CLASSES: &[&str] = &[
    "Numeric",
    "Additive",
    "Subtractive",
    "Multiplicative",
    "Divisible",
    "FloorDivisible",
    "Powerable",
    "Negatable",
    "BitLike",
    "Concatable",
    "Iterable",
    "Default",
    "Ord",
    "Eq",
    "Display",
    "Formattable",
];

/// HKT classes (kind `* -> *`): constraint does NOT take `[T]`.
///
/// These are higher-kinded; the element type is specified at usage sites
/// (`F[T]` in type position), not in the constraint (`F: Fallible`).
const HKT_CLASSES: &[&str] = &[
    "Fallible",
    "Wrappable",
    "Chainable",
    "Mappable",
    "Foldable",
    "Filterable",
    "Bimappable",
];

/// Multi-param classes: constraint REQUIRES `[T]` arguments.
const MULTI_PARAM_CLASSES: &[&str] = &["Into", "TryInto", "Indexable"];

/// Parse a single class bound.
fn parse_var_class(input: ParseStream) -> Result<VarClass> {
    let class_name: Ident = input.parse()?;
    let cname = class_name.to_string();

    if SIMPLE_CLASSES.contains(&cname.as_str()) {
        Ok(VarClass::Simple(cname))
    } else if HKT_CLASSES.contains(&cname.as_str()) {
        // HKT class: NO type args in constraint
        Ok(VarClass::Simple(cname))
    } else if MULTI_PARAM_CLASSES.contains(&cname.as_str()) {
        // Multi-param class: parse bracketed, comma-separated type args
        let content;
        syn::bracketed!(content in input);
        let args: syn::punctuated::Punctuated<Ident, Token![,]> =
            syn::punctuated::Punctuated::parse_terminated(&content)?;
        let args: SmallVec<[String; 2]> =
            args.into_iter().map(|id| id.to_string()).collect();
        Ok(VarClass::Parameterized(cname, args))
    } else {
        panic!(
            "unknown class: `{cname}`; use one of {:?}, {:?}, or {:?}",
            SIMPLE_CLASSES, HKT_CLASSES, MULTI_PARAM_CLASSES
        )
    }
}

/// Parse class bounds after `:`.
fn parse_var_classes(input: ParseStream) -> Result<Vec<VarClass>> {
    let class = parse_var_class(input)?;
    if input.peek(Token![+]) {
        input.parse::<Token![+]>()?;
        parse_var_classes(input).map(|tail| {
            let mut classes = vec![class];
            classes.extend(tail);
            classes
        })
    } else {
        Ok(vec![class])
    }
}

/// Parse a single type variable with optional class bounds.
///
/// Syntax:
/// - `T` (no class)
/// - `T: Numeric` (simple class)
/// - `F: Fallible` (HKT class; element type at usage via `F[T]`)
/// - `T: Into[U]` (multi-param class)
/// - `T: Default + Concatable` (multiple class bounds)
fn parse_type_var(input: ParseStream) -> Result<(Ident, Vec<VarClass>)> {
    let name: Ident = input.parse()?;

    // Check for class: `: Class`, `: Class[T]`, or `: Class + Class`
    let classes = if input.peek(Token![:]) {
        input.parse::<Token![:]>()?;
        parse_var_classes(input)?
    } else {
        Vec::new()
    };

    Ok((name, classes))
}

fn parse_type_vars(input: ParseStream) -> Result<Vec<(Ident, Vec<VarClass>)>> {
    if input.peek(Token![.]) {
        Ok(Vec::new())
    } else {
        let var = parse_type_var(input)?;
        let _ = input.parse::<Token![,]>();
        parse_type_vars(input)
            .map(|tail| std::iter::once(var).chain(tail).collect())
    }
}

impl Parse for SchemeInput {
    fn parse(input: ParseStream) -> Result<Self> {
        let arena: Ident = input.parse()?;
        input.parse::<Token![,]>()?;
        let ctx = if input.peek(Ident) && input.peek2(Token![,]) {
            let ctx = input.parse()?;
            input.parse::<Token![,]>()?;
            Some(ctx)
        } else {
            None
        };

        // Check for `forall` keyword
        if input.peek(Ident) && input.peek2(Ident) {
            let kw: Ident = input.parse()?;
            if kw == "forall" {
                // Parse type variables with optional constraints until `.`.
                let vars = parse_type_vars(input)?;
                input.parse::<Token![.]>()?;
                let ty = parse_ty(input)?;
                Ok(Self {
                    arena,
                    ctx,
                    vars,
                    ty,
                })
            } else {
                // Not `forall`; the ident we parsed is part of the type
                let ty = parse_ty_starting_with(input, kw)?;
                Ok(Self {
                    arena,
                    ctx,
                    vars: vec![],
                    ty,
                })
            }
        } else {
            let ty = parse_ty(input)?;
            Ok(Self {
                arena,
                ctx,
                vars: vec![],
                ty,
            })
        }
    }
}

fn class_id(name: &str) -> TokenStream2 {
    let id = match name {
        "Numeric" => "NUMERIC",
        "Additive" => "ADDITIVE",
        "Subtractive" => "SUBTRACTIVE",
        "Multiplicative" => "MULTIPLICATIVE",
        "Divisible" => "DIVISIBLE",
        "FloorDivisible" => "FLOOR_DIVISIBLE",
        "Powerable" => "POWERABLE",
        "Iterable" => "ITERABLE",
        "Concatable" => "CONCATABLE",
        "BitLike" => "BIT_LIKE",
        "Negatable" => "NEGATABLE",
        "Default" => "DEFAULT",
        "Fallible" => "FALLIBLE",
        "Into" => "INTO",
        "TryInto" => "TRY_INTO",
        "Indexable" => "INDEXABLE",
        "Ord" => "ORD",
        "Mappable" => "MAPPABLE",
        "Foldable" => "FOLDABLE",
        "Filterable" => "FILTERABLE",
        "Display" => "DISPLAY",
        "Formattable" => "FORMATTABLE",
        "Eq" => "EQ",
        "Wrappable" => "WRAPPABLE",
        "Chainable" => "CHAINABLE",
        "Bimappable" => "BIMAPPABLE",
        _ => panic!("unknown class: `{name}`"),
    };
    let id = Ident::new(id, proc_macro2::Span::call_site());
    quote! { crate::ClassId::#id }
}

fn primitive_id(name: &str) -> TokenStream2 {
    let id = match name {
        "Bool" => "BOOL",
        "Int" => "INT",
        "Word" => "WORD",
        "Float" => "FLOAT",
        "Char" => "CHAR",
        "String" => "STRING",
        "Unit" => "UNIT",
        "Time" => "TIME",
        "Range" => "RANGE",
        "Json" => "JSON",
        "Ordering" => "ORDERING",
        "DataStatus" => "DATA_STATUS",
        "FilePath" => "FILEPATH",
        "Path" => "PATH",
        "Regex" => "REGEX",
        "RuntimeError" => "RUNTIME_ERROR",
        "Local" => "LOCAL",
        "Global" => "GLOBAL",
        "Unknown" => "UNKNOWN",
        "Error" => "ERROR",
        _ => panic!("unknown primitive type: `{name}`"),
    };
    let id = Ident::new(id, proc_macro2::Span::call_site());
    quote! { crate::typecheck::TyArena::#id }
}

fn named_id(name: &str) -> TokenStream2 {
    let id = Ident::new(name, proc_macro2::Span::call_site());
    quote! { crate::typecheck::TyArena::#id }
}

fn bind(
    stmts: &mut Vec<TokenStream2>,
    n: &mut usize,
    expr: TokenStream2,
) -> TokenStream2 {
    let id = format_ident!("__rumps_ty_{n}");
    *n += 1;
    stmts.push(quote! { let #id = #expr; });
    quote! { #id }
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

        let arena = &self.arena;
        let mut n = 0;
        let ty = self.ty.to_ty_id(arena, &var_map, self.ctx.as_ref(), &mut n);
        let mut stmts = ty.stmts;
        let ty_expr = ty.expr;

        if self.vars.is_empty() {
            quote! {
                {
                    #(#stmts)*
                    crate::typecheck::Scheme::mono(#ty_expr)
                }
            }
        } else {
            let var_indices: Vec<u32> = (0..self.vars.len() as u32).collect();

            // Generate class entries: (TyVar, Class) tuples
            let class_entries: Vec<TokenStream2> = self
                .vars
                .iter()
                .enumerate()
                .flat_map(|(i, (_, classes))| {
                    classes.iter().map(|c| {
                        let var_idx = i as u32;
                        match c {
                            VarClass::Simple(name) => {
                                let id = class_id(name);
                                let class_tokens =
                                    if HKT_CLASSES.contains(&name.as_str()) {
                                        quote! {
                                            crate::typecheck::TypeClass::hkt(#id)
                                        }
                                    } else {
                                        quote! {
                                            crate::typecheck::TypeClass::simple(#id)
                                        }
                                    };
                                quote! {
                                    (
                                        crate::typecheck::TyVar::new(#var_idx),
                                        #class_tokens
                                    )
                                }
                            }
                            VarClass::Parameterized(name, args) => {
                                let id = class_id(name);
                                let arg_tokens: Vec<TokenStream2> = args
                                    .iter()
                                    .map(|arg| {
                                        let idx =
                                            var_map.get(arg).copied().unwrap_or_else(
                                                || {
                                                    panic!(
                                                    "unbound type variable in class: `{arg}`"
                                                )
                                                },
                                            );
                                        bind(
                                            &mut stmts,
                                            &mut n,
                                            quote! { #arena.var(#idx) },
                                        )
                                    })
                                    .collect();
                                let class_tokens = match arg_tokens.as_slice() {
                                    [arg] => {
                                        quote! { crate::typecheck::TypeClass::param(#id, #arg) }
                                    }
                                    _ => panic!(
                                        "class `{name}` expects exactly one type argument"
                                    ),
                                };
                                quote! {
                                    (
                                        crate::typecheck::TyVar::new(#var_idx),
                                        #class_tokens
                                    )
                                }
                            }
                        }
                    }).collect::<Vec<_>>()
                })
                .collect();

            quote! {
                {
                    #(#stmts)*
                    crate::typecheck::Scheme {
                        vars: smallvec::smallvec![
                            #(crate::typecheck::TyVar::new(#var_indices)),*
                        ],
                        ty: #ty_expr,
                        constraints: smallvec::smallvec![#(#class_entries),*],
                    }
                }
            }
        }
    }
}

struct TyBuild {
    stmts: Vec<TokenStream2>,
    expr: TokenStream2,
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
    /// Associated type projection: `T:Class:Assoc` (e.g., `B:Indexable:Index`).
    ///
    /// The type variable `T` must have a `Class` constraint, and `Assoc` is the
    /// name of the associated type defined by that class.
    AssocType(String, String, String),
}

impl TyExpr {
    fn to_ty_id(
        &self,
        arena: &Ident,
        vars: &HashMap<String, u32>,
        ctx: Option<&Ident>,
        n: &mut usize,
    ) -> TyBuild {
        match self {
            Self::Prim(name) => TyBuild {
                stmts: vec![],
                expr: primitive_id(name),
            },
            Self::Var(name) => {
                let idx = vars.get(name).copied().unwrap_or_else(|| {
                    panic!("unbound type variable: `{name}`")
                });
                let mut stmts = Vec::new();
                let expr = bind(&mut stmts, n, quote! { #arena.var(#idx) });
                TyBuild { stmts, expr }
            }
            Self::App(name, args) => {
                let (mut stmts, arg_tokens): (Vec<_>, Vec<_>) = args
                    .iter()
                    .map(|arg| arg.to_ty_id(arena, vars, ctx, n))
                    .fold((Vec::new(), Vec::new()), |(mut ss, mut es), b| {
                        ss.extend(b.stmts);
                        es.push(b.expr);
                        (ss, es)
                    });
                let alloc = match name.as_str() {
                    "Array" => {
                        let inner = &arg_tokens[0];
                        quote! { #arena.array(#inner) }
                    }
                    "Option" => {
                        let inner = &arg_tokens[0];
                        quote! { #arena.option(#inner) }
                    }
                    "Result" => {
                        let ok = &arg_tokens[0];
                        let err = &arg_tokens[1];
                        quote! { #arena.result(#ok, #err) }
                    }
                    "Map" => {
                        let k = &arg_tokens[0];
                        let v = &arg_tokens[1];
                        quote! { #arena.map_ty(#k, #v) }
                    }
                    _ => panic!("unknown parameterized type: `{name}`"),
                };
                let expr = bind(&mut stmts, n, alloc);
                TyBuild { stmts, expr }
            }
            Self::Fn(params, ret) => {
                let (mut stmts, param_tokens): (Vec<_>, Vec<_>) = params
                    .iter()
                    .map(|p| p.to_ty_id(arena, vars, ctx, n))
                    .fold((Vec::new(), Vec::new()), |(mut ss, mut es), b| {
                        ss.extend(b.stmts);
                        es.push(b.expr);
                        (ss, es)
                    });
                let ret = ret.to_ty_id(arena, vars, ctx, n);
                stmts.extend(ret.stmts);
                let ret_tokens = ret.expr;
                let expr = bind(
                    &mut stmts,
                    n,
                    quote! {
                        #arena.func(
                            smallvec::smallvec![#(#param_tokens),*],
                            #ret_tokens
                        )
                    },
                );
                TyBuild { stmts, expr }
            }
            Self::Tuple(elems) => {
                let (mut stmts, elem_tokens): (Vec<_>, Vec<_>) = elems
                    .iter()
                    .map(|e| e.to_ty_id(arena, vars, ctx, n))
                    .fold((Vec::new(), Vec::new()), |(mut ss, mut es), b| {
                        ss.extend(b.stmts);
                        es.push(b.expr);
                        (ss, es)
                    });
                let expr = bind(
                    &mut stmts,
                    n,
                    quote! {
                        #arena.alloc(
                            crate::typecheck::Ty::Tuple(
                                smallvec::smallvec![#(#elem_tokens),*]
                            )
                        )
                    },
                );
                TyBuild { stmts, expr }
            }
            Self::Union(members) => {
                let (mut stmts, member_tokens): (Vec<_>, Vec<_>) = members
                    .iter()
                    .map(|m| m.to_ty_id(arena, vars, ctx, n))
                    .fold((Vec::new(), Vec::new()), |(mut ss, mut es), b| {
                        ss.extend(b.stmts);
                        es.push(b.expr);
                        (ss, es)
                    });
                let expr = bind(
                    &mut stmts,
                    n,
                    quote! {
                        #arena.alloc(
                            crate::typecheck::Ty::Union(
                                None,
                                smallvec::smallvec![#(#member_tokens),*]
                            )
                        )
                    },
                );
                TyBuild { stmts, expr }
            }
            Self::Object(fields) => {
                let ctx = ctx.unwrap_or_else(|| {
                    panic!("object types require a context; use `scheme!(a, ctx, ...)`")
                });
                let (mut stmts, field_entries): (Vec<_>, Vec<_>) = fields
                    .iter()
                    .map(|(name, ty)| {
                        let field = format_ident!("__rumps_field_{n}");
                        *n += 1;
                        let name = LitStr::new(
                            name.as_str(),
                            proc_macro2::Span::call_site(),
                        );
                        let ty = ty.to_ty_id(arena, vars, Some(ctx), n);
                        let ty_expr = ty.expr;
                        let mut ss = ty.stmts;
                        ss.push(quote! { let #field = #ctx(#name); });
                        (ss, quote! { #field => #ty_expr })
                    })
                    .fold((Vec::new(), Vec::new()), |(mut ss, mut es), v| {
                        ss.extend(v.0);
                        es.push(v.1);
                        (ss, es)
                    });
                let expr = bind(
                    &mut stmts,
                    n,
                    quote! {
                        #arena.alloc(
                            crate::typecheck::Ty::Object(indexmap::indexmap! {
                                #(#field_entries),*
                            })
                        )
                    },
                );
                TyBuild { stmts, expr }
            }
            Self::Named(type_id) => TyBuild {
                stmts: vec![],
                expr: named_id(type_id),
            },
            Self::Apply(var_name, args) => {
                let idx = vars.get(var_name).copied().unwrap_or_else(|| {
                    panic!("unbound type variable in Apply: `{var_name}`")
                });
                let (mut stmts, arg_tokens): (Vec<_>, Vec<_>) = args
                    .iter()
                    .map(|a| a.to_ty_id(arena, vars, ctx, n))
                    .fold((Vec::new(), Vec::new()), |(mut ss, mut es), b| {
                        ss.extend(b.stmts);
                        es.push(b.expr);
                        (ss, es)
                    });
                let expr = bind(
                    &mut stmts,
                    n,
                    quote! {
                        #arena.hkt(
                            crate::typecheck::TyVar::new(#idx),
                            smallvec::smallvec![#(#arg_tokens),*]
                        )
                    },
                );
                TyBuild { stmts, expr }
            }
            Self::AssocType(var_name, class_name, assoc_name) => {
                let ctx = ctx.unwrap_or_else(|| {
                    panic!("associated types require a context; use `scheme!(a, ctx, ...)`")
                });
                let idx = vars.get(var_name).copied().unwrap_or_else(|| {
                    panic!("unbound type variable in associated type: `{var_name}`")
                });
                let class = class_id(class_name);
                let assoc = format_ident!("__rumps_assoc_{n}");
                *n += 1;
                let expr = format_ident!("__rumps_ty_{n}");
                *n += 1;
                let assoc_name = LitStr::new(
                    assoc_name.as_str(),
                    proc_macro2::Span::call_site(),
                );
                TyBuild {
                    stmts: vec![
                        quote! { let #assoc = #ctx(#assoc_name); },
                        quote! {
                            let #expr = #arena.alloc(
                                crate::typecheck::Ty::AssocType(
                                    crate::typecheck::TyVar::new(#idx),
                                    #class,
                                    #assoc
                                )
                            );
                        },
                    ],
                    expr: quote! { #expr },
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
        let ret = parse_ty_union(input)?;
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

/// Check if an identifier is a known class name (for associated type parsing).
fn is_class_name(s: &str) -> bool {
    SIMPLE_CLASSES.contains(&s)
        || HKT_CLASSES.contains(&s)
        || MULTI_PARAM_CLASSES.contains(&s)
}

/// Parse an identifier-based type: primitive, named, type var, parameterized, or assoc type.
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
    } else if input.peek(Token![:]) && input.peek2(Ident) {
        // Potential associated type: `T:Class:Assoc`
        let fork = input.fork();
        fork.parse::<Token![:]>()?;
        let maybe_class: Ident = fork.parse()?;
        let class_name = maybe_class.to_string();
        if is_class_name(&class_name)
            && fork.peek(Token![:])
            && fork.peek2(Ident)
        {
            fork.parse::<Token![:]>()?;
            let assoc: Ident = fork.parse()?;
            // Commit to forked state
            input.parse::<Token![:]>()?;
            input.parse::<Ident>()?;
            input.parse::<Token![:]>()?;
            input.parse::<Ident>()?;
            Ok(TyExpr::AssocType(name, class_name, assoc.to_string()))
        } else {
            Ok(TyExpr::Var(name))
        }
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
    } else if input.peek(Token![:]) && input.peek2(Ident) {
        // Potential associated type: `T:Class:Assoc`
        let fork = input.fork();
        fork.parse::<Token![:]>()?;
        let maybe_class: Ident = fork.parse()?;
        let class_name = maybe_class.to_string();
        if is_class_name(&class_name)
            && fork.peek(Token![:])
            && fork.peek2(Ident)
        {
            fork.parse::<Token![:]>()?;
            let assoc: Ident = fork.parse()?;
            // Commit to forked state
            input.parse::<Token![:]>()?;
            input.parse::<Ident>()?;
            input.parse::<Token![:]>()?;
            input.parse::<Ident>()?;
            TyExpr::AssocType(name, class_name, assoc.to_string())
        } else {
            TyExpr::Var(name)
        }
    } else {
        TyExpr::Var(name)
    };

    // Continue parsing for `->` or `|`
    if input.peek(Token![->]) {
        input.parse::<Token![->]>()?;
        let ret = parse_ty_union(input)?;
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
