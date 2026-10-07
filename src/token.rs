// src/token.rs

//! Defines the tokens produced by the Kāra lexer.

/// Source location attached to every token.
///
/// `Copy` on purpose: four words of plain position data, cloned at ~1,800
/// call sites before the derive landed (project-review-2026-08-16 item 9a).
/// Keep it `Copy` — a future non-`Copy` field here would silently reintroduce
/// a clone obligation across every phase.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
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
    /// `42usize` — pointer-width unsigned, 64-bit in Kāra. A DISTINCT type
    /// from `u64` (`UIntSize::Usize`), which is why the suffix cannot simply
    /// be lexed as `U64`: `let n: usize = 42u64` is a type mismatch, so
    /// mapping it there would move the error rather than remove it
    /// (B-2026-08-19-29).
    Usize,
    /// `42isize` — pointer-width signed. The `Usize` note above applies
    /// verbatim with the signs swapped: `isize` is a DISTINCT type from `i64`,
    /// so lexing the suffix as `I64` would relocate `let n: isize = 42i64`'s
    /// mismatch rather than remove it.
    Isize,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum FloatSuffix {
    /// `1.0f16` — IEEE 754-2008 half precision.
    F16,
    /// `1.0bf16` — bfloat16 (truncated-mantissa half).
    BF16,
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
    Enum,
    Trait,
    Impl,
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
    // Closure capture (kept reserved for future ref/mut-ref capture work; bare `|...|` is owned-by-default and `move` is not used)
    Move,
    // Effects
    Effect,
    With,
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
    Integer(i128, Option<IntSuffix>),
    /// A decimal / hex / binary / octal integer literal whose MAGNITUDE does
    /// not fit `i64` but does fit `u64`. B-2026-08-06-13.
    ///
    /// The lexer cannot decide whether the `-` before a literal is unary or
    /// binary, so it cannot fold the sign itself. It hands the magnitude up
    /// instead and the PARSER decides: under a unary minus, exactly
    /// `9223372036854775808` folds to `i64::MIN`; anywhere else this is an
    /// out-of-range error. That is what makes `i64::MIN` writable at all —
    /// its positive half is one past `i64::MAX`, so a plain `Neg(Integer(n))`
    /// with `n` positive can never represent it.
    IntegerOutOfRange(u128, Option<IntSuffix>),
    Float(f64, Option<FloatSuffix>),
    CharLiteral(char),
    /// `'outer` — a loop or labeled-block label, without its quote.
    Label(String),
    /// `b'A'` byte char literal — type `u8` (design.md § Byte and
    /// Byte-String Literals; phase-1-lexer slice).
    ByteLiteral(u8),
    /// `b"..."` byte-string literal — the escape-resolved bytes. design.md
    /// § Byte and Byte-String Literals: "`b"..."` has type `[u8; N]` where
    /// `N` is the byte count of the literal after escape resolution. Not
    /// `Slice[u8]`". The parser desugars it to the `Array[u8, N]` literal
    /// `[b'h', b'e', …]`, which is the form design.md § Reserved
    /// Single-Letter String-Prefix Syntax already called the current
    /// spelling — so the quoted form is pure surface sugar over a shape
    /// typecheck / interp / codegen already handle (B-2026-08-20-37).
    ByteStringLiteral(Vec<u8>),
    /// `"..."`, a dedented `"""` block, or a raw `r"..."` / `r#"..."#`.
    StringLiteral(String),
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
    /// A word from design.md § Reserved-for-Future-Use Keywords, carrying its
    /// source spelling.
    ///
    /// These are NOT v1 features, so they have no token of their own and
    /// nothing in the parser ever accepts one — but they must still reach the
    /// parser as a *keyword*, not as a lexer `Error`. Before B-2026-09-02-29
    /// they lexed to `Error(msg)`, which `keyword_spelling` reports as a
    /// non-keyword, so the parser fell through to its `found {tok:?}` fallback
    /// and rendered the compiler's internal token — `Expected pattern, found
    /// Error("'async' is reserved for future use …")` — instead of the clean
    /// `E0003` the active keywords get. Keeping the spelling as a payload lets
    /// them share that one diagnostic path.
    ReservedFuture(&'static str),
    /// A refused `r#NAME` escape, carrying the structural marker's spelling.
    ///
    /// The sibling of [`Token::ReservedFuture`], and it exists for the same
    /// reason: the rejection used to be a `Token::Error`, which
    /// `keyword_spelling` reports as a non-keyword, so the parser rendered it
    /// through the `found {tok:?}` fallback — `Expected pattern, found
    /// Error("'r#self' is not legal; …")` — under the generic `E0002`
    /// (B-2026-09-02-36).
    ///
    /// Unlike `ReservedFuture` this token deliberately reports NO
    /// `keyword_spelling`: the keyword paths answer a reserved name by
    /// advising `write \`r#NAME\``, which is exactly what the writer just
    /// tried. It carries its own message and its own code instead.
    RawIdentNotAllowed(&'static str),
    EOF,
}

/// The words design.md § Reserved-for-Future-Use Keywords holds back from v1.
///
/// The lexer maps each to [`Token::ReservedFuture`]; `r#NAME` bypasses the
/// table entirely, so every one of these stays usable as an identifier today
/// (except `priv`, which [`UNESCAPABLE_MARKERS`] also claims).
pub const RESERVED_FUTURE_KEYWORDS: &[&str] = &[
    "async", "await", "become", "box", "do", "final", "gen", "override", "priv", "pure", "typeof",
    "virtual",
];

/// Words the `r#` escape does NOT rescue: structural markers whose meaning is
/// positional rather than nominal, so there is no identifier for `r#NAME` to
/// denote.
///
/// This is the authority for both the lexer's `r#` rejection and the parser's
/// decision to *suggest* `r#` at all (B-2026-09-02-30). One list, because a
/// diagnostic that recommends an escape the lexer then rejects is worse than a
/// diagnostic that recommends nothing.
pub const UNESCAPABLE_MARKERS: &[&str] = &[
    "self", "Self", "_", "super", "crate", "pub", "priv", "private", "mut", "ref", "own",
];

/// Whether `kw` can be escaped to an ordinary identifier with `r#kw`.
pub fn is_raw_escapable(kw: &str) -> bool {
    !UNESCAPABLE_MARKERS.contains(&kw)
}

/// The [`UNESCAPABLE_MARKERS`] entry `name` matches, as a `&'static str` the
/// token can carry without allocating, or `None` if `name` is escapable.
///
/// A linear scan is fine here where it would not be in the keyword table:
/// this runs only inside `raw_identifier`, after an `r#` has already been
/// seen, not once per identifier in the source.
pub fn unescapable_marker_spelling(name: &str) -> Option<&'static str> {
    UNESCAPABLE_MARKERS.iter().copied().find(|m| *m == name)
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
            Token::Enum => "enum",
            Token::Trait => "trait",
            Token::Impl => "impl",
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
            Token::Move => "move",
            Token::Effect => "effect",
            Token::With => "with",
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
            Token::Comptime => "comptime",
            Token::True => "true",
            Token::False => "false",
            Token::SelfValue => "self",
            Token::SelfType => "Self",
            // Not a v1 keyword, but a keyword-shaped rejection: reporting it
            // here is what routes it to the `E0003` path rather than the
            // internal-token fallback (B-2026-09-02-29).
            Token::ReservedFuture(kw) => kw,
            _ => return None,
        };
        Some(s)
    }
}
