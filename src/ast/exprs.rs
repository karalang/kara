//! Expression AST — every expression form (literals, calls, blocks,
//! control flow, loops, closures, ranges, patterns-in-expression
//! positions, etc.) plus the operator and label/capture-mode enums and
//! the call-argument and struct-init field shapes used in expression
//! positions.

use crate::token::{FloatSuffix, IntSuffix, Span};

use super::{Attribute, Block, GenericArg, MatchArm, Pattern, TypeExpr};

// ── Expressions ──────────────────────────────────────────────────

/// A part of a parsed f-string — static text or a fully-parsed expression.
/// Replaces `token::InterpolationPart::Expr(raw_string)` after the parser
/// sub-parses each interpolation hole at parse time.
#[derive(Debug, Clone)]
pub enum ParsedInterpolationPart {
    Text(String),
    /// An interpolation hole `{expr[:spec]}`. The optional `spec` is the raw
    /// format-specifier source after the first depth-0 `:` (e.g. `"04"`,
    /// `".2"`, `">10"`, `"x"`); `None` when the hole is a bare `{expr}`. The
    /// spec is parsed + applied at format time (interpreter + codegen) via
    /// `crate::format_spec::FormatSpec::parse`.
    Expr(Box<Expr>, Option<String>),
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

/// Narrow an AST integer LITERAL from the i128 node to `i64`, for a consumer
/// that is not 128-bit ready.
///
/// STAGE-2 MARKER (B-2026-08-19-8), the sibling of the interpreter's
/// `narrow_to_i64` one layer down. `ExprKind::Integer` carries i128 so a
/// 128-bit literal has somewhere to live, but the LEXER still caps magnitudes
/// at the i64/u64 thresholds and 128-bit is still rejected at type-check
/// (B-2026-08-19-6) — so nothing reaches here that does not fit today.
///
/// A hard check rather than a bare cast, for the same reason as its sibling:
/// every call site is a place stage 5 must revisit when the lexer's thresholds
/// widen, and `grep -rn narrow_literal_to_i64 src/` is that worklist. A silent
/// `as i64` would leave a dozen truncation points indistinguishable from
/// ordinary code.
pub fn narrow_literal_to_i64(n: i128) -> i64 {
    match i64::try_from(n) {
        Ok(v) => v,
        Err(_) => panic!(
            "internal error: 128-bit integer literal {n} reached an i64-only \
             consumer. The AST literal node is i128 but this consumer is not \
             128-bit ready yet (B-2026-08-19-8 stage 5)."
        ),
    }
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    // Literals
    Integer(i128, Option<IntSuffix>),
    Float(f64, Option<FloatSuffix>),
    CharLit(char),
    /// `b'A'` byte char literal — type `u8` (design.md § Byte and
    /// Byte-String Literals; phase-1-lexer slice).
    ByteLit(u8),
    StringLit(String),
    MultiStringLit(String),
    InterpolatedStringLit(Vec<ParsedInterpolationPart>),
    /// `c"..."` C-string literal — UTF-8 bytes without the trailing
    /// NUL (codegen appends it). `source_len` records the textual
    /// length of the body so `len()` / `as_bytes()` can return the
    /// pre-NUL byte count without re-walking. Spec: design.md §
    /// C-String Literals (v60 item 18); tracker: phase-5-diagnostics
    /// lines 507 (lex acceptance, shipped) / 587 (parser + stdlib).
    CStringLit {
        bytes: Vec<u8>,
        source_len: usize,
    },
    Bool(bool),

    // Identifiers
    Identifier(String),
    Path {
        segments: Vec<String>,
        /// Mixed type / const generic arguments at the expression position.
        /// Const generics slice 1b (2026-05-11) widened this from
        /// `Vec<TypeExpr>` to `Vec<GenericArg>` so call-site expressions
        /// like `make_arr[i64, 4]()` carry the `4` literal through to the
        /// codegen mango key.
        generic_args: Option<Vec<GenericArg>>,
    },
    SelfValue,
    SelfType,

    // Operators
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },

