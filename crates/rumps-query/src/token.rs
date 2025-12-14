//! Token definitions for the RUMPS query language.
//!
//! This is a minimal subset for Phase 1. See `TODOS/dsl.md` for the full
//! language specification.

#![allow(dead_code)]

use std::fmt;

/// A token in the RUMPS query language tokenizer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Token {
    // Keywords
    Set,
    Kill,
    Output,
    If,
    Else,
    And,
    Or,
    Not,
    True,
    False,

    // Literals
    Int(i64),
    Float(f64),
    String(String),

    // Identifiers
    Ident(String),
    /// Global variable (prefixed with `^` in source).
    Global(String),

    // Arithmetic operators
    Plus,     // +
    Minus,    // -
    Mul,      // *
    Div,      // /
    FloorDiv, // //
    Modulo,   // %

    // Comparison operators
    Eq, // ==
    Ne, // !=
    Lt, // <
    Gt, // >
    Le, // <=
    Ge, // >=

    // Logical operators
    AmpAmp,   // &&
    PipePipe, // ||
    Bang,     // !

    // Assignment
    Assign, // =

    // Concatenation
    Concat, // ++

    // Punctuation
    LParen,   // (
    RParen,   // )
    LBrace,   // {
    RBrace,   // }
    LBracket, // [
    RBracket, // ]
    Comma,    // ,
    Colon,    // :
    Dot,      // .
    DotDot,   // ..

    // Special
    Newline,
    Indent,
    Dedent,
    Eof,
}

impl Token {
    /// Returns the keyword token for a given identifier, if it matches
    /// (case-insensitive).
    pub(crate) fn keyword(s: &str) -> Option<Self> {
        let upper = s.to_ascii_uppercase();
        match upper.as_str() {
            "SET" => Some(Self::Set),
            "KILL" => Some(Self::Kill),
            "OUTPUT" => Some(Self::Output),
            "IF" => Some(Self::If),
            "ELSE" => Some(Self::Else),
            "AND" => Some(Self::And),
            "OR" => Some(Self::Or),
            "NOT" => Some(Self::Not),
            "TRUE" => Some(Self::True),
            "FALSE" => Some(Self::False),
            _ => None,
        }
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Set => write!(f, "SET"),
            Self::Kill => write!(f, "KILL"),
            Self::Output => write!(f, "OUTPUT"),
            Self::If => write!(f, "IF"),
            Self::Else => write!(f, "ELSE"),
            Self::And => write!(f, "AND"),
            Self::Or => write!(f, "OR"),
            Self::Not => write!(f, "NOT"),
            Self::True => write!(f, "TRUE"),
            Self::False => write!(f, "FALSE"),
            Self::Int(n) => write!(f, "{n}"),
            Self::Float(n) => write!(f, "{n}"),
            Self::String(s) => write!(f, "\"{s}\""),
            Self::Ident(s) => write!(f, "{s}"),
            Self::Global(s) => write!(f, "^{s}"),
            Self::Plus => write!(f, "+"),
            Self::Minus => write!(f, "-"),
            Self::Mul => write!(f, "*"),
            Self::Div => write!(f, "/"),
            Self::FloorDiv => write!(f, "//"),
            Self::Modulo => write!(f, "%"),
            Self::Eq => write!(f, "=="),
            Self::Ne => write!(f, "!="),
            Self::Lt => write!(f, "<"),
            Self::Gt => write!(f, ">"),
            Self::Le => write!(f, "<="),
            Self::Ge => write!(f, ">="),
            Self::AmpAmp => write!(f, "&&"),
            Self::PipePipe => write!(f, "||"),
            Self::Bang => write!(f, "!"),
            Self::Assign => write!(f, "="),
            Self::Concat => write!(f, "++"),
            Self::LParen => write!(f, "("),
            Self::RParen => write!(f, ")"),
            Self::LBrace => write!(f, "{{"),
            Self::RBrace => write!(f, "}}"),
            Self::LBracket => write!(f, "["),
            Self::RBracket => write!(f, "]"),
            Self::Comma => write!(f, ","),
            Self::Colon => write!(f, ":"),
            Self::Dot => write!(f, "."),
            Self::DotDot => write!(f, ".."),
            Self::Newline => write!(f, "newline"),
            Self::Indent => write!(f, "indent"),
            Self::Dedent => write!(f, "dedent"),
            Self::Eof => write!(f, "end of file"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_case_insensitive() {
        assert_eq!(Token::keyword("SET"), Some(Token::Set));
        assert_eq!(Token::keyword("set"), Some(Token::Set));
        assert_eq!(Token::keyword("Set"), Some(Token::Set));
        assert_eq!(Token::keyword("sEt"), Some(Token::Set));
    }

    #[test]
    fn keyword_not_found() {
        assert_eq!(Token::keyword("foo"), None);
        assert_eq!(Token::keyword("SETS"), None);
    }

    #[test]
    fn all_keywords() {
        assert_eq!(Token::keyword("SET"), Some(Token::Set));
        assert_eq!(Token::keyword("KILL"), Some(Token::Kill));
        assert_eq!(Token::keyword("OUTPUT"), Some(Token::Output));
        assert_eq!(Token::keyword("IF"), Some(Token::If));
        assert_eq!(Token::keyword("ELSE"), Some(Token::Else));
        assert_eq!(Token::keyword("AND"), Some(Token::And));
        assert_eq!(Token::keyword("OR"), Some(Token::Or));
        assert_eq!(Token::keyword("NOT"), Some(Token::Not));
        assert_eq!(Token::keyword("TRUE"), Some(Token::True));
        assert_eq!(Token::keyword("FALSE"), Some(Token::False));
    }

    #[test]
    fn display_tokens() {
        assert_eq!(Token::Set.to_string(), "SET");
        assert_eq!(Token::Int(42).to_string(), "42");
        assert_eq!(Token::Float(3.14).to_string(), "3.14");
        assert_eq!(Token::String("hello".into()).to_string(), "\"hello\"");
        assert_eq!(Token::Ident("foo".into()).to_string(), "foo");
        assert_eq!(Token::Global("PATIENT".into()).to_string(), "^PATIENT");
        assert_eq!(Token::Concat.to_string(), "++");
        assert_eq!(Token::FloorDiv.to_string(), "//");
    }
}
