// src/parser.rs

//! Recursive-descent parser for Kāra source code.
//! Produces an AST from a token stream with error recovery and multi-error reporting.

use crate::ast::*;
use crate::lexer::{classify_ident, IdentClass};
use crate::token::{is_raw_escapable, Span, SpannedToken, Token};
use rustc_hash::FxHashMap;

mod attributes;
mod exprs;
pub(crate) use exprs::is_primitive_type_name;
mod generics;
mod items;
mod items_effects;
mod items_extern;
mod items_trait;
mod patterns;
mod rust_rc;
mod rust_thread;
mod stmts;
mod types;

// ── Parse Errors ─────────────────────────────────────────────────

/// Machine-readable family of a parse diagnostic — the parse-phase twin of
/// `resolver::ResolveErrorKind`. Until this existed every parse error reached
/// the JSON surface as one undifferentiated `E0001`, which made parse the
/// weakest diagnostic surface for machine consumers (`karac fix`, the Mend
/// loop, IDEs): a reserved-keyword misuse and a stray token were the same
/// code. One kind per FAMILY, not per message — the message carries the
/// specifics; the kind is what a tool can dispatch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParseErrorKind {
    /// Uncategorized syntax error — the default every pre-existing
    /// construction site lands on, and the same `E0001` those errors have
    /// always carried. Classify sites OUT of this bucket over time; never
    /// dispatch on it.
    #[default]
    Syntax,
    /// The parser required a specific token / production and found another
    /// (`expect`, and the identifier/expression/pattern-position fallbacks).
    UnexpectedToken,
    /// A reserved keyword used where an identifier is required
    /// (the B-2026-07-08-13 family — `let mut group = …`).
    ReservedKeyword,
    /// An `r#NAME` escape refused because `NAME` is a structural marker —
    /// `self`, `mut`, `pub`, … (`token::UNESCAPABLE_MARKERS`). syntax.md § 1.1
    /// and design.md § Raw Identifiers call this `E_RAW_IDENT_NOT_ALLOWED`.
    ///
    /// Deliberately NOT folded into `ReservedKeyword`: that family's whole
    /// remedy is "write `r#NAME`", and this family is the one place where
    /// that advice is wrong, because the writer already did (B-2026-09-02-36).
    RawIdentNotAllowed,
    /// Syntax recognized and deliberately frozen at v1 (reserved ABIs and
    /// peers) — valid-looking code the language reserves rather than rejects
    /// as nonsense; tools should surface these as "not yet", not "wrong".
    ReservedSyntax,
    /// One lint named twice in one lint-level attribute,
    /// `error[E_DUPLICATE_LINT_LEVEL]` (design.md § Lint levels).
    DuplicateLintLevel,
}

impl ParseErrorKind {
    /// Stable diagnostic code, `E00xx` (parse owns the block below the
    /// resolver's `E01xx`). `Syntax` keeps the historical `E0001`.
    pub fn code(self) -> &'static str {
        match self {
            ParseErrorKind::Syntax => "E0001",
            ParseErrorKind::UnexpectedToken => "E0002",
            ParseErrorKind::ReservedKeyword => "E0003",
            // Fills the one gap the parse band had left. Distinct from `E0003`
            // on purpose: a refused `r#` escape is not "you used a reserved
            // word", it is "this word cannot be escaped at all", and the two
            // want opposite advice (B-2026-09-02-36). syntax.md § 1.1 and
            // design.md § Raw Identifiers name it `E_RAW_IDENT_NOT_ALLOWED`.
            ParseErrorKind::RawIdentNotAllowed => "E0004",
            ParseErrorKind::ReservedSyntax => "E0005",
            ParseErrorKind::DuplicateLintLevel => "E0006",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
    pub kind: ParseErrorKind,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: {}",
            self.span.line, self.span.column, self.message
        )
    }
}

pub struct ParseResult {
    pub program: Program,
    pub errors: Vec<ParseError>,
    /// Did the source carry TOP-LEVEL STATEMENTS (script mode)?
    ///
    /// True whenever at least one statement was found outside any item,
    /// regardless of what happened next -- synthesized into a `fn main()`
    /// (script mode), rejected because an explicit `main` was also present,
    /// or rejected because the context is item-only ([`Parser::items_only`]).
    /// So it answers "was this written as statements", not "did that
    /// succeed", which is what a caller classifying INPUT needs.
    ///
    /// B-2026-09-01-37: the REPL's cell classifier needs exactly this
    /// question answered, and had been guessing at it from line prefixes.
    /// The answer is not otherwise recoverable from the result -- a
    /// synthesized `main` is byte-identical to a user-written one in the
    /// AST, so `items` alone cannot distinguish them.
    pub had_top_level_statements: bool,
    /// Span-keyed machine-applicable edits synthesized during parsing (e.g.
    /// delete a stray comma in a comma-separated `with` clause). Consumed by
    /// `collect_diagnostics` (JSON `replacement`) and `cmd_fix`.
    pub fix_edits: FxHashMap<crate::resolver::SpanKey, crate::resolver::TextEdit>,
    /// Multi-edit envelopes, keyed by the DIAGNOSTIC's span exactly like
    /// [`Self::fix_edits`]. The parse twin of
    /// [`crate::resolver::ResolveResult::error_fix_diffs`], and here for the
    /// same reason: some repairs are inherently two-sided. Wrapping a bare
    /// assignment arm body in braces is an insert at the body's start *and*
    /// one after its end, and neither half is a valid program on its own — so
    /// they cannot go in the single-edit slot, where a consumer that applied
    /// only what it found would leave an unbalanced brace behind. A diagnostic
    /// uses one channel or the other, never both.
    pub fix_diffs: FxHashMap<crate::resolver::SpanKey, Vec<crate::resolver::TextEdit>>,
}

/// Surrounding signature kind for parameter parsing — selects between the
/// trait-method and free-function anonymous-parameter diagnostics, and gates
/// parameter modes that only make sense on a Kāra function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FnContext {
    /// `fn` inside a `trait { ... }` body. Drives `E_TRAIT_METHOD_ANONYMOUS_PARAM`.
    TraitMethod,
    /// Free function or impl method. Drives `E_FN_ANONYMOUS_PARAM`. Impl
    /// methods reuse the free-function diagnostic per design.md § Trait method
    /// parameter names — required; the rule only requires *trait declarations*
    /// to name parameters, but the focused diagnostic for free fns helps catch
    /// the equivalent type-only paste typo.
    Function,
    /// A foreign-import signature — `unsafe extern "C" { fn … }` or a host
    /// function. Shares `Function`'s anonymous-param diagnostic (it falls
    /// through the same default arm), but is distinguished because a foreign
    /// signature is ABI, not Kāra code: there is no body to analyze and the
    /// callee is opaque, so parameter modes whose meaning comes from checking
    /// the callee — `frozen` (B-2026-08-01-33) — are rejected here rather than
    /// accepted-and-ignored.
    Extern,
}

// `.` is both the path separator and the field/method access operator. The parser
// disambiguates using the case class of the initial identifier: Type- and Const-class
// names (uppercase leading letter) start paths; Value-class names (lowercase) start
// postfix chains. See docs/design.md § Identifiers and Naming.
/// Kāra's name for a Rust standard collection (`HashMap` -> `Map`). Rust's
/// `BinaryHeap` pops the largest first and Kāra's `PriorityQueue` the
/// smallest, so its rename holds for a type and not for a constructor, which
/// becomes `PriorityQueue.max_first` / `max_first_from`. B-2026-11-04-5.
pub(crate) fn rust_collection_rename(name: &str) -> Option<&'static str> {
    match name {
        "HashMap" => Some("Map"),
        "HashSet" => Some("Set"),
        "BTreeMap" => Some("SortedMap"),
        "BTreeSet" => Some("SortedSet"),
        "BinaryHeap" => Some("PriorityQueue"),
        // Rust's string iterators (B-2026-11-10-35).
        "Chars" => Some("StrChars"),
        "CharIndices" => Some("StrCharIndices"),
        _ => None,
    }
}

pub(crate) fn starts_upper(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// Whether segment `at` of a Rust path is a module Kāra's own expressions
/// name, followed by one of its functions (`std::fs::read_to_string` is
/// `fs.read_to_string`, `std.env.args` is `env.args`): the fix that deletes
/// a Rust path's module segments keeps it, since the function alone does
/// not resolve (B-2026-11-10-16). A type after it (`std::fs::File`) goes on
/// alone as before.
pub(crate) fn kara_module_call<S: AsRef<str>>(names: &[S], at: usize) -> bool {
    const MODULES: &[&str] = &[
        "fs",
        "env",
        "process",
        "clock",
        "rand",
        "ptr",
        "cpu",
        "gpu",
        "critical_section",
    ];
    MODULES.contains(&names[at].as_ref())
        && names.get(at + 1).is_some_and(|n| !starts_upper(n.as_ref()))
}

/// Whether a `format!` argument's text can stand in an f-string hole as it
/// is. An `if`, `match` or block expression can: its braces balance, and a
/// hole takes them (B-2026-11-10-17). A `:` outside a literal (a Rust path, an
/// ascription) would read as the hole's format spec, a literal holding a
/// brace, a colon or an escape would end or split the hole, and a nested
/// `format!` or f-string is a rewrite of its own; those, a comment and a line
/// break keep the call unrewritten.
fn format_arg_fits_hole(arg: &str) -> bool {
    if arg.contains('\n') || arg.contains("!(") || arg.contains("![") {
        return false;
    }
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut prev = ' ';
    // The character before `prev`: an `f` opens an f-string only where it
    // starts a word, so `"f"`, a literal ending in `f`, is no f-string
    // (B-2026-11-10-50).
    let mut before = ' ';
    let chars: Vec<char> = arg.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        match quote {
            Some(q) => match c {
                '{' | '}' | ':' | '\\' => return false,
                c if c == q => quote = None,
                _ => {}
            },
            None => match c {
                '"' if prev == 'f' && !(before.is_alphanumeric() || before == '_') => {
                    return false;
                }
                '"' | '\'' => quote = Some(c),
                // A Rust path's `::` (`String::from_utf8_lossy(&p)`), which
                // the hole writes as Kāra's `.` (B-2026-11-11-21); a single
                // `:` would start a format spec, and a turbofish `::<` has
                // no hole form.
                ':' if is_path_colons(&chars, i) => {}
                ':' | '\\' => return false,
                '/' if prev == '/' => return false,
                '*' if prev == '/' => return false,
                '{' => depth += 1,
                '}' => match depth.checked_sub(1) {
                    Some(d) => depth = d,
                    None => return false,
                },
                _ => {}
            },
        }
        before = prev;
        prev = c;
    }
    depth == 0 && quote.is_none()
}

/// Whether the `:` at `at` is half of a path's `::`: a `::` between an
/// identifier and the start of another.
fn is_path_colons(chars: &[char], at: usize) -> bool {
    let word = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric() || *c == '_');
    let start = |c: Option<&char>| c.is_some_and(|c| c.is_alphabetic() || *c == '_');
    if chars.get(at + 1) == Some(&':') {
        at > 0 && word(chars.get(at - 1)) && start(chars.get(at + 2))
    } else if at > 0 && chars[at - 1] == ':' {
        at > 1 && word(chars.get(at - 2)) && start(chars.get(at + 1))
    } else {
        false
    }
}

/// `arg` with each path `::` outside a quote written as Kāra's `.`.
fn path_colons_as_dots(arg: &str) -> String {
    let chars: Vec<char> = arg.chars().collect();
    let mut out = String::with_capacity(arg.len());
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == ':' && is_path_colons(&chars, i) => {
                out.push('.');
                i += 2;
                continue;
            }
            None => {}
        }
        out.push(c);
        i += 1;
    }
    out
}

// ── Parser ───────────────────────────────────────────────────────

/// FFI calling-convention ABIs the grammar recognizes but reserves for a
/// future release: they parse (freezing the syntax at v1 so a later
/// implementation is not source-breaking) but are rejected with a targeted
/// "reserved" diagnostic distinct from the generic "unknown ABI" error.
/// `extern "C"` / `extern "C-unwind"` are the implemented set. See the
/// Phase-11 embedded/hardware tracker and design.md § FFI.
const RESERVED_FFI_ABIS: &[&str] = &["stdcall", "fastcall", "win64", "sysv64"];

/// Maximum shared nesting depth for expressions, types, and patterns — the
/// SEMANTIC ceiling: far deeper than any human- or machine-written program
/// nests, so hitting it means pathological input, not a real program.
/// Overflow safety no longer depends on it: the three descent entries wrap
/// their recursion in `stacker::maybe_grow` (512 KiB red zone, 16 MiB
/// chunks — one level costs ≤~85 KiB in a debug build), so parsing survives
/// any thread size, including Rust's default 2 MiB spawned threads that a
/// debug build used to overflow at ~24 levels. The regression tests in
/// `tests/parser.rs` pin both halves: the diagnostic at the ceiling, and a
/// clean 100-deep parse on a 512 KiB thread. B-2026-08-16-4.
pub(crate) const MAX_RECURSION_DEPTH: usize = 128;

