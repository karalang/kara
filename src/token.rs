// src/token.rs

//! Defines the tokens produced by the Kāra lexer.

/// Source location attached to every token.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Span {
    pub line: usize,
    pub column: usize,
    pub offset: usize,
    pub length: usize,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum IntSuffix {
    I8,
    I16,
    I32,
    I64,
    I128,
    U8,
    U16,
    U32,
    U64,
    U128,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum FloatSuffix {
    F32,
    F64,
}

#[derive(Debug, PartialEq, Clone)]
pub enum InterpolationPart {
    Text(String),
    /// A `{...}` interpolation hole. `raw` is the verbatim expression source
    /// (parsing is deferred to the parser); `offset`/`line`/`column` are the
    /// absolute source position of `raw`'s first byte. The parser uses these to
    /// rebase the re-parsed sub-expression's spans to absolute source
    /// coordinates — without it, every interpolation expr would carry spans
    /// relative to the synthetic `fn __interp__() { … }` re-parse wrapper:
    /// `offset` collisions corrupted the `(offset, length)` SpanKey that
    /// codegen/typecheck side-tables key on (B-2026-06-09-1), and the
    /// wrapper-relative `line`/`column` made any diagnostic pointing *into* a
    /// hole report the wrong source position (B-2026-06-09-1a).
    Expr {
        raw: String,
        offset: usize,
        line: usize,
        column: usize,
    },
}

/// A token with its source location.
#[derive(Debug, Clone, PartialEq)]
pub struct SpannedToken {
    pub token: Token,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub enum Token {
    // ── Keywords ──────────────────────────────────────────────
    // Declarations
    Fn,
    Struct,
    Union,
    Enum,
    Trait,
    Impl,
    Mod,
    Use,
    Import,
    Const,
    Type,
    /// `comptime` — compile-time evaluation. Backs the `comptime fn`
    /// declaration modifier, the `comptime { ... }` block expression, and
    /// the `comptime` parameter prefix. Spec: design.md § Reserved-for-Future-Use
    /// Keywords (graduating) + deferred.md § Comptime — AST→AST `comptime fn`.
    Comptime,
    // Visibility
    Pub,
    Private,
    // Control flow
    If,
    Else,
    Match,
    While,
    For,
    In,
    Loop,
    Return,
    Break,
    Continue,
    // Bindings
    Let,
    Mut,
    // Logical operators (keyword forms; symbol `&&`/`||`/`!` are not accepted)
    And,
    Or,
    Not,
    // Ownership
    Own,
    Ref,
    Weak,
    Lock,
    // Closure capture (kept reserved for future ref/mut-ref capture work; bare `|...|` is owned-by-default and `move` is not used)
    Move,
    // Effects
    Effect,
    Resource,
    Verb,
    Reads,
    Writes,
    Sends,
    Receives,
    Allocates,
    Panics,
    Blocks,
    Suspends,
    With,
    Transparent,
    Stable,
    Seq,
    Par,
    Yield,
    // Type system
    As,
    Where,
    Dyn,
    // Safety
    Unsafe,
    Extern,
    // Shared
    Shared,
    // Layout
    Layout,
    Group,
    // Literals
    True,
    False,
    // Contracts
    Requires,
    Ensures,
    Invariant,
    // Defer
    Defer,
    ErrDefer,
    /// `try { ... }` — try block. Parsed at v1; the typechecker pipeline
    /// (?-retargeting + error-type unification) lands in P1. See
    /// design.md § Error Handling > Try Blocks (try { ... }).
    Try,
    /// `marker trait NAME;` — marker-trait declaration. Per design.md §
    /// Marker Traits (v60 item 55). Users with a local binding named
    /// `marker` must rename or use `r#marker`.
    Marker,
    // Assembly
    Asm,
    GlobalAsm,
    // `providers` is parsed as a contextual keyword: the lexer emits it
    // as `Identifier { name: "providers" }`, and the parser dispatches to
    // `parse_providers_block` when an identifier expression named
    // "providers" is followed by `{`. This frees the bareword for module
    // names, function names, variable bindings, etc. (e.g.,
    // `examples/parallax/src/providers.kara`).
    // Other
    Distinct,
    Alias,
    Independent,
    SelfValue, // self
    SelfType,  // Self

    // ── Symbols ───────────────────────────────────────────────
    // Delimiters
    LeftParen,    // (
    RightParen,   // )
    LeftBrace,    // {
    RightBrace,   // }
    LeftBracket,  // [
    RightBracket, // ]

    // Punctuation
    Colon,            // :
    ColonColon,       // :: (attribute path separator only — syntax.md §8)
    Comma,            // ,
    Semicolon,        // ;
    Dot,              // .
    DotDot,           // ..
    DotDotEq,         // ..=
    DotDotDot,        // ... (variadic shape splice — syntax.md § SHAPE_LIT)
    QuestionDot,      // ?.
    QuestionQuestion, // ??
    Arrow,            // ->
    FatArrow,         // =>
    Question,         // ?
    Pound,            // #
    Underscore,       // _ (as a token, e.g., in patterns)
    At,               // @ (pattern bindings)

    // Arithmetic
    Plus,    // +
    Minus,   // -
    Star,    // *
    Slash,   // /
    Percent, // %

    // Comparison
    EqualEqual,         // ==
    BangEqual,          // !=
    LessThan,           // <
    LessThanOrEqual,    // <=
    GreaterThan,        // >
    GreaterThanOrEqual, // >=

    // Logical (legacy — produce an error in parse position; still produced by the
    // lexer so the parser can emit a targeted "use `and`/`or`/`not` instead" message
    // rather than a confusing generic error. PipePipe also opens an empty-param
    // closure (`|| body`); Bang is also the prefix of `!=`, lexed separately as
    // BangEqual.)
    AmpAmp,   // &&  -> "use `and`"
    PipePipe, // ||  -> "use `or`" (in operator position only)
    Bang,     // !   -> "use `not`"

    // Bitwise
    Amp,            // &
    Pipe,           // |
    PipeArrow,      // |>
    Caret,          // ^
    Tilde,          // ~
    LessLess,       // <<
    GreaterGreater, // >>

    // Assignment
    Equal,               // =
    PlusEqual,           // +=
    MinusEqual,          // -=
    StarEqual,           // *=
    SlashEqual,          // /=
    PercentEqual,        // %=
    AmpEqual,            // &=
    PipeEqual,           // |=
    CaretEqual,          // ^=
    LessLessEqual,       // <<=
    GreaterGreaterEqual, // >>=

    // ── Literals ──────────────────────────────────────────────
    Identifier {
        name: String,
        /// `true` when the source wrote `r#NAME` (raw-identifier escape).
        /// The `name` field stores the bare identifier without the `r#` prefix.
        raw: bool,
    },
    Integer(i64, Option<IntSuffix>),
    Float(f64, Option<FloatSuffix>),
    CharLiteral(char),
    /// `b'A'` byte char literal — type `u8` (design.md § Byte and
    /// Byte-String Literals; phase-1-lexer slice).
    ByteLiteral(u8),
    StringLiteral(String),
    MultiStringLiteral(String),
    InterpolatedStringLiteral(Vec<InterpolationPart>),
    /// `c"..."` — C-string literal. `bytes` excludes the trailing NUL (the
    /// codegen layer appends it); `source_len` records the textual length
    /// of the source-form body so `len()` / `as_bytes()` can answer
    /// without re-walking. Interior NUL bytes are rejected at lex time.
    /// See design.md § C-String Literals (v60 item 18).
    CStringLiteral {
        bytes: Vec<u8>,
        source_len: usize,
    },

    // ── Special ───────────────────────────────────────────────
    DocComment(String),
    /// `//!` module-level doc comment. Distinct from `///` so the parser
    /// can attach it to the enclosing module rather than the next item.
    ModuleDocComment(String),
    Error(String),
    EOF,
}

impl Token {
    /// The source spelling of a reserved-keyword token, or `None` for any
    /// non-keyword token (identifiers, literals, punctuation, `Error`, `EOF`).
    ///
    /// This is the inverse of the lexer's `text -> Token` keyword table
    /// (`lexer.rs`, `let token = match text { … }`). It lets the parser turn a
    /// cryptic "Expected pattern, found Group" (the token's Rust `Debug` name)
    /// into an actionable "'group' is a reserved keyword and cannot be used as an
    /// identifier" when a user names a binding/parameter after a keyword
    /// (B-2026-07-08-13). Keep the arms in sync with the lexer table.
    pub fn keyword_spelling(&self) -> Option<&'static str> {
        let s = match self {
            Token::Fn => "fn",
            Token::Struct => "struct",
            Token::Union => "union",
            Token::Enum => "enum",
            Token::Trait => "trait",
            Token::Marker => "marker",
            Token::Impl => "impl",
            Token::Mod => "mod",
            Token::Use => "use",
            Token::Import => "import",
            Token::Const => "const",
            Token::Type => "type",
            Token::Distinct => "distinct",
            Token::Pub => "pub",
            Token::Private => "private",
            Token::If => "if",
            Token::Else => "else",
            Token::Match => "match",
            Token::While => "while",
            Token::For => "for",
            Token::In => "in",
            Token::Loop => "loop",
            Token::Return => "return",
            Token::Break => "break",
            Token::Continue => "continue",
            Token::Defer => "defer",
            Token::ErrDefer => "errdefer",
            Token::Try => "try",
            Token::Asm => "asm",
            Token::GlobalAsm => "global_asm",
            Token::Let => "let",
            Token::Mut => "mut",
            Token::And => "and",
            Token::Or => "or",
            Token::Not => "not",
            Token::Own => "own",
            Token::Ref => "ref",
            Token::Weak => "weak",
            Token::Lock => "lock",
            Token::Move => "move",
            Token::Effect => "effect",
            Token::Resource => "resource",
            Token::Verb => "verb",
            Token::Reads => "reads",
            Token::Writes => "writes",
            Token::Sends => "sends",
            Token::Receives => "receives",
            Token::Allocates => "allocates",
            Token::Panics => "panics",
            Token::Blocks => "blocks",
            Token::Suspends => "suspends",
            Token::With => "with",
            Token::Transparent => "transparent",
            Token::Stable => "stable",
            Token::Seq => "seq",
            Token::Par => "par",
            Token::Yield => "yield",
            Token::As => "as",
            Token::Where => "where",
            Token::Dyn => "dyn",
            Token::Requires => "requires",
            Token::Ensures => "ensures",
            Token::Invariant => "invariant",
            Token::Unsafe => "unsafe",
            Token::Extern => "extern",
            Token::Shared => "shared",
            Token::Layout => "layout",
            Token::Group => "group",
            Token::Comptime => "comptime",
            Token::True => "true",
            Token::False => "false",
            Token::Alias => "alias",
            Token::Independent => "independent",
            Token::SelfValue => "self",
            Token::SelfType => "Self",
            _ => return None,
        };
        Some(s)
    }
}
