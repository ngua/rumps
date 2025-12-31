//! Token definitions for the RUMPS query language.

#![allow(dead_code)]

use std::fmt;

use ordered_float::OrderedFloat;

/// A token in the RUMPS query language tokenizer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Token {
    // Keywords
    Let,
    Set,
    Get,
    Kill,
    Output,
    If,
    Else,
    Is,
    As,
    Read,
    And,
    Or,
    Not,
    True,
    False,
    Fun,
    Type,
    Match,
    Matches,
    Union,
    Module,
    Data,
    Order,
    Query,
    Forever,
    Transaction,

    // Literals
    Int(i64),
    Float(OrderedFloat<f64>),
    Char(char),
    String(String),
    /// Regex literal: `/pattern/`.
    Regex(String),
    /// JSON null; only valid in JSON contexts (quoted-key objects, arrays).
    Null,

    // Identifiers
    Ident(String),
    /// Global variable (prefixed with `^` in source).
    Global(String),

    // Arithmetic operators
    Plus,     // +
    Minus,    // -
    Mul,      // *
    StarStar, // **
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
    LParen,           // (
    RParen,           // )
    LBrace,           // {
    RBrace,           // }
    LBracket,         // [
    RBracket,         // ]
    Comma,            // ,
    Colon,            // :
    Dot,              // .
    DotDot,           // .. (with space before; range operator)
    DotDotNoSpace,    // .. (no space before; JSON scalar access)
    DotDotEquals,     // ..=
    DotDotDot,        // ...
    Arrow,            // ->
    ArrowArrow,       // ->>
    QuestionQuestion, // ??
    QuestionDot,      // ?.
    /// Prefix `?` wraps a value in `Option.Some`.
    /// Note: `??x` is lexed as coalesce (`??`) + `x`, not `?(?x)`.
    /// For nested wrapping, use explicit parens: `?(?x)`.
    Question,
    FatArrow,   // =>
    Pipe,       // |>
    SinglePipe, // | (variant separator)

    // Special
    Newline,
    Indent,
    Dedent,
    Eof,
}

impl Token {
    /// Returns the keyword token for a given identifier, if it matches.
    ///
    /// Keywords are case-insensitive (e.g., `LET`, `let`, `Let`).
    /// Exception: `null` is case-sensitive (JSON literal).
    ///
    /// Note: MUMPS intrinsics (`$SET`, `$GET`, etc.) are handled by
    /// [`Token::intrinsic`] instead.
    pub(crate) fn keyword(s: &str) -> Option<Self> {
        // `null` is case-sensitive (JSON requires lowercase)
        if s == "null" {
            Some(Self::Null)
        } else {
            // All other keywords are case-insensitive
            match s.to_ascii_uppercase().as_str() {
                "LET" => Some(Self::Let),
                "IF" => Some(Self::If),
                "ELSE" => Some(Self::Else),
                "IS" => Some(Self::Is),
                "AS" => Some(Self::As),
                "READ" => Some(Self::Read),
                "AND" => Some(Self::And),
                "OR" => Some(Self::Or),
                "NOT" => Some(Self::Not),
                "TRUE" => Some(Self::True),
                "FALSE" => Some(Self::False),
                "FUN" => Some(Self::Fun),
                "TYPE" => Some(Self::Type),
                "MATCH" => Some(Self::Match),
                "MATCHES" => Some(Self::Matches),
                "UNION" => Some(Self::Union),
                "MODULE" => Some(Self::Module),
                "FOREVER" => Some(Self::Forever),
                "TRANSACTION" => Some(Self::Transaction),
                // `null` is case-sensitive; other casings are identifiers
                "NULL" => None,
                _ => None,
            }
        }
    }