pub struct Parser {
    pub(crate) tokens: Vec<SpannedToken>,
    pub(crate) pos: usize,
    pub(crate) errors: Vec<ParseError>,
    /// Current depth across the mutually-recursive descent entries
    /// (`parse_expr_bp_with_ctx`, `parse_type`, `parse_pattern`), guarded
    /// against [`MAX_RECURSION_DEPTH`]: pathologically nested input (hundreds
    /// of `(`) must produce a structured diagnostic, not a stack-overflow
    /// abort. One shared counter because the three entries interleave on the
    /// same call stack — an expression nested inside a type inside a pattern
    /// consumes one budget. B-2026-08-16-4.
    pub(crate) recursion_depth: usize,
    /// B-2026-08-01-33 mechanism 3, stage 1: `true` only while parsing the
    /// TOP-LEVEL type of a parameter, which is the one position the stage
    /// claims. `frozen` elsewhere (a `let` annotation, a struct field, a
    /// generic argument like `Vec[frozen N]`) is REJECTED rather than silently
    /// accepted: those positions will mean something different once the mode
    /// is sticky (stage 2), and accepting them now would let programs depend
    /// on a spelling whose semantics are still undecided. Fail-closed.
    pub(crate) frozen_ok: bool,
    /// `true` only while parsing a trait's, struct's or enum's own generic
    /// list, the places a parameter may name a default (`trait Add[Rhs =
    /// Self]`, `struct Map[K, V, H = SipHash13BuildHasher]`).
    pub(crate) generic_defaults_ok: bool,
    /// `true` only for the outermost type of a function parameter, the one
    /// position `escaping Fn(...)` is accepted (`docs/core-semantics.md`
    /// §9.3). Taken by `parse_type` like [`Self::frozen_ok`].
    pub(crate) escaping_ok: bool,
    /// See [`crate::ast::Program::escaping_fn_types`].
    pub(crate) escaping_fn_types: rustc_hash::FxHashSet<crate::resolver::SpanKey>,
    /// See [`crate::ast::Program::vec_macro_literals`].
    pub(crate) vec_macro_literals:
        rustc_hash::FxHashMap<crate::resolver::SpanKey, Option<crate::resolver::TextEdit>>,
    /// See [`crate::ast::Program::rust_box_new`].
    pub(crate) rust_box_new: rustc_hash::FxHashMap<usize, crate::resolver::TextEdit>,
    /// See [`crate::ast::Program::foreign_calls`].
    pub(crate) foreign_calls: rustc_hash::FxHashMap<usize, crate::resolver::TextEdit>,
    /// See [`crate::ast::Program::expr_stmt_semis`].
    pub(crate) expr_stmt_semis: rustc_hash::FxHashMap<crate::resolver::SpanKey, crate::token::Span>,
    /// See [`crate::ast::Program::rust_heap_ctor`].
    pub(crate) rust_heap_ctor: rustc_hash::FxHashMap<usize, crate::resolver::TextEdit>,
    /// See [`crate::ast::Program::parse_target_edits`].
    pub(crate) parse_target_edits: rustc_hash::FxHashMap<usize, crate::resolver::TextEdit>,
    /// See [`crate::ast::Program::range_arg_edits`].
    pub(crate) range_arg_edits: rustc_hash::FxHashMap<usize, crate::resolver::TextEdit>,
    /// See [`crate::ast::Program::paren_return_types`].
    pub(crate) paren_return_types: rustc_hash::FxHashSet<crate::resolver::SpanKey>,
    /// See [`crate::ast::Program::cast_operand_ends`].
    pub(crate) cast_operand_ends: rustc_hash::FxHashMap<crate::resolver::SpanKey, usize>,
    /// See [`crate::ast::Program::paren_casts`].
    pub(crate) paren_casts: rustc_hash::FxHashMap<crate::resolver::SpanKey, (Span, String)>,
    /// See [`crate::ast::Program::whole_value_casts`].
    pub(crate) whole_value_casts: rustc_hash::FxHashMap<crate::resolver::SpanKey, (Span, String)>,
    /// See [`crate::ast::Program::mut_fn_types`].
    pub(crate) mut_fn_types: rustc_hash::FxHashSet<crate::resolver::SpanKey>,
    /// See [`crate::ast::Program::cross_task_fn_types`].
    pub(crate) cross_task_fn_types: rustc_hash::FxHashSet<crate::resolver::SpanKey>,
    /// See [`crate::ast::Program::fn_type_marker_points`].
    pub(crate) fn_type_marker_points:
        rustc_hash::FxHashMap<crate::resolver::SpanKey, crate::token::Span>,
    /// `true` only while parsing the bracket list of a TRAIT BOUND, which is
    /// the one position where `IDENT = TYPE` means an associated-type binding
    /// rather than a positional type argument (B-2026-08-21-9). Cleared while
    /// the binding's own type is parsed, so a nested `Vec[X = Y]` inside
    /// `Src[Item = Vec[...]]` stays the error it should be.
    pub(crate) assoc_bindings_allowed: bool,
    /// Bindings collected by [`Parser::parse_generic_type_args`] while
    /// `assoc_bindings_allowed` is set; drained by `parse_trait_bound`.
    pub(crate) pending_assoc_bindings: Vec<crate::ast::AssocBinding>,
    /// Set while a type path's bracket list is parsed: the one list that may
    /// end in an effect argument (`EventBus[with E]`), which is left in
    /// `pending_effect_args` for the path to take.
    pub(crate) effect_args_allowed: bool,
    pub(crate) pending_effect_args: Option<EffectList>,
    /// Set by `parse_type` when it consumes a *permitted* `frozen` keyword,
    /// read and cleared by `parse_param` into [`Param::is_frozen`]. The
    /// keyword is recognized deep inside the type parse (so the misplaced-use
    /// diagnostic can be focused wherever it appears) but belongs to the
    /// parameter, so it has to travel back up — this is the wire. Never set
    /// when `frozen_ok` was false, so a rejected `frozen` cannot mark its
    /// parameter.
    pub(crate) frozen_consumed: bool,
    /// Set by `try_parse_self_param` when it consumes a `frozen self`
    /// receiver, read and cleared by `parse_fn` into
    /// [`crate::ast::Function::self_is_frozen`]. The sibling of
    /// [`Self::frozen_consumed`], and the same wire for the same reason: the
    /// keyword is recognized in the receiver parse but belongs to the
    /// function. B-2026-08-01-33 stage 2.7.
    pub(crate) frozen_self_consumed: bool,
    /// Set when the receiver just parsed was written `own self` (D5); taken
    /// by the function or trait method that owns the receiver.
    pub(crate) own_self_consumed: bool,
    /// Set for exactly one `parse_type` call: a function's return type.
    /// There a trailing `with` is the function's own clause, so a `Fn(..)`
    /// or `impl Trait` at the tail does not take it; a function-type or
    /// `impl` return with its own clause is parenthesized
    /// (`-> (Fn(Str) with writes(Log)) with allocates(Heap)`).
    pub(crate) return_tail: bool,
    /// The token position where a function type's return type begins
    /// (`Fn() -> (Fn() -> i64) + CrossTask`): a parenthesized type there
    /// leaves a following `+ CrossTask` to the function type around it
    /// (core semantics §6.3, B-2026-11-02-20).
    pub(crate) fn_return_at: Option<usize>,
    /// While the type after `as` is parsed: a `<` after it is a comparison
    /// (`x as i64 < y`), not Rust's generic arguments.
    pub(crate) cast_type: bool,
    /// Set by an `impl` body just before it parses a method: the next
    /// `parse_function` reads its name as a member name, which any keyword
    /// may be (design.md §3). Taken one-shot, like `return_tail`.
    pub(crate) member_fn_name: bool,
    /// `freeze <place>` initializer spans collected during the parse, moved
    /// onto [`crate::ast::Program::freeze_spans`] at the end. B-2026-08-01-33
    /// stage 3.
    pub(crate) freeze_spans: rustc_hash::FxHashSet<crate::resolver::SpanKey>,
    /// `ref name` binding spans, moved onto
    /// [`crate::ast::Program::ref_binding_spans`] at the end.
    pub(crate) ref_binding_spans: rustc_hash::FxHashSet<crate::resolver::SpanKey>,
    /// Moved onto [`crate::ast::Program::block_const_spans`] at the end.
    pub(crate) block_const_spans: rustc_hash::FxHashSet<crate::resolver::SpanKey>,
    /// `mut ref name` binding spans, moved onto
    /// [`crate::ast::Program::mut_ref_binding_spans`] at the end.
    pub(crate) mut_ref_binding_spans: rustc_hash::FxHashSet<crate::resolver::SpanKey>,
    /// `ref` keyword offsets of those bindings, moved onto
    /// [`crate::ast::Program::ref_binding_keywords`] at the end.
    pub(crate) ref_binding_keywords: rustc_hash::FxHashMap<crate::resolver::SpanKey, usize>,
    /// Statement-position lint-level overrides, moved onto
    /// [`crate::ast::Program::stmt_lint_overrides`] at the end.
    pub(crate) stmt_lint_overrides:
        rustc_hash::FxHashMap<crate::resolver::SpanKey, Vec<crate::lints::LintLevelOverride>>,
    /// `Map[K, V, H]` / `Set[T, H]` hasher selectors collected during the
    /// parse, moved onto [`crate::ast::Program::container_hashers`] at the end.
    /// B-2026-08-21-6.
    pub(crate) container_hashers:
        rustc_hash::FxHashMap<crate::resolver::SpanKey, crate::hasher_kind::HasherKind>,
    /// Script mode (design.md § Script mode): when `true` (the default —
    /// root source files), top-level statements synthesize a unit
    /// `fn main()`. Item-only contexts — comptime `ast.item` quotes,
    /// `mod`-loaded module files — set this `false` via
    /// [`Parser::items_only`], turning top-level statements into a parse
    /// error instead of a synthesized entry point.
    pub(crate) allow_script_mode: bool,
    /// Set when top-level statements are seen; surfaced as
    /// [`ParseResult::had_top_level_statements`].
    pub(crate) saw_top_level_statements: bool,
    /// Active labels for disambiguating `break label` vs `break value` and
    /// for routing labeled-block label scopes. Each entry carries a
    /// `LabelKind` tag (`Loop` for labeled loops, `Block` for labeled
    /// blocks) — the kind is consulted by the resolver, not the parser,
    /// but the parser tracks it so that `is_known_label` lookups (which
    /// disambiguate `break label expr`) work uniformly across both label
    /// kinds. Pushed at the entry to a labeled construct, popped on exit.
    pub(crate) loop_labels: Vec<(String, LabelKind)>,
    /// Labels declared in the pre-sigil spelling (`outer: loop`). A `break
    /// outer` naming one of them is that label, diagnosed with the `'` fix;
    /// any other bare name after `break` is a value.
    pub(crate) bare_label_names: std::collections::HashSet<String>,
    /// Items produced by a single `parse_item` call beyond the first. Only
    /// `import` fills this today: design.md defines nested grouping
    /// (`import a.{b.{c, d}, e};`) as *equivalent to* three flat imports, and
    /// three flat imports are three `Item::Import`s. `parse_item` returns one
    /// `Item`, so the extras queue here and the item loop drains them before
    /// its next `parse_item` call — the statement's decls therefore land in
    /// `Program::items` contiguously and in source order.
    pub(crate) pending_extra_items: Vec<Item>,
    /// Doc-comment text accumulated by the leading-`///` collection at the
    /// top of `parse_item`. Each item-construction site calls
    /// `Self::take_pending_doc` when filling the new node's `doc_comment`
    /// field. Cleared after consumption so a subsequent item without docs
    /// gets `None`.
    pub(crate) pending_doc: Option<String>,
    /// The span of the `///` run behind [`Self::pending_doc`], first line to
    /// last, so a run nothing takes can be reported where it is written.
    pending_doc_span: Option<Span>,
    /// See [`crate::ast::Program::discarded_doc_comments`].
    discarded_doc_comments: Vec<(Span, &'static str)>,
    /// See [`crate::ast::Program::nested_fstrings`].
    pub(crate) nested_fstrings: rustc_hash::FxHashMap<usize, Option<crate::resolver::TextEdit>>,
    /// Stack of `FnContext`s for the function-like signature we are currently
    /// parsing parameters for. Drives the anonymous-parameter focused
    /// diagnostic: trait-method bodies emit `E_TRAIT_METHOD_ANONYMOUS_PARAM`
    /// while free / impl / extern function bodies emit
    /// `E_FN_ANONYMOUS_PARAM`. Empty when we are not inside a parameter
    /// list (e.g., parsing a struct field or top-level expression).
    pub(crate) fn_context_stack: Vec<FnContext>,
    /// Stack of effect-variable names declared in the enclosing function /
    /// trait method's `[with E]` generic params. Pushed when entering a
    /// signature with declared effect vars; popped when leaving. Consulted
    /// when parsing nested `Fn(...) with E` types in parameter / return
    /// position so an `E` token resolves to `EffectItem::Variable(E)`
    /// instead of `EffectItem::Group(E)`. Empty top frame means no
    /// effect variables in scope (parser treats all `with X` items as
    /// group references).
    pub(crate) effect_var_stack: Vec<Vec<String>>,
    /// Stack of `impl Trait` rejection reasons for the current
    /// type-expression position. `impl Trait` slice 1: parsing a
    /// `TypeKind::ImplTrait` is only legal in four positions
    /// (argument types, return types, trait-method return types, RHS
    /// of `type` aliases — see design.md § `impl Trait` (Existential
    /// Types)). Every other type-position pushes a `BlockReason` onto
    /// the stack via [`Parser::push_impl_trait_block`] before
    /// descending into [`Parser::parse_type`]; the top-of-stack
    /// reason is consulted in `parse_type` when an `impl` keyword is
    /// observed and produces the corresponding rejection diagnostic
    /// (one of `E_IMPL_TRAIT_IN_NESTED_POSITION`,
    /// `E_IMPL_TRAIT_IN_TRAIT_METHOD_ARG`). Empty stack means the
    /// current position is legal for `impl Trait`.
    pub(crate) impl_trait_block_stack: Vec<ImplTraitBlockReason>,
    /// Suppresses the Value-class name lint on a parameter binding while
    /// parsing a `comptime` parameter. A `comptime` param may bind a `Type`
    /// pseudovalue, for which a Type-class name (`comptime T: Type`) is
    /// idiomatic and spec-sanctioned (deferred.md § Comptime, form 3). Set
    /// around the `parse_param_pattern` call for comptime params; the binding
    /// case consults it instead of unconditionally requiring snake_case.
    pub(crate) allow_type_class_param_name: bool,
    /// When set, a `Name { … }` at the current position is NOT treated as a
    /// struct literal — the trailing `{` opens a block/body instead. Set only
    /// while parsing an `if` / `while` / `for` CONDITION (where `Name { body }`
    /// must read as `Name` + the loop/branch body block, e.g. `if DEBUG { … }`),
    /// and cleared again inside any delimited sub-expression (parens, call args,
    /// array literals, a committed struct-literal body) so a struct literal
    /// nested in a condition — `while cond(P { x }) { … }` — still parses. NOT
    /// set for a `match` scrutinee: Kāra allows an unparenthesized struct-literal
    /// scrutinee (`match P { x: 1 } { … }`), which the restriction would break.
    /// See `looks_like_struct_literal`.
    pub(crate) no_struct_literal: bool,
    /// The type name an `impl` block's methods are for (`P` in `impl P`,
    /// `Stack` in `impl[T] Stack[T]`), while its body parses: a Rust-style
    /// `Self { .. }` struct literal is named and rewritten to it
    /// (B-2026-11-07-42), and so is Rust's `Self::name(..)` path (kata 1067).
    pub(crate) impl_self_name: Option<String>,
    /// When set, `seq {` and `par {` at the current position read as the
    /// NAME `seq` / `par` followed by the body block, not as a `seq {}` /
    /// `par {}` block: the words are contextual, so `let seq = true; if seq {
    /// … }` is a branch on a local (B-2026-10-15-43). Set for an `if` /
    /// `while` / `for` condition, as `no_struct_literal` is, and also for a
    /// `match` scrutinee, where a block is never what `match seq {` means.
    /// Cleared with `no_struct_literal` inside delimited sub-expressions.
    pub(crate) keyword_block_is_name: bool,
    /// Machine-applicable fix edits synthesized during parsing, keyed by the
    /// span of the diagnostic they resolve. Mirrors ownership.rs's
    /// `error_fix_diffs` side-channel: rather than widen `ParseError` (built
    /// at ~48 sites) with a `replacement` field, edits are hung here and
    /// matched back to their diagnostic by span in `collect_diagnostics`
    /// (JSON `replacement`) and `cmd_fix`. Currently populated only by the
    /// comma-separated-effect-clause recovery in `parse_effect_list`.
    pub(crate) fix_edits: FxHashMap<crate::resolver::SpanKey, crate::resolver::TextEdit>,
    /// Multi-edit sibling of [`Self::fix_edits`], same key, same rationale —
    /// see [`ParseResult::fix_diffs`] for why a repair sometimes needs both
    /// halves or neither.
    pub(crate) fix_diffs: FxHashMap<crate::resolver::SpanKey, Vec<crate::resolver::TextEdit>>,
    /// Whether a `use std::rc::Rc;` / `use std::cell::RefCell;` line has
    /// carried the `Rc<RefCell<T>>` rewrite already (`parser/rust_rc.rs`):
    /// the second such line deletes only itself. B-2026-11-08-65.
    pub(crate) rc_envelope_done: bool,
    /// While a `let` pattern is parsed: where the pattern starts, and
    /// whether `mut` already stands before it. A `mut` inside the pattern
    /// (Rust's `let (mut a, b)`) is then answered with the Kāra spelling
    /// and a fix that moves it there (B-2026-10-14-57).
    pub(crate) let_pattern: Option<(usize, bool)>,
    /// Each `mut` reported inside a pattern outside a `let`: the report's
    /// key, the edit deleting the `mut`, and the name. The arm, `if let`,
    /// `while let` or `for` that parsed the pattern turns them into a fix
    /// once its body is parsed (B-2026-10-25-8).
    pub(crate) pattern_muts: Vec<(crate::resolver::SpanKey, crate::resolver::TextEdit, String)>,
    /// The span of each Rust `&` / `&mut` pattern sigil reported, so an
    /// `if let` / `while let` over `Some(&x)` can turn the sigil's deletion
    /// into `.cloned()` on its scrutinee (B-2026-11-08-13).
    pub(crate) amp_sigils: Vec<Span>,
    /// Offsets of the `!`s already reported as a Rust macro call. Two
    /// recognizers see the same `!` when recovery re-parses it (`expect` and
    /// the prefix parser), and each reported it (B-2026-10-20-5).
    pub(crate) macro_bangs_reported: rustc_hash::FxHashSet<usize>,
    /// The keywords a binding was named by and recovered as `r#NAME`
    /// ([`Self::recover_keyword_as_ident`]): a later use of one before a `{`
    /// (`if and { .. }`) is that binding, and gets the same rename
    /// (B-2026-10-29-13).
    pub(crate) escaped_bindings: rustc_hash::FxHashSet<&'static str>,
    /// The text the tokens were lexed from, and the offset its first byte
    /// has in span coordinates, when the caller supplied it
    /// ([`Self::with_source`]). A fix that moves source text (the
    /// `format!` rewrite carries each argument into the literal) reads it
    /// here; without it, that fix is not offered.
    pub(crate) source: Option<(std::sync::Arc<str>, usize)>,
    /// Where the top-level item being parsed starts, the place a nested
    /// `fn`'s fix moves it to ([`Self::hoist_nested_fn`]).
    pub(crate) item_start: Option<usize>,
    /// Each turbofish rejected so far (`collect::<Vec<char>>`): the
    /// diagnostic's span, the method's name, the index of the token after
    /// the list, and the list's Kāra spelling when it is one type a binding
    /// annotation can carry. A `let` whose whole initializer is that call
    /// moves the type onto its binding (B-2026-10-24-13).
    pub(crate) turbofishes: Vec<(Span, String, usize, Option<String>)>,
    /// Each closure written with Rust's return type (`|x: i64| -> T`): where
    /// its `|` starts, the arrow's span (its diagnostic's key) and `T`'s
    /// text. A `let` whose whole initializer is that closure moves the type
    /// onto its binding (B-2026-10-29-7).
    pub(crate) closure_returns: Vec<(usize, Span, String)>,
}

/// The Rust macros whose name is also a Kāra function, so deleting the `!`
/// of `name!(...)` leaves a valid call. Any other macro has no such twin and
/// its `!` must not be deleted (B-2026-10-20-5). A test resolves each name.
pub const MACRO_FUNCTION_TWINS: &[&str] = &[
    "println",
    "print",
    "eprintln",
    "panic",
    "assert",
    "assert_eq",
    "assert_ne",
    "todo",
    "unreachable",
    "dbg",
];

/// The macros of [`MACRO_FUNCTION_TWINS`] whose first argument is a Rust
/// format string. Their Kāra twins take one string, so a call with format
/// arguments is rewritten to an f-string rather than losing its `!`
/// (B-2026-11-01-8).
/// Why a `///` run documents nothing (design.md §3 Doc comments), the tail
/// of the warning the type checker reports from
/// `Program::discarded_doc_comments`.
/// A blank line or a plain comment after the run, a `}`, or the end of the
/// file: one wording, since `karac check` appends the library after the
/// user's source and the parser cannot tell the end of the file from a gap.
pub(crate) const DOC_NO_ITEM: &str =
    "no item directly follows it: a blank line, a comment, a `}` or the end of the file comes first";
pub(crate) const DOC_STMT: &str = "a statement takes no doc comment";
pub(crate) const DOC_DECL: &str = "what follows it takes no doc comment";
pub(crate) const DOC_IMPL: &str =
    "an `impl` block takes no doc comment; its methods document themselves";

const PRINT_MACROS: &[&str] = &["println", "print", "eprintln", "panic"];

/// Reason an `impl Trait` type expression is rejected at the current
/// parser position. Encoded as an enum (rather than a single
/// `disallow_impl_trait: bool` flag) so the rejection diagnostic can
/// route to the position-specific error code + suggestion text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImplTraitBlockReason {
    /// `impl Trait` inside a nested generic-argument list — e.g.
    /// `Vec[impl T]`. Rejected with `E_IMPL_TRAIT_IN_NESTED_POSITION`
    /// per the design.md spec: "`Vec[impl T]` is rejected at v1 with
    /// a diagnostic suggesting an explicit generic parameter — `impl
    /// Trait` deep in nested type positions is post-v1".
    NestedGenericArg,
    /// `impl Trait` in an argument-type position of a trait method
    /// declaration. Rejected with `E_IMPL_TRAIT_IN_TRAIT_METHOD_ARG`
    /// per design.md: "The compiler does not allow argument-position
    /// `impl Trait` in trait methods (use the explicit generic form
    /// there instead)." Argument-position `impl Trait` in free
    /// functions and impl-block methods stays legal (slice 2 desugars
    /// those to anonymous generic parameters).
    TraitMethodArg,
}

impl Parser {
    pub fn new(tokens: Vec<SpannedToken>) -> Self {
        Parser {
            tokens,
            pos: 0,
            loop_labels: Vec::new(),
            bare_label_names: std::collections::HashSet::new(),
            errors: Vec::new(),
            recursion_depth: 0,
            pending_extra_items: Vec::new(),
            frozen_ok: false,
            generic_defaults_ok: false,
            escaping_ok: false,
            escaping_fn_types: rustc_hash::FxHashSet::default(),
            vec_macro_literals: rustc_hash::FxHashMap::default(),
            rust_box_new: rustc_hash::FxHashMap::default(),
            foreign_calls: rustc_hash::FxHashMap::default(),
            expr_stmt_semis: rustc_hash::FxHashMap::default(),
            rust_heap_ctor: rustc_hash::FxHashMap::default(),
            parse_target_edits: rustc_hash::FxHashMap::default(),
            range_arg_edits: rustc_hash::FxHashMap::default(),
            paren_return_types: rustc_hash::FxHashSet::default(),
            cast_operand_ends: rustc_hash::FxHashMap::default(),
            paren_casts: rustc_hash::FxHashMap::default(),
            whole_value_casts: rustc_hash::FxHashMap::default(),
            mut_fn_types: rustc_hash::FxHashSet::default(),
            cross_task_fn_types: rustc_hash::FxHashSet::default(),
            fn_type_marker_points: rustc_hash::FxHashMap::default(),
            assoc_bindings_allowed: false,
            pending_assoc_bindings: Vec::new(),
            effect_args_allowed: false,
            pending_effect_args: None,
            frozen_consumed: false,
            frozen_self_consumed: false,
            own_self_consumed: false,
            return_tail: false,
            fn_return_at: None,
            cast_type: false,
            member_fn_name: false,
            freeze_spans: rustc_hash::FxHashSet::default(),
            ref_binding_spans: rustc_hash::FxHashSet::default(),
            block_const_spans: rustc_hash::FxHashSet::default(),
            mut_ref_binding_spans: rustc_hash::FxHashSet::default(),
            ref_binding_keywords: rustc_hash::FxHashMap::default(),
            stmt_lint_overrides: rustc_hash::FxHashMap::default(),
            container_hashers: rustc_hash::FxHashMap::default(),
            allow_script_mode: true,
            saw_top_level_statements: false,
            pending_doc: None,
            pending_doc_span: None,
            discarded_doc_comments: Vec::new(),
            nested_fstrings: rustc_hash::FxHashMap::default(),
            fn_context_stack: Vec::new(),
            effect_var_stack: Vec::new(),
            impl_trait_block_stack: Vec::new(),
            allow_type_class_param_name: false,
            no_struct_literal: false,
            impl_self_name: None,
            keyword_block_is_name: false,
            fix_edits: FxHashMap::default(),
            fix_diffs: FxHashMap::default(),
            rc_envelope_done: false,
            let_pattern: None,
            pattern_muts: Vec::new(),
            amp_sigils: Vec::new(),
            macro_bangs_reported: rustc_hash::FxHashSet::default(),
            escaped_bindings: rustc_hash::FxHashSet::default(),
            source: None,
            item_start: None,
            turbofishes: Vec::new(),
            closure_returns: Vec::new(),
        }
    }