    // Postfix
    Question(Box<Expr>),
    OptionalChain {
        object: Box<Expr>,
        field_or_method: String,
        args: Option<Vec<CallArg>>, // None for field, Some for method
    },

    // Infix
    NilCoalesce {
        left: Box<Expr>,
        right: Box<Expr>,
    },

    Call {
        callee: Box<Expr>,
        args: Vec<CallArg>,
    },
    MethodCall {
        object: Box<Expr>,
        method: String,
        turbofish: Option<Vec<TypeExpr>>,
        args: Vec<CallArg>,
        /// Span of the closing `)` token of the args list. The outer
        /// `Expr.span` for a `MethodCall` covers only the receiver
        /// (`lhs.span.clone()`); this sidecar lets code-edit consumers
        /// (L205 lock-block wrapping; future `karac fix`-style rewrites)
        /// derive the call's true end-of-extent without re-scanning
        /// source text. Synthetic method calls produced by lowering use
        /// a zero-length placeholder — those never reach user-source
        /// edit emission because they don't sit inside `par` blocks
        /// with user-bound shared/plain-struct receivers.
        args_close_span: Span,
    },
    FieldAccess {
        object: Box<Expr>,
        field: String,
    },
    TupleIndex {
        object: Box<Expr>,
        index: u64,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },

