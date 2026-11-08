//! Canonical formatter for Kāra source code.
//!
//! Parses AST and reprints in deterministic canonical form.
//! Scope: syntactic canonicalization (like gofmt/rustfmt), not semantic normalization.

use crate::ast::*;
use crate::token::{FloatSuffix, IntSuffix};

const INDENT: &str = "    ";

mod comments;
mod exprs;
mod items;
mod patterns;
mod stmts;
mod types;

/// Format a program parsed from `source`, keeping the source's comments,
/// which the AST does not carry (B-2026-10-30-16). The `karac fmt` and
/// [`crate::format_source`] entry point.
pub fn format_program_with_comments(program: &Program, source: &str) -> String {
    format_program_inner(program, comments::collect_comments(source))
}

/// Format a program from its AST alone. Comments are not in the tree, so a
/// program parsed from source loses them here: use
/// [`format_program_with_comments`] for that.
pub fn format_program(program: &Program) -> String {
    format_program_inner(program, Vec::new())
}

fn format_program_inner(program: &Program, comments: Vec<comments::Comment>) -> String {
    let mut f = Formatter::new();
    f.comments = comments;
    f.freeze_spans = program.freeze_spans.clone();
    f.ref_binding_spans = program.ref_binding_spans.clone();
    f.block_const_spans = program.block_const_spans.clone();
    f.mut_ref_binding_spans = program.mut_ref_binding_spans.clone();
    f.escaping_fn_types = program.escaping_fn_types.clone();
    f.mut_fn_types = program.mut_fn_types.clone();
    f.cross_task_fn_types = program.cross_task_fn_types.clone();
    f.paren_return_types = program.paren_return_types.clone();
    f.container_hashers = program.container_hashers.clone();
    f.stmt_lint_overrides = program.stmt_lint_overrides.clone();
    f.vec_macros = program.vec_macro_literals.keys().copied().collect();
    f.format_program(program);
    f.output
}

/// Render a single [`TypeExpr`] to its canonical surface form — the same
/// shape the formatter prints inside a larger item. Useful to outside
/// modules (e.g. `karac catalog`) that need to emit a type's display
/// string without round-tripping through a full item.
pub fn render_type_expr(ty: &TypeExpr) -> String {
    let mut f = Formatter::new();
    f.format_type_expr(ty);
    f.output
}

/// Render an [`EffectList`] to its surface form without the leading
/// ` with ` keyword — the caller decides whether to wrap. Mirrors
/// [`render_type_expr`].
pub fn render_effect_list(effects: &EffectList) -> String {
    // A function type's declared empty set, `with ()` (design.md § How a
    // call through a function value gets its effects).
    if effects.items.is_empty() {
        return "()".to_string();
    }
    let mut f = Formatter::new();
    let mut first = true;
    for item in &effects.items {
        if !first {
            f.write_str(" ");
        }
        first = false;
        match item {
            EffectItem::Verb(v) => f.format_effect_verb(v),
            EffectItem::Group(g) => f.write_ident(g),
            EffectItem::Polymorphic => f.write_str("_"),
            EffectItem::Variable(v) => f.write_ident(v),
        }
    }
    f.output
}

/// Render a single [`Expr`] to its canonical surface form — for catalog
/// emitters that surface refinement predicates (`requires` / `ensures`)
/// as source strings.
pub fn render_expr(expr: &Expr) -> String {
    let mut f = Formatter::new();
    f.format_expr(expr);
    f.output
}

/// Render a single [`TraitBound`] (`Trait[Args]`) to its surface form.
pub fn render_trait_bound(bound: &TraitBound) -> String {
    let mut f = Formatter::new();
    f.write_path(&bound.path);
    f.format_generic_args_opt(&bound.generic_args);
    f.output
}

/// A generic parameter list (`[T: Bound, U]`), or the empty string when
/// there is none.
pub fn render_generic_params(gp: &Option<GenericParams>) -> String {
    let mut f = Formatter::new();
    f.format_generic_params(gp);
    f.output
}