    /// Supply the text the tokens came from, whose first byte sits at span
    /// offset `base`. See [`Self::source`].
    pub fn with_source(mut self, source: &str, base: usize) -> Self {
        self.source = Some((std::sync::Arc::from(source), base));
        self
    }

    /// Push an `impl Trait` rejection reason — see
    /// [`ImplTraitBlockReason`]. Caller is responsible for the
    /// matching [`Parser::pop_impl_trait_block`] after the
    /// `parse_type` (or `parse_generic_type_args`) call returns.
    pub(crate) fn push_impl_trait_block(&mut self, reason: ImplTraitBlockReason) {
        self.impl_trait_block_stack.push(reason);
    }

    pub(crate) fn pop_impl_trait_block(&mut self) {
        self.impl_trait_block_stack.pop();
    }

    /// Returns the current top-of-stack rejection reason, if any.
    pub(crate) fn current_impl_trait_block(&self) -> Option<ImplTraitBlockReason> {
        self.impl_trait_block_stack.last().copied()
    }

    fn current_effect_vars(&self) -> &[String] {
        self.effect_var_stack
            .last()
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Opens an item's effect-variable scope: the enclosing item's variables
    /// (an `impl[with E]` around its methods) and the item's own `[with E]`
    /// (design.md § Effect variables). Returns the combined list; the caller
    /// pops it when the item ends.
    pub(crate) fn push_effect_scope(&mut self, gp: Option<&GenericParams>) -> Vec<String> {
        let mut vars = self.current_effect_vars().to_vec();
        for ep in gp.iter().flat_map(|gp| gp.effect_params.iter()) {
            if !vars.contains(&ep.name) {
                vars.push(ep.name.clone());
            }
        }
        self.effect_var_stack.push(vars.clone());
        vars
    }

    /// Drain pending leading `///` doc-comment text. Returns `None` when no
    /// doc comments are attached. Called from each item-construction site.
    fn take_pending_doc(&mut self) -> Option<String> {
        self.pending_doc_span = None;
        self.pending_doc.take()
    }

    /// A pending `///` run that nothing takes: design.md §3 Doc comments
    /// discards it with a warning (`Program::discarded_doc_comments`).
    /// `why` says what follows the run instead of a documented item.
    pub(crate) fn discard_pending_doc(&mut self, why: &'static str) {
        let span = self.pending_doc_span.take();
        if self.pending_doc.take().is_some() {
            if let Some(span) = span {
                self.discarded_doc_comments.push((span, why));
            }
        }
    }

    /// Consume any leading `Token::DocComment` tokens at the current
    /// position and accumulate their text (newline-joined) into
    /// `pending_doc`. Idempotent — safe to call when no doc comments
    /// follow. Stops at the first non-doc-comment token.
    ///
    /// design.md §3: a run is consecutive `///` lines, and it attaches to
    /// what directly follows it, "with no blank line and no other token in
    /// between". So a blank line (or a plain `//` comment, which leaves no
    /// token) inside or after the run, or the end of the file, leaves the
    /// lines before it documenting nothing: they are discarded with a
    /// warning rather than attached to whatever comes later.
    fn collect_leading_doc_comments(&mut self) {
        let mut buf: Option<String> = None;
        // The run so far: its span (offset of the first line to the end of
        // the last) and the lines it starts and ends on.
        let mut run: Option<(Span, usize)> = None;
        while let Token::DocComment(text) = self.peek_token() {
            let line = text.clone();
            let span = self.current_span();
            self.advance();
            if let Some((r, last)) = run {
                if span.line > last + 1 {
                    self.discarded_doc_comments.push((r, DOC_NO_ITEM));
                    buf = None;
                    run = None;
                }
            }
            run = Some(match run {
                Some((r, _)) => (
                    Span {
                        length: (span.offset + span.length).saturating_sub(r.offset),
                        ..r
                    },
                    span.line,
                ),
                None => (span, span.line),
            });
            match &mut buf {
                Some(s) => {
                    s.push('\n');
                    s.push_str(&line);
                }
                None => buf = Some(line),
            }
        }
        let Some((at, last)) = run else {
            return;
        };
        if self.is_at_end() || self.check(&Token::RightBrace) || self.current_span().line > last + 1
        {
            self.discarded_doc_comments.push((at, DOC_NO_ITEM));
        } else {
            self.discard_pending_doc(DOC_DECL);
            self.pending_doc = buf;
            self.pending_doc_span = Some(at);
        }
    }

    /// Disable script-mode synthesis for item-only contexts (comptime
    /// `ast.item` quotes, module files). Top-level statements then produce
    /// a parse error rather than a synthesized `fn main()`.
    pub fn items_only(mut self) -> Self {
        self.allow_script_mode = false;
        self
    }

    pub fn parse(mut self) -> ParseResult {
        let mut program = self.parse_program();
        crate::node_ids::assign_node_ids(&mut program);
        ParseResult {
            program,
            errors: self.errors,
            had_top_level_statements: self.saw_top_level_statements,
            fix_edits: self.fix_edits,
            fix_diffs: self.fix_diffs,
        }
    }

    // ── Program ──────────────────────────────────────────────────

    fn parse_program(&mut self) -> Program {
        // Consume any leading `//!` module-level doc comments. They may
        // only appear before the first item; subsequent `//!` lines are
        // a parse error, but for the v1 MVP we silently treat trailing
        // `//!` as a regular line comment via the lexer's normal path.
        let mut module_doc_lines: Vec<String> = Vec::new();
        while let Token::ModuleDocComment(text) = self.peek_token() {
            module_doc_lines.push(text.clone());
            self.advance();
        }
        let module_doc_comment = if module_doc_lines.is_empty() {
            None
        } else {
            Some(module_doc_lines.join("\n"))
        };

        // Phase-7 line 43 — module-level inner attributes
        // (`#![rc_budget(max: N)]`, etc.). Must appear before any
        // top-level item; subsequent outer attributes (`#[...]`)
        // belong to the first item that follows.
        let inner_attrs = self.parse_inner_attributes();

        let mut items = Vec::new();
        // Script mode (design.md § Script mode, phase-8 Q7): top-level
        // STATEMENTS — expression statements, assignments, Value-class
        // (lowercase) `let`s — collect here in file order and become the
        // body of a synthesized `fn main()` after the loop. Items hoist to
        // module scope exactly as before. Previously these statements were
        // SILENTLY DROPPED (`parse_item` returned `None` with no error and
        // `synchronize_to_item` skipped them), so a main-less script
        // "passed" `karac check` and did nothing under `run --interp`.
        let mut script_stmts: Vec<Stmt> = Vec::new();
        while !self.is_at_end() {
            let errs_before = self.errors.len();
            self.item_start = Some(self.current_span().offset);
            match self.parse_item() {
                Some(item) => {
                    items.push(item);
                    items.append(&mut self.pending_extra_items);
                    self.discard_pending_doc(DOC_DECL);
                }
                None if self.errors.len() == errs_before && !self.is_at_end() => {
                    self.discard_pending_doc(DOC_STMT);
                    // Non-item head with no diagnostic — a top-level
                    // statement. Parse it as one; a statement parse that
                    // ALSO fails falls back to the item-level recovery so
                    // the loop always makes progress.
                    let pos_before = self.pos;
                    match self.parse_top_level_script_stmt() {
                        Some(stmt) => script_stmts.push(stmt),
                        None => {
                            if self.pos == pos_before {
                                self.synchronize_to_item();
                            }
                        }
                    }
                }
                None => {
                    // Error recovery: skip to next item-starting token. A
                    // doc run before the failed item is dropped unreported:
                    // the error already names the line.
                    self.pending_doc = None;
                    self.pending_doc_span = None;
                    self.synchronize_to_item();
                }
            }
        }
        // B-2026-08-01-36 — a top-level `let` with a plain lowercase name in a
        // file that ALSO defines `fn main()`. The recognizer in `items.rs` keys
        // module-binding-ness on the name's CASE, so `let mut count: i64 = 0;`
        // fell through to a script statement and then collided with the
        // explicit main, producing an error about a completely different
        // subject — one whose two suggestions ("move the statements into
        // `main`", "remove the explicit `main`") are both destructive, and
        // neither of which mentions capitalisation.
        //
        // design.md § Script mode makes script mode and an explicit `main`
        // MUTUALLY EXCLUSIVE, so in a file with explicit main a top-level `let`
        // cannot be a script statement — it can only be an (incorrectly named)
        // module binding. Reclassify those so the resolver's
        // E_MODULE_BINDING_NAMING fires instead: it names the rule and carries
        // a machine-applicable rename that rewrites use sites too, turning a
        // misdirecting parse error into a `karac fix` case. Statements that are
        // NOT module-binding shaped (expression statements, assignments, tuple
        // destructures) stay behind and keep the ambiguity error, which is the
        // right diagnostic for them.
        if self.allow_script_mode
            && !script_stmts.is_empty()
            && items
                .iter()
                .any(|it| matches!(it, Item::Function(f) if f.name == "main"))
        {
            let mut kept: Vec<Stmt> = Vec::new();
            for st in std::mem::take(&mut script_stmts) {
                let reclassified = match (&st.kind, &st.span) {
                    (
                        StmtKind::Let {
                            is_mut,
                            pattern:
                                Pattern {
                                    kind: PatternKind::Binding(name),
                                    span: name_span,
                                    ..
                                },
                            ty,
                            value,
                        },
                        stmt_span,
                    ) => Some(ModuleBinding {
                        span: *stmt_span,
                        attributes: Vec::new(),
                        doc_comment: None,
                        is_pub: false,
                        is_private: false,
                        is_mut: *is_mut,
                        name: name.clone(),
                        name_span: *name_span,
                        ty: ty.clone(),
                        value: value.clone(),
                        deprecation: None,
                        unstable: None,
                        lint_overrides: Vec::new(),
                    }),
                    _ => None,
                };
                match reclassified {
                    Some(mb) => items.push(Item::ModuleBinding(mb)),
                    None => kept.push(st),
                }
            }
            script_stmts = kept;
        }
        // Record the shape BEFORE branching on what to do about it: both
        // arms below mean the source carried top-level statements, and the
        // item-only arm turns them into an error that erases the evidence.
        self.saw_top_level_statements |= !script_stmts.is_empty();
        let mut script_main = false;
        if !script_stmts.is_empty() && !self.allow_script_mode {
            // Item-only context (`ast.item` quote, module file): top-level
            // statements are an error here, never a synthesized main.
            self.error_at(
                "top-level statements are not allowed in this context — only item \
                 declarations (fn, struct, enum, trait, impl, use); script mode \
                 applies only to a root source file",
                script_stmts[0].span,
            );
        } else if !script_stmts.is_empty() {
            // Ambiguity rule (design.md § Script mode > Rejecting ambiguous
            // files): top-level statements + an explicit `fn main` is a
            // compile error with one obvious fix in each direction.
            let explicit_main = items.iter().find_map(|it| match it {
                Item::Function(f) if f.name == "main" => Some(f.span),
                _ => None,
            });
            let first_span = script_stmts[0].span;
            if let Some(main_span) = explicit_main {
                self.error_at(
                    &format!(
                        "file contains both top-level statements and an explicit `fn main()` \
                         (defined at line {}); move the statements into `main`, or remove the \
                         explicit `main` to use script mode",
                        main_span.line
                    ),
                    first_span,
                );
            } else {
                // Synthesize `fn main() -> Result[(), AnyError]` wrapping
                // the statements in file order and ending with `Ok(())`
                // (design.md § Script mode), so a top-level `?` works and an
                // `Err` exits 1 with `Error: ..` (B-2026-11-04-19). Effects
                // are inferred exactly as for a user-written private
                // `fn main()`.
                let last_span = script_stmts.last().map(|s| s.span).unwrap();
                let (return_type, tail) = self.script_main_signature();
                script_main = true;
                let body_span = Span {
                    line: first_span.line,
                    column: first_span.column,
                    offset: first_span.offset,
                    length: (last_span.offset + last_span.length).saturating_sub(first_span.offset),
                };
                items.push(Item::Function(Function {
                    span: first_span,
                    attributes: Vec::new(),
                    doc_comment: None,
                    is_pub: false,
                    is_private: false,
                    is_unsafe: false,
                    is_comptime: false,
                    name: "main".to_string(),
                    generic_params: None,
                    params: Vec::new(),
                    self_param: None,
                    self_is_frozen: false,
                    self_is_own: false,
                    self_span: None,
                    return_type,
                    effects: None,
                    requires: Vec::new(),
                    ensures: Vec::new(),
                    where_clause: None,
                    body: Block {
                        stmts: script_stmts,
                        final_expr: tail.map(Box::new),
                        span: body_span,
                    },
                    stdlib_origin: false,
                    deprecation: None,
                    unstable: None,
                    is_track_caller: false,
                    inline_hint: None,
                    is_cold: false,
                    is_gpu: false,
                    lint_overrides: Vec::new(),
                    profile_compat: Vec::new(),
                    no_effect: Vec::new(),
                    abi: None,
                }));
            }
        }
        let std_cmp_reverse_import = self.std_cmp_reverse_import(&items);
        Program {
            script_main,
            items,
            module_doc_comment,
            inner_attrs,
            freeze_spans: std::mem::take(&mut self.freeze_spans),
            ref_binding_spans: std::mem::take(&mut self.ref_binding_spans),
            block_const_spans: std::mem::take(&mut self.block_const_spans),
            mut_ref_binding_spans: std::mem::take(&mut self.mut_ref_binding_spans),
            ref_binding_keywords: std::mem::take(&mut self.ref_binding_keywords),
            escaping_fn_types: std::mem::take(&mut self.escaping_fn_types),
            vec_macro_literals: std::mem::take(&mut self.vec_macro_literals),
            discarded_doc_comments: std::mem::take(&mut self.discarded_doc_comments),
            nested_fstrings: std::mem::take(&mut self.nested_fstrings),
            rust_box_new: self.rust_box_new_and_types(),
            foreign_calls: std::mem::take(&mut self.foreign_calls),
            expr_stmt_semis: std::mem::take(&mut self.expr_stmt_semis),
            rust_heap_ctor_sides: self.rust_heap_ctor_sides(),
            rust_heap_ctor: std::mem::take(&mut self.rust_heap_ctor),
            rust_reverse_wraps: if self.rust_reverse_stays() {
                rustc_hash::FxHashMap::default()
            } else {
                self.rust_reverse_wraps()
            },
            rust_reverse_sort_keys: self.rust_reverse_sort_keys(),
            std_cmp_reverse_import,
            rust_std_paths: self.rust_std_paths(),
            parse_target_edits: std::mem::take(&mut self.parse_target_edits),
            range_arg_edits: std::mem::take(&mut self.range_arg_edits),
            paren_return_types: std::mem::take(&mut self.paren_return_types),
            cast_operand_ends: std::mem::take(&mut self.cast_operand_ends),
            paren_casts: std::mem::take(&mut self.paren_casts),
            whole_value_casts: std::mem::take(&mut self.whole_value_casts),
            mut_fn_types: std::mem::take(&mut self.mut_fn_types),
            cross_task_fn_types: std::mem::take(&mut self.cross_task_fn_types),
            fn_type_marker_points: std::mem::take(&mut self.fn_type_marker_points),
            stmt_lint_overrides: std::mem::take(&mut self.stmt_lint_overrides),
            container_hashers: std::mem::take(&mut self.container_hashers),
            ..Program::default()
        }
    }

    /// Which way each Rust heap constructor in [`Self::rust_heap_ctor`] pops,
    /// told from the heap it builds (kata 2034): a program may hold a plain
    /// `BinaryHeap` (largest first) beside a `BinaryHeap<Reverse<T>>`
    /// (smallest first), so one program-wide answer reverses one of them.
    /// The heap is the binding the constructor initializes: a field in a
    /// struct literal (`max_heap: BinaryHeap::new()`), a `let` (`let mut h =
    /// BinaryHeap::new()`, with or without a type), or an assignment
    /// (`self.h = ..`). That name is reversed when it is declared
    /// `NAME: BinaryHeap[Reverse[..]]` or something pushes `NAME.push(Reverse(
    /// ..))`, and plain when it is declared `NAME: BinaryHeap[T]` with another
    /// `T` or is pushed a bare item. A constructor whose name is neither
    /// follows the program: smallest first when some heap's items are
    /// wrapped in `Reverse` and no heap is known plain, largest first when
    /// nothing is wrapped. A name with both, or an unnamed constructor in a
    /// program with both kinds of heap, is unsure, and then no heap and no
    /// `Reverse` in the program gets a fix (the resolver holds them back).
    fn rust_heap_ctor_sides(&self) -> crate::ast::RustHeapSides {
        let mut sides = crate::ast::RustHeapSides::default();
        // Where a `Reverse` stays, every one does, as `std.cmp.Reverse`, so
        // every heap keeps Rust's largest-first order (B-2026-11-10-41).
        if self.rust_heap_ctor.is_empty() || self.rust_reverse_stays() {
            return sides;
        }
        let tok = |i: usize| self.tokens.get(i).map(|t| &t.token);
        let name_at = |i: usize| match tok(i) {
            Some(Token::Identifier { name, .. }) => Some(name.as_str()),
            _ => None,
        };
        let opens = |i: usize| matches!(tok(i), Some(Token::LeftBracket | Token::LessThan));
        let mut reversed: rustc_hash::FxHashSet<&str> = Default::default();
        let mut plain: rustc_hash::FxHashSet<&str> = Default::default();
        let mut pushed_bare: rustc_hash::FxHashSet<&str> = Default::default();
        for i in 0..self.tokens.len() {
            let Some(name) = name_at(i) else { continue };
            if matches!(tok(i + 1), Some(Token::Colon))
                && name_at(i + 2) == Some("BinaryHeap")
                && opens(i + 3)
            {
                if name_at(i + 4) == Some("Reverse") && opens(i + 5) {
                    reversed.insert(name);
                } else {
                    plain.insert(name);
                }
            }
            if matches!(tok(i + 1), Some(Token::Dot))
                && name_at(i + 2) == Some("push")
                && matches!(tok(i + 3), Some(Token::LeftParen))
            {
                if name_at(i + 4) == Some("Reverse") && matches!(tok(i + 5), Some(Token::LeftParen))
                {
                    reversed.insert(name);
                } else {
                    pushed_bare.insert(name);
                }
            }
        }
        // The binding each constructor initializes.
        let mut named: Vec<(usize, Option<&str>)> = Vec::new();
        for (k, t) in self.tokens.iter().enumerate() {
            if !self.rust_heap_ctor.contains_key(&t.span.offset) {
                continue;
            }
            let name = match tok(k.wrapping_sub(1)) {
                Some(Token::Colon) => name_at(k.wrapping_sub(2)),
                Some(Token::Equal) => {
                    // back to the statement's start: a `let` names its first
                    // identifier, an assignment the one before `=`
                    let mut j = k - 1;
                    let mut at_let = None;
                    while j > 0 {
                        j -= 1;
                        match tok(j) {
                            Some(Token::Let) => {
                                at_let = Some(j);
                                break;
                            }
                            Some(Token::Semicolon | Token::LeftBrace | Token::RightBrace) => break,
                            _ => {}
                        }
                    }
                    match at_let {
                        Some(l) if matches!(tok(l + 1), Some(Token::Mut)) => name_at(l + 2),
                        Some(l) => name_at(l + 1),
                        None => name_at(k.wrapping_sub(2)),
                    }
                }
                _ => None,
            };
            named.push((t.span.offset, name));
        }
        let side = |name: Option<&str>| -> Option<bool> {
            let name = name?;
            let rev = reversed.contains(name);
            let pl = plain.contains(name) || pushed_bare.contains(name);
            match (rev, pl) {
                (true, false) => Some(true),
                (false, true) => Some(false),
                _ => None,
            }
        };
        let any_plain = named.iter().any(|(_, n)| side(*n) == Some(false));
        let any_wrapped = !reversed.is_empty();
        for (offset, name) in named {
            let known = side(name);
            let both = name.is_some_and(|n| {
                reversed.contains(n) && (plain.contains(n) || pushed_bare.contains(n))
            });
            match known {
                Some(true) => {
                    sides.smallest.insert(offset);
                }
                Some(false) => {}
                None if both || (any_wrapped && any_plain) => {
                    sides.unsure.insert(offset);
                }
                None if any_wrapped => {
                    sides.smallest.insert(offset);
                }
                None => {}
            }
        }
        sides
    }

    /// [`crate::ast::Program::rust_box_new`]: the `Box.new(e)` calls the
    /// expression parser recorded, and each `Box[T]` written as a type
    /// (Rust's `Box<T>`, kata 2046) with the edit that writes `T` in its
    /// place. A `Box[T]` followed by `.` or `(` is an expression, not a
    /// type, and gets no edit.
    fn rust_box_new_and_types(
        &mut self,
    ) -> rustc_hash::FxHashMap<usize, crate::resolver::TextEdit> {
        let mut out = std::mem::take(&mut self.rust_box_new);
        let tok = |i: usize| self.tokens.get(i).map(|t| &t.token);
        for i in 0..self.tokens.len() {
            if !matches!(tok(i), Some(Token::Identifier { name, .. }) if name == "Box")
                || !matches!(tok(i + 1), Some(Token::LeftBracket))
                || (i > 0 && matches!(tok(i - 1), Some(Token::Dot)))
            {
                continue;
            }
            let mut depth = 0usize;
            let mut close = None;
            for (j, t) in self.tokens.iter().enumerate().skip(i + 1) {
                match t.token {
                    Token::LeftParen | Token::LeftBracket | Token::LeftBrace => depth += 1,
                    Token::RightParen | Token::RightBracket | Token::RightBrace => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(j);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(close) = close else { continue };
            if matches!(tok(close + 1), Some(Token::Dot | Token::LeftParen)) {
                continue;
            }
            let Some((source, base)) = self.source.as_ref() else {
                continue;
            };
            let open = self.tokens[i + 1].span;
            let (from, to) = (open.offset + open.length, self.tokens[close].span.offset);
            let Some(inner) = from
                .checked_sub(*base)
                .zip(to.checked_sub(*base))
                .and_then(|(a, b)| source.get(a..b))
                .map(str::trim)
                .filter(|t| !t.is_empty() && !t.contains(','))
            else {
                continue;
            };
            let start = self.tokens[i].span.offset;
            out.entry(start).or_insert(crate::resolver::TextEdit {
                offset: start,
                length: to + 1 - start,
                replacement: inner.to_string(),
            });
        }
        out
    }

    /// Each `Reverse(` written as a heap's item: the argument of a `.push(`,
    /// the payload of a `Some(` (kata 1057) or the pattern of a `let` (kata
    /// 2034), and each `Reverse[` written
    /// as the item type of a `BinaryHeap[` (kata 2008). See
    /// [`crate::ast::Program::rust_reverse_wraps`].
    fn rust_reverse_wraps(&self) -> rustc_hash::FxHashMap<usize, crate::resolver::TextEdit> {
        let mut out = rustc_hash::FxHashMap::default();
        let tok = |i: usize| self.tokens.get(i).map(|t| &t.token);
        let ident = |i: usize, want: &str| matches!(tok(i), Some(Token::Identifier { name, .. }) if name == want);
        for i in 2..self.tokens.len() {
            if !ident(i, "Reverse") {
                continue;
            }
            // The heap's item type, `BinaryHeap[Reverse[T]]` (kata 2008):
            // the edit writes `T`.
            let in_type = matches!(tok(i + 1), Some(Token::LeftBracket))
                && matches!(tok(i - 1), Some(Token::LeftBracket))
                && ident(i - 2, "BinaryHeap");
            // `let Reverse(x) = heap.pop().unwrap();`, the item taken apart
            // by a `let` (kata 2034): the edit writes `x`.
            let let_bound = matches!(tok(i + 1), Some(Token::LeftParen))
                && matches!(tok(i - 1), Some(Token::Let));
            if !in_type && !let_bound {
                if !matches!(tok(i + 1), Some(Token::LeftParen))
                    || !matches!(tok(i - 1), Some(Token::LeftParen))
                {
                    continue;
                }
                let pushed =
                    ident(i - 2, "push") && i >= 3 && matches!(tok(i - 3), Some(Token::Dot));
                if !pushed && !ident(i - 2, "Some") {
                    continue;
                }
            }
            // The `)` that closes `Reverse(`; the edit writes what lies
            // between the two in the call's place.
            let mut depth = 0usize;
            let mut close = None;
            for (j, t) in self.tokens.iter().enumerate().skip(i + 1) {
                match t.token {
                    Token::LeftParen | Token::LeftBracket | Token::LeftBrace => depth += 1,
                    Token::RightParen | Token::RightBracket | Token::RightBrace => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(j);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(close) = close else { continue };
            let Some((source, base)) = self.source.as_ref() else {
                continue;
            };
            let open = self.tokens[i + 1].span;
            let (from, to) = (open.offset + open.length, self.tokens[close].span.offset);
            let Some(inner) = from
                .checked_sub(*base)
                .zip(to.checked_sub(*base))
                .and_then(|(a, b)| source.get(a..b))
            else {
                continue;
            };
            let start = self.tokens[i].span.offset;
            out.insert(
                start,
                crate::resolver::TextEdit {
                    offset: start,
                    length: to + 1 - start,
                    replacement: inner.trim().to_string(),
                },
            );
        }
        out
    }

    /// `m.range(a..b)` or `m.range(a..=b)`, Rust's one-argument range: the
    /// edit that writes `range(a, b)` or `range_inclusive(a, b)` from the
    /// method's name to the call's `)`, keyed by the argument's offset
    /// ([`crate::ast::Program::range_arg_edits`], kata 2008). A range open
    /// at an end gets the edit with a stand-in for the missing bound, which
    /// the typechecker fills from the element type (kata 1099).
    fn note_range_arg(&mut self, name: Span, arg: &Expr, close: Span) {
        let (from, to, inclusive) = match &arg.kind {
            ExprKind::Range {
                start,
                end,
                inclusive,
            } => (start.as_deref(), end.as_deref(), *inclusive),
            ExprKind::Binary {
                op: op @ (BinOp::Range | BinOp::RangeInclusive),
                left,
                right,
            } => (
                Some(left.as_ref()),
                Some(right.as_ref()),
                *op == BinOp::RangeInclusive,
            ),
            _ => return,
        };
        let text = |e: Option<&Expr>| -> Result<Option<String>, ()> {
            match e {
                Some(e) => self
                    .source_at(e.span.offset, e.span.length)
                    .map(|t| Some(t.to_string()))
                    .ok_or(()),
                None => Ok(None),
            }
        };
        let (Ok(a), Ok(b)) = (text(from), text(to)) else {
            return;
        };
        let method = if inclusive {
            "range_inclusive"
        } else {
            "range"
        };
        let replacement = match (a, b) {
            (Some(a), Some(b)) => format!("{method}({a}, {b})"),
            (None, Some(b)) => format!("{method}({}, {b})", crate::ast::OPEN_RANGE_MIN),
            (Some(a), None) => format!("range_inclusive({a}, {})", crate::ast::OPEN_RANGE_MAX),
            (None, None) => "iter()".to_string(),
        };
        self.range_arg_edits.insert(
            arg.span.offset,
            crate::resolver::TextEdit {
                offset: name.offset,
                length: close.offset + close.length - name.offset,
                replacement,
            },
        );
    }

    /// `recv.parse()`: the receiver's text, keyed by the call's offset, so
    /// the typechecker can write `T.parse(recv)` once a later use names `T`
    /// ([`crate::ast::Program::parse_target_edits`], kata 1058).
    fn note_untargeted_parse(&mut self, recv: &Expr, call: Span) {
        let Some(text) = self.source_at(recv.span.offset, recv.span.length) else {
            return;
        };
        let text = text.to_string();
        self.parse_target_edits.insert(
            call.offset,
            crate::resolver::TextEdit {
                offset: call.offset,
                length: call.length,
                replacement: text,
            },
        );
    }

    /// Each `Reverse(` that is the whole body of a sort key,
    /// `v.sort_by_key(|x| Reverse(k))` (or `sort_unstable_by_key`, or the
    /// parameter written `&x`): keyed by the offset of `Reverse`, the edit
    /// that writes the call `sort_by(|a, b| k[b].cmp(k[a]))` from the method's
    /// name to the call's `)` (kata 1058). A unary `*` on the parameter goes
    /// with it, since a closure's parameter is no reference to undo.
    /// Rust's library paths written with `.`: `std.mem.take(mut v)`,
    /// `std.cmp.max(a, b)`, `core.i64.MAX`, `cmp.min(a, b)`. The `::` form is
    /// reported by [`Self::rewrite_rust_path`]; this one parses as field
    /// accesses on an undeclared `std`, which alone said only "undefined
    /// name 'std'" (kata 2076). Each such chain gets the message and the edit
    /// that deletes its module segments: every lowercase segment before a
    /// type, a primitive or the last, but `mem`, which is Kāra's
    /// (`mem.take`, `mem.swap`). The resolver reports it only when the root
    /// is undeclared, so a local named `std` (a standard deviation) is left
    /// alone.
    fn rust_std_paths(&self) -> rustc_hash::FxHashMap<usize, (String, crate::resolver::TextEdit)> {
        const ROOTS: &[&str] = &["std", "core", "alloc", "cmp", "collections"];
        const PRIMITIVES: &[&str] = &[
            "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
            "f32", "f64", "bool", "char", "str",
        ];
        let mut out = rustc_hash::FxHashMap::default();
        let tok = |i: usize| self.tokens.get(i).map(|t| &t.token);
        let name_at = |i: usize| match tok(i) {
            Some(Token::Identifier { name, .. }) => Some(name.as_str()),
            _ => None,
        };
        for i in 0..self.tokens.len() {
            let Some(root) = name_at(i) else { continue };
            if !ROOTS.contains(&root)
                || (i > 0 && matches!(tok(i - 1), Some(Token::Dot | Token::ColonColon)))
            {
                continue;
            }
            // the segments, at token indices i, i + 2, ..
            let mut segments = vec![i];
            while let Some(&last) = segments.last() {
                if matches!(tok(last + 1), Some(Token::Dot)) && name_at(last + 2).is_some() {
                    segments.push(last + 2);
                } else {
                    break;
                }
            }
            if segments.len() < 2 {
                continue;
            }
            let names: Vec<&str> = segments.iter().filter_map(|&k| name_at(k)).collect();
            let mut dropped = 0;
            while dropped < names.len() - 1
                && !starts_upper(names[dropped])
                && !PRIMITIVES.contains(&names[dropped])
                && names[dropped] != "mem"
                && !kara_module_call(&names, dropped)
            {
                dropped += 1;
            }
            let start = self.tokens[i].span.offset;
            let keep = self.tokens[segments[dropped]].span.offset;
            let written = names.join(".");
            let kara = names[dropped..].join(".");
            let message = format!(
                "`{written}` is a Rust path -- Kāra's library needs no `{}` module path: \
                 write `{kara}`",
                names[..dropped].join(".")
            );
            let edit = crate::resolver::TextEdit {
                offset: start,
                length: keep - start,
                replacement: String::new(),
            };
            out.insert(start, (message, edit));
        }
        out
    }

    fn rust_reverse_sort_keys(&self) -> rustc_hash::FxHashMap<usize, crate::resolver::TextEdit> {
        let mut out = rustc_hash::FxHashMap::default();
        let tok = |i: usize| self.tokens.get(i).map(|t| &t.token);
        let name_at = |i: usize| match tok(i) {
            Some(Token::Identifier { name, .. }) => Some(name.as_str()),
            _ => None,
        };
        // The token that closes the group opened at `open`.
        let close_of = |open: usize| {
            let mut depth = 0usize;
            for (j, t) in self.tokens.iter().enumerate().skip(open) {
                match t.token {
                    Token::LeftParen | Token::LeftBracket | Token::LeftBrace => depth += 1,
                    Token::RightParen | Token::RightBracket | Token::RightBrace => {
                        depth = depth.checked_sub(1)?;
                        if depth == 0 {
                            return Some(j);
                        }
                    }
                    _ => {}
                }
            }
            None
        };
        for i in 6..self.tokens.len() {
            if name_at(i) != Some("Reverse")
                || !matches!(tok(i + 1), Some(Token::LeftParen))
                || !matches!(tok(i - 1), Some(Token::Pipe))
            {
                continue;
            }
            let Some(param) = name_at(i - 2) else {
                continue;
            };
            let open_pipe = if matches!(tok(i - 3), Some(Token::Amp)) {
                i - 4
            } else {
                i - 3
            };
            if open_pipe < 3
                || !matches!(tok(open_pipe), Some(Token::Pipe))
                || !matches!(tok(open_pipe - 1), Some(Token::LeftParen))
                || !matches!(tok(open_pipe - 3), Some(Token::Dot))
                || !matches!(
                    name_at(open_pipe - 2),
                    Some("sort_by_key" | "sort_unstable_by_key")
                )
            {
                continue;
            }
            let Some(close) = close_of(i + 1) else {
                continue;
            };
            // `Reverse(k)` is the whole body: the call's `)` comes next.
            if close_of(open_pipe - 1) != Some(close + 1) {
                continue;
            }
            let key = &self.tokens[i + 2..close];
            if key.is_empty() {
                continue;
            }
            let names: Vec<&str> = key
                .iter()
                .filter_map(|t| match &t.token {
                    Token::Identifier { name, .. } => Some(name.as_str()),
                    _ => None,
                })
                .collect();
            let Some((a, b)) = [("a", "b"), ("l", "r"), ("lhs", "rhs")]
                .into_iter()
                .find(|(a, b)| !names.contains(a) && !names.contains(b))
            else {
                continue;
            };
            // The key's text with the parameter named `with`, and whether
            // it needs parentheses as the receiver of `.cmp`.
            let write = |with: &str| -> Option<(String, bool)> {
                let mut text = String::new();
                // Only names, fields and numbers: no parentheses needed.
                let mut plain = true;
                let mut cursor = key[0].span.offset;
                for (n, t) in key.iter().enumerate() {
                    let after_dot = n > 0 && matches!(key[n - 1].token, Token::Dot);
                    let is_param = !after_dot
                        && matches!(&t.token, Token::Identifier { name, .. } if name == param);
                    let unary = n == 0
                        || matches!(
                            key[n - 1].token,
                            Token::LeftParen | Token::LeftBracket | Token::Comma
                        );
                    let deref = matches!(t.token, Token::Star)
                        && unary
                        && matches!(key.get(n + 1).map(|t| &t.token), Some(Token::Identifier { name, .. }) if name == param);
                    text += self.source_at(cursor, t.span.offset.checked_sub(cursor)?)?;
                    if is_param {
                        text += with;
                    } else if !deref {
                        text += self.source_at(t.span.offset, t.span.length)?;
                        plain &= matches!(
                            t.token,
                            Token::Identifier { .. } | Token::Dot | Token::Integer(..)
                        );
                    }
                    cursor = t.span.offset + t.span.length;
                }
                // A key of more than a name, a field or a bracketed group
                // goes in parentheses before `.cmp`.
                let grouped = matches!(key[0].token, Token::LeftParen | Token::LeftBracket)
                    && close_of(i + 2) == Some(close - 1);
                Some((text, !plain && !grouped))
            };
            let (Some((ka, _)), Some((kb, wrap))) = (write(a), write(b)) else {
                continue;
            };
            let kb = if wrap { format!("({kb})") } else { kb };
            let start = self.tokens[open_pipe - 2].span.offset;
            let end = self.tokens[close + 1].span.offset + 1;
            out.insert(
                self.tokens[i].span.offset,
                crate::resolver::TextEdit {
                    offset: start,
                    length: end - start,
                    replacement: format!("sort_by(|{a}, {b}| {kb}.cmp({ka}))"),
                },
            );
        }
        out
    }

    /// Script mode's implicit `main` signature and tail: the return type
    /// `Result[(), AnyError]` and the closing `Ok(())` (design.md § Script
    /// mode). Parsed from their own text, with every offset moved past the
    /// file's end, so no synthesized node's span is any written node's
    /// (types are recorded by span).
    fn script_main_signature(&self) -> (Option<TypeExpr>, Option<Expr>) {
        let end = self.tokens.last().map(|t| t.span).unwrap_or_default();
        let mut tokens = crate::tokenize("Result[(), AnyError] Ok(())");
        for t in &mut tokens {
            t.span.offset += end.offset + 1;
            t.span.line = end.line;
            t.span.column += end.column;
        }
        let mut sub = Parser::new(tokens);
        let ty = sub.parse_type();
        let tail = sub.parse_expression();
        (ty, tail)
    }

    // ── Path Helpers ─────────────────────────────────────────────

    fn parse_path_segments(&mut self) -> Option<Vec<String>> {
        let mut segments = Vec::new();

        match self.peek_token_ref() {
            Token::Identifier { .. } => segments.push(self.expect_identifier()?),
            Token::SelfType => {
                self.advance();
                segments.push("Self".to_string());
            }
            _ => {
                self.error("Expected identifier");
                return None;
            }
        }

        while self.eat(&Token::Dot) {
            segments.push(self.expect_identifier()?);
        }
        Some(segments)
    }

    // ── Token Helpers ────────────────────────────────────────────

    fn peek_token(&self) -> Token {
        self.peek_token_ref().clone()
    }

    /// Borrowed view of the current token — the no-clone peek. Kind-only
    /// probes (`check` / `eat` / `expect`, `==` comparisons, `matches!`)
    /// go through this; the cloning `peek_token` stays for match arms
    /// that move a payload out. The distinction is load-bearing: cloning
    /// a payload token (identifier / string literal) allocates, and the
    /// kind-only probes were ~17% of ALL front-end allocations
    /// (name-interning spike, DHAT).
    fn peek_token_ref(&self) -> &Token {
        if self.pos < self.tokens.len() {
            &self.tokens[self.pos].token
        } else {
            &Token::EOF
        }
    }

    fn peek_token_at(&self, offset: usize) -> Token {
        self.peek_token_ref_at(offset).clone()
    }

    /// Borrowed sibling of [`Self::peek_token_at`] — same no-clone
    /// rationale as [`Self::peek_token_ref`].
    fn peek_token_ref_at(&self, offset: usize) -> &Token {
        let idx = self.pos + offset;
        if idx < self.tokens.len() {
            &self.tokens[idx].token
        } else {
            &Token::EOF
        }
    }

    /// Record the machine-applicable deletion for a call-site mode keyword
    /// (`ref` at an argument, or the `ref` inside `mut ref`) — B-2026-08-05-12.
    ///
    /// `token_idx` is the token to delete; `diag_span` anchors the edit to the
    /// diagnostic that reported it, which for `mut ref` is the `mut` token
    /// rather than the token being removed.
    ///
    /// Deletes the keyword AND the whitespace up to the next token, not just
    /// the keyword's own span. Deleting the span alone turns `f(ref x)` into
    /// `f( x)` — still valid, but `karac fix` output that needs a formatting
    /// pass afterwards is a fix people stop trusting. When there is no next
    /// token (malformed tail), fall back to the keyword span so the edit is
    /// still well-formed rather than skipped.
    fn record_call_site_mode_deletion(&mut self, token_idx: usize, diag_span: &Span) {
        self.record_token_range_deletion(token_idx, token_idx + 1, diag_span);
    }

    /// Record a machine-applicable deletion of the token range
    /// `[start_idx, end_idx)`, anchored to `diag_span`.
    ///
    /// The whitespace-swallowing rule of [`Self::record_call_site_mode_deletion`]
    /// generalized: delete up to the offset of the token that FOLLOWS the range,
    /// so `impl T for U with panics {` collapses to `impl T for U {` rather than
    /// `impl T for U  {` with a doubled space. When the range runs to the end of
    /// the token stream, fall back to the last token's own extent so the edit is
    /// still well-formed rather than skipped.
    fn record_token_range_deletion(&mut self, start_idx: usize, end_idx: usize, diag_span: &Span) {
        let (Some(first), Some(last)) = (
            self.tokens.get(start_idx),
            self.tokens.get(end_idx.saturating_sub(1)),
        ) else {
            return;
        };
        let start = first.span.offset;
        let end = self
            .tokens
            .get(end_idx)
            .map(|next| next.span.offset)
            .filter(|next_off| *next_off > start)
            .unwrap_or(last.span.offset + last.span.length);
        self.fix_edits.insert(
            crate::resolver::SpanKey::from_span(diag_span),
            crate::resolver::TextEdit {
                offset: start,
                length: end.saturating_sub(start),
                replacement: String::new(),
            },
        );
    }

    fn current_span(&self) -> Span {
        if self.pos < self.tokens.len() {
            self.tokens[self.pos].span
        } else if !self.tokens.is_empty() {
            self.tokens.last().unwrap().span
        } else {
            Span {
                line: 1,
                column: 1,
                offset: 0,
                length: 0,
            }
        }
    }

    fn advance(&mut self) -> &SpannedToken {
        let tok = &self.tokens[self.pos];
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn check(&self, expected: &Token) -> bool {
        std::mem::discriminant(self.peek_token_ref()) == std::mem::discriminant(expected)
    }

    fn eat(&mut self, expected: &Token) -> bool {
        if self.check(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, expected: &Token) -> Option<()> {
        if self.check(expected) {
            self.advance();
            Some(())
        } else {
            let found_span = self.current_span();
            // A MACRO CALL stops here rather than in the prefix parser whenever
            // it sits inside a larger expression: `let s = format!("{}", 1);`
            // wants a `;` and finds the `!`, and `id(panic!("never"))` wants a
            // `)` and finds it. B-2026-08-18-46, widened for B-2026-08-18-42.
            //
            // KEYED ON THE FOUND TOKEN, not on which token was expected. The
            // first cut asked only about `Semicolon`, which left argument
            // position -- the single most common place a `panic!` appears in
            // design.md -- reporting "Expected RightParen, found Bang" while
            // the statement and value spellings of the identical mistake got a
            // diagnostic naming it. Three positions, one recognizer.
            //
            // The shape test is what keeps this from over-firing: an identifier
            // ABUTTING the `!` followed by an opening bracket. `vec![…]` never
            // reaches `expect` at all (the postfix path consumes it), and a real
            // prefix negation is preceded by an operator or keyword.
            if self.check(&Token::Bang) {
                if let Some(name) = self.macro_call_name_before_bang(&found_span) {
                    self.report_macro_call(&name, found_span);
                    // Read past the call, as if its name were the whole
                    // expression, so the rest of the statement parses instead
                    // of cascading (B-2026-10-20-5).
                    self.advance();
                    self.skip_bracketed_group();
                    if self.check(expected) {
                        self.advance();
                        return Some(());
                    }
                    return None;
                }
            }
            self.error_kind(
                ParseErrorKind::UnexpectedToken,
                &format!(
                    "Expected {}, found {}",
                    expected.describe(),
                    self.peek_token_ref().describe()
                ),
            );
            if matches!(expected, Token::Semicolon) {
                self.record_missing_semicolon_insertion(&found_span);
                // A `;` missing before the `}` that closes the block: parse
                // on as if it were there, so the block closes where it
                // should and a statement after it is not misread
                // (B-2026-10-28-31).
                if self.check(&Token::RightBrace) {
                    return Some(());
                }
            }
            None
        }
    }

    /// If the `!` at `bang_span` is the `!` of a Rust MACRO CALL --
    /// `ident!(`, `ident![`, `ident!{` with no gap between the name and the
    /// `!` -- return the macro's name. B-2026-08-18-46.
    ///
    /// The no-gap requirement is the whole test. A genuine prefix negation is
    /// preceded by an operator or a keyword (`if !b`, `x = !b`, `and !b`),
    /// never by an identifier abutting it, so this cannot misfire on real
    /// negation; and `foo !bar` (identifier, space, negation) is excluded by
    /// the offsets not touching.
    /// Report the Rust macro call whose `!` is the current token, once.
    /// Both recognizers (`expect` and the prefix parser) come here, so the
    /// message and the fix are the same whichever sees it first.
    ///
    /// The usual repair deletes the `!`, leaving an ordinary call, and that
    /// is right only for the macros in [`MACRO_FUNCTION_TWINS`]. Deleting the
    /// `!` of any other produced a call to an undefined name
    /// (B-2026-10-20-5): `matches!` is rewritten to the `match` it stands
    /// for, and the rest are named with no fix.
    pub(crate) fn report_macro_call(&mut self, name: &str, bang_span: Span) {
        if !self.macro_bangs_reported.insert(bang_span.offset) {
            return;
        }
        let key = crate::resolver::SpanKey::from_span(&bang_span);
        if name == "matches" {
            self.error_at(
                "Kāra has no macros, and `matches!` has no function twin: write \
                 `match x { P => true, _ => false }`, or `if let P = x` in a condition",
                bang_span,
            );
            if let Some(edits) = self.matches_macro_rewrite() {
                self.fix_diffs.insert(key, edits);
            }
            return;
        }
        if name == "format" {
            self.error_at(
                "Kāra has no macros; `format!(...)` is Rust syntax: write an f-string, \
                 `f\"{x} and {y}\"`",
                bang_span,
            );
            if let Some(edit) = self.format_macro_rewrite() {
                self.fix_edits.insert(key, edit);
            }
            return;
        }
        if PRINT_MACROS.contains(&name) && self.macro_has_format_arguments() {
            // B-2026-11-01-8: deleting the `!` of `println!("{}", n)` left
            // `println("{}", n)`, an arity error with no fix, and of
            // `println!("{n}")` a call that prints the braces. The format
            // string and its arguments become one f-string instead.
            match self.format_macro_rewrite() {
                Some(edit) => {
                    self.error_at(
                        &format!(
                            "Kāra has no macros; `{name}!(...)` is Rust syntax: it takes one \
                             string, so its format arguments go into an f-string, \
                             `{name}(f\"{{x}} and {{y}}\")`"
                        ),
                        bang_span,
                    );
                    self.fix_edits.insert(
                        key,
                        crate::resolver::TextEdit {
                            replacement: format!("{name}({})", edit.replacement),
                            ..edit
                        },
                    );
                }
                None => self.error_at(
                    &format!(
                        "Kāra has no macros; `{name}!(...)` is Rust syntax: it takes one \
                         string, so write its format arguments into an f-string, \
                         `{name}(f\"{{x}} and {{y}}\")`"
                    ),
                    bang_span,
                ),
            }
            return;
        }
        if !MACRO_FUNCTION_TWINS.contains(&name) {
            let instead = match name {
                "vec" => "write a list literal, `[a, b]`, or `Vec.new()`".to_string(),
                _ => format!("there is no `{name}` function, so write it without the macro"),
            };
            self.error_at(
                &format!("Kāra has no macros; `{name}!(...)` is Rust syntax: {instead}"),
                bang_span,
            );
            return;
        }
        self.error_at(
            &format!(
                "Kāra has no macros; `{name}!(...)` is Rust syntax — call it as \
                 `{name}(...)` and remove the `!`"
            ),
            bang_span,
        );
        self.fix_edits.insert(
            key,
            crate::resolver::TextEdit {
                offset: bang_span.offset,
                length: bang_span.length,
                replacement: String::new(),
            },
        );
    }

    /// The edits that turn `matches!(x, P)` (the `!` being the current token)
    /// into `match x { P => true, _ => false }`. A guard comes along as the
    /// arm's guard, and a trailing comma is dropped. Anything else (no
    /// pattern, a second argument, an unclosed call) gets no fix.
    fn matches_macro_rewrite(&self) -> Option<Vec<crate::resolver::TextEdit>> {
        let name = &self.tokens.get(self.pos.checked_sub(1)?)?.span;
        let open = self.tokens.get(self.pos + 1)?;
        if !matches!(open.token, Token::LeftParen) {
            return None;
        }
        let mut depth = 0usize;
        let mut commas = Vec::new();
        let mut close = None;
        for (i, t) in self.tokens.iter().enumerate().skip(self.pos + 1) {
            match t.token {
                Token::LeftParen | Token::LeftBracket | Token::LeftBrace => depth += 1,
                Token::RightParen | Token::RightBracket | Token::RightBrace => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(i);
                        break;
                    }
                }
                Token::Comma if depth == 1 => commas.push(i),
                Token::EOF => return None,
                _ => {}
            }
        }
        let close = close.filter(|&i| matches!(self.tokens[i].token, Token::RightParen))?;
        let (comma, tail) = match commas.as_slice() {
            [c] => (*c, close),
            [c, t] if t + 1 == close => (*c, *t),
            _ => return None,
        };
        if comma == self.pos + 2 || comma + 1 == tail {
            return None;
        }
        let comma = &self.tokens[comma].span;
        let tail = &self.tokens[tail].span;
        let close = &self.tokens[close].span;
        Some(vec![
            crate::resolver::TextEdit {
                offset: name.offset,
                length: open.span.offset + open.span.length - name.offset,
                replacement: "match ".to_string(),
            },
            crate::resolver::TextEdit {
                offset: comma.offset,
                length: comma.length,
                replacement: " {".to_string(),
            },
            crate::resolver::TextEdit {
                offset: tail.offset,
                length: close.offset + close.length - tail.offset,
                replacement: " => true, _ => false }".to_string(),
            },
        ])
    }

    /// The edit that turns `format!("..{}..{:spec}..", a, b)` (the `!` being
    /// the current token) into `f"..{a}..{b:spec}.."` (B-2026-10-24-12).
    /// Each `{}` / `{:spec}` hole takes the next argument's text, and `{{` /
    /// `}}` carry over, since an f-string doubles braces the same way. There
    /// is no fix when the first argument is not a one-line `"..."` literal,
    /// when a hole names its argument (`{0}`, `{name}`) or its spec is not
    /// one an f-string accepts, when the counts disagree, or when an
    /// argument's text could not sit inside a hole as written (a brace, a
    /// `:`, a backslash, a comment or a line break in it). A quote can: a
    /// hole balances quotes (design.md § strings, "Full expressions"), so
    /// `s.replace("a", "b")` is carried as is (B-2026-11-01-13).
    /// Whether the macro call at the current `!` passes a format string
    /// that needs formatting: a string literal first, then more arguments,
    /// or a `{` hole in the literal that is not an escaped `{{`. A call
    /// with a plain literal, or with no literal first (`println!(s)`), is
    /// the same call once the `!` is gone.
    fn macro_has_format_arguments(&self) -> bool {
        if !matches!(
            self.tokens.get(self.pos + 1).map(|t| &t.token),
            Some(Token::LeftParen)
        ) {
            return false;
        }
        let Some(lit) = self.tokens.get(self.pos + 2) else {
            return false;
        };
        let Token::StringLiteral(body) = &lit.token else {
            return false;
        };
        if !matches!(
            self.tokens.get(self.pos + 3).map(|t| &t.token),
            Some(Token::RightParen)
        ) {
            return true;
        }
        body.replace("{{", "").contains('{')
    }

    fn format_macro_rewrite(&self) -> Option<crate::resolver::TextEdit> {
        let (source, base) = self.source.as_ref()?;
        let text = |span: &Span| {
            source.get(span.offset.checked_sub(*base)?..span.offset - base + span.length)
        };
        let name = &self.tokens.get(self.pos.checked_sub(1)?)?.span;
        if !matches!(self.tokens.get(self.pos + 1)?.token, Token::LeftParen) {
            return None;
        }
        let lit = self.tokens.get(self.pos + 2)?;
        if !matches!(lit.token, Token::StringLiteral(_)) {
            return None;
        }
        let raw = text(&lit.span)?;
        let body = raw.strip_prefix('"')?.strip_suffix('"')?;
        if body.starts_with("\"\"") || body.contains('\n') {
            return None;
        }
        // The argument token runs after the literal, split at depth-0 commas.
        let mut args: Vec<(Span, Span)> = Vec::new();
        let mut depth = 0usize;
        let mut first: Option<Span> = None;
        let mut last: Option<Span> = None;
        let mut i = self.pos + 3;
        let close = loop {
            let t = self.tokens.get(i)?;
            match t.token {
                Token::EOF => return None,
                Token::RightParen if depth == 0 => break t.span,
                Token::Comma if depth == 0 => {
                    if i != self.pos + 3 {
                        args.push((first.take()?, last.take()?));
                    }
                }
                _ => {
                    if i == self.pos + 3 {
                        return None;
                    }
                    match t.token {
                        Token::LeftParen | Token::LeftBracket | Token::LeftBrace => depth += 1,
                        Token::RightParen | Token::RightBracket | Token::RightBrace => {
                            depth = depth.checked_sub(1)?
                        }
                        _ => {}
                    }
                    first.get_or_insert(t.span);
                    last = Some(t.span);
                }
            }
            i += 1;
        };
        if let (Some(f), Some(l)) = (first, last) {
            args.push((f, l));
        }
        let mut args = args.into_iter().map(|(f, l)| {
            let start = f.offset.checked_sub(*base)?;
            let arg = source.get(start..l.offset - base + l.length)?;
            format_arg_fits_hole(arg).then(|| path_colons_as_dots(arg))
        });
        let mut out = String::from("f\"");
        let mut chars = body.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    out.push(c);
                    out.push(chars.next()?);
                }
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    out.push_str("{{");
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    out.push_str("}}");
                }
                '}' => return None,
                '{' => {
                    let mut hole = String::new();
                    loop {
                        match chars.next()? {
                            '}' => break,
                            '{' => return None,
                            h => hole.push(h),
                        }
                    }
                    // `{}` / `{:spec}` takes the next argument; `{name}` /
                    // `{name:spec}` is Rust 2021's inline capture of a
                    // variable, which an f-string writes the same way
                    // (B-2026-11-01-8). A hole that is neither (`{0}`, an
                    // expression) has no rewrite.
                    let (captured, spec) = match hole.split_once(':') {
                        Some((name, spec)) => (name, Some(spec)),
                        None => (hole.as_str(), None),
                    };
                    if let Some(spec) = spec {
                        crate::format_spec::FormatSpec::parse(spec).ok()?;
                    }
                    let is_ident = captured
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_alphabetic() || c == '_')
                        && captured.chars().all(|c| c.is_alphanumeric() || c == '_');
                    out.push('{');
                    if captured.is_empty() {
                        // `format!("{}", &w[..i])`: Rust borrows an argument
                        // it only displays, and a hole displays the value it
                        // names, so the `&` goes (a `&` kept in the hole was
                        // a parse error with no fix).
                        let arg = args.next()??;
                        let arg = arg.trim_start_matches('&').trim_start();
                        if arg.starts_with("mut ") || arg.starts_with("mut\t") {
                            return None;
                        }
                        // A block argument opens with the brace that, after
                        // the hole's own, would read as an escaped `{{`.
                        if arg.starts_with('{') {
                            out.push('(');
                            out.push_str(arg);
                            out.push(')');
                        } else {
                            out.push_str(arg);
                        }
                    } else if is_ident {
                        out.push_str(captured);
                    } else {
                        return None;
                    }
                    if let Some(spec) = spec {
                        out.push(':');
                        out.push_str(spec);
                    }
                    out.push('}');
                }
                c => out.push(c),
            }
        }
        if args.next().is_some() {
            return None;
        }
        out.push('"');
        Some(crate::resolver::TextEdit {
            offset: name.offset,
            length: close.offset + close.length - name.offset,
            replacement: out,
        })
    }