    // Compound expressions
    Block(Block),
    /// `comptime { ... }` — a block whose body is evaluated at compile time.
    /// The block's value becomes a compile-time constant spliced in at the
    /// use site. Carries the block verbatim; the comptime evaluator (a later
    /// slice) runs it and substitutes the result. Spec: deferred.md §
    /// Comptime — AST→AST `comptime fn` (form 2, the `comptime { ... }` block).
    Comptime(Block),
    If {
        condition: Box<Expr>,
        then_block: Block,
        else_branch: Option<Box<Expr>>,
    },
    IfLet {
        pattern: Pattern,
        value: Box<Expr>,
        then_block: Block,
        else_branch: Option<Box<Expr>>,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    While {
        label: Option<String>,
        condition: Box<Expr>,
        body: Block,
        /// Outer attributes on the loop expression (`#[par_order_free]` etc.).
        /// Empty unless the parser saw one or more `#[...]` lines before the
        /// `while` keyword. The concurrency analyzer reads this set to gate
        /// shape-recognition that has unordered-output semantics.
        attributes: Vec<Attribute>,
    },
    WhileLet {
        label: Option<String>,
        pattern: Pattern,
        value: Box<Expr>,
        body: Block,
        /// See [`ExprKind::While::attributes`].
        attributes: Vec<Attribute>,
    },
    For {
        label: Option<String>,
        pattern: Pattern,
        iterable: Box<Expr>,
        body: Block,
        /// See [`ExprKind::While::attributes`].
        attributes: Vec<Attribute>,
    },
    Loop {
        label: Option<String>,
        body: Block,
        /// See [`ExprKind::While::attributes`].
        attributes: Vec<Attribute>,
    },
    /// Labeled block expression — `label: { ... }` (design.md § Loops >
    /// Labeled blocks; syntax.md §5.3). The block becomes a `break` target
    /// (with optional value); `continue label` referring to a labeled block
    /// is rejected by the resolver. The block's type is the LUB of all
    /// reachable `break label expr` value sites and the tail expression.
    /// Unlabeled blocks continue to use `ExprKind::Block` — the
    /// `LabeledBlock` variant is added rather than mutating `Block` so
    /// existing AST consumers (which heavily destructure `Block`) keep
    /// working unchanged.
    LabeledBlock {
        label: String,
        /// Source span of the label identifier (the `IDENT` before the
        /// colon). Threaded through for diagnostic span fidelity —
        /// `error[E_CONTINUE_LABEL_BLOCK]` points its secondary span at
        /// the label binding using this.
        label_span: Span,
        body: Block,
    },
    Closure {
        params: Vec<ClosureParam>,
        /// Explicit per-closure capture-mode prefix (design.md § Closures,
        /// Rule 2½). `None` = bare `|...|` — each capture's mode is
        /// inferred from the body's first classifying use per Rule 2
        /// (read → `Ref`, mutate → `MutRef`, consume → `Own`).
        /// `Some(Own | Ref | MutRef)` = explicit prefix pinning every
        /// captured path to the declared mode; the ownership checker
        /// fires K2 violations when body usage exceeds the declared
        /// mode (consume under `ref` / `mut ref`) and a perf note when
        /// `mut ref` is declared but the body only reads.
        capture_mode: Option<CaptureMode>,
        /// Span of the explicit prefix tokens (`mut ref` / `ref` / `own` /
        /// `move`) when present. `None` for bare `|...|` closures. Lets
        /// diagnostics target the prefix region precisely — used by N0507
        /// (UnusedMutCaptureNote) to attach a machine-applicable
        /// `mut ref` → `ref` rewrite without disturbing the closure body.
        prefix_span: Option<Span>,
        body: Box<Expr>,
    },
    Return(Option<Box<Expr>>),
    Break {
        label: Option<String>,
        value: Option<Box<Expr>>,
    },
    Continue {
        label: Option<String>,
        /// Span of the label identifier alone, when a label is present.
        /// Lets the resolver anchor a machine-applicable rename edit on a
        /// misspelled `continue <label>` (B-2026-07-07-3). `None` for a
        /// bare `continue`. (`break <typo>` never reaches here — the parser
        /// only treats a *known* label as a break label, so a misspelled
        /// break target parses as a value expression and surfaces as E0100.)
        label_span: Option<Span>,
    },

    // Composite literals
    Tuple(Vec<Expr>),
    ArrayLiteral(Vec<Expr>),
    /// `b"..."` — the escape-resolved bytes. design.md § Byte and
    /// Byte-String Literals: "`b"..."` has type `[u8; N]` where `N` is the
    /// byte count of the literal after escape resolution. Not `Slice[u8]`,
    /// not `&[u8; N]`."
    ///
    /// A node of its own rather than a desugaring to `ArrayLiteral`: an
    /// array literal with no expected type infers `Vec[u8]`, so the
    /// desugared form only carried the spec'd type when the binding was
    /// annotated. The type rule is intrinsic to the literal, so the literal
    /// needs to say so itself (B-2026-08-20-37).
    ByteStringLit(Vec<u8>),
    /// `TypeName[e1, e2, ...]` — prefix collection literal.
    /// `type_name` is one of `Vec`, `Array`, `Set`, `Map`.
    /// `Array[e1, e2, e3]` produces a fixed-size array; `Vec[...]` produces a growable vec.
    PrefixCollectionLiteral {
        type_name: String,
        items: Vec<Expr>,
    },
    /// `[value; count]` (bare) or `Vec[value; count]` / `Array[value; count]`
    /// (prefix). Equivalent to a literal with `count` copies of `value`. Bare
    /// form defaults to `Vec[T]` in synthesis mode and coerces to `Array[T, N]`
    /// in check mode against an Array-typed expected. `Array[v; n]` requires
    /// `count` to be a compile-time integer literal. Restricted to `Vec` /
    /// `Array` only; repeating into `Set` / `Map` is rejected.
    RepeatLiteral {
        /// `None` → bare `[v; n]`; `Some("Vec")` / `Some("Array")` → prefix form.
        type_name: Option<String>,
        value: Box<Expr>,
        count: Box<Expr>,
    },
    MapLiteral(Vec<(Expr, Expr)>),
    StructLiteral {
        path: Vec<String>,
        fields: Vec<FieldInit>,
        spread: Option<Box<Expr>>,
    },

    // Pipe
    Pipe {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// `_` placeholder in pipe argument position — replaced by left-hand value during desugaring
    PipePlaceholder,

    // Cast
    Cast {
        expr: Box<Expr>,
        ty: TypeExpr,
    },

    /// `offset_of[T](field.path)` — compile-time byte offset of a field
    /// (or nested field path) from the start of a value of type `T`.
    /// Parser special form because the second argument is a field-name
    /// path, not a value expression. The typechecker walks `field_path`
    /// against `T`'s declared fields, validating each segment and
    /// emitting `E_OFFSET_OF_OPAQUE_TYPE` / `E_OFFSET_OF_GENERIC_PARAM`
    /// / `E_OFFSET_OF_UNKNOWN_FIELD` / `E_OFFSET_OF_PRIVATE_FIELD` /
    /// `E_OFFSET_OF_ENUM_VARIANT` as appropriate. The codegen lowers
    /// to inkwell's `TargetData::offset_of_element` (chained for
    /// nested paths). Returns `usize`. See `design.md § Field Offsets`.
    OffsetOf {
        ty: TypeExpr,
        field_path: Vec<String>,
    },

    // Range — start and/or end may be absent for half-open forms.
    // `a..b`   → start=Some, end=Some, inclusive=false  → Range[T]
    // `a..=b`  → start=Some, end=Some, inclusive=true   → RangeInclusive[T]
    // `a..`    → start=Some, end=None, inclusive=false  → RangeFrom[T]
    // `..b`    → start=None, end=Some, inclusive=false  → RangeTo[T]
    // `..=b`   → start=None, end=Some, inclusive=true   → RangeToInclusive[T]
    // `..`     → start=None, end=None, inclusive=false  → RangeFull
    Range {
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
        inclusive: bool,
    },

    // Unsafe
    Unsafe(Block),

    /// `try { ... }` — try block. The body may use `?` to short-circuit
    /// out of the block; the block itself produces a `Result`-shaped
    /// value. Parsed at v1; the typechecker pipeline (?-retargeting
    /// against the block, error-type unification, From-chain coercion)
    /// lands in P1. See design.md § Error Handling > Try Blocks.
    Try(Block),

    // Sequential block (suppresses auto-parallelism)
    Seq(Block),

    // Parallel block (explicit fork-join)
    Par(Block),

    // Lock block — `lock <place> [alias] { body }`. `mutex` is a place
    // expression naming the `Mutex[T]` to acquire: an `Identifier` (a local /
    // parameter binding) or a `FieldAccess` (a `Mutex` field of a `par` /
    // `shared` struct, e.g. `self.state`). The optional `alias` binds the inner
    // `T` as a `mut ref T` for the body; without one, an `Identifier` place's
    // own name is shadowed to the inner value (a `FieldAccess` place requires
    // an alias — there is no name to shadow).
    Lock {
        mutex: Box<Expr>,
        alias: Option<String>,
        body: Block,
    },

    // `providers { R => p, ... } in { body }` — multi-provider bootstrapping
    // (design.md § `providers { } in { }` Block).
    Providers {
        bindings: Vec<ProviderBinding>,
        body: Block,
    },

    // Error recovery placeholder
    Error,
}

#[derive(Debug, Clone)]
pub struct ProviderBinding {
    pub resource: String,
    pub resource_span: Span,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BinOp {
    // Arithmetic
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    // Comparison
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    // Logical
    And,
    Or,
    // Bitwise
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    // Range
    Range,
    RangeInclusive,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UnaryOp {
    Neg,    // -
    Not,    // !
    BitNot, // ~
    Deref,  // *
}

// ── Closures ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ClosureParam {
    pub pattern: Pattern,
    pub ty: Option<TypeExpr>,
    pub span: Span,
}

/// Discriminator for active label-stack entries — distinguishes labeled
/// loops (which accept both `break label` and `continue label`) from
/// labeled blocks (which accept `break label` only). Carried alongside
/// the label name in the parser's and resolver's label stacks; the
/// resolver consults this when validating `continue label` targets.
/// See design.md § Loops > "Labeled blocks".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelKind {
    Loop,
    Block,
}

/// Explicit closure capture-mode prefix (design.md § Closure Behavior, Rule 2½).
/// Bare `|...|` (no prefix) runs per-capture-path inference; the three variants
/// here pin every captured path to the declared mode. `Own` is Kāra's spelling
/// of capture-by-value; the Rust idiom `move` is rejected with a redirect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    Own,
    Ref,
    MutRef,
}

// ── Call Arguments ───────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CallArg {
    pub label: Option<String>,
    /// Call-site mutation marker (`mut <expr>`). Required for fresh bindings
    /// passed to `mut ref T` / `mut Slice[T]` parameters; rejected elsewhere.
    /// See design.md Feature 4 Part 1½: Call-site Mutation Markers.
    pub mut_marker: bool,
    /// Source span of the `mut` keyword itself, when the author wrote one.
    /// `None` for an unmarked argument and for every synthesized `CallArg`
    /// (desugaring, lowering, codegen-side call synthesis) — those have no
    /// source token to point at.
    ///
    /// B-2026-08-10-2 — the two call-site marker diagnostics prescribe deleting
    /// this one token, and both shipped without a machine-applicable edit, so
    /// `karac fix` skipped the least ambiguous fix in the file. `span` cannot
    /// stand in for it: `span` covers the WHOLE argument, which is right for
    /// the caret but starts at the LABEL in `f(name: mut x)` — deriving the
    /// edit from it would delete the label too. The typechecker has neither
    /// source text nor tokens, so the marker's own span has to be recorded
    /// here, at the one place that saw the token.
    pub mut_marker_span: Option<Span>,
    pub value: Expr,
    pub span: Span,
}

