use std::borrow::Cow;

use ordered_float::OrderedFloat;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum Expr<'a> {
    Value(#[serde(borrow)] Value<'a>),
    BinOp(BinOp, Value<'a>, Value<'a>),
    UnOp(UnOp, Value<'a>),
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum BinOp {
    Eq,
    Gt,
    Lt,
    Gte,
    Lte,
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Mod,
    Concat,
    And,
    Or,
    Xor,
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum UnOp {
    Not,
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum Value<'a> {
    Literal(Literal<'a, f64>),
    Var(#[serde(borrow)] Var<'a>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Var<'a> {
    /// Simple label for value
    Simple(Ident<'a>),
    /// Reference, e.g. `@var`
    Ref(Ident<'a>),
    /// Sparse array var, e.g. `var("1", "2")`
    ///
    /// FIXME indices should allow variables as well
    Indexed(#[serde(borrow)] Indices<'a>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Indices<'a> {
    #[serde(borrow)]
    pub(crate) ident: Ident<'a>,
    pub(crate) path: Vec<Literal<'a, OrderedFloat<f64>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Literal<'a, F> {
    // FIXME Better numeric types
    Int(i64),
    Float(F),
    Char(char),
    Bool(bool),
    String(Cow<'a, str>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Ident<'a>(pub(crate) Cow<'a, str>);

impl<'a> From<Cow<'a, str>> for Ident<'a> {
    fn from(c: Cow<'a, str>) -> Self {
        Self(c)
    }
}

impl<'a> From<&'a str> for Ident<'a> {
    fn from(s: &'a str) -> Self {
        Self(Cow::from(s))
    }
}