    /// Consume a bracketed group starting at the current token, through its
    /// matching close (or to the end of input). A no-op on any other token.
    pub(crate) fn skip_bracketed_group(&mut self) {
        if !matches!(
            self.peek_token_ref(),
            Token::LeftParen | Token::LeftBracket | Token::LeftBrace
        ) {
            return;
        }
        let mut depth = 0usize;
        loop {
            match self.peek_token_ref() {
                Token::LeftParen | Token::LeftBracket | Token::LeftBrace => depth += 1,
                Token::RightParen | Token::RightBracket | Token::RightBrace => depth -= 1,
                Token::EOF => return,
                _ => {}
            }
            self.advance();
            if depth == 0 {
                return;
            }
        }
    }

    /// Whether the bracketed group at the current token holds a `;` at its
    /// own depth, as Rust's array type `[T; N]` does. Reads ahead only.
    pub(crate) fn bracket_holds_semicolon(&self) -> bool {
        if !self.check(&Token::LeftBracket) {
            return false;
        }
        let mut depth = 0usize;
        for t in &self.tokens[self.pos..] {
            match t.token {
                Token::LeftParen | Token::LeftBracket | Token::LeftBrace => depth += 1,
                Token::RightParen | Token::RightBracket | Token::RightBrace => {
                    depth -= 1;
                    if depth == 0 {
                        return false;
                    }
                }
                Token::Semicolon if depth == 1 => return true,
                Token::EOF => return false,
                _ => {}
            }
        }
        false
    }