/// Desugar one pipe stage `left |> right` into the call it stands for, per
/// design.md § Pipe Operator: `a |> f` is `f(a)`, `a |> f(b)` is `f(a, b)`,
/// and `a |> f(b, _)` is `f(b, a)` — the `_` placeholder naming where the
/// piped value lands instead of the default leading position. Returns `None`
/// for a right-hand side that is neither a name nor a call, which the
/// typechecker rejects (`E_PIPE_RHS_NOT_CALLABLE`).
///
/// B-2026-08-17-25 — this exists so the three phases cannot disagree about
/// what a pipe MEANS. The typechecker and the interpreter each grew their own
/// copy of this rewrite and had already drifted apart (they disagreed on the
/// synthesized call's span, and on whether a `mut _` placeholder keeps its
/// marker); codegen had no copy at all, so `ExprKind::Pipe` fell through
/// `compile_expr`'s catch-all and every compiled pipe evaluated to 0. A pipe
/// is defined by its rewrite, so the rewrite is written once, here, and the
/// evaluators share it.
///
/// The synthesized call carries the PIPE's span, not the right-hand side's.
/// The typechecker records per-call facts — generic instantiations, narrow
/// integer types, `?`-payload types — keyed by the call expression's span,
/// and it types a pipe by inferring this same call at the pipe's span. An
/// evaluator that rebuilt the call at `right.span` would look those facts up
/// under a key nothing was ever recorded at.
pub fn desugar_pipe(left: &Expr, right: &Expr, span: Span) -> Option<Expr> {
    let piped = |label: Option<String>, mut_marker: bool| CallArg {
        label,
        mut_marker,
        // Synthesized: there is no `mut` token in the source to point at.
        mut_marker_span: None,
        value: left.clone(),
        span: left.span,
    };

    let (callee, args) = match &right.kind {
        // `a |> f` => `f(a)`, and the same for a CLOSURE LITERAL right-hand
        // side: `a |> |x| body` => `(|x| body)(a)`.
        //
        // B-2026-08-17-26 — the closure form was rejected ("right-hand side of
        // pipe must be a function name or function call") even though design.md
        // § Pipe Operator prescribes exactly it as the escape hatch from its own
        // `_` restrictions: "The correct form is `let d = data |> g; d |> |d|
        // f(d, extra)`", and for multi-use, "wrap in a closure — `data |> |d|
        // f(d, d)`". Both restrictions were documented with a workaround that
        // did not compile, so they had no way out at all.
        ExprKind::Identifier(_) | ExprKind::Path { .. } | ExprKind::Closure { .. } => {
            (Box::new(right.clone()), vec![piped(None, false)])
        }

        // `a |> f(args...)` => `f(a, args...)`, or `f(args...)` with the
        // piped value substituted for the `_` placeholder when one is written.
        ExprKind::Call { callee, args } => {
            let has_placeholder = args
                .iter()
                .any(|arg| matches!(arg.value.kind, ExprKind::PipePlaceholder));

            let desugared = if has_placeholder {
                args.iter()
                    .map(|arg| {
                        if matches!(arg.value.kind, ExprKind::PipePlaceholder) {
                            piped(arg.label.clone(), arg.mut_marker)
                        } else {
                            arg.clone()
                        }
                    })
                    .collect()
            } else {
                let mut prepended = vec![piped(None, false)];
                prepended.extend(args.iter().cloned());
                prepended
            };

            (callee.clone(), desugared)
        }

        _ => return None,
    };

    Some(Expr {
        span,
        kind: ExprKind::Call { callee, args },
    })
}

