//! Type representation for static type checking.
//!
//! Defines the core types: `Ty` (types), `TyVar` (type variables), `Scheme`
//! (polymorphic type schemes), and `Subst` (type substitutions).

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;

use crate::intern::StringId;
use crate::TypeId;

/// A type variable; placeholder for an unknown type during inference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TyVar(u32);

impl TyVar {
    /// Create a new type variable with the given index.
    pub(crate) const fn new(idx: u32) -> Self {
        Self(idx)
    }

    /// Get the index of this type variable.
    pub(crate) const fn idx(self) -> u32 {
        self.0
    }
}

/// Static types used during type checking.
///
/// Unlike runtime `TypeExpr`, these include type variables (`Var`) for
/// inference and structural object types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ty {
    /// Unresolved type variable (from inference).
    Var(TyVar),

    // Primitives
    Bool,
    Int,
    Float,
    Char,
    String,
    Unit,
    Time,
    Range,
    Json,
    Ordering,
    FilePath,
    Path,
    Regex,

    // Parameterized builtins
    Array(Box<Self>),
    Option(Box<Self>),
    Result(Box<Self>, Box<Self>),
    Map(Box<Self>, Box<Self>),

    // Compound types
    Tuple(Vec<Self>),
    Fn(Vec<Self>, Box<Self>),

    /// Anonymous structural record; compatible if fields match.
    Object(IndexMap<StringId, Self>),

    /// Anonymous union type; value is one of the member types.
    ///
    /// For inline `Int | String` syntax. Named unions (`Storable`, `Scalar`,
    /// user-defined `UNION`) use `Named(TypeId, params)` instead.
    ///
    /// # Why Named Unions Are Separate
    ///
    /// Named unions preserve nominal identity, which matters for:
    ///
    /// 1. **Special `AS` semantics**: `Storable` has infallible `AS` casts that
    ///    may fail at runtime with `Error::RuntimeType`. Anonymous unions don't
    ///    have this special case; `x AS T` on an anonymous union is a static error.
    ///
    /// 2. **Type parameters**: Named unions can be generic (`UNION F[T] = Int | Option[T]`),
    ///    requiring parameter substitution during type checking.
    ///
    /// 3. **Error messages**: Named unions display their registered name (`Storable`)
    ///    rather than the expanded member list.
    Union(Vec<Self>),

    /// User-defined type (sum types, structs, unions) with type parameters.
    Named(TypeId, Vec<Self>),

    /// Unresolved; database reads before inference narrows.
    Unknown,

    /// Error recovery sentinel; unifies with anything.
    Error,
}