    pub(crate) fn macro_call_name_before_bang(&self, bang_span: &Span) -> Option<String> {
        let prev = self.tokens.get(self.pos.checked_sub(1)?)?;
        let Token::Identifier { name, .. } = &prev.token else {
            return None;
        };
        if prev.span.offset + prev.span.length != bang_span.offset {
            return None;
        }
        matches!(
            self.peek_token_ref_at(1),
            Token::LeftParen | Token::LeftBracket | Token::LeftBrace
        )
        .then(|| name.clone())
    }

    /// Attach the machine-applicable `;` insertion to a missing-statement-
    /// terminator diagnostic — B-2026-08-18-43.
    ///
    /// `Expected Semicolon` is the most common parse error in the corpus and
    /// carried no `replacement`, so `karac fix` — the Mend loop's primary fix
    /// path — reported it and changed nothing.
    ///
    /// THE OFFSET IS EXACT, NOT A HEURISTIC, and that is the whole reason this
    /// belongs in the parser. The `;` goes immediately after the END of the
    /// previous token, which is the last token of the statement that just
    /// parsed. A text-level pass cannot recover that: B-2026-08-17-31's
    /// line-based attempt was reverted because a `let` whose right-hand side
    /// continues onto later lines has no end-of-line to append to, and brackets
    /// balance on each line taken alone. Anchoring to the token also keeps a
    /// trailing comment intact — comments are not tokens, so `let x = 1  // n`
    /// becomes `let x = 1;  // n` rather than `let x = 1  // n;`.
    ///
    /// GUARDED, because not every `Expected Semicolon` wants a `;`. Rust habits
    /// — `std::mem::swap(...)`, `println!(...)` — surface under this same
    /// message, stopping the parser at `ColonColon` / `Bang` mid-expression;
    /// inserting a `;` there produces `foo; ::bar()`, which is a WRONG fix, and
    /// a wrong fix in the primary fix path is worse than none. So the edit is
    /// emitted only when the offending token plausibly begins a new statement:
    /// it starts on a LATER LINE than the previous token began, or it is the
    /// `}` closing the block (`unsafe { *out = v }`, where the terminator goes
    /// before the brace). Same-line mid-expression junk gets no edit and the
    /// diagnostic stands alone, exactly as before.
    fn record_missing_semicolon_insertion(&mut self, found_span: &Span) {
        let Some(prev_span) = self
            .pos
            .checked_sub(1)
            .and_then(|i| self.tokens.get(i))
            .map(|t| t.span)
        else {
            return;
        };
        let begins_new_line = found_span.line > prev_span.line;
        let closes_block = matches!(self.peek_token_ref(), Token::RightBrace);
        if !begins_new_line && !closes_block {
            return;
        }
        self.fix_edits.insert(
            crate::resolver::SpanKey::from_span(found_span),
            crate::resolver::TextEdit {
                offset: prev_span.offset + prev_span.length,
                length: 0,
                replacement: ";".to_string(),
            },
        );
    }

