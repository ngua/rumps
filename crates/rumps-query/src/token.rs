//! Token definitions for the RUMPS query language.

#![allow(dead_code)]

use std::fmt;

use ordered_float::OrderedFloat;

/// A token in the RUMPS query language tokenizer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Token {
    // Keywords
    Let,
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
    NewType,
    Match,
    Matches,
    Union,
    Module,
    Forever,
    Transaction,
    From,
    Write,
    Raise,
    Catch,
    Import,

    // DB intrinsics (prefixed with `@`)
    Set,
    Get,
    Kill,
    Data,
    Order,
    Query,

    // Literals
    Int(i64),
    Float(OrderedFloat<f64>),
    Char(char),
    String(String),
    /// Interpolated string literal: `"{expr} text {expr2}"`.
    ///
    /// The vector contains alternating literal parts and expression strings.
    /// - Even indices (0, 2, 4, ...): literal text segments
    /// - Odd indices (1, 3, 5, ...): expression source code
    ///
    /// For example, `"Hello {name}!"` becomes `["Hello ", "name", "!"]`.
    /// Escaped braces (`{{` and `}}`) are converted to literal `{` and `}`.
    Interpolation(Vec<String>),
    /// Regex literal: `/pattern/`.
    Regex(String),
    /// JSON null; only valid in JSON contexts (quoted-key objects, arrays).
    Null,

    // Identifiers
    Ident(String),
    /// Identifier immediately followed by `{` (no space); for DB ref literals.
    ///
    /// Example: `data{1, 2}` lexes as `IdentBrace("data")`, `1`, `,`, `2`, `}`.
    /// Contrast: `data { ... }` lexes as `Ident("data")`, `LBrace`, etc.
    IdentBrace(String),
    /// Global variable (prefixed with `^` in source).
    Global(String),
    /// Global immediately followed by `{` (no space); for DB ref literals.
    ///
    /// Example: `^data{1}` lexes as `GlobalBrace("data")`, `1`, `}`.
    GlobalBrace(String),

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

    // Bitwise operators
    Amp, // &
    Shl, // <<
    Shr, // >>

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
    Colon,            // : (with space before; type annotation)
    ColonNoSpace,     // : (no space before; class method)
    Dot,              // .
    DotDot,           // .. (with space before; range operator)
    DotDotNoSpace,    // .. (no space before; JSON scalar access)
    DotDotEquals,     // ..=
    DotDotDot,        // ...
    Arrow,            // ->
    ArrowArrow,       // ->>
    QuestionQuestion, // ??
    QuestionDot,      // ?.
    QuestionLBracket, // ?[
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
    /// Note: DB intrinsics (`@SET`, `@GET`, etc.) are handled by
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
                "NEWTYPE" => Some(Self::NewType),
                "MATCH" => Some(Self::Match),
                "MATCHES" => Some(Self::Matches),
                "UNION" => Some(Self::Union),
                "MODULE" => Some(Self::Module),
                "FOREVER" => Some(Self::Forever),
                "TRANSACTION" => Some(Self::Transaction),
                "FROM" => Some(Self::From),
                "WRITE" => Some(Self::Write),
                "RAISE" => Some(Self::Raise),
                "CATCH" => Some(Self::Catch),
                "IMPORT" => Some(Self::Import),
                // `null` is case-sensitive; other casings are identifiers
                "NULL" => None,
                _ => None,
            }
        }
    }

    /// Returns the token for a DB intrinsic (prefixed with `@`).
    ///
    /// Case-insensitive (e.g., `@SET`, `@set`, `@Set` all work).
    /// The `@` prefix is already stripped by the lexer.
    pub(crate) fn intrinsic(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "SET" => Some(Self::Set),
            "GET" => Some(Self::Get),
            "KILL" => Some(Self::Kill),
            "DATA" => Some(Self::Data),
            "ORDER" => Some(Self::Order),
            "QUERY" => Some(Self::Query),
            _ => None,
        }
    }

    /// Returns the string form of a keyword token when used as a contextual
    /// identifier (e.g., as an enum variant name like `Action.Raise`).
    ///
    /// Keywords can appear as variant names in TYPE declarations and pattern
    /// matching. This method is used by the parser to accept keywords in
    /// identifier positions.
    pub(crate) fn as_contextual_ident(&self) -> Option<&'static str> {
        match self {
            Self::Let => Some("Let"),
            Self::If => Some("If"),
            Self::Else => Some("Else"),
            Self::Is => Some("Is"),
            Self::As => Some("As"),
            Self::Read => Some("Read"),
            Self::And => Some("And"),
            Self::Or => Some("Or"),
            Self::Not => Some("Not"),
            Self::True => Some("True"),
            Self::False => Some("False"),
            Self::Fun => Some("Fun"),
            Self::Type => Some("Type"),
            Self::NewType => Some("NewType"),
            Self::Match => Some("Match"),
            Self::Matches => Some("Matches"),
            Self::Union => Some("Union"),
            Self::Module => Some("Module"),
            Self::Forever => Some("Forever"),
            Self::Transaction => Some("Transaction"),
            Self::From => Some("From"),
            Self::Write => Some("Write"),
            Self::Raise => Some("Raise"),
            Self::Catch => Some("Catch"),
            Self::Import => Some("Import"),
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
            Self::NewType => write!(f, "NEWTYPE"),
            Self::Match => write!(f, "MATCH"),
            Self::Matches => write!(f, "MATCHES"),
            Self::Union => write!(f, "UNION"),
            Self::Module => write!(f, "MODULE"),
            Self::Forever => write!(f, "FOREVER"),
            Self::Transaction => write!(f, "TRANSACTION"),
            Self::From => write!(f, "FROM"),
            Self::Write => write!(f, "WRITE"),
            Self::Raise => write!(f, "RAISE"),
            Self::Catch => write!(f, "CATCH"),
            Self::Import => write!(f, "IMPORT"),
            // DB intrinsics (prefixed with `@`)
            Self::Set => write!(f, "@SET"),
            Self::Get => write!(f, "@GET"),
            Self::Kill => write!(f, "@KILL"),
            Self::Data => write!(f, "@DATA"),
            Self::Order => write!(f, "@ORDER"),
            Self::Query => write!(f, "@QUERY"),
            Self::Int(n) => write!(f, "{n}"),
            Self::Float(n) => write!(f, "{}", n.0),
            Self::Char(c) => write!(f, "'{c}'"),
            Self::String(s) => write!(f, "\"{s}\""),
            Self::Interpolation(parts) => {
                write!(f, "\"")?;
                parts.iter().enumerate().try_for_each(|(i, part)| {
                    if i % 2 == 0 {
                        write!(f, "{part}")
                    } else {
                        write!(f, "{{{part}}}")
                    }
                })?;
                write!(f, "\"")
            }
            Self::Regex(p) => write!(f, "/{p}/"),
            Self::Null => write!(f, "null"),
            Self::Ident(s) => write!(f, "{s}"),
            Self::IdentBrace(s) => write!(f, "{s}{{"),
            Self::Global(s) => write!(f, "^{s}"),
            Self::GlobalBrace(s) => write!(f, "^{s}{{"),
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
            Self::Amp => write!(f, "&"),
            Self::Shl => write!(f, "<<"),
            Self::Shr => write!(f, ">>"),
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
            Self::ColonNoSpace => write!(f, ":"),
            Self::Dot => write!(f, "."),
            Self::DotDot => write!(f, ".."),
            Self::DotDotNoSpace => write!(f, ".."),
            Self::DotDotEquals => write!(f, "..="),
            Self::DotDotDot => write!(f, "..."),
            Self::Arrow => write!(f, "->"),
            Self::ArrowArrow => write!(f, "->>"),
            Self::QuestionQuestion => write!(f, "??"),
            Self::QuestionDot => write!(f, "?."),
            Self::QuestionLBracket => write!(f, "?["),
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