impl Ty {
    /// Member types of the `Storable` union: values that can be stored in the database.
    ///
    /// Matches `UNION Storable = Bool | Int | Float | Char | String | Json`.
    pub(crate) const STORABLE_MEMBERS: &'static [Self] = &[
        Self::Bool,
        Self::Int,
        Self::Float,
        Self::Char,
        Self::String,
        Self::Json,
    ];

    /// Member types of the `Scalar` union: JSON scalar extraction results.
    ///
    /// Matches `UNION Scalar = Bool | Int | Float | String`.
    /// Excludes `Char` (JSON has no char type) and `Null` (handled by `Option`).
    pub(crate) const SCALAR_MEMBERS: &'static [Self] =
        &[Self::Bool, Self::Int, Self::Float, Self::String];

    /// Construct a function type: `Fn([A, B, ...], R)`.
    pub(crate) fn func(params: impl Into<Vec<Self>>, ret: Self) -> Self {
        Self::Fn(params.into(), Box::new(ret))
    }

    /// Collect all free type variables in this type.
    pub(crate) fn free_vars(&self) -> HashSet<TyVar> {
        let mut acc = HashSet::new();
        self.collect_free_vars(&mut acc);
        acc
    }

    fn collect_free_vars(&self, acc: &mut HashSet<TyVar>) {
        match self {
            Self::Var(v) => {
                acc.insert(*v);
            }
            Self::Bool
            | Self::Int
            | Self::Float
            | Self::Char
            | Self::String
            | Self::Unit
            | Self::Time
            | Self::Range
            | Self::Json
            | Self::Ordering
            | Self::FilePath
            | Self::Path
            | Self::Regex
            | Self::Unknown
            | Self::Error => {}
            Self::Array(t) | Self::Option(t) => t.collect_free_vars(acc),
            Self::Result(ok, err) => {
                ok.collect_free_vars(acc);
                err.collect_free_vars(acc);
            }
            Self::Map(k, v) => {
                k.collect_free_vars(acc);
                v.collect_free_vars(acc);
            }
            Self::Tuple(ts) => ts.iter().for_each(|t| t.collect_free_vars(acc)),
            Self::Fn(params, ret) => {
                params.iter().for_each(|t| t.collect_free_vars(acc));
                ret.collect_free_vars(acc);
            }
            Self::Object(fields) => {
                fields.values().for_each(|t| t.collect_free_vars(acc));
            }
            Self::Union(members) => {
                members.iter().for_each(|t| t.collect_free_vars(acc));
            }
            Self::Named(_, args) => {
                args.iter().for_each(|t| t.collect_free_vars(acc));
            }
        }
    }

    /// Check if type variable `v` occurs anywhere in this type (occurs check).
    pub(crate) fn occurs(&self, v: TyVar) -> bool {
        match self {
            Self::Var(w) => *w == v,
            Self::Bool
            | Self::Int
            | Self::Float
            | Self::Char
            | Self::String
            | Self::Unit
            | Self::Time
            | Self::Range
            | Self::Json
            | Self::Ordering
            | Self::FilePath
            | Self::Path
            | Self::Regex
            | Self::Unknown
            | Self::Error => false,
            Self::Array(t) | Self::Option(t) => t.occurs(v),
            Self::Result(ok, err) => ok.occurs(v) || err.occurs(v),
            Self::Map(k, val) => k.occurs(v) || val.occurs(v),
            Self::Tuple(ts) => ts.iter().any(|t| t.occurs(v)),
            Self::Fn(params, ret) => {
                params.iter().any(|t| t.occurs(v)) || ret.occurs(v)
            }
            Self::Object(fields) => fields.values().any(|t| t.occurs(v)),
            Self::Union(members) => members.iter().any(|t| t.occurs(v)),
            Self::Named(_, args) => args.iter().any(|t| t.occurs(v)),
        }
    }

    /// Apply a substitution, replacing type variables with their bindings.
    pub(crate) fn apply(&self, subst: &Subst) -> Self {
        match self {
            Self::Var(v) => subst
                .0
                .get(v)
                .map_or_else(|| self.clone(), |t| t.apply(subst)),
            Self::Bool => Self::Bool,
            Self::Int => Self::Int,
            Self::Float => Self::Float,
            Self::Char => Self::Char,
            Self::String => Self::String,
            Self::Unit => Self::Unit,
            Self::Time => Self::Time,
            Self::Range => Self::Range,
            Self::Json => Self::Json,
            Self::Ordering => Self::Ordering,
            Self::FilePath => Self::FilePath,
            Self::Path => Self::Path,
            Self::Regex => Self::Regex,
            Self::Unknown => Self::Unknown,
            Self::Error => Self::Error,
            Self::Array(t) => Self::Array(Box::new(t.apply(subst))),
            Self::Option(t) => Self::Option(Box::new(t.apply(subst))),
            Self::Result(ok, err) => Self::Result(
                Box::new(ok.apply(subst)),
                Box::new(err.apply(subst)),
            ),
            Self::Map(k, v) => {
                Self::Map(Box::new(k.apply(subst)), Box::new(v.apply(subst)))
            }
            Self::Tuple(ts) => {
                Self::Tuple(ts.iter().map(|t| t.apply(subst)).collect())
            }
            Self::Fn(params, ret) => Self::Fn(
                params.iter().map(|t| t.apply(subst)).collect(),
                Box::new(ret.apply(subst)),
            ),
            Self::Object(fields) => Self::Object(
                fields.iter().map(|(k, t)| (*k, t.apply(subst))).collect(),
            ),
            Self::Union(members) => {
                Self::Union(members.iter().map(|t| t.apply(subst)).collect())
            }
            Self::Named(id, args) => {
                Self::Named(*id, args.iter().map(|t| t.apply(subst)).collect())
            }
        }
    }
}