/// A function's head as declared, without its body: modifiers, name,
/// generic parameters, receiver and parameters, return type, declared
/// effects and `where` clause. `karac doc` shows it as the signature
/// (B-2026-11-08-34).
pub fn render_fn_head(func: &Function) -> String {
    let mut f = Formatter::new();
    if func.is_comptime {
        f.write_str("comptime ");
    }
    if func.is_unsafe {
        f.write_str("unsafe ");
    }
    f.write_str("fn ");
    f.write_str(&func.name);
    f.format_generic_params(&func.generic_params);
    f.write_str("(");
    f.format_fn_params_with_receiver(
        &func.self_param,
        func.self_is_frozen,
        func.self_is_own,
        &func.params,
    );
    f.write_str(")");
    if let Some(ref rt) = func.return_type {
        f.write_str(" -> ");
        f.format_return_type(rt);
    }
    f.format_effects(&func.effects);
    f.format_where_clause(&func.where_clause);
    f.output
}

/// [`render_fn_head`] for a trait method declaration.
pub fn render_trait_method_head(m: &TraitMethod) -> String {
    let mut f = Formatter::new();
    if m.is_unsafe {
        f.write_str("unsafe ");
    }
    f.write_str("fn ");
    f.write_str(&m.name);
    f.format_generic_params(&m.generic_params);
    f.write_str("(");
    f.format_fn_params(&m.self_param, m.self_is_own, &m.params);
    f.write_str(")");
    if let Some(ref rt) = m.return_type {
        f.write_str(" -> ");
        f.format_return_type(rt);
    }
    f.format_effects(&m.effects);
    f.format_where_clause(&m.where_clause);
    f.output
}

/// An `impl` block's head (`impl[T: Show] Show for Vec[T]`), without its
/// body.
pub fn render_impl_head(imp: &ImplBlock) -> String {
    let mut f = Formatter::new();
    f.write_str("impl");
    f.format_generic_params(&imp.generic_params);
    if let Some(ref trait_name) = imp.trait_name {
        f.write_str(" ");
        f.write_path(&trait_name.segments);
        f.format_generic_args_opt(&trait_name.generic_args);
        f.write_str(" for");
    }
    f.write_str(" ");
    f.format_type_expr(&imp.target_type);
    f.format_where_clause(&imp.where_clause);
    f.output
}

/// A `where` clause as the formatter writes it (on its own line, with the
/// leading newline), or the empty string when there is none.
pub fn render_where_clause(wc: &Option<WhereClause>) -> String {
    let mut f = Formatter::new();
    f.format_where_clause(wc);
    f.output
}