    /// Returns the token for a MUMPS intrinsic (prefixed with `$`).
    ///
    /// Case-insensitive (e.g., `$SET`, `$set`, `$Set` all work).
    /// The `$` prefix is already stripped by the lexer.
    pub(crate) fn intrinsic(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "SET" => Some(Self::Set),
            "GET" => Some(Self::Get),
            "KILL" => Some(Self::Kill),
            "OUTPUT" => Some(Self::Output),
            "DATA" => Some(Self::Data),
            "ORDER" => Some(Self::Order),
            "QUERY" => Some(Self::Query),
            _ => None,
        }
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Keywords
            Self::Let => write!(f, "LET"),
            Self::If => write!(f, "IF"),
            Self::Else => write!(f, "ELSE"),
            Self::Is => write!(f, "IS"),
            Self::As => write!(f, "AS"),
            Self::Read => write!(f, "READ"),
            Self::And => write!(f, "AND"),
            Self::Or => write!(f, "OR"),
            Self::Not => write!(f, "NOT"),
            Self::True => write!(f, "TRUE"),
            Self::False => write!(f, "FALSE"),
            Self::Fun => write!(f, "FUN"),
            Self::Type => write!(f, "TYPE"),
            Self::Match => write!(f, "MATCH"),
            Self::Matches => write!(f, "MATCHES"),
            Self::Union => write!(f, "UNION"),
            Self::Module => write!(f, "MODULE"),
            Self::Forever => write!(f, "FOREVER"),
            Self::Transaction => write!(f, "TRANSACTION"),
            // MUMPS intrinsics (prefixed with `$`)
            Self::Set => write!(f, "$SET"),
            Self::Get => write!(f, "$GET"),
            Self::Kill => write!(f, "$KILL"),
            Self::Output => write!(f, "$OUTPUT"),
            Self::Data => write!(f, "$DATA"),
            Self::Order => write!(f, "$ORDER"),
            Self::Query => write!(f, "$QUERY"),
            Self::Int(n) => write!(f, "{n}"),
            Self::Float(n) => write!(f, "{}", n.0),
            Self::Char(c) => write!(f, "'{c}'"),
            Self::String(s) => write!(f, "\"{s}\""),
            Self::Regex(p) => write!(f, "/{p}/"),
            Self::Null => write!(f, "null"),
            Self::Ident(s) => write!(f, "{s}"),
            Self::Global(s) => write!(f, "^{s}"),
            Self::Plus => write!(f, "+"),
            Self::Minus => write!(f, "-"),
            Self::Mul => write!(f, "*"),
            Self::StarStar => write!(f, "**"),
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
            Self::DotDotNoSpace => write!(f, ".."),
            Self::DotDotEquals => write!(f, "..="),
            Self::DotDotDot => write!(f, "..."),
            Self::Arrow => write!(f, "->"),
            Self::ArrowArrow => write!(f, "->>"),
            Self::QuestionQuestion => write!(f, "??"),
            Self::QuestionDot => write!(f, "?."),
            Self::Question => write!(f, "?"),
            Self::FatArrow => write!(f, "=>"),
            Self::Pipe => write!(f, "|>"),
            Self::SinglePipe => write!(f, "|"),
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
        assert_eq!(Token::keyword("LET"), Some(Token::Let));
        assert_eq!(Token::keyword("let"), Some(Token::Let));
        assert_eq!(Token::keyword("Let"), Some(Token::Let));
        assert_eq!(Token::keyword("lEt"), Some(Token::Let));
    }

    #[test]
    fn keyword_not_found() {
        assert_eq!(Token::keyword("foo"), None);
        assert_eq!(Token::keyword("LETS"), None);
        // MUMPS intrinsics are NOT keywords (need `$` prefix)
        assert_eq!(Token::keyword("SET"), None);
        assert_eq!(Token::keyword("GET"), None);
        assert_eq!(Token::keyword("OUTPUT"), None);
    }

    #[test]
    fn all_keywords() {
        assert_eq!(Token::keyword("LET"), Some(Token::Let));
        assert_eq!(Token::keyword("IF"), Some(Token::If));
        assert_eq!(Token::keyword("ELSE"), Some(Token::Else));
        assert_eq!(Token::keyword("IS"), Some(Token::Is));
        assert_eq!(Token::keyword("AS"), Some(Token::As));
        assert_eq!(Token::keyword("READ"), Some(Token::Read));
        assert_eq!(Token::keyword("AND"), Some(Token::And));
        assert_eq!(Token::keyword("OR"), Some(Token::Or));
        assert_eq!(Token::keyword("NOT"), Some(Token::Not));
        assert_eq!(Token::keyword("TRUE"), Some(Token::True));
        assert_eq!(Token::keyword("FALSE"), Some(Token::False));
        assert_eq!(Token::keyword("FUN"), Some(Token::Fun));
        assert_eq!(Token::keyword("MATCH"), Some(Token::Match));
        assert_eq!(Token::keyword("MATCHES"), Some(Token::Matches));
        assert_eq!(Token::keyword("UNION"), Some(Token::Union));
        assert_eq!(Token::keyword("MODULE"), Some(Token::Module));
        assert_eq!(Token::keyword("TYPE"), Some(Token::Type));
        assert_eq!(Token::keyword("TRANSACTION"), Some(Token::Transaction));
        // Case-sensitive: null
        assert_eq!(Token::keyword("null"), Some(Token::Null));
    }

    #[test]
    fn intrinsic_case_insensitive() {
        assert_eq!(Token::intrinsic("SET"), Some(Token::Set));
        assert_eq!(Token::intrinsic("set"), Some(Token::Set));
        assert_eq!(Token::intrinsic("Set"), Some(Token::Set));
        assert_eq!(Token::intrinsic("sEt"), Some(Token::Set));
    }

    #[test]
    fn all_intrinsics() {
        assert_eq!(Token::intrinsic("SET"), Some(Token::Set));
        assert_eq!(Token::intrinsic("GET"), Some(Token::Get));
        assert_eq!(Token::intrinsic("KILL"), Some(Token::Kill));
        assert_eq!(Token::intrinsic("OUTPUT"), Some(Token::Output));
        assert_eq!(Token::intrinsic("DATA"), Some(Token::Data));
        assert_eq!(Token::intrinsic("ORDER"), Some(Token::Order));
        assert_eq!(Token::intrinsic("QUERY"), Some(Token::Query));
        // READ is a keyword, not an intrinsic
        assert_eq!(Token::intrinsic("READ"), None);
    }

    #[test]
    fn intrinsic_not_found() {
        assert_eq!(Token::intrinsic("foo"), None);
        assert_eq!(Token::intrinsic("LET"), None);
        assert_eq!(Token::intrinsic("IF"), None);
    }

    #[test]
    fn null_case_sensitive() {
        // `null` is case-sensitive (JSON literal)
        assert_eq!(Token::keyword("null"), Some(Token::Null));
        assert_eq!(Token::keyword("NULL"), None);
        assert_eq!(Token::keyword("Null"), None);
    }

    #[test]
    fn display_tokens() {
        // MUMPS intrinsics display with `$` prefix
        assert_eq!(Token::Set.to_string(), "$SET");
        assert_eq!(Token::Get.to_string(), "$GET");
        assert_eq!(Token::Kill.to_string(), "$KILL");
        assert_eq!(Token::Output.to_string(), "$OUTPUT");
        assert_eq!(Token::Data.to_string(), "$DATA");
        assert_eq!(Token::Order.to_string(), "$ORDER");
        assert_eq!(Token::Query.to_string(), "$QUERY");
        // Keywords display without prefix
        assert_eq!(Token::Let.to_string(), "LET");
        assert_eq!(Token::Read.to_string(), "READ");
        // Other tokens
        assert_eq!(Token::Int(42).to_string(), "42");
        assert_eq!(Token::Float(OrderedFloat(3.14)).to_string(), "3.14");
        assert_eq!(Token::String("hello".into()).to_string(), "\"hello\"");
        assert_eq!(Token::Ident("foo".into()).to_string(), "foo");
        assert_eq!(Token::Global("PATIENT".into()).to_string(), "^PATIENT");
        assert_eq!(Token::Concat.to_string(), "++");
        assert_eq!(Token::FloorDiv.to_string(), "//");
        assert_eq!(Token::Null.to_string(), "null");
    }
}