/// A polymorphic type scheme: `forall vars. ty`.
///
/// For example, `forall a. Array[a] -> Int` is the scheme for `Array.length`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Scheme {
    /// Universally quantified type variables.
    pub(crate) vars: Vec<TyVar>,
    /// The body type (may contain the quantified variables).
    pub(crate) ty: Ty,
}

impl Scheme {
    /// Create a monomorphic scheme (no quantified variables).
    pub(crate) fn mono(ty: Ty) -> Self {
        Self { vars: vec![], ty }
    }

    /// Polymorphic with 1 type variable: `forall T. ...`
    pub(crate) fn poly(f: impl FnOnce(Ty) -> Ty) -> Self {
        let t = Ty::Var(TyVar(0));
        Self {
            vars: vec![TyVar(0)],
            ty: f(t),
        }
    }

    /// Polymorphic with 2 type variables: `forall T U. ...`
    pub(crate) fn poly2(f: impl FnOnce(Ty, Ty) -> Ty) -> Self {
        let t = Ty::Var(TyVar(0));
        let u = Ty::Var(TyVar(1));
        Self {
            vars: vec![TyVar(0), TyVar(1)],
            ty: f(t, u),
        }
    }

    /// Polymorphic with 3 type variables: `forall T U V. ...`
    pub(crate) fn poly3(f: impl FnOnce(Ty, Ty, Ty) -> Ty) -> Self {
        let t = Ty::Var(TyVar(0));
        let u = Ty::Var(TyVar(1));
        let v = Ty::Var(TyVar(2));
        Self {
            vars: vec![TyVar(0), TyVar(1), TyVar(2)],
            ty: f(t, u, v),
        }
    }

    /// Instantiate the scheme with fresh type variables.
    ///
    /// Takes a mutable counter for generating fresh `TyVar`s. Returns a
    /// concrete `Ty` with all quantified variables replaced by fresh ones.
    pub(crate) fn instantiate(&self, next: &mut u32) -> Ty {
        if self.vars.is_empty() {
            self.ty.clone()
        } else {
            let subst = Subst(
                self.vars
                    .iter()
                    .filter_map(|v| {
                        let fresh = TyVar::new(*next);
                        *next += 1;
                        // Skip identity mappings (v -> Var(v)) to avoid
                        // infinite recursion in apply
                        (*v != fresh).then_some((*v, Ty::Var(fresh)))
                    })
                    .collect(),
            );
            self.ty.apply(&subst)
        }
    }

    /// Apply a substitution to the scheme's body.
    ///
    /// Only substitutes free variables; quantified ones are shadowed.
    pub(crate) fn apply(&self, subst: &Subst) -> Self {
        let filtered = Subst(
            subst
                .0
                .iter()
                .filter(|(v, _)| !self.vars.contains(v))
                .map(|(v, t)| (*v, t.clone()))
                .collect(),
        );
        Self {
            vars: self.vars.clone(),
            ty: self.ty.apply(&filtered),
        }
    }

    /// Collect free type variables (excludes quantified variables).
    pub(crate) fn free_vars(&self) -> HashSet<TyVar> {
        let mut fv = self.ty.free_vars();
        self.vars.iter().for_each(|v| {
            fv.remove(v);
        });
        fv
    }
}

/// A substitution mapping type variables to types.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Subst(pub(crate) HashMap<TyVar, Ty>);

impl Subst {
    /// Empty substitution.
    pub(crate) fn empty() -> Self {
        Self(HashMap::new())
    }

    /// Substitution mapping a single variable.
    pub(crate) fn singleton(v: TyVar, ty: Ty) -> Self {
        Self(std::iter::once((v, ty)).collect())
    }

