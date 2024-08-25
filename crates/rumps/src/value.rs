use std::borrow::Cow;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Scalar<'a, F> {
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
