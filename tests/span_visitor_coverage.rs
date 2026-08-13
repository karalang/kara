//! Source-level audit of `src/span_visitor.rs`'s coverage of the AST.
//!
//! B-2026-08-11-35 found that `CallArg::mut_marker_span` was absent from the
//! walker, so it kept its wrapper-relative offset while every span around it
//! was rebased — and `karac fix` then computed a 77-byte deletion at the wrong
//! place and ATE THE FILE. The audit that found it was mechanical: for every
//! `pub <field>: Span | Option<Span>` in `src/ast/*.rs`, check the field name
//! appears in BOTH halves of the walker. It turned up SEVEN unvisited fields.
//! B-2026-08-12-30 was the seventh — an entire missing subtree rather than a
//! field — and its row asks for the audit to be kept as a test. This is it.
//!
//! WHY A SOURCE-TEXT TEST RATHER THAN A BEHAVIOURAL ONE. The failure is a
//! field that nobody wrote a line for, so there is no behaviour to exercise
//! until a consumer is built on that span — by which time it corrupts source.
//! Grepping the walker is a direct check of the property that actually
//! matters: every span the AST can mint is reachable from both walks. It costs
//! nothing and it fires the moment a new `Span` field lands without a visitor
//! line, which is exactly when it is cheap to fix.
//!
//! IT IS A NAME CHECK, and that is a deliberate limit worth stating: it proves
//! the field is MENTIONED in each half, not that it is mentioned correctly. A
//! typo'd GEP or a visit of the wrong sub-span passes. It is a floor against
//! the omission class, not a proof of correctness.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn ast_sources() -> Vec<String> {
    let mut out = vec![std::fs::read_to_string(repo("src/ast.rs")).expect("src/ast.rs")];
    let dir = repo("src/ast");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("src/ast/")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "no src/ast/*.rs found — audit would vacuously pass"
    );
    for p in paths {
        out.push(std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())));
    }
    out
}

/// Every `pub <name>: Span` / `pub <name>: Option<Span>` field name declared
/// anywhere in the AST, minus the ubiquitous `span` itself (which every walker
/// arm visits by construction and which would drown the signal).
fn declared_span_fields() -> BTreeSet<String> {
    let mut fields = BTreeSet::new();
    for src in ast_sources() {
        for line in src.lines() {
            let t = line.trim();
            let Some(rest) = t.strip_prefix("pub ") else {
                continue;
            };
            let Some((name, ty)) = rest.split_once(':') else {
                continue;
            };
            let name = name.trim();
            let ty = ty.trim().trim_end_matches(',').trim();
            if (ty == "Span" || ty == "Option<Span>") && name != "span" {
                fields.insert(name.to_string());
            }
        }
    }
    assert!(
        fields.len() >= 5,
        "extracted only {} span fields — the parse is probably broken, which \
         would make this audit vacuous",
        fields.len()
    );
    fields
}

/// The walker split into `(fn_name, body)` pairs. The two "halves" are
/// distinguished by name: everything reachable from `visit_item_spans_mut`
/// lives in a `*_mut` function, everything reachable from `visit_item_spans`
/// does not. Splitting on the file offset of `visit_item_spans_mut` does NOT
/// work — several `_spans_mut` helpers are defined above it.
fn walker_fns() -> Vec<(String, String)> {
    let src = std::fs::read_to_string(repo("src/span_visitor.rs")).expect("src/span_visitor.rs");
    let mut starts: Vec<usize> = Vec::new();
    for (i, line) in src.match_indices('\n') {
        let after = &src[i + 1..];
        if after.starts_with("fn ") || after.starts_with("pub fn ") {
            starts.push(i + 1);
        }
        let _ = line;
    }
    let mut out = Vec::new();
    for (k, &pos) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(src.len());
        let body = &src[pos..end];
        let name = body
            .trim_start_matches("pub ")
            .trim_start_matches("fn ")
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .next()
            .unwrap_or("")
            .to_string();
        out.push((name, body.to_string()));
    }
    assert!(
        out.len() > 20,
        "found only {} fns in span_visitor.rs — the split is broken",
        out.len()
    );
    out
}

fn covered(fns: &[(String, String)], needle: &str) -> (bool, bool) {
    let ro = fns
        .iter()
        .any(|(n, b)| !n.contains("mut") && b.contains(needle));
    let mu = fns
        .iter()
        .any(|(n, b)| n.contains("mut") && b.contains(needle));
    (ro, mu)
}

