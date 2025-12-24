//! Type representation for static type checking.
//!
//! Defines the core types: `Ty` (types), `TyVar` (type variables), `Scheme`
//! (polymorphic type schemes), and `Subst` (type substitutions).

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::intern::StringId;
use crate::TypeId;

/// A type variable; placeholder for an unknown type during inference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TyVar(pub(crate) u32);

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

    // Parameterized builtins
    Array(Box<Ty>),
    Option(Box<Ty>),
    Result(Box<Ty>, Box<Ty>),
    Map(Box<Ty>, Box<Ty>),

    // Compound types
    Tuple(Vec<Ty>),
    Fn(Vec<Ty>, Box<Ty>),

    /// Anonymous structural record; compatible if fields match.
    Object(BTreeMap<StringId, Ty>),

    /// User-defined type (sum types, structs, unions) with type parameters.
    Named(TypeId, Vec<Ty>),

    /// Unresolved; database reads before inference narrows.
    Unknown,

    /// Error recovery sentinel; unifies with anything.
    Error,
}

impl Ty {
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
            Self::Named(_, args) => args.iter().any(|t| t.occurs(v)),
        }
    }

    /// Apply a substitution, replacing type variables with their bindings.
    pub(crate) fn apply(&self, subst: &Subst) -> Ty {
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
                    .map(|v| {
                        let fresh = TyVar(*next);
                        *next += 1;
                        (*v, Ty::Var(fresh))
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
    pub(crate) fn compose(&self, other: &Subst) -> Subst {
        let applied: HashMap<TyVar, Ty> =
            other.0.iter().map(|(v, t)| (*v, t.apply(self))).collect();
        let mut merged = self.0.clone();
        applied.into_iter().for_each(|(v, t)| {
            merged.entry(v).or_insert(t);
        });
        Subst(merged)
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
        let v = TyVar(0);
        let fv = Ty::Var(v).free_vars();
        assert!(fv.contains(&v));
        assert_eq!(fv.len(), 1);
    }

    #[test]
    fn free_vars_array() {
        let v = TyVar(1);
        let arr = Ty::Array(Box::new(Ty::Var(v)));
        let fv = arr.free_vars();
        assert!(fv.contains(&v));
    }

    #[test]
    fn free_vars_fn() {
        let a = TyVar(0);
        let b = TyVar(1);
        let f = Ty::Fn(vec![Ty::Var(a)], Box::new(Ty::Var(b)));
        let fv = f.free_vars();
        assert!(fv.contains(&a));
        assert!(fv.contains(&b));
        assert_eq!(fv.len(), 2);
    }

    #[test]
    fn occurs_check() {
        let v = TyVar(0);
        assert!(Ty::Var(v).occurs(v));
        assert!(!Ty::Int.occurs(v));
        assert!(Ty::Array(Box::new(Ty::Var(v))).occurs(v));
        assert!(!Ty::Array(Box::new(Ty::Int)).occurs(v));
    }

    #[test]
    fn apply_subst_var() {
        let v = TyVar(0);
        let subst = Subst::singleton(v, Ty::Int);
        assert_eq!(Ty::Var(v).apply(&subst), Ty::Int);
    }

    #[test]
    fn apply_subst_nested() {
        let v = TyVar(0);
        let subst = Subst::singleton(v, Ty::String);
        let arr = Ty::Array(Box::new(Ty::Var(v)));
        assert_eq!(arr.apply(&subst), Ty::Array(Box::new(Ty::String)));
    }

    #[test]
    fn apply_subst_no_match() {
        let v = TyVar(0);
        let w = TyVar(1);
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
        let v = TyVar(0);
        let s = Scheme {
            vars: vec![v],
            ty: Ty::Array(Box::new(Ty::Var(v))),
        };
        let mut next = 100;
        let inst = s.instantiate(&mut next);
        // Should have replaced `v` with fresh var `TyVar(100)`
        assert_eq!(next, 101);
        assert_eq!(inst, Ty::Array(Box::new(Ty::Var(TyVar(100)))));
    }

    #[test]
    fn scheme_free_vars_excludes_bound() {
        let a = TyVar(0);
        let b = TyVar(1);
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
        let a = TyVar(0);
        let b = TyVar(1);
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
        let a = TyVar(0);
        let b = TyVar(1);
        let mut s = Subst::singleton(a, Ty::Int);
        s.extend(b, Ty::String);
        assert_eq!(s.apply(&Ty::Var(a)), Ty::Int);
        assert_eq!(s.apply(&Ty::Var(b)), Ty::String);
    }
}