pub(super) struct Formatter {
    pub(super) output: String,
    pub(super) indent: usize,
    /// `freeze <place>` initializer spans (B-2026-08-01-33 stage 3), copied
    /// from the program being formatted. The keyword lives on
    /// `Program::freeze_spans` rather than inside the expression tree, so
    /// `format_expr` will never see it and the `let` printer has to write it
    /// back — without this, `karac fmt` rewrites `let g = freeze t;` to `let g
    /// = t;`, which deletes a guarantee and turns a non-counting binding into
    /// a counted one. Empty for every entry point that formats a fragment
    /// rather than a program (`render_type_expr` and friends), where no `let`
    /// is reachable.
    pub(super) freeze_spans: rustc_hash::FxHashSet<crate::resolver::SpanKey>,
    /// `ref name` pattern bindings, copied from `Program::ref_binding_spans`
    /// for the same reason: the `ref` is not in the tree.
    pub(super) ref_binding_spans: rustc_hash::FxHashSet<crate::resolver::SpanKey>,
    /// `Program::block_const_spans`: a block's `const`, parsed as a `let`.
    pub(super) block_const_spans: rustc_hash::FxHashSet<crate::resolver::SpanKey>,
    /// The `mut ref name` bindings among them.
    pub(super) mut_ref_binding_spans: rustc_hash::FxHashSet<crate::resolver::SpanKey>,
    /// `escaping Fn(...)` parameter types, copied from
    /// `Program::escaping_fn_types`: the keyword is not in the tree either.
    pub(super) escaping_fn_types: rustc_hash::FxHashSet<crate::resolver::SpanKey>,
    /// `MutFn(...)` types, copied from `Program::mut_fn_types`.
    pub(super) mut_fn_types: rustc_hash::FxHashSet<crate::resolver::SpanKey>,
    /// `Fn(..) + CrossTask` types, copied from `Program::cross_task_fn_types`.
    pub(super) cross_task_fn_types: rustc_hash::FxHashSet<crate::resolver::SpanKey>,
    /// Parenthesized return types, copied from `Program::paren_return_types`:
    /// `-> (dyn T) with X` keeps its parentheses, which silence
    /// `return_clause_on_fn`.
    pub(super) paren_return_types: rustc_hash::FxHashSet<crate::resolver::SpanKey>,
    /// The hasher argument of `Map[K, V, H]` / `Set[T, H]`, which the parser
    /// moves out of the tree (`Program::container_hashers`).
    pub(super) container_hashers:
        rustc_hash::FxHashMap<crate::resolver::SpanKey, crate::hasher_kind::HasherKind>,
    /// `#[allow(lint)]` and the other lint levels written on a statement,
    /// which the parser keeps beside the tree (`Program::stmt_lint_overrides`).
    /// The `Vec` literals written `vec![..]` (`Program::vec_macro_literals`):
    /// the formatter prints a literal as it was written, so it neither adds
    /// the deprecated spelling nor takes it away (`karac fix` does that).
    pub(super) vec_macros: rustc_hash::FxHashSet<crate::resolver::SpanKey>,
    pub(super) stmt_lint_overrides:
        rustc_hash::FxHashMap<crate::resolver::SpanKey, Vec<crate::lints::LintLevelOverride>>,
    /// The source's comments, in source order, and the index of the first
    /// one not yet written (`comments.rs`). Empty for a fragment.
    pub(super) comments: Vec<comments::Comment>,
    pub(super) next_comment: usize,
    /// `output.len()` just after a `//` comment ended a line.
    pub(super) line_comment_end: usize,
    /// The expression being printed begins a statement (`exprs.rs`).
    pub(super) stmt_start: bool,
    /// The expression being printed is in the head of an `if`, `while`,
    /// `if let`, `while let` or `for`, outside any delimiter, where a struct
    /// literal must be parenthesized (design.md § 6, `exprs.rs`).
    pub(super) in_head: bool,
}

impl Formatter {
    pub(super) fn new() -> Self {
        Formatter {
            output: String::new(),
            indent: 0,
            freeze_spans: rustc_hash::FxHashSet::default(),
            ref_binding_spans: rustc_hash::FxHashSet::default(),
            block_const_spans: rustc_hash::FxHashSet::default(),
            mut_ref_binding_spans: rustc_hash::FxHashSet::default(),
            escaping_fn_types: rustc_hash::FxHashSet::default(),
            mut_fn_types: rustc_hash::FxHashSet::default(),
            cross_task_fn_types: rustc_hash::FxHashSet::default(),
            paren_return_types: rustc_hash::FxHashSet::default(),
            container_hashers: rustc_hash::FxHashMap::default(),
            stmt_lint_overrides: rustc_hash::FxHashMap::default(),
            vec_macros: rustc_hash::FxHashSet::default(),
            comments: Vec::new(),
            next_comment: 0,
            line_comment_end: usize::MAX,
            stmt_start: false,
            in_head: false,
        }
    }

    pub(super) fn push_indent(&mut self) {
        self.indent += 1;
    }

    pub(super) fn pop_indent(&mut self) {
        self.indent -= 1;
    }

    pub(super) fn write_indent(&mut self) {
        for _ in 0..self.indent {
            self.output.push_str(INDENT);
        }
    }

    pub(super) fn writeln(&mut self, s: &str) {
        self.write_indent();
        self.output.push_str(s);
        self.output.push('\n');
    }

    pub(super) fn write_str(&mut self, s: &str) {
        self.output.push_str(s);
    }

    /// Emit an AST-level identifier name, prepending `r#` when the bare name
    /// would otherwise lex as a keyword or reserved-for-future-use word — i.e.
    /// when round-tripping requires the raw-identifier escape (design.md §
    /// Raw Identifiers). Structural markers (`self`/`Self`/`_`/etc.) are
    /// rejected at lex time and never reach the formatter as plain `name`s.
    pub(super) fn write_ident(&mut self, name: &str) {
        if needs_raw_escape(name) {
            self.output.push_str("r#");
        }
        self.output.push_str(name);
    }

