use ordered_float::OrderedFloat;
use serde::{Deserialize, Serialize};

use crate::value::{Ident, Scalar};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Var<'a> {
    Simple(Ident<'a>),
    Ref(Ident<'a>),
    Indexed(#[serde(borrow)] Indices<'a>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Indices<'a> {
    #[serde(borrow)]
    pub(crate) ident: Ident<'a>,
    pub(crate) path: Vec<Scalar<'a, OrderedFloat<f64>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Global<T>(pub(crate) T);