/// B-2026-08-11-35 / B-2026-08-12-30 — every auxiliary `Span` field the AST
/// declares must be named in BOTH halves of the walker.
///
/// A field in one half and not the other is worse than a field in neither: the
/// read-only walk decides which spans get a module attributed to them and the
/// mut walk decides which get REBASED, so a one-sided field is rebased without
/// being attributed, or attributed at an offset that has moved.
#[test]
fn every_ast_span_field_is_visited_by_both_walker_halves() {
    let fns = walker_fns();
    let mut missing: Vec<String> = Vec::new();
    for field in declared_span_fields() {
        let (ro, mu) = covered(&fns, &field);
        if !(ro && mu) {
            missing.push(format!(
                "{field} (read-only: {}, mut: {})",
                if ro { "yes" } else { "MISSING" },
                if mu { "yes" } else { "MISSING" }
            ));
        }
    }
    assert!(
        missing.is_empty(),
        "AST span fields not reachable from both halves of src/span_visitor.rs:\n  {}\n\n\
         A span the walk misses keeps its FILE-LOCAL offset under `module.rs`'s \
         multi-module rebase while every span around it shifts. That is silent \
         until something is built on it — B-2026-08-11-35 was `karac fix` \
         computing an edit range from one such span and deleting 77 bytes of \
         unrelated source while reporting success.\n\
         Add the field to `visit_item_spans` AND `visit_item_spans_mut` (or to \
         a helper each reaches).",
        missing.join("\n  ")
    );
}

/// B-2026-08-12-30 — the SUBTREES, which is the shape the field check alone
/// cannot express. Generic parameters, trait bounds and where-clauses were
/// absent from the walker entirely: not a missing line but a missing
/// traversal, so no individual field name would have flagged them until one of
/// their spans was declared with a distinctive name (only `variance_span` was).
#[test]
fn generics_bounds_and_where_clauses_are_reachable_from_both_halves() {
    let fns = walker_fns();
    let mut missing: Vec<&str> = Vec::new();
    // The four carrier field names, plus the two types that only appear once a
    // real traversal exists — naming `TraitBound` / `WhereClause` is what
    // distinguishes "walks the subtree" from "happens to mention the field".
    for needle in [
        "generic_params",
        "bounds",
        "where_clause",
        "supertraits",
        "TraitBound",
        "WhereClause",
        "GenericParams",
    ] {
        let (ro, mu) = covered(&fns, needle);
        if !(ro && mu) {
            missing.push(needle);
        }
    }
    assert!(
        missing.is_empty(),
        "generics subtree not reachable from both walker halves: {missing:?}\n\
         See B-2026-08-12-30 — `visit_generics` / `visit_generics_mut` must be \
         called from every generics-carrying item arm.",
    );
}

/// B-2026-08-12-30 — the BEHAVIOURAL half, and the one that actually proves
/// the property `module.rs` depends on. The two audits above are name checks;
/// this one shifts every span through the mut walk and asserts that nothing
/// reachable from the READ-ONLY walk was left behind at its file-local offset.
///
/// That asymmetry is the real failure mode. A field visited by one half and
/// not the other is worse than a field in neither: the read-only walk is what
/// attributes a span to a module, so a span it can see but the mut walk cannot
/// move is precisely a span that gets attributed at an offset that has already
/// shifted underneath it — `module.rs`'s "a span this walk MISSES stays at its
/// file-local offset and can still collide", made executable.
///
/// The fixture is deliberately generics-dense: variance markers, const
/// generics, multi-bound params with generic args on the bounds, supertraits,
/// an associated type with its own bound, and all three where-constraint
/// shapes that carry spans.
#[test]
fn mut_walk_shifts_every_span_the_read_walk_can_see() {
    const SHIFT: usize = 1_000_000;
    let src = r#"
trait Shown: Base + Other[i64] {
    type Item: Base;
    fn show[U: Base](self, u: U) -> String where U: Other[i64];
}
struct Holder[+T: Base + Other[i64], const N: i64] where T: Other[i64] {
    items: Vec[T],
}
enum Choice[=E: Base] where E: Other[i64] { Yes(E), No }
fn run[T: Base, const K: i64](t: T) -> i64 where T: Other[i64] { return K; }
impl[T: Base] Holder[T, 2] where T: Other[i64] {
    type Item = T;
    fn len(ref self) -> i64 { return 0; }
}
type Alias[T: Base] = Vec[T];
distinct type Id[T: Base] = i64;
"#;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "fixture must parse or the walk sees a recovery stub instead of \
         generics: {:?}",
        parsed.errors
    );
    assert!(
        parsed.program.items.len() >= 7,
        "fixture lost items to recovery: {} parsed",
        parsed.program.items.len()
    );

    // Baseline: how many distinct spans the read-only walk can see at all.
    let mut before = 0usize;
    for it in &parsed.program.items {
        karac::span_visitor::visit_item_spans(it, &mut |_| before += 1);
    }
    assert!(
        before > 50,
        "fixture is too thin to be a real check: {before}"
    );

    for it in &mut parsed.program.items {
        karac::span_visitor::visit_item_spans_mut(it, &mut |s| s.offset += SHIFT);
    }

    // Every span the read-only walk reaches must have moved. A span still
    // below the shift is one the mut walk cannot see.
    let mut stragglers: Vec<(usize, usize)> = Vec::new();
    for it in &parsed.program.items {
        karac::span_visitor::visit_item_spans(it, &mut |s| {
            if s.offset < SHIFT {
                stragglers.push((s.offset, s.length));
            }
        });
    }
    assert!(
        stragglers.is_empty(),
        "{} span(s) visible to `visit_item_spans` were NOT moved by \
         `visit_item_spans_mut` — they keep their file-local offset under the \
         multi-module rebase: {:?}",
        stragglers.len(),
        stragglers
    );
}