    /// Emit a dotted path, escaping each segment independently.
    pub(super) fn write_path(&mut self, segments: &[String]) {
        for (i, seg) in segments.iter().enumerate() {
            if i > 0 {
                self.output.push('.');
            }
            self.write_ident(seg);
        }
    }

    /// Emit a `, `-separated list of identifiers, escaping each independently.
    pub(super) fn write_ident_list(&mut self, names: &[String]) {
        for (i, n) in names.iter().enumerate() {
            if i > 0 {
                self.output.push_str(", ");
            }
            self.write_ident(n);
        }
    }

    /// Emit the visibility keyword (`pub ` / `private ` / `""`) for items
    /// that carry the three-level `Visibility`.
    pub(super) fn write_visibility(&mut self, v: Visibility) {
        match v {
            Visibility::Pub => self.write_str("pub "),
            Visibility::Private => self.write_str("private "),
            Visibility::Default => {}
        }
    }

    // ── Program ─────────────────────────────────────────────────

    pub(super) fn format_program(&mut self, program: &Program) {
        for attr in &program.inner_attrs {
            self.flush_comments_before(attr.span.offset);
            self.format_attribute(attr, true);
        }
        if !program.inner_attrs.is_empty() && !program.items.is_empty() {
            self.output.push('\n');
        }
        // Sort: use / import decls, then rest (preserving relative order within categories)
        let mut uses = Vec::new();
        let mut rest = Vec::new();

        for item in &program.items {
            match item {
                Item::Import(_) => uses.push(item),
                _ => rest.push(item),
            }
        }

        // Sort import decls alphabetically by path, unless a comment sits
        // among them: it would stay where it is while the imports moved.
        let import_lo = uses.iter().map(|i| i.span().offset).min().unwrap_or(0);
        let import_hi = uses
            .iter()
            .map(|i| i.span().offset + i.span().length)
            .max()
            .unwrap_or(0);
        let keep_order = self.has_comment_in(import_lo, import_hi);
        if !keep_order {
            uses.sort_by(|a, b| {
                let path_a = match a {
                    Item::Import(i) => {
                        let mut p = i.path.clone();
                        if let Some(first) = i.items.first() {
                            p.push(first.name.clone());
                        }
                        p
                    }
                    _ => unreachable!(),
                };
                let path_b = match b {
                    Item::Import(i) => {
                        let mut p = i.path.clone();
                        if let Some(first) = i.items.first() {
                            p.push(first.name.clone());
                        }
                        p
                    }
                    _ => unreachable!(),
                };
                path_a.cmp(&path_b)
            });
        }

        for item in &uses {
            self.flush_comments_before(item.span().offset);
            self.format_item(item);
        }
        if !uses.is_empty() && !rest.is_empty() {
            self.flush_trailing_comments(rest[0].span().offset);
            self.output.push('\n');
        }

        // A script's statements go back among the items, in file order,
        // and the `main` the parser wrapped them in is not written
        // (design.md § Script mode, B-2026-11-04-36).
        let mut pieces: Vec<(usize, Result<&Item, &Stmt>)> = Vec::new();
        for (i, item) in rest.iter().enumerate() {
            match item {
                Item::Function(f) if program.script_main && i + 1 == rest.len() => {
                    pieces.extend(f.body.stmts.iter().map(|s| (s.span.offset, Err(s))));
                }
                _ => pieces.push((item.span().offset, Ok(*item))),
            }
        }
        if program.script_main {
            pieces.sort_by_key(|(at, _)| *at);
        }
        let mut prev_stmt = false;
        for (n, (at, piece)) in pieces.iter().enumerate() {
            let is_stmt = piece.is_err();
            // Consecutive statements share a paragraph; anything next to an
            // item is set apart by a blank line.
            if n > 0 && !(is_stmt && prev_stmt) {
                self.flush_trailing_comments(*at);
                self.output.push('\n');
            }
            prev_stmt = is_stmt;
            self.flush_comments_before(*at);
            match piece {
                Ok(item) => self.format_item(item),
                Err(stmt) => self.format_top_level_stmt(stmt),
            }
        }
        self.flush_comments_before(usize::MAX);
        while self.output.ends_with("\n\n") {
            self.output.pop();
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────

/// String-returning equivalent of `Formatter::write_ident`. Used when the
/// caller is composing output via `format!(...)` and can't take `&mut self`.
pub(super) fn ident_str(name: &str) -> String {
    if needs_raw_escape(name) {
        format!("r#{name}")
    } else {
        name.to_string()
    }
}

/// String-returning equivalent of `Formatter::write_path`.
pub(super) fn path_str(segments: &[String]) -> String {
    segments
        .iter()
        .map(|s| ident_str(s))
        .collect::<Vec<_>>()
        .join(".")
}

/// True iff emitting `name` bare would lex as a keyword / reserved-future-use
/// word (i.e. anything other than `Token::Identifier`). The list mirrors the
/// keyword table in `src/lexer.rs::identifier()` plus the reserved-future-use
/// set; structural markers (`self`/`Self`/`_`/...) are excluded because they
/// cannot reach the formatter as a plain `name` — the lexer rejects raw
/// escapes for them.
pub(super) fn needs_raw_escape(name: &str) -> bool {
    // A structural marker (`own`, ...) cannot take `r#`; where one appears
    // as a name the parser reads it bare.
    crate::token::is_raw_escapable(name)
        && matches!(
            name,
            // Declarations
            "fn" | "struct" | "enum" | "trait" | "impl" | "mod" | "use" | "import"
        | "const" | "type" | "distinct"
        // Visibility
        | "pub" | "private"
        // Control flow
        | "if" | "else" | "match" | "while" | "for" | "in" | "loop"
        | "return" | "break" | "continue"
        | "defer" | "errdefer" | "asm" | "global_asm"
        // Bindings
        | "let" | "mut"
        // Logical (keyword forms)
        | "and" | "or" | "not"
        // Ownership
        | "own" | "ref" | "weak" | "lock" | "move"
        // Effects
        | "effect" | "resource" | "verb"
        | "reads" | "writes" | "sends" | "receives" | "allocates" | "panics"
        | "blocks" | "suspends"
        | "with" | "transparent" | "stable" | "seq" | "par" | "yield"
        // Type system
        | "as" | "where" | "dyn"
        // Contracts
        | "requires" | "ensures" | "invariant"
        // Safety
        | "unsafe" | "extern"
        // Shared / layout
        | "shared" | "layout" | "group"
        // Comptime
        | "comptime"
        // Bool literals
        | "true" | "false"
        // Providers / misc
        | "providers" | "alias" | "independent"
        // Reserved-for-future-use numeric types
        | "f16" | "bf16"
        // Reserved-for-future-use keywords
        | "gen" | "become" | "do" | "final" | "override" | "priv" | "try"
        | "typeof" | "virtual" | "async" | "await" | "pure" | "box"
        )
}

pub(super) fn format_effect_verb_kind(v: &EffectVerbKind) -> String {
    // Single source of truth for the verb→keyword spelling (`effect_render`).
    crate::effect_render::verb_keyword(v).to_string()
}

/// An integer literal as the source wrote it. An unsigned one past the
/// signed carrier's top rides it as the wrapped bit pattern (`u128::MAX` is
/// `-1`), so it is printed as the unsigned value, not the carrier.
pub(super) fn int_literal(n: i128, sfx: Option<IntSuffix>) -> String {
    use IntSuffix::*;
    let digits = match sfx {
        Some(U128) if n < 0 => (n as u128).to_string(),
        Some(U8 | U16 | U32 | U64 | Usize) if n < 0 => (n as i64 as u64).to_string(),
        _ => n.to_string(),
    };
    digits + sfx.map_or("", int_suffix_str)
}

pub(super) fn int_suffix_str(s: IntSuffix) -> &'static str {
    match s {
        IntSuffix::I8 => "i8",
        IntSuffix::I16 => "i16",
        IntSuffix::I32 => "i32",
        IntSuffix::I64 => "i64",
        IntSuffix::I128 => "i128",
        IntSuffix::U8 => "u8",
        IntSuffix::U16 => "u16",
        IntSuffix::U32 => "u32",
        IntSuffix::U64 => "u64",
        IntSuffix::U128 => "u128",
        IntSuffix::Usize => "usize",
        IntSuffix::Isize => "isize",
    }
}

pub(super) fn float_suffix_str(s: FloatSuffix) -> &'static str {
    match s {
        FloatSuffix::F16 => "f16",
        FloatSuffix::BF16 => "bf16",
        FloatSuffix::F32 => "f32",
        FloatSuffix::F64 => "f64",
    }
}

pub(super) fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            c => push_escaped(&mut out, c),
        }
    }
    out
}

