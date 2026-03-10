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
    Class,
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
    /// Keywords are lowercase-only (e.g., `let`, `if`, `else`).
    /// Exception: `null` is a JSON literal and also lowercase.
    ///
    /// Note: DB intrinsics (`@set`, `@get`, etc.) are handled by
    /// [`Token::intrinsic`] instead.
    pub(crate) fn keyword(s: &str) -> Option<Self> {
        match s {
            "null" => Some(Self::Null),
            "let" => Some(Self::Let),
            "if" => Some(Self::If),
            "else" => Some(Self::Else),
            "is" => Some(Self::Is),
            "as" => Some(Self::As),
            "read" => Some(Self::Read),
            "and" => Some(Self::And),
            "or" => Some(Self::Or),
            "not" => Some(Self::Not),
            "true" => Some(Self::True),
            "false" => Some(Self::False),
            "fun" => Some(Self::Fun),
            "type" => Some(Self::Type),
            "newtype" => Some(Self::NewType),
            "match" => Some(Self::Match),
            "matches" => Some(Self::Matches),
            "union" => Some(Self::Union),
            "module" => Some(Self::Module),
            "class" => Some(Self::Class),
            "forever" => Some(Self::Forever),
            "transaction" => Some(Self::Transaction),
            "from" => Some(Self::From),
            "write" => Some(Self::Write),
            "raise" => Some(Self::Raise),
            "catch" => Some(Self::Catch),
            "import" => Some(Self::Import),
            _ => None,
        }
    }

    /// Returns the token for a DB intrinsic (prefixed with `@`).
    ///
    /// Lowercase-only (e.g., `@set`, `@get`, `@kill`).
    /// The `@` prefix is already stripped by the lexer.
    pub(crate) fn intrinsic(s: &str) -> Option<Self> {
        match s {
            "set" => Some(Self::Set),
            "get" => Some(Self::Get),
            "kill" => Some(Self::Kill),
            "data" => Some(Self::Data),
            "order" => Some(Self::Order),
            "query" => Some(Self::Query),
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
            Self::Class => Some("Class"),
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
            Self::Let => write!(f, "let"),
            Self::If => write!(f, "if"),
            Self::Else => write!(f, "else"),
            Self::Is => write!(f, "is"),
            Self::As => write!(f, "as"),
            Self::Read => write!(f, "read"),
            Self::And => write!(f, "and"),
            Self::Or => write!(f, "or"),
            Self::Not => write!(f, "not"),
            Self::True => write!(f, "true"),
            Self::False => write!(f, "false"),
            Self::Fun => write!(f, "fun"),
            Self::Type => write!(f, "type"),
            Self::NewType => write!(f, "newtype"),
            Self::Match => write!(f, "match"),
            Self::Matches => write!(f, "matches"),
            Self::Union => write!(f, "union"),
            Self::Module => write!(f, "module"),
            Self::Class => write!(f, "class"),
            Self::Forever => write!(f, "forever"),
            Self::Transaction => write!(f, "transaction"),
            Self::From => write!(f, "from"),
            Self::Write => write!(f, "write"),
            Self::Raise => write!(f, "raise"),
            Self::Catch => write!(f, "catch"),
            Self::Import => write!(f, "import"),
            // DB intrinsics (prefixed with `@`)
            Self::Set => write!(f, "@set"),
            Self::Get => write!(f, "@get"),
            Self::Kill => write!(f, "@kill"),
            Self::Data => write!(f, "@data"),
            Self::Order => write!(f, "@order"),
            Self::Query => write!(f, "@query"),
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
