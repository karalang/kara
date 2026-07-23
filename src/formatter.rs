//! Canonical formatter for Kāra source code.
//!
//! Parses AST and reprints in deterministic canonical form.
//! Scope: syntactic canonicalization (like gofmt/rustfmt), not semantic normalization.

use crate::ast::*;
use crate::token::{FloatSuffix, IntSuffix};

const INDENT: &str = "    ";

mod exprs;
mod items;
mod patterns;
mod stmts;
mod types;

pub fn format_program(program: &Program) -> String {
    let mut f = Formatter::new();
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
    let mut f = Formatter::new();
    let mut first = true;
    for item in &effects.items {
        if !first {
            f.write_str(" ");
        }
        first = false;
        match item {
            EffectItem::Verb(v) => {
                f.write_str(&format_effect_verb_kind(&v.kind));
                f.write_str("(");
                for (j, r) in v.resources.iter().enumerate() {
                    if j > 0 {
                        f.write_str(", ");
                    }
                    f.write_path(&r.path);
                }
                f.write_str(")");
            }
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

pub(super) struct Formatter {
    pub(super) output: String,
    pub(super) indent: usize,
}

impl Formatter {
    pub(super) fn new() -> Self {
        Formatter {
            output: String::new(),
            indent: 0,
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
        // Sort: use / import decls, then rest (preserving relative order within categories)
        let mut uses = Vec::new();
        let mut rest = Vec::new();

        for item in &program.items {
            match item {
                Item::UseDecl(_) | Item::Import(_) => uses.push(item),
                _ => rest.push(item),
            }
        }

        // Sort use / import decls alphabetically by path.
        uses.sort_by(|a, b| {
            let path_a = match a {
                Item::UseDecl(u) => u.path.clone(),
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
                Item::UseDecl(u) => u.path.clone(),
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

        for item in &uses {
            self.format_item(item);
        }
        if !uses.is_empty() && !rest.is_empty() {
            self.output.push('\n');
        }

        let mut first = true;
        for item in &rest {
            if !first {
                self.output.push('\n');
            }
            first = false;
            self.format_item(item);
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
    matches!(
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

pub(super) fn impl_item_name(item: &ImplItem) -> &str {
    match item {
        ImplItem::Method(m) => &m.name,
        ImplItem::AssocType(a) => &a.name,
    }
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
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
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