    /// Whether the token `offset` ahead is the contextual keyword `kw`: an
    /// identifier spelled `kw`, not written `r#kw`. Only words that start
    /// items, statements or expressions are hard keywords; the rest
    /// (`union`, `lock`, `par`, the effect verbs, …) are names everywhere
    /// except the one position where they introduce syntax (design review
    /// 2026-10-07, §3 "Contextual keywords").
    fn at_kw_at(&self, offset: usize, kw: &str) -> bool {
        matches!(
            self.peek_token_ref_at(offset),
            Token::Identifier { name, raw: false } if name == kw
        )
    }

    fn at_kw(&self, kw: &str) -> bool {
        self.at_kw_at(0, kw)
    }

    /// Whether the `effect` here starts an effect declaration: a name
    /// follows it (`effect resource Db;`, `effect group G = …`).
    fn effect_decl_follows(&self) -> bool {
        matches!(self.peek_token_ref_at(1), Token::Identifier { .. })
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.at_kw(kw) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_kw(&mut self, kw: &str) -> Option<()> {
        if self.eat_kw(kw) {
            Some(())
        } else {
            self.error(&format!("Expected `{kw}`"));
            None
        }
    }

    /// The spelling of the current token when it is an identifier not
    /// written `r#…`: what a contextual keyword is matched against.
    fn kw_here(&self) -> Option<&str> {
        match self.peek_token_ref() {
            Token::Identifier { name, raw: false } => Some(name.as_str()),
            _ => None,
        }
    }

    fn expect_identifier(&mut self) -> Option<String> {
        match self.peek_token_ref() {
            Token::Identifier { name, .. } => {
                let name = name.clone();
                self.advance();
                Some(name)
            }
            _ => {
                if self.recover_keyword_as_ident("identifier") {
                    return self.expect_identifier();
                }
                self.error_unexpected_ident("identifier");
                None
            }
        }
    }

    /// Like [`expect_identifier`] but accepts a small set of contextual
    /// keywords as identifier-equivalents in attribute-arg position. The
    /// design uses `requires` (already a contract keyword) and `ensures`
    /// inside `#[test(...)]`-style attributes; treating them as identifiers
    /// only here keeps the contract grammar untouched while letting
    /// design-conformant attribute syntax parse without quoting.
    /// Like [`expect_identifier`] but accepts the logical-operator
    /// keywords `not` / `and` / `or` as identifier-equivalents. Used
    /// at trait-method-name and impl-method-name positions so the
    /// stdlib can declare `trait Not { fn not(self) -> Self }` —
    /// design.md's bitwise-Not trait — without a separate raw-ident
    /// syntax. The lexer eagerly tokenizes these as `Token::Not` /
    /// `Token::And` / `Token::Or` regardless of context, so a
    /// targeted post-parse escape is the cleanest fix. Mirrors
    /// `expect_attr_arg_name`'s treatment of `requires` / `ensures`.
    /// A member name: after `.`, or the name of a method or associated
    /// function declared in an `impl` or `trait`. Any keyword, hard or
    /// reserved, is a member name here without `r#` (design.md §3), so
    /// `opt.or(x)`, `handle.await` and `fn not(own self)` all parse.
    fn expect_method_name(&mut self) -> Option<String> {
        if let Some(kw) = self.peek_token_ref().keyword_spelling() {
            self.advance();
            return Some(kw.to_string());
        }
        match self.peek_token_ref() {
            Token::Identifier { name, .. } => {
                let name = name.clone();
                self.advance();
                Some(name)
            }
            _ => {
                self.error(&format!(
                    "Expected method name, found {}",
                    self.peek_token_ref().describe()
                ));
                None
            }
        }
    }

    fn expect_attr_arg_name(&mut self) -> Option<String> {
        match self.peek_token_ref() {
            Token::Identifier { name, .. } => {
                let name = name.clone();
                self.advance();
                Some(name)
            }
            _ => {
                self.error(&format!(
                    "Expected attribute argument name, found {}",
                    self.peek_token_ref().describe()
                ));
                None
            }
        }
    }

    fn is_at_end(&self) -> bool {
        self.pos >= self.tokens.len() || self.peek_token_ref() == &Token::EOF
    }

    /// Lookahead used by `parse_attribute` to decide whether the current
    /// attribute-arg position starts a named (`ident = value` /
    /// `ident: value`) or positional (bare expression) argument. A named
    /// arg is an identifier-shaped token whose immediate successor is
    /// `=` or `:`; everything else — including a bare identifier followed
    /// by `,` / `)` / `.` — is treated as the start of an expression.
    /// The contract keywords `requires` / `ensures` are treated as
    /// identifiers here to match `expect_attr_arg_name`.
    fn is_named_attr_arg_head(&self) -> bool {
        if self.pos + 1 >= self.tokens.len() {
            return false;
        }
        let is_name_token = matches!(self.tokens[self.pos].token, Token::Identifier { .. });
        if !is_name_token {
            return false;
        }
        matches!(self.tokens[self.pos + 1].token, Token::Equal | Token::Colon)
    }

    fn span_from(&self, start: &Span) -> Span {
        let end = if self.pos > 0 {
            &self.tokens[self.pos - 1].span
        } else {
            start
        };
        Span {
            line: start.line,
            column: start.column,
            offset: start.offset,
            length: (end.offset + end.length).saturating_sub(start.offset),
        }
    }

    // ── Error Recovery ───────────────────────────────────────────

    /// Claim one level of the shared recursion budget. Returns `false` — after
    /// emitting a structured diagnostic — once [`MAX_RECURSION_DEPTH`] is
    /// reached; the caller must then return `None` WITHOUT calling
    /// [`Self::exit_recursion`]. The limit is far below the depth where the
    /// descent overflows the stack in a debug build (~210 nested `(` at the
    /// time of measurement), and deep enough that no human- or
    /// machine-written program should meet it. B-2026-08-16-4.
    #[must_use]
    pub(crate) fn enter_recursion(&mut self) -> bool {
        if self.recursion_depth >= MAX_RECURSION_DEPTH {
            self.error(&format!(
                "nesting too deep: expressions, types, and patterns may nest at \
                 most {MAX_RECURSION_DEPTH} levels"
            ));
            return false;
        }
        self.recursion_depth += 1;
        true
    }

    /// Release the level claimed by a successful [`Self::enter_recursion`].
    pub(crate) fn exit_recursion(&mut self) {
        self.recursion_depth -= 1;
    }

    fn error(&mut self, message: &str) {
        self.error_kind(ParseErrorKind::Syntax, message);
    }

    /// [`error`] with an explicit machine-readable family. New diagnostics
    /// should come through here; `error` is the legacy `Syntax` bucket.
    fn error_kind(&mut self, kind: ParseErrorKind, message: &str) {
        let span = self.current_span();
        // At a token the lexer could not read, its own message is the
        // error, reported once however many productions trip on it, not
        // `Expected …, found Error("…")` (B-2026-10-24-27).
        let (lexed, fixed) = match self.peek_token_ref() {
            Token::Error(m) => (m.clone(), None),
            Token::ErrorWithFix { message, fixed } => (message.clone(), Some(fixed.clone())),
            _ => (String::new(), None),
        };
        // A literal left open runs to the end of the file, so the parser's
        // complaint there (a missing `}`) is the same error again.
        if *self.peek_token_ref() == Token::EOF && self.tokens.len() >= 2 {
            let last = &self.tokens[self.tokens.len() - 2];
            let open = matches!(&last.token, Token::Error(m) if m.starts_with("Unterminated"));
            if open && self.errors.iter().any(|e| e.span == last.span) {
                return;
            }
        }
        if !lexed.is_empty() {
            if !self.errors.iter().any(|e| e.span == span) {
                if let Some(replacement) = fixed {
                    self.fix_edits.insert(
                        crate::resolver::SpanKey::from_span(&span),
                        crate::resolver::TextEdit {
                            offset: span.offset,
                            length: span.length,
                            replacement,
                        },
                    );
                }
                self.errors.push(ParseError {
                    kind: ParseErrorKind::Syntax,
                    message: lexed,
                    span,
                });
            }
            return;
        }
        self.errors.push(ParseError {
            kind,
            message: message.to_string(),
            span,
        });
    }

    /// Build the "unexpected token" message for a position that expected an
    /// identifier / pattern / expression. When the offending token is a reserved
    /// keyword (`group`, `type`, `match`, …) the message names the keyword and
    /// says it is reserved, instead of printing the token's internal Rust `Debug`
    /// name — e.g. `let mut group = …` now reports `'group' is a reserved keyword
    /// and cannot be used as an identifier` rather than `Expected pattern, found
    /// Group` (B-2026-07-08-13). `expected` is the noun used in the non-keyword
    /// fallback ("identifier" / "pattern" / "expression").
    /// Push the [`unexpected_ident_msg`] diagnostic with the matching kind:
    /// `ReservedKeyword` when the offending token is a reserved keyword,
    /// `UnexpectedToken` otherwise. Call this instead of pairing
    /// `unexpected_ident_msg` with plain `error`, which loses the family.
    fn error_unexpected_ident(&mut self, expected: &str) {
        let kind = match self.peek_token() {
            Token::RawIdentNotAllowed(_) => ParseErrorKind::RawIdentNotAllowed,
            tok => match tok.keyword_spelling() {
                Some(_) => ParseErrorKind::ReservedKeyword,
                None => ParseErrorKind::UnexpectedToken,
            },
        };
        let msg = self.unexpected_ident_msg(expected);
        self.error_kind(kind, &msg);
    }

    /// B-2026-10-04-60 — a raw-escapable keyword written where a NAME belongs
    /// (`let mut distinct = 0;`, `while distinct > k`, `return distinct;`).
    /// The diagnostic already names the remedy, `r#distinct`; this makes it
    /// `karac fix`'s machine-applicable edit, and then retypes the token as
    /// that identifier so parsing continues exactly as if the escape had
    /// been written. Before, each site returned `None`, and the recovery
    /// that followed could skip a closing brace: one keyword-named local in
    /// a nested block ended its function early and reported the rest of the
    /// body as top-level statements beside `fn main`, and `return distinct;`
    /// re-read the keyword as the start of a `distinct type` item.
    ///
    /// Returns `true` when it recovered, so the caller re-dispatches on the
    /// retyped token. A structural marker (`self`, `mut`, …) is not
    /// escapable and keeps the old path, as does a refused `r#` escape.
    fn recover_keyword_as_ident(&mut self, expected: &str) -> bool {
        let Some(kw) = self.peek_token_ref().keyword_spelling() else {
            return false;
        };
        if matches!(self.peek_token_ref(), Token::RawIdentNotAllowed(_))
            || !crate::token::is_raw_escapable(kw)
        {
            return false;
        }
        // Only where the NEXT token reads as something that follows a name
        // (`;`, `,`, a closer, `:`, `.`, `(`, `[`, an operator, an
        // assignment or compound assignment (the bitwise ones too, kata
        // 2044's `or |= nums[i]`), a bitwise operator), and in a pattern also `in`, `=>`,
        // `if` and `@`. Malformed code also puts keywords here -- the `let` of
        // an unsupported `if let ... and let Some(x) = e` chain, a stray
        // `else {` -- and retyping those as names would trade one honest
        // error for a cascade from a binding the author never meant.
        let follows_a_name = matches!(
            self.tokens.get(self.pos + 1).map(|t| &t.token),
            Some(
                Token::Semicolon
                    | Token::Comma
                    | Token::RightParen
                    | Token::RightBracket
                    | Token::RightBrace
                    | Token::Colon
                    | Token::Dot
                    | Token::QuestionDot
                    | Token::Question
                    | Token::LeftParen
                    | Token::LeftBracket
                    | Token::Equal
                    | Token::PlusEqual
                    | Token::MinusEqual
                    | Token::StarEqual
                    | Token::SlashEqual
                    | Token::PercentEqual
                    | Token::AmpEqual
                    | Token::PipeEqual
                    | Token::CaretEqual
                    | Token::LessLessEqual
                    | Token::GreaterGreaterEqual
                    | Token::Plus
                    | Token::Minus
                    | Token::Star
                    | Token::Slash
                    | Token::Percent
                    | Token::EqualEqual
                    | Token::BangEqual
                    | Token::LessThan
                    | Token::LessThanOrEqual
                    | Token::GreaterThan
                    | Token::GreaterThanOrEqual
                    | Token::And
                    | Token::Or
                    | Token::Amp
                    | Token::Pipe
                    | Token::Caret
                    | Token::LessLess
                    | Token::GreaterGreater
            )
        ) || (expected == "pattern"
            // What follows a binding in a pattern: a `for` binding's `in`,
            // an arm's `=>` or guard, an `@` binding
            // (B-2026-10-24-31).
            && matches!(
                self.tokens.get(self.pos + 1).map(|t| &t.token),
                Some(Token::In | Token::FatArrow | Token::If | Token::At)
            ))
            // A use of a binding already recovered under this name, as the
            // condition or scrutinee before a block (B-2026-10-29-13).
            || (expected == "expression"
                && self.escaped_bindings.contains(kw)
                && matches!(
                    self.tokens.get(self.pos + 1).map(|t| &t.token),
                    Some(Token::LeftBrace)
                ));
        if !follows_a_name {
            return false;
        }
        self.error_unexpected_ident(expected);
        let span = self.current_span();
        self.fix_edits.insert(
            crate::resolver::SpanKey::from_span(&span),
            crate::resolver::TextEdit {
                offset: span.offset,
                length: span.length,
                replacement: format!("r#{kw}"),
            },
        );
        if expected != "expression" {
            self.escaped_bindings.insert(kw);
        }
        self.tokens[self.pos].token = Token::Identifier {
            name: kw.to_string(),
            raw: true,
        };
        true
    }

    /// A structural marker written as a field's name, directly before the
    /// `:` of a field declaration or a struct literal's field
    /// (`struct Directives { private: bool }`, `Directives { private: false }`):
    /// `r#` cannot escape it, so no spelling of that name exists. Before,
    /// the declaration read `private` as the field's visibility and said
    /// "Expected identifier, found `:`" at the colon, and the literal failed
    /// and cascaded past its closing brace (B-2026-11-09-92). Reports it
    /// once, by name, and retypes the token as the name, so the rest of the
    /// struct and its uses parse as written. Returns whether it did.
    pub(crate) fn recover_marker_as_field_name(&mut self) -> bool {
        let Some(kw) = self.peek_token_ref().keyword_spelling() else {
            return false;
        };
        // `priv` is reserved for future use too, and keeps that message.
        if is_raw_escapable(kw)
            || matches!(self.peek_token_ref(), Token::ReservedFuture(_))
            || !matches!(
                self.tokens.get(self.pos + 1).map(|t| &t.token),
                Some(Token::Colon)
            )
        {
            return false;
        }
        let span = self.current_span();
        self.errors.push(ParseError {
            kind: ParseErrorKind::ReservedKeyword,
            message: format!(
                "`{kw}` cannot name a field: it is a structural marker, which `r#` does not \
                 escape, so give the field another name"
            ),
            span,
        });
        self.tokens[self.pos].token = Token::Identifier {
            name: kw.to_string(),
            raw: false,
        };
        true
    }

    fn unexpected_ident_msg(&self, expected: &str) -> String {
        let tok = self.peek_token();
        // A refused `r#` escape answers for itself, and must be handled BEFORE
        // the keyword arms: those close by advising `write \`r#NAME\``, which
        // is precisely what this writer already tried (B-2026-09-02-36). The
        // wording is design.md § Raw Identifiers' own.
        if let Token::RawIdentNotAllowed(marker) = tok {
            return format!(
                "'r#{marker}' is not legal; '{marker}' is a structural marker, \
                 not a reservable keyword"
            );
        }
        match tok.keyword_spelling() {
            Some(kw) => {
                // Two families, one diagnostic path. An ACTIVE keyword is a v1
                // feature the name collides with; a RESERVED-FOR-FUTURE-USE one
                // is not a feature at all, merely held back — worth saying,
                // because it tells the reader nothing they can write today uses
                // this word (B-2026-09-02-29).
                let mut msg = if matches!(tok, Token::ReservedFuture(_)) {
                    format!("'{kw}' is reserved for future use and cannot be used as an identifier")
                } else {
                    format!("'{kw}' is a reserved keyword and cannot be used as an identifier")
                };
                // Name the remedy. design.md § Keywords calls `r#` "the
                // forward-compatibility lever" and says tooling can apply it
                // mechanically; the compiler is that tooling and it already
                // knows the name the writer wanted (B-2026-09-02-30). Gated on
                // `is_raw_escapable` because the structural markers (`self`,
                // `mut`, `priv`, …) are the one set `r#` does not rescue, and
                // the lexer would reject the suggestion we had just made.
                if is_raw_escapable(kw) {
                    msg.push_str(&format!(
                        "; write `r#{kw}` to use it as an ordinary identifier"
                    ));
                }
                msg
            }
            None => format!("Expected {expected}, found {}", tok.describe()),
        }
    }

    /// Like [`error`], but anchors the diagnostic at an explicit `span`
    /// rather than the current token — for cases where the offending
    /// construct was already consumed (e.g. a bad `extern "ABI"` string).
    /// `Vec::new()`, `i64::MAX`, `Dir::Up =>`, `std::cmp::max(a, b)`: Rust's
    /// `::` paths. Kāra writes a path with `.`, and its free functions
    /// (`max`, `min`, `swap`, `replace`) and types need no `std::` module
    /// path. Called at an identifier: when a `::` follows it, report the
    /// whole path once, offer the Kāra spelling (the module prefix of a
    /// `std::` / `core::` path deleted, every other `::` written `.`), and
    /// rewrite those `::` tokens to `.` so the parse goes on as the fixed
    /// text would (B-2026-11-04-5). Leaves turbofish (`f::<T>`) alone.
    pub(crate) fn rewrite_rust_path(&mut self) {
        let is_segment = |t: Option<&Token>| matches!(t, Some(Token::Identifier { .. }));
        let at = self.pos;
        if !is_segment(self.tokens.get(at).map(|t| &t.token))
            || !matches!(
                self.tokens.get(at + 1).map(|t| &t.token),
                Some(Token::ColonColon)
            )
            || !is_segment(self.tokens.get(at + 2).map(|t| &t.token))
        {
            return;
        }
        // The segments, at token indices `at`, `at + 2`, ..; the `::`
        // between them at the odd offsets.
        let mut segments = vec![at];
        while matches!(
            self.tokens
                .get(segments[segments.len() - 1] + 1)
                .map(|t| &t.token),
            Some(Token::ColonColon)
        ) && is_segment(
            self.tokens
                .get(segments[segments.len() - 1] + 2)
                .map(|t| &t.token),
        ) {
            let next = segments[segments.len() - 1] + 2;
            segments.push(next);
        }
        let name = |i: usize| match &self.tokens[i].token {
            Token::Identifier { name, .. } => name.clone(),
            _ => String::new(),
        };
        let names: Vec<String> = segments.iter().map(|&i| name(i)).collect();
        // A `std::` / `core::` path, or one through a module Rust code
        // imports by name (`cmp::max` after `use std::cmp;`), loses its
        // module segments: every lowercase one before the first type
        // (`HashMap`, `i64`) or the last segment.
        const PRIMITIVES: &[&str] = &[
            "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
            "f32", "f64", "bool", "char", "str",
        ];
        let mut dropped = 0;
        if matches!(
            names[0].as_str(),
            "std" | "core" | "alloc" | "cmp" | "mem" | "collections"
        ) {
            while dropped < names.len() - 1
                && !starts_upper(&names[dropped])
                && !PRIMITIVES.contains(&names[dropped].as_str())
                && !kara_module_call(&names, dropped)
            {
                dropped += 1;
            }
        }
        // Rust's `thread::sleep(d)` / `std::thread::sleep(d)` is Kāra's
        // `std.time.sleep(d)`: the path goes, and the program imports
        // `sleep` unless a `use std::thread;` line does it for the path
        // (B-2026-11-11-73).
        let n = names.len();
        let thread_sleep = n >= 2
            && names[n - 1] == "sleep"
            && names[n - 2] == "thread"
            && (n == 2 || (n == 3 && matches!(names[0].as_str(), "std" | "core")));
        if thread_sleep {
            dropped = n - 1;
        }
        let first = self.tokens[at].span;
        let last = self.tokens[segments[segments.len() - 1]].span;
        let whole = Span {
            length: last.offset + last.length - first.offset,
            ..first
        };
        let written = names.join("::");
        // Rust's `str::from_utf8` (`std::str::from_utf8(&bytes)`) is Kāra's
        // `String.from_utf8`: Kāra has no `str` to call it on (kata 2096).
        let mut kara_names = names[dropped..].to_vec();
        let to_string =
            kara_names.len() == 2 && kara_names[0] == "str" && kara_names[1] == "from_utf8";
        if to_string {
            kara_names[0] = "String".to_string();
        }
        let kara = kara_names.join(".");
        let mut edits = Vec::new();
        if dropped > 0 {
            let keep = self.tokens[segments[dropped]].span.offset;
            edits.push(crate::resolver::TextEdit {
                offset: first.offset,
                length: keep - first.offset,
                replacement: String::new(),
            });
        }
        for &seg in &segments[dropped..segments.len() - 1] {
            let colons = self.tokens[seg + 1].span;
            edits.push(crate::resolver::TextEdit {
                offset: colons.offset,
                length: colons.length,
                replacement: ".".to_string(),
            });
        }
        if to_string {
            let str_at = segments[dropped];
            let span = self.tokens[str_at].span;
            edits.push(crate::resolver::TextEdit {
                offset: span.offset,
                length: span.length,
                replacement: "String".to_string(),
            });
            self.tokens[str_at].token = Token::Identifier {
                name: "String".to_string(),
                raw: false,
            };
        }
        // `std::thread::spawn` / `thread::spawn` / `tokio::spawn`: deleting
        // the module path would leave a free `spawn`, which Kāra does not
        // have (core-semantics §9.3, §11.1), so the fix would lead straight
        // into a second error. Name the spawning forms instead, with no
        // machine fix: the rewrite needs a group the program does not yet
        // have (B-2026-11-07-41).
        let n = names.len();
        if names[n - 1] == "spawn" && matches!(names[n - 2].as_str(), "thread" | "task" | "tokio") {
            let message = format!(
                "`{written}` is Rust: Kāra has no free `spawn` (core-semantics §11.1); start a \
                 task in a group, `let g = TaskGroup.new(); let h = g.spawn(|| ..);` (joined \
                 when `g` drops, its value read with `h.join()`), or run branches with \
                 `par {{ .. }}`"
            );
            self.error_at(&message, whole);
            for &seg in &segments[..segments.len() - 1] {
                self.tokens[seg + 1].token = Token::Dot;
            }
            return;
        }
        let sleep_import = if thread_sleep {
            self.std_time_sleep_import()
        } else {
            None
        };
        let message = if thread_sleep {
            let import = if sleep_import.is_some() {
                ", imported with `import std.time.sleep;`"
            } else {
                ""
            };
            format!(
                "`{written}` is Rust's -- Kāra's `sleep` is `std.time.sleep`, which takes the \
                 same `Duration`: write `sleep`{import}"
            )
        } else if dropped > 0 {
            format!(
                "`{written}` is a Rust path -- Kāra's `{}` needs no module path, and a path \
                 is written with `.`: write `{kara}`",
                kara_names[0]
            )
        } else {
            format!("`{written}` is Rust's `::` path -- Kāra writes a path with `.`: `{kara}`")
        };
        edits.extend(sleep_import);
        self.error_at(&message, whole);
        self.fix_diffs
            .insert(crate::resolver::SpanKey::from_span(&whole), edits);
        for &seg in &segments[..segments.len() - 1] {
            self.tokens[seg + 1].token = Token::Dot;
        }
    }

    /// `Self::count(d, n)` inside `impl Solution`, the Rust template's call
    /// of an associated function: Rust's `::` path through `Self`. Called at
    /// `Self` when a `::` and a name follow. Reports the path once, with the
    /// fix that writes the impl's type name and a `.` (`Solution.count`),
    /// which needs no reading of `Self` in value position; outside an impl,
    /// or for a target that is not a plain name, the fix is left out. The
    /// `::` is rewritten to `.` so the parse goes on, and no "expected `;`"
    /// follows at `Self`, whose fix used to write `Self;::count(..)`.
    pub(crate) fn rewrite_rust_self_path(&mut self) {
        let at = self.pos;
        if !matches!(self.tokens.get(at).map(|t| &t.token), Some(Token::SelfType))
            || !matches!(
                self.tokens.get(at + 1).map(|t| &t.token),
                Some(Token::ColonColon)
            )
        {
            return;
        }
        let Some(Token::Identifier { name: member, .. }) =
            self.tokens.get(at + 2).map(|t| t.token.clone())
        else {
            return;
        };
        let first = self.tokens[at].span;
        let name_span = self.tokens[at + 2].span;
        let whole = Span {
            length: name_span.offset + name_span.length - first.offset,
            ..first
        };
        match self.impl_self_name.clone() {
            Some(ty) => {
                self.error_at(
                    &format!(
                        "`Self::{member}` is Rust's `::` path -- Kāra names the type and \
                         writes a path with `.`: `{ty}.{member}`"
                    ),
                    whole,
                );
                self.fix_edits.insert(
                    crate::resolver::SpanKey::from_span(&whole),
                    crate::resolver::TextEdit {
                        offset: first.offset,
                        length: name_span.offset - first.offset,
                        replacement: format!("{ty}."),
                    },
                );
            }
            None => self.error_at(
                &format!(
                    "`Self::{member}` is Rust's `::` path -- Kāra names the type and \
                     writes a path with `.`: `T.{member}` for the type `T`"
                ),
                whole,
            ),
        }
        self.tokens[at + 1].token = Token::Dot;
    }

    /// The source text at `offset .. offset + length`, when the parser was
    /// given it ([`Self::with_source`]).
    /// Rust's nested `fn`, at the keyword inside a function body. Kāra has
    /// no nested items, and the keyword read as a misplaced identifier
    /// (`'fn' is a reserved keyword ... write r#fn`). A Rust nested `fn`
    /// captures nothing from the body around it, so it means the same at the
    /// top level: one diagnostic whose fix moves its text there, above the
    /// item it was in and with the body's indentation taken off, and the
    /// parse skips it (kata 1061's `impl Solution`).
    pub(crate) fn hoist_nested_fn(&mut self) -> Option<Stmt> {
        let start = self.current_span();
        self.parse_function(Vec::new(), false, false, false, false)?;
        let whole = self.span_from(&start);
        self.error_at(
            "a `fn` cannot be declared inside a function body: Kāra has no nested items. A \
             nested `fn` captures nothing from the body around it, so it means the same at \
             the top level: move it above the item it is in",
            whole,
        );
        if let Some(edits) = self.hoist_edits(whole) {
            self.fix_diffs
                .insert(crate::resolver::SpanKey::from_span(&whole), edits);
        }
        Some(Stmt {
            id: crate::ids::NodeId::DUMMY,
            span: whole,
            kind: StmtKind::Expr(Expr {
                id: crate::ids::NodeId::DUMMY,
                span: whole,
                kind: ExprKind::Tuple(Vec::new()),
            }),
        })
    }

    /// The edits [`Self::hoist_nested_fn`] offers: the function's lines
    /// deleted, and their text, less the indentation of its first line,
    /// inserted where the top-level item starts. Offered only when the `fn`
    /// begins its line and each later line carries that indentation (or is
    /// blank).
    fn hoist_edits(&self, whole: Span) -> Option<Vec<crate::resolver::TextEdit>> {
        let item = self.item_start?;
        let (source, base) = self.source.as_ref()?;
        let at = whole.offset.checked_sub(*base)?;
        let line_start = source.get(..at)?.rfind('\n').map_or(0, |i| i + 1);
        let indent = source.get(line_start..at)?;
        if !indent.chars().all(|c| c == ' ' || c == '\t') || item >= whole.offset {
            return None;
        }
        let text = source.get(at..at + whole.length)?;
        let mut moved = String::new();
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                moved.push('\n');
                if line.trim().is_empty() {
                    continue;
                }
                moved.push_str(line.strip_prefix(indent)?);
            } else {
                moved.push_str(line);
            }
        }
        moved.push_str("\n\n");
        let mut end = at + whole.length;
        if source.get(end..end + 1) == Some("\n") {
            end += 1;
        }
        Some(vec![
            crate::resolver::TextEdit {
                offset: item,
                length: 0,
                replacement: moved,
            },
            crate::resolver::TextEdit {
                offset: line_start + base,
                length: end - line_start,
                replacement: String::new(),
            },
        ])
    }

    /// Whether the program uses Rust's `std::cmp::Reverse`: it imports it
    /// (a `cmp` path segment followed, before the statement's `;`, by
    /// `Reverse`: `use std::cmp::Reverse;`, `use std::cmp::{Ordering,
    /// Reverse};`), or, once `karac fix` has deleted the import, it still
    /// writes a `BinaryHeap<Reverse<..>>` or `BinaryHeap[Reverse[..]]`.
    fn uses_rust_reverse(&self) -> bool {
        let tok = |i: usize| self.tokens.get(i).map(|t| &t.token);
        let ident = |i: usize, want: &str| matches!(tok(i), Some(Token::Identifier { name, .. }) if name == want);
        let opens = |i: usize| matches!(tok(i), Some(Token::LeftBracket | Token::LessThan));
        (0..self.tokens.len()).any(|i| {
            (ident(i, "BinaryHeap") && opens(i + 1) && ident(i + 2, "Reverse"))
                || (ident(i, "cmp")
                    && self.tokens[i + 1..]
                        .iter()
                        .take_while(|t| !matches!(t.token, Token::Semicolon))
                        .enumerate()
                        .any(|(k, _)| ident(i + 1 + k, "Reverse")))
        })
    }

    /// Whether a `Reverse` of Rust's `use std::cmp::Reverse` stays one once
    /// `karac fix` is done: it is not one the resolver's fix drops, around a
    /// heap's item (`.push(Reverse(x))`, `Some(Reverse(x))`, `let
    /// Reverse(x) =`, `BinaryHeap<Reverse<T>>`; [`Self::rust_reverse_wraps`])
    /// or as a whole sort key ([`Self::rust_reverse_sort_keys`]). When one
    /// stays, every `Reverse` does, as `std.cmp.Reverse` (B-2026-11-10-41):
    /// none is dropped, so a heap of them stays largest-first.
    fn rust_reverse_stays(&self) -> bool {
        let tok = |i: usize| self.tokens.get(i).map(|t| &t.token);
        let ident = |i: usize, want: &str| matches!(tok(i), Some(Token::Identifier { name, .. }) if name == want);
        let opens = |i: usize| matches!(tok(i), Some(Token::LeftBracket | Token::LessThan));
        let sort_keys = self.rust_reverse_sort_keys();
        let mut in_import = false;
        for i in 0..self.tokens.len() {
            match tok(i) {
                Some(Token::Import) => in_import = true,
                Some(Token::Identifier { name, .. }) if name == "use" => in_import = true,
                Some(Token::Semicolon) => in_import = false,
                _ => {}
            }
            if in_import || !ident(i, "Reverse") || i < 2 {
                continue;
            }
            let call = matches!(tok(i + 1), Some(Token::LeftParen));
            let wrapped = call
                && matches!(tok(i - 1), Some(Token::LeftParen))
                && ((ident(i - 2, "push") && i >= 3 && matches!(tok(i - 3), Some(Token::Dot)))
                    || ident(i - 2, "Some"));
            let let_bound = call && matches!(tok(i - 1), Some(Token::Let));
            // `|Reverse(s)| s` over such a heap's items (kata 1086).
            let param = call && matches!(tok(i - 1), Some(Token::Pipe));
            let heap_item = opens(i + 1) && opens(i - 1) && ident(i - 2, "BinaryHeap");
            let sort_key = sort_keys.contains_key(&self.tokens[i].span.offset);
            if !(wrapped || let_bound || param || heap_item || sort_key) {
                return true;
            }
        }
        false
    }

    /// The edit that imports `std.cmp.Reverse`: a line after the program's
    /// last import, or above its first item. See
    /// [`crate::ast::Program::std_cmp_reverse_import`].
    fn std_cmp_reverse_import(&self, items: &[Item]) -> Option<crate::resolver::TextEdit> {
        let uses = self
            .tokens
            .iter()
            .any(|t| matches!(&t.token, Token::Identifier { name, .. } if name == "Reverse"));
        if !uses {
            return None;
        }
        let (source, base) = self.source.as_ref()?;
        let last_import = items
            .iter()
            .filter_map(|i| match i {
                Item::Import(imp) => Some(imp.span.offset + imp.span.length),
                _ => None,
            })
            .max();
        let (at, replacement) = match last_import {
            Some(end) => (
                crate::library_imports::line_end(source, end.checked_sub(*base)?),
                "import std.cmp.Reverse;\n",
            ),
            None => (
                crate::library_imports::above_first_item(source),
                "import std.cmp.Reverse;\n\n",
            ),
        };
        Some(crate::resolver::TextEdit {
            offset: at + base,
            length: 0,
            replacement: replacement.to_string(),
        })
    }

    /// A `let`'s value `(e) as T` starting at `start`, its operand
    /// parenthesized: see [`crate::ast::Program::whole_value_casts`].
    pub(crate) fn note_whole_value_cast(&mut self, start: &Span, value: &Expr) {
        let ExprKind::Cast { expr: operand, .. } = &value.kind else {
            return;
        };
        let key = crate::resolver::SpanKey::from_span(&value.span);
        if !self.cast_operand_ends.contains_key(&key)
            || self.source_at(start.offset, 1) != Some("(")
            || start.offset >= operand.span.offset
        {
            return;
        }
        let end = value.span.offset + value.span.length;
        let Some(text) = self.source_at(operand.span.offset, operand.span.length) else {
            return;
        };
        let text = text.to_string();
        let whole = Span {
            offset: start.offset,
            length: end - start.offset,
            ..*start
        };
        self.whole_value_casts.insert(key, (whole, text));
    }

    fn source_at(&self, offset: usize, length: usize) -> Option<&str> {
        let (source, base) = self.source.as_ref()?;
        let at = offset.checked_sub(*base)?;
        source.get(at..at + length)
    }

    /// How much of the source a fix deleting the Rust `use` line at `whole`
    /// takes: the statement, its newline, and a blank line after it when a
    /// blank line (or the top of the file) is before it too, so the two do
    /// not become two blank lines in a row (katas 1120, 1122). Two `use`
    /// lines in a row go in separate `karac fix` rounds, so each round sees
    /// the lines the last one left.
    pub(crate) fn rust_use_line_length(&self, whole: Span) -> usize {
        let mut length = whole.length;
        if self.source_at(whole.offset + whole.length, 1) != Some("\n") {
            return length;
        }
        length += 1;
        let blank_before = whole.offset == 0
            || (whole.offset >= 2 && self.source_at(whole.offset - 2, 2) == Some("\n\n"));
        if blank_before && self.source_at(whole.offset + length, 1) == Some("\n") {
            length += 1;
        }
        length
    }

    /// Rust's `use std::collections::HashMap;`, or the `import` `karac fix`
    /// makes of its `use`, at the `use` / `import` keyword. Rust's
    /// collections, `cmp` and `mem` are in Kāra's prelude (`Map`, `max`,
    /// `swap`), so the statement goes: one diagnostic whose fix deletes the
    /// line, in place of the `use` -> `import` one so the edits never
    /// overlap, and the parse skips it. A use of `HashMap` then gets the
    /// resolver's rename to `Map`. Returns `false`, consuming nothing, for any
    /// other import, and for a renaming one (`as`), whose name the line
    /// would take with it. B-2026-11-04-5.
    pub(crate) fn reject_rust_prelude_import(&mut self) -> bool {
        let at = self.pos;
        let token = |i: usize| self.tokens.get(i).map(|t| &t.token);
        let ident = |i: usize| match token(i) {
            Some(Token::Identifier { name, .. }) => Some(name.as_str()),
            _ => None,
        };
        // The path's separators are Rust's `::`, or, after Rust's `use`, the
        // dots of `use std.collections.VecDeque;` (kata 2101), whose `import`
        // would name a module that does not exist. An `import` keeps its dots
        // to itself: `import std.collections.*` is the spelling syntax.md
        // gives, and `karac fmt` must still parse it.
        let after_use = matches!(token(at), Some(Token::Identifier { name, .. }) if name == "use");
        let sep = |i: usize| {
            matches!(token(i), Some(Token::ColonColon))
                || (after_use && matches!(token(i), Some(Token::Dot)))
        };
        // `use std::sync::mpsc::{channel, Receiver, Sender}`: Kāra's
        // channels are in its prelude too (kata 1114).
        let mpsc = matches!(ident(at + 3), Some("sync"))
            && sep(at + 4)
            && matches!(ident(at + 5), Some("mpsc"));
        if !matches!(ident(at + 1), Some("std" | "core" | "alloc"))
            || !sep(at + 2)
            || !(mpsc
                || matches!(
                    ident(at + 3),
                    Some("collections" | "cmp" | "mem" | "iter" | "str")
                ))
            || !(sep(at + 4) || matches!(token(at + 4), Some(Token::Semicolon)))
        {
            return false;
        }
        // `std::iter` and `std::str` hold items Kāra has elsewhere too
        // (`std.iter.repeat` is a real import), so only their prelude
        // types go: `Peekable`, the iterator traits, and `Chars` /
        // `CharIndices`, which are Kāra's `StrChars` / `StrCharIndices`
        // (B-2026-11-10-35, kata 1106).
        let prelude_leaf: fn(&str) -> bool = match ident(at + 3) {
            Some("iter") => |n| {
                matches!(
                    n,
                    "Peekable"
                        | "Iterator"
                        | "IntoIterator"
                        | "FromIterator"
                        | "DoubleEndedIterator"
                        | "ExactSizeIterator"
                )
            },
            Some("str") => |n| matches!(n, "Chars" | "CharIndices"),
            _ => |_| true,
        };
        // The leaves (`HashMap`, or each name of a `{..}` group) up to the
        // statement's `;`.
        let mut leaves: Vec<String> = Vec::new();
        // `use std::mem;` imports the module, whose functions Kāra's prelude
        // holds; `mem::swap` loses its `mem::` at the use site.
        let mut semi = if matches!(token(at + 4), Some(Token::Semicolon)) {
            at + 4
        } else {
            at + 5
        };
        loop {
            match token(semi) {
                Some(Token::Semicolon) => break,
                Some(Token::Identifier { name, .. }) => {
                    if !sep(semi + 1) {
                        leaves.push(name.clone());
                    }
                }
                Some(Token::Dot) if after_use => {}
                Some(
                    Token::ColonColon
                    | Token::LeftBrace
                    | Token::RightBrace
                    | Token::Comma
                    | Token::Star
                    | Token::SelfValue,
                ) => {}
                _ => return false,
            }
            semi += 1;
        }
        // Only the names the prelude has: `mpsc` itself, `SyncSender` or
        // `sync_channel` stay for the import's own error.
        if mpsc
            && (leaves.is_empty()
                || leaves
                    .iter()
                    .any(|n| !matches!(n.as_str(), "channel" | "Sender" | "Receiver")))
        {
            return false;
        }
        if !mpsc
            && (leaves.is_empty() || !leaves.iter().all(|n| prelude_leaf(n)))
            && !matches!(ident(at + 3), Some("collections" | "cmp" | "mem"))
        {
            return false;
        }
        let first = self.tokens[at].span;
        let end = self.tokens[semi].span;
        let whole = Span {
            length: end.offset + end.length - first.offset,
            ..first
        };
        let path_start = self.tokens[at + 1].span.offset;
        let path_end = {
            let last = self.tokens[semi - 1].span;
            last.offset + last.length
        };
        let written = self
            .source_at(path_start, path_end - path_start)
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "{}::{}",
                    ident(at + 1).unwrap_or_default(),
                    ident(at + 3).unwrap_or_default()
                )
            });
        // `Reverse` is not in the prelude. It is `std.cmp.Reverse`, the line's
        // one import, when a use of it stays; the resolver's fix drops it
        // around a heap's items (kata 1057) and a whole sort key (kata 1058),
        // and a program with no other use loses the line (B-2026-11-10-41).
        let reverse = leaves.iter().any(|n| n == "Reverse");
        let keep_reverse = reverse && self.rust_reverse_stays();
        let kara: Vec<String> = leaves
            .iter()
            .filter(|n| n.as_str() != "self" && n.as_str() != "Reverse")
            .map(|n| format!("`{}`", rust_collection_rename(n).unwrap_or(n)))
            .collect();
        let named = if kara.is_empty() {
            "its names".to_string()
        } else {
            kara.join(", ")
        };
        let no_reverse = "no need of `Reverse` here, since its `PriorityQueue` pops the \
                          smallest first (each `Reverse(..)` is reported where it is used)";
        let message = if keep_reverse {
            let prelude = if kara.is_empty() {
                String::new()
            } else {
                format!(", and {named} are in its prelude with no import")
            };
            format!(
                "`{written}` is Rust's standard library -- Kāra's `Reverse` is `std.cmp.Reverse`\
                 {prelude}: `import std.cmp.Reverse;`"
            )
        } else if reverse && kara.is_empty() {
            format!(
                "`{written}` is Rust's standard library -- Kāra has {no_reverse}: delete the line"
            )
        } else if reverse {
            format!(
                "`{written}` is Rust's standard library -- Kāra has {named} in its prelude, with \
                 no import, and {no_reverse}: delete the line"
            )
        } else {
            format!(
                "`{written}` is Rust's standard library -- Kāra has {named} in its prelude, with \
                 no import: delete the line"
            )
        };
        // The edit takes the line's newline too, so no blank line is left.
        let edit_length = if keep_reverse {
            whole.length
        } else {
            self.rust_use_line_length(whole)
        };
        self.error_at(&message, whole);
        self.fix_diffs.insert(
            crate::resolver::SpanKey::from_span(&whole),
            vec![crate::resolver::TextEdit {
                offset: whole.offset,
                length: edit_length,
                replacement: if keep_reverse {
                    "import std.cmp.Reverse;".to_string()
                } else {
                    String::new()
                },
            }],
        );
        self.pos = semi + 1;
        true
    }

    /// An import path written with Rust's `::` (`import std::sync::mpsc;`,
    /// what `karac fix` makes of a `use`), with the cursor just past the
    /// keyword: one diagnostic naming the path, whose fix writes `.` for each
    /// `::`, and the tokens rewritten so the import parses. B-2026-11-04-5.
    pub(crate) fn rewrite_rust_import_path(&mut self) {
        let at = self.pos;
        let mut colons = Vec::new();
        let mut end = at;
        while let Some(t) = self.tokens.get(end).map(|t| &t.token) {
            match t {
                Token::ColonColon => colons.push(end),
                Token::Identifier { .. }
                | Token::Dot
                | Token::LeftBrace
                | Token::RightBrace
                | Token::Comma
                | Token::Star
                | Token::As
                | Token::SelfValue => {}
                _ => break,
            }
            end += 1;
        }
        if colons.is_empty() || end == at {
            return;
        }
        let first = self.tokens[at].span;
        let last = self.tokens[end - 1].span;
        let whole = Span {
            length: last.offset + last.length - first.offset,
            ..first
        };
        let edits: Vec<crate::resolver::TextEdit> = colons
            .iter()
            .map(|&i| crate::resolver::TextEdit {
                offset: self.tokens[i].span.offset,
                length: self.tokens[i].span.length,
                replacement: ".".to_string(),
            })
            .collect();
        let message = match self.source_at(whole.offset, whole.length) {
            Some(written) => format!(
                "`{written}` is Rust's `::` path -- Kāra writes an import path with `.`: `{}`",
                written.replace("::", ".")
            ),
            None => "Rust's `::` path -- Kāra writes an import path with `.`".to_string(),
        };
        self.error_at(&message, whole);
        self.fix_diffs
            .insert(crate::resolver::SpanKey::from_span(&whole), edits);
        for &i in &colons {
            self.tokens[i].token = Token::Dot;
        }
    }

    fn error_at(&mut self, message: &str, span: Span) {
        self.error_at_kind(ParseErrorKind::Syntax, message, span);
    }

    /// [`error_at`] with an explicit machine-readable family.
    fn error_at_kind(&mut self, kind: ParseErrorKind, message: &str, span: Span) {
        self.errors.push(ParseError {
            kind,
            message: message.to_string(),
            span,
        });
    }

    /// If `abi` names one of the calling conventions reserved-but-unimplemented
    /// at v1, emit the targeted "reserved" diagnostic at `span` and return
    /// `true`; otherwise return `false` so the caller applies its own
    /// valid/unknown-ABI handling. Recognizing the syntax now (rather than
    /// lumping it into the generic "unknown ABI" error) freezes it at v1 so a
    /// future implementation is not source-breaking — the same reservation
    /// posture as the `f16` / `bf16` numeric keywords. See the Phase-11
    /// embedded/hardware tracker and design.md § FFI.
    fn reserved_abi_diagnostic(&mut self, abi: &str, span: Span) -> bool {
        if RESERVED_FFI_ABIS.contains(&abi) {
            self.error_at_kind(
                ParseErrorKind::ReservedSyntax,
                &format!(
                    "the `\"{abi}\"` calling convention is reserved for a future release \
                     and cannot be used yet — in 0.8 an `extern` ABI may only be `\"C\"` or \
                     `\"C-unwind\"`. (`\"stdcall\"`, `\"fastcall\"`, `\"win64\"`, `\"sysv64\"` \
                     are recognized but unimplemented; see design.md § FFI.)"
                ),
                span,
            );
            true
        } else {
            false
        }
    }

    /// Emit a non-fatal diagnostic if `name` does not have the expected
    /// `IdentClass`. The `context` string is the declaration kind (e.g.
    /// `"struct"`, `"fn"`, `"const"`). The diagnostic includes a rename
    /// suggestion.
    fn check_ident_class(&mut self, name: &str, expected: IdentClass, context: &str, span: Span) {
        let actual = classify_ident(name);
        // design.md §3 CN-5: only a Value-class name may begin with `_`, the
        // mark of an intentionally unused binding (B-2026-11-06-18).
        if actual == expected && expected != IdentClass::Value && name.starts_with('_') {
            let trimmed = name.trim_start_matches('_');
            self.errors.push(ParseError {
                kind: ParseErrorKind::Syntax,
                message: format!(
                    "`{name}` begins with `_`, but {context} names may not: a leading \
                     underscore marks an unused Value-class binding, and Type-class and \
                     Const-class names never take one (design.md §3 CN-5); rename it to \
                     `{trimmed}`"
                ),
                span,
            });
            return;
        }
        if actual == expected {
            return;
        }
        let expected_desc = match expected {
            IdentClass::Type => "Type-class (PascalCase)",
            IdentClass::Value => "Value-class (snake_case)",
            IdentClass::Const => "Const-class (SCREAMING_SNAKE_CASE)",
        };
        let actual_desc = match actual {
            IdentClass::Type => "Type-class",
            IdentClass::Value => "Value-class",
            IdentClass::Const => "Const-class",
        };
        // B-2026-07-31-32 — same guard as the resolver's
        // `E_MODULE_BINDING_NAMING`: name a replacement only when the case
        // transform actually reaches `expected`. `const M = 1;` used to advise
        // "consider renaming to `M`", because uppercasing a lone uppercase
        // letter is the identity while CN-7 classifies a single-letter name as
        // Type-class regardless. This site carries no machine-applicable edit,
        // so the identity suggestion was merely useless rather than
        // loop-inducing — but it is the same defect and shares the fix.
        let advice = match crate::lexer::suggest_name_for_class(name, expected) {
            Some(s) => format!("consider renaming to `{s}`"),
            None if crate::lexer::has_single_letter(name) => format!(
                "a name with a single letter is always Type-class, so no capitalization change \
                 can make it {expected_desc} — use a name with more than one letter"
            ),
            None => format!("rename it to {expected_desc}"),
        };
        self.errors.push(ParseError {
            kind: ParseErrorKind::Syntax,
            message: format!(
                "`{name}` is {actual_desc} but {context} names must be {expected_desc}; {advice}"
            ),
            span,
        });
    }

    /// The fix for a private `struct` or `enum` whose name is not Type
    /// class (LeetCode's `class H2O`, which is Const class here): the
    /// CN-4 rename, `H2o`, at every use of the name in the file, which a
    /// type name's uses all are, since a private type is named nowhere
    /// else (B-2026-11-11-35). Withheld when an f-string's hole names it,
    /// whose text the token edits cannot reach.
    fn offer_type_rename(&mut self, name: &str, error_span: Span) {
        if classify_ident(name) == IdentClass::Type {
            return;
        }
        let Some(new_name) = crate::lexer::suggest_name_for_class(name, IdentClass::Type) else {
            return;
        };
        let mut edits = Vec::new();
        for t in &self.tokens {
            match &t.token {
                Token::Identifier {
                    name: n,
                    raw: false,
                } if n == name => {
                    edits.push(crate::resolver::TextEdit {
                        offset: t.span.offset,
                        length: t.span.length,
                        replacement: new_name.clone(),
                    });
                }
                Token::InterpolatedStringLiteral(parts)
                    if parts.iter().any(|p| {
                        matches!(p, crate::token::InterpolationPart::Expr { raw, .. }
                            if raw.contains(name))
                    }) =>
                {
                    return;
                }
                _ => {}
            }
        }
        if !edits.is_empty() {
            self.fix_diffs
                .insert(crate::resolver::SpanKey::from_span(&error_span), edits);
        }
    }

    fn synchronize_to_item(&mut self) {
        while !self.is_at_end() {
            match self.peek_token_ref() {
                Token::Fn
                | Token::Struct
                | Token::Enum
                | Token::Trait
                | Token::Impl
                | Token::Import
                | Token::Const
                | Token::Extern
                | Token::Type
                | Token::Pub
                | Token::Pound => return,
                // A contextual keyword resyncs only where it starts an item
                // (`union Name`, `layout Name`, …), not as a plain name.
                Token::Identifier { name, raw: false }
                    if matches!(name.as_str(), "union" | "layout" | "alias" | "independent")
                        && matches!(self.peek_token_ref_at(1), Token::Identifier { .. })
                        || matches!(name.as_str(), "transparent" | "stable")
                            && self.at_kw_at(1, "effect")
                        || name == "effect" && self.effect_decl_follows()
                        || name == "distinct"
                            && matches!(self.peek_token_ref_at(1), Token::Type)
                        || name == "shared"
                            && matches!(self.peek_token_ref_at(1), Token::Struct | Token::Enum) =>
                {
                    return
                }
                _ => {
                    self.advance();
                }
            }
        }
    }

    /// Recovery after a block statement that began at token `before`
    /// failed: skip to the next statement, and past at least one token.
    /// A statement the recursion budget refuses fails without consuming
    /// its keyword, and [`Self::synchronize_to_stmt`] stops at that same
    /// keyword, so without the step the block retried it forever, one
    /// diagnostic a turn, until memory ran out (`fuzz_parser`, 128 `{`
    /// then `if`; B-2026-11-02-25).
    pub(crate) fn recover_stmt(&mut self, before: usize) {
        self.synchronize_to_stmt();
        if self.pos == before && !self.is_at_end() && !self.check(&Token::RightBrace) {
            self.advance();
            self.synchronize_to_stmt();
        }
    }

    fn synchronize_to_stmt(&mut self) {
        while !self.is_at_end() {
            match self.peek_token_ref() {
                Token::Semicolon => {
                    self.advance();
                    return;
                }
                Token::RightBrace => return,
                Token::Let
                | Token::Return
                | Token::If
                | Token::While
                | Token::For
                | Token::Loop
                | Token::Match
                | Token::Break
                | Token::Continue => return,
                _ => {
                    self.advance();
                }
            }
        }
    }
}