/// Desugar `left ?? right` into `left.unwrap_or(right)`, per design.md line
/// 782: "`expr ?? fallback` — short-circuits to `fallback` if `expr` is `None`
/// (for `Option`) or `Err(_)` (for `Result`). The result type ... is the inner
/// `T` — the wrapper is stripped." That is `unwrap_or`'s contract exactly, so
/// `??` is spelled as sugar for it rather than given its own evaluation.
///
/// B-2026-08-17-27 — `??` had a hand-rolled implementation in the interpreter
/// and none at all in codegen, and it was wrong on three of its four legs: it
/// returned `Some(7)` where the spec says `7`, skipped the fallback entirely
/// on `Err`, and compiled to a constant 0. Every one of those legs was already
/// correct in `unwrap_or`, which carries hardened payload reconstruction and
/// three separate double-free fixes for heap fallbacks (B-2026-07-16-23).
/// Routing `??` through it inherits all of that instead of re-deriving it — a
/// second implementation of one semantics is what produced the divergence.
///
/// The caller must have established that `left` is an `Option`/`Result`; on
/// any other receiver this yields a method-not-found error naming `unwrap_or`,
/// a method the author never wrote. The typechecker checks that first and
/// reports against `??` itself.
pub fn desugar_nil_coalesce(left: &Expr, right: &Expr, span: Span) -> Expr {
    Expr {
        span,
        kind: ExprKind::MethodCall {
            object: Box::new(left.clone()),
            method: "unwrap_or".to_string(),
            turbofish: None,
            args: vec![CallArg {
                label: None,
                mut_marker: false,
                mut_marker_span: None,
                value: right.clone(),
                span: right.span,
            }],
            // Method-call side tables are keyed on the (call, args-close) span
            // pair so chained calls sharing a receiver span stay distinct. The
            // fallback's span plays that role here: it is what distinguishes
            // the two `??` sites in `a ?? b ?? c`, and every phase derives it
            // from the same node, so the keys agree.
            args_close_span: right.span,
        },
    }
}

// ── Struct Literal Fields ────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FieldInit {
    pub name: String,
    pub value: Expr,
    pub shorthand: bool, // true for `Point { x }` (name == value identifier)
    pub span: Span,
}