    /// Apply this substitution to a type.
    pub(crate) fn apply(&self, ty: &Ty) -> Ty {
        ty.apply(self)
    }

    /// Compose two substitutions: `self . other`.
    ///
    /// Applying the result is equivalent to applying `other` then `self`.
    pub(crate) fn compose(&self, other: &Self) -> Self {
        let applied: HashMap<TyVar, Ty> =
            other.0.iter().map(|(v, t)| (*v, t.apply(self))).collect();
        let mut merged = self.0.clone();
        applied.into_iter().for_each(|(v, t)| {
            merged.entry(v).or_insert(t);
        });
        Self(merged)
    }

    /// Extend this substitution with a new binding.
    pub(crate) fn extend(&mut self, v: TyVar, ty: Ty) {
        self.0.insert(v, ty);
    }

    /// Check if this substitution is empty.
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_vars_primitive() {
        assert!(Ty::Int.free_vars().is_empty());
        assert!(Ty::Bool.free_vars().is_empty());
    }

    #[test]
    fn free_vars_var() {
        let v = TyVar::new(0);
        let fv = Ty::Var(v).free_vars();
        assert!(fv.contains(&v));
        assert_eq!(fv.len(), 1);
    }

    #[test]
    fn free_vars_array() {
        let v = TyVar::new(1);
        let arr = Ty::Array(Box::new(Ty::Var(v)));
        let fv = arr.free_vars();
        assert!(fv.contains(&v));
    }

    #[test]
    fn free_vars_fn() {
        let a = TyVar::new(0);
        let b = TyVar::new(1);
        let f = Ty::Fn(vec![Ty::Var(a)], Box::new(Ty::Var(b)));
        let fv = f.free_vars();
        assert!(fv.contains(&a));
        assert!(fv.contains(&b));
        assert_eq!(fv.len(), 2);
    }

    #[test]
    fn occurs_check() {
        let v = TyVar::new(0);
        assert!(Ty::Var(v).occurs(v));
        assert!(!Ty::Int.occurs(v));
        assert!(Ty::Array(Box::new(Ty::Var(v))).occurs(v));
        assert!(!Ty::Array(Box::new(Ty::Int)).occurs(v));
    }

    #[test]
    fn apply_subst_var() {
        let v = TyVar::new(0);
        let subst = Subst::singleton(v, Ty::Int);
        assert_eq!(Ty::Var(v).apply(&subst), Ty::Int);
    }

    #[test]
    fn apply_subst_nested() {
        let v = TyVar::new(0);
        let subst = Subst::singleton(v, Ty::String);
        let arr = Ty::Array(Box::new(Ty::Var(v)));
        assert_eq!(arr.apply(&subst), Ty::Array(Box::new(Ty::String)));
    }

    #[test]
    fn apply_subst_no_match() {
        let v = TyVar::new(0);
        let w = TyVar::new(1);
        let subst = Subst::singleton(v, Ty::Int);
        assert_eq!(Ty::Var(w).apply(&subst), Ty::Var(w));
    }

    #[test]
    fn scheme_mono() {
        let s = Scheme::mono(Ty::Int);
        assert!(s.vars.is_empty());
        assert_eq!(s.ty, Ty::Int);
    }

    #[test]
    fn scheme_instantiate() {
        let v = TyVar::new(0);
        let s = Scheme {
            vars: vec![v],
            ty: Ty::Array(Box::new(Ty::Var(v))),
        };
        let mut next = 100;
        let inst = s.instantiate(&mut next);
        // Should have replaced `v` with fresh var `TyVar::new(100)`
        assert_eq!(next, 101);
        assert_eq!(inst, Ty::Array(Box::new(Ty::Var(TyVar::new(100)))));
    }

    #[test]
    fn scheme_free_vars_excludes_bound() {
        let a = TyVar::new(0);
        let b = TyVar::new(1);
        let s = Scheme {
            vars: vec![a],
            ty: Ty::Fn(vec![Ty::Var(a)], Box::new(Ty::Var(b))),
        };
        let fv = s.free_vars();
        assert!(!fv.contains(&a)); // bound
        assert!(fv.contains(&b)); // free
    }