/// A char literal's body: `'` is the character escaped, not `"`.
pub(super) fn escape_char(c: char) -> String {
    let mut out = String::new();
    match c {
        '\'' => out.push_str("\\'"),
        c => push_escaped(&mut out, c),
    }
    out
}

/// One character of a string or char literal body, in a form the lexer reads
/// back as that character: the named escapes, `\u{..}` for any other control
/// character, and the character itself otherwise.
fn push_escaped(out: &mut String, c: char) {
    match c {
        '\\' => out.push_str("\\\\"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        '\0' => out.push_str("\\0"),
        c if c.is_control() => {
            use std::fmt::Write as _;
            let _ = write!(out, "\\u{{{:X}}}", c as u32);
        }
        c => out.push(c),
    }
}

#[cfg(test)]
mod tests {
    use super::format_program;
    use crate::parse;

    fn fmt_ok(source: &str) -> String {
        let result = parse(source);
        assert!(
            result.errors.is_empty(),
            "parse errors: {:?}",
            result.errors
        );
        format_program(&result.program)
    }

    #[test]
    fn no_effect_attribute_keeps_its_verbs() {
        // The verbs live in `Attribute::effect_args`, not `args`, because they
        // are reserved keywords. A formatter that renders only `args` emitted a
        // bare `#[no_effect]` — silently DROPPING a safety declaration, and
        // producing source that no longer parses (an empty list is
        // `E_NO_EFFECT_EMPTY`). Formatting must never be able to weaken a
        // guarantee, so this pins the round-trip.
        let out = fmt_ok("#[no_effect(allocates(Heap), panics, reads(Config))]\nfn f() { }");
        assert!(
            out.contains("#[no_effect(allocates(Heap), panics, reads(Config))]"),
            "verbs must survive formatting:\n{out}"
        );
        // A resource-less verb renders bare. `panics()` would not re-parse:
        // `try_parse_effect_verb` returns before looking for `(`, so the empty
        // parens would be left over.
        assert!(!out.contains("panics()"), "no empty parens:\n{out}");
        // Idempotent, and the output is still parseable — the property the
        // dropped-verb bug broke.
        let again = fmt_ok(&out);
        assert_eq!(out, again, "second format pass must be a fixpoint");
    }

    #[test]
    fn fn_type_keeps_its_case_once_ness_and_effect_row() {
        // B-2026-08-24-14, and the same class as
        // `no_effect_attribute_keeps_its_verbs` above: formatting must never
        // be able to weaken a guarantee. Three things were dropped by one
        // arm — the head printed lowercase `fn(`, which is not the surface
        // syntax and does not parse in type position; `is_once` was matched
        // away, so an `OnceFn` printed as a plain `Fn` and lost
        // once-callability; and `effect_spec` was matched away, so
        // `Fn(i64) -> i64 with writes(D)` printed as a PURE slot.
        //
        // The unparseable head is the only reason the two silent losses were
        // not silent corruption: `karac fmt` output failed at the next parse
        // instead of quietly changing two types.
        let out = fmt_ok(
            "effect resource D;\n\
             fn f(a: Fn(i64) -> i64 with writes(D), b: OnceFn(i64) -> i64, \
             c: Fn(i64) -> i64 with _) -> i64 { 0 }",
        );
        assert!(
            out.contains("Fn(i64) -> i64 with writes(D)"),
            "the effect row must survive formatting:\n{out}"
        );
        assert!(
            out.contains("OnceFn(i64) -> i64"),
            "once-callability must survive formatting:\n{out}"
        );
        assert!(
            out.contains("with _"),
            "the polymorphic row must survive formatting:\n{out}"
        );
        assert!(
            !out.contains("fn(i64)"),
            "the head is `Fn`/`OnceFn`; lowercase `fn(` does not parse in \
             type position:\n{out}"
        );
        // Parseable and a fixpoint — the two properties the bug broke.
        let again = fmt_ok(&out);
        assert_eq!(out, again, "second format pass must be a fixpoint");
    }

    #[test]
    fn multi_bound_provider_declaration_keeps_every_bound() {
        // `effect resource UserDB: Store[Row] + Healthy;` (design.md:7216,
        // B-2026-08-19-3). A formatter that renders only the first bound
        // rewrites the declaration into a WEAKER contract — one the
        // typechecker no longer requires the provider to satisfy — which is
        // the same class of silent meaning-change as the dropped generic
        // arguments below, not a cosmetic loss.
        let out = fmt_ok("effect resource UserDB: Store[Row] + Healthy;\n");
        assert!(
            out.contains("Store[Row] + Healthy"),
            "every bound must survive formatting:\n{out}"
        );
        let again = fmt_ok(&out);
        assert_eq!(out, again, "second format pass must be a fixpoint");
    }

    #[test]
    fn generic_provider_bound_keeps_its_arguments() {
        // `effect resource C: Provider[Request];` — the bound's generic
        // arguments live in `provider_trait_args`, not in the trait NAME, so a
        // formatter that renders only the name silently rewrote the declaration
        // into a different one (B-2026-08-18-41). Same class as the
        // dropped-`no_effect`-verbs case above: formatting must never be able
        // to change what a declaration means.
        //
        // For a GENERIC provider trait the loss is not even cosmetic — the
        // argument is the only thing that binds the trait's type parameter, so
        // the reformatted source stops typechecking at every call site.
        let out = fmt_ok("effect resource RequestCh: Channel[Request];");
        assert!(
            out.contains("effect resource RequestCh: Channel[Request];"),
            "the bound's arguments must survive formatting:\n{out}"
        );
        let again = fmt_ok(&out);
        assert_eq!(out, again, "second format pass must be a fixpoint");

        // A plain bound gains no empty bracket pair — `Channel[]` would not
        // re-parse.
        let plain = fmt_ok("effect resource UserDB: DatabaseProvider;");
        assert!(
            plain.contains("effect resource UserDB: DatabaseProvider;"),
            "a plain bound must render unchanged:\n{plain}"
        );
        assert!(!plain.contains("[]"), "no empty argument list:\n{plain}");
    }

    #[test]
    fn enum_explicit_discriminants_roundtrip() {
        // `= VALUE` is emitted after each payload with a single space on either
        // side of `=` (design.md § Explicit Discriminants on Payload Variants);
        // a variant with no explicit value emits none. Idempotent under a second
        // format pass.
        let out = fmt_ok(
            "#[repr(u8)] enum Op { Reset = 1, Connect { addr: u32 } = 5, Send(u32) = 6, Plain }",
        );
        assert!(out.contains("Reset = 1,"), "unit `= N`:\n{out}");
        assert!(out.contains("} = 5,"), "struct `= N`:\n{out}");
        assert!(out.contains("Send(u32) = 6,"), "tuple `= N`:\n{out}");
        assert!(out.contains("Plain,"), "no-discriminant variant:\n{out}");
        assert!(
            !out.contains("Plain ="),
            "Plain must carry no `= N`:\n{out}"
        );
        assert_eq!(out, fmt_ok(&out), "format must be idempotent:\n{out}");
    }

    #[test]
    fn closure_ref_capture_mode_prefix_roundtrips() {
        let out = fmt_ok("fn main() { let f = ref |x| x + 1; }");
        assert!(
            out.contains("ref |x|"),
            "expected `ref |x|` in formatted output, got:\n{out}"
        );
    }

    #[test]
    fn closure_mut_ref_capture_mode_prefix_roundtrips() {
        let out = fmt_ok("fn main() { let f = mut ref |x| x + 1; }");
        assert!(
            out.contains("mut ref |x|"),
            "expected `mut ref |x|` in formatted output, got:\n{out}"
        );
    }

    #[test]
    fn closure_no_prefix_does_not_emit_capture_mode() {
        let out = fmt_ok("fn main() { let f = |x| x + 1; }");
        assert!(
            !out.contains("ref |") && !out.contains("mut ref |"),
            "bare closure must not emit a capture-mode prefix, got:\n{out}"
        );
    }
}