// ── Diagnostic helpers ──────────────────────────────────────────

/// Render a `TypeExpr` back to a compact source-style string for inclusion
/// in parser diagnostics (e.g., the `_: <type>` fix-it on
/// `E_FN_ANONYMOUS_PARAM`). Covers every `TypeKind` variant the parser
/// can build; not byte-for-byte identical to the original source, but
/// produces a copy-pasteable surface form.
pub(crate) fn render_type_for_diagnostic(ty: &TypeExpr) -> String {
    let mut out = String::new();
    write_type_for_diagnostic(ty, &mut out);
    out
}

fn write_type_for_diagnostic(ty: &TypeExpr, out: &mut String) {
    match &ty.kind {
        TypeKind::Path(path) => {
            for (i, seg) in path.segments.iter().enumerate() {
                if i > 0 {
                    out.push('.');
                }
                out.push_str(seg);
            }
            if let Some(args) = &path.generic_args {
                if !args.is_empty() {
                    out.push('[');
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        match arg {
                            crate::ast::GenericArg::Type(t) => write_type_for_diagnostic(t, out),
                            crate::ast::GenericArg::Const(_) => out.push('_'),
                            crate::ast::GenericArg::Shape(s) => {
                                out.push('[');
                                for (j, _) in s.dims.iter().enumerate() {
                                    if j > 0 {
                                        out.push_str(", ");
                                    }
                                    out.push('_');
                                }
                                out.push(']');
                            }
                        }
                    }
                    out.push(']');
                }
            }
        }
        TypeKind::Tuple(types) => {
            out.push('(');
            for (i, t) in types.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_type_for_diagnostic(t, out);
            }
            out.push(')');
        }
        TypeKind::Array { element, .. } => {
            out.push('[');
            write_type_for_diagnostic(element, out);
            out.push_str("; _]");
        }
        TypeKind::Pointer { is_mut, inner } => {
            out.push('*');
            out.push_str(if *is_mut { "mut " } else { "const " });
            write_type_for_diagnostic(inner, out);
        }
        TypeKind::FnType {
            params,
            return_type,
            is_once,
            ..
        } => {
            out.push_str(if *is_once { "OnceFn(" } else { "Fn(" });
            for (i, p) in params.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_type_for_diagnostic(p, out);
            }
            out.push(')');
            if let Some(rt) = return_type {
                out.push_str(" -> ");
                write_type_for_diagnostic(rt, out);
            }
        }
        TypeKind::Frozen(inner) => {
            out.push_str("frozen ");
            write_type_for_diagnostic(inner, out);
        }
        TypeKind::Ref(inner) => {
            out.push_str("ref ");
            write_type_for_diagnostic(inner, out);
        }
        TypeKind::MutRef(inner) => {
            out.push_str("mut ref ");
            write_type_for_diagnostic(inner, out);
        }
        TypeKind::MutSlice(element) => {
            out.push_str("mut Slice[");
            write_type_for_diagnostic(element, out);
            out.push(']');
        }
        TypeKind::Weak(inner) => {
            out.push_str("weak ");
            write_type_for_diagnostic(inner, out);
        }
        // `impl Trait` slice 1 stub: render the surface form for the
        // anonymous-parameter `_: <type>` fix-it suggestion. The full
        // existential-effect rendering is unimportant for the
        // diagnostic surface — we use the bare `impl TraitPath[args]`
        // form (no `with` clause), since the `with` half is rarely
        // load-bearing in a parameter-type-paste typo.
        TypeKind::ImplTrait {
            trait_path, args, ..
        } => {
            out.push_str("impl ");
            for (i, seg) in trait_path.segments.iter().enumerate() {
                if i > 0 {
                    out.push('.');
                }
                out.push_str(seg);
            }
            if !args.is_empty() {
                out.push('[');
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    match arg {
                        crate::ast::GenericArg::Type(t) => write_type_for_diagnostic(t, out),
                        crate::ast::GenericArg::Const(_) => out.push('_'),
                        crate::ast::GenericArg::Shape(s) => {
                            out.push('[');
                            for (j, _) in s.dims.iter().enumerate() {
                                if j > 0 {
                                    out.push_str(", ");
                                }
                                out.push('_');
                            }
                            out.push(']');
                        }
                    }
                }
                out.push(']');
            }
        }
        // `dyn Trait` slice 5 stub: render the surface form so the
        // anonymous-parameter `_: <type>` fix-it covers `dyn Trait`
        // signatures uniformly with the `impl Trait` shape above.
        TypeKind::Dyn {
            trait_path,
            args,
            cross_task,
            ..
        } => {
            out.push_str("dyn ");
            for (i, seg) in trait_path.segments.iter().enumerate() {
                if i > 0 {
                    out.push('.');
                }
                out.push_str(seg);
            }
            if !args.is_empty() {
                out.push('[');
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    match arg {
                        crate::ast::GenericArg::Type(t) => write_type_for_diagnostic(t, out),
                        crate::ast::GenericArg::Const(_) => out.push('_'),
                        crate::ast::GenericArg::Shape(s) => {
                            out.push('[');
                            for (j, _) in s.dims.iter().enumerate() {
                                if j > 0 {
                                    out.push_str(", ");
                                }
                                out.push('_');
                            }
                            out.push(']');
                        }
                    }
                }
                out.push(']');
            }
            if *cross_task {
                out.push_str(" + CrossTask");
            }
        }
        TypeKind::Unit => out.push_str("()"),
        TypeKind::Error => out.push('_'),
    }
}