    #[test]
    fn subst_compose() {
        let a = TyVar::new(0);
        let b = TyVar::new(1);
        // s1: a -> Int
        // s2: b -> a
        // composed: b -> Int, a -> Int
        let s1 = Subst::singleton(a, Ty::Int);
        let s2 = Subst::singleton(b, Ty::Var(a));
        let composed = s1.compose(&s2);
        assert_eq!(composed.apply(&Ty::Var(b)), Ty::Int);
        assert_eq!(composed.apply(&Ty::Var(a)), Ty::Int);
    }

    #[test]
    fn subst_extend() {
        let a = TyVar::new(0);
        let b = TyVar::new(1);
        let mut s = Subst::singleton(a, Ty::Int);
        s.extend(b, Ty::String);
        assert_eq!(s.apply(&Ty::Var(a)), Ty::Int);
        assert_eq!(s.apply(&Ty::Var(b)), Ty::String);
    }

    // --- Union type tests ---

    #[test]
    fn union_free_vars_empty() {
        let u = Ty::Union(vec![Ty::Int, Ty::String]);
        assert!(u.free_vars().is_empty());
    }

    #[test]
    fn union_free_vars_with_var() {
        let v = TyVar::new(0);
        let u = Ty::Union(vec![Ty::Int, Ty::Var(v), Ty::String]);
        let fv = u.free_vars();
        assert!(fv.contains(&v));
        assert_eq!(fv.len(), 1);
    }

    #[test]
    fn union_free_vars_multiple_vars() {
        let a = TyVar::new(0);
        let b = TyVar::new(1);
        let u = Ty::Union(vec![Ty::Var(a), Ty::Var(b)]);
        let fv = u.free_vars();
        assert!(fv.contains(&a));
        assert!(fv.contains(&b));
        assert_eq!(fv.len(), 2);
    }

    #[test]
    fn union_occurs_positive() {
        let v = TyVar::new(0);
        let u = Ty::Union(vec![Ty::Int, Ty::Var(v)]);
        assert!(u.occurs(v));
    }

    #[test]
    fn union_occurs_negative() {
        let v = TyVar::new(0);
        let u = Ty::Union(vec![Ty::Int, Ty::String]);
        assert!(!u.occurs(v));
    }

    #[test]
    fn union_occurs_nested() {
        let v = TyVar::new(0);
        let u = Ty::Union(vec![Ty::Int, Ty::Array(Box::new(Ty::Var(v)))]);
        assert!(u.occurs(v));
    }

    #[test]
    fn union_apply_subst() {
        let v = TyVar::new(0);
        let subst = Subst::singleton(v, Ty::Bool);
        let u = Ty::Union(vec![Ty::Int, Ty::Var(v)]);
        let result = u.apply(&subst);
        assert_eq!(result, Ty::Union(vec![Ty::Int, Ty::Bool]));
    }

    #[test]
    fn union_apply_subst_no_match() {
        let v = TyVar::new(0);
        let w = TyVar::new(1);
        let subst = Subst::singleton(v, Ty::Bool);
        let u = Ty::Union(vec![Ty::Int, Ty::Var(w)]);
        let result = u.apply(&subst);
        assert_eq!(result, Ty::Union(vec![Ty::Int, Ty::Var(w)]));
    }

    #[test]
    fn union_apply_subst_nested() {
        let v = TyVar::new(0);
        let subst = Subst::singleton(v, Ty::String);
        let u = Ty::Union(vec![Ty::Int, Ty::Option(Box::new(Ty::Var(v)))]);
        let result = u.apply(&subst);
        assert_eq!(
            result,
            Ty::Union(vec![Ty::Int, Ty::Option(Box::new(Ty::String))])
        );
    }

    // --- Ty::func tests ---

    #[test]
    fn func_helper_empty_params() {
        let f = Ty::func([], Ty::Int);
        assert_eq!(f, Ty::Fn(vec![], Box::new(Ty::Int)));
    }

    #[test]
    fn func_helper_single_param() {
        let f = Ty::func([Ty::String], Ty::Bool);
        assert_eq!(f, Ty::Fn(vec![Ty::String], Box::new(Ty::Bool)));
    }

    #[test]
    fn func_helper_multiple_params() {
        let f = Ty::func([Ty::Int, Ty::String, Ty::Bool], Ty::Float);
        assert_eq!(
            f,
            Ty::Fn(vec![Ty::Int, Ty::String, Ty::Bool], Box::new(Ty::Float))
        );
    }

    // --- Scheme::poly tests ---

    #[test]
    fn scheme_poly_creates_one_var() {
        let s = Scheme::poly(|t| Ty::Array(Box::new(t)));
        assert_eq!(s.vars, vec![TyVar::new(0)]);
        assert_eq!(s.ty, Ty::Array(Box::new(Ty::Var(TyVar::new(0)))));
    }

    #[test]
    fn scheme_poly_fn_type() {
        // forall T. Array[T] -> Int
        let s = Scheme::poly(|t| Ty::func([Ty::Array(Box::new(t))], Ty::Int));
        assert_eq!(s.vars, vec![TyVar::new(0)]);
        assert_eq!(
            s.ty,
            Ty::Fn(
                vec![Ty::Array(Box::new(Ty::Var(TyVar::new(0))))],
                Box::new(Ty::Int)
            )
        );
    }

    #[test]
    fn scheme_poly2_creates_two_vars() {
        // forall T U. (T, U) -> (U, T)
        let s = Scheme::poly2(|t, u| {
            Ty::func(
                [Ty::Tuple(vec![t.clone(), u.clone()])],
                Ty::Tuple(vec![u, t]),
            )
        });
        assert_eq!(s.vars, vec![TyVar::new(0), TyVar::new(1)]);
    }

    #[test]
    fn scheme_poly2_map_type() {
        // forall T U. (Array[T], (T -> U)) -> Array[U]
        let s = Scheme::poly2(|t, u| {
            Ty::func(
                [Ty::Array(Box::new(t.clone())), Ty::func([t], u.clone())],
                Ty::Array(Box::new(u)),
            )
        });
        assert_eq!(s.vars, vec![TyVar::new(0), TyVar::new(1)]);
        let t = Ty::Var(TyVar::new(0));
        let u = Ty::Var(TyVar::new(1));
        assert_eq!(
            s.ty,
            Ty::Fn(
                vec![
                    Ty::Array(Box::new(t.clone())),
                    Ty::Fn(vec![t], Box::new(u.clone())),
                ],
                Box::new(Ty::Array(Box::new(u)))
            )
        );
    }

    #[test]
    fn scheme_poly3_creates_three_vars() {
        // forall T U V. (T, U, V) -> T
        let s = Scheme::poly3(|t, u, v| {
            Ty::func([Ty::Tuple(vec![t.clone(), u, v])], t)
        });
        assert_eq!(s.vars, vec![TyVar::new(0), TyVar::new(1), TyVar::new(2)]);
    }

    #[test]
    fn scheme_poly_instantiate() {
        let s = Scheme::poly(|t| Ty::Array(Box::new(t)));
        let mut next = 100;
        let inst = s.instantiate(&mut next);
        // Should replace TyVar(0) with fresh TyVar(100)
        assert_eq!(next, 101);
        assert_eq!(inst, Ty::Array(Box::new(Ty::Var(TyVar::new(100)))));
    }

    #[test]
    fn scheme_poly2_instantiate() {
        let s = Scheme::poly2(|t, u| Ty::Tuple(vec![t, u]));
        let mut next = 50;
        let inst = s.instantiate(&mut next);
        assert_eq!(next, 52);
        assert_eq!(
            inst,
            Ty::Tuple(vec![Ty::Var(TyVar::new(50)), Ty::Var(TyVar::new(51))])
        );
    }
}
