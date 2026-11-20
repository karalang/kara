//! The lsp7 feature sweep over corpus/core and corpus/apps
//! (probe-fuzzers/lsp7/sweep.py in the shared folder): each mismatch class
//! it found, pinned on a small program.

use kara_lsp::analysis;
use lsp_types::{HoverContents, Position};

/// The `(line, column)` of byte `off` in `src` (ASCII sources).
fn pos(src: &str, off: usize) -> Position {
    let before = &src[..off];
    let line = before.matches('\n').count() as u32;
    let col = (off - before.rfind('\n').map_or(0, |i| i + 1)) as u32;
    Position::new(line, col)
}

/// The offset of the `k`th occurrence of `needle`, plus `delta`.
fn at(src: &str, needle: &str, k: usize, delta: usize) -> Position {
    let off = src
        .match_indices(needle)
        .nth(k)
        .unwrap_or_else(|| panic!("no {needle}"))
        .0;
    pos(src, off + delta)
}

fn hover_text(src: &str, p: Position) -> Option<String> {
    match analysis::hover(src, p)?.contents {
        HoverContents::Markup(m) => Some(m.value),
        other => panic!("{other:?}"),
    }
}

/// B-2026-11-20-55: a call with fewer arguments than a function's
/// positional parameters, where the function also has named ones
/// (`log.init()`), indexed past the arguments in the named-call repair:
/// `karac check` aborted, and so did kara-lsp, whose release build's
/// `panic = "abort"` voided its `catch_unwind` guards. Signature help
/// inside `log.init(` checks exactly that call (the server's guard keeps a
/// dev build alive either way; the release one now unwinds too).
#[test]
fn feature_sweep_a_short_call_to_a_named_parameter_fn_is_diagnosed() {
    let src = "import std.log;\nfn main() {\n    log.init();\n}\n";
    let errs: Vec<_> = karac::check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .collect();
    assert!(!errs.is_empty(), "a missing argument is an error");
    let src = "import std.log;\nfn main() {\n    log.init(log.text_backend(log.Stream.Stdout), level: log.Level.Debug);\n}\n";
    let errs: Vec<_> = karac::check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .collect();
    assert!(errs.is_empty(), "{errs:#?}");
}

/// B-2026-11-20-56: a binding inside a tuple-variant pattern or a
/// closure parameter inside a struct literal went to the enclosing enum or
/// struct, whose resolution spans the whole pattern or literal.
#[test]
fn feature_sweep_a_binding_inside_a_pattern_or_literal_is_its_own() {
    let src = "enum E { A(i64), B }
struct S { f: Fn(i64) -> i64 }
fn pick(e: E) -> i64 { match e { E.A(r) => r + 1, E.B => 0 } }
fn main() {
    let s = S { f: |x| x + 1 };
    match Some(3) { Some(w) if w > 1 => println(w), _ => {} }
    println(pick(E.B) + (s.f)(1));
}
";
    let binding = at(src, "E.A(r)", 0, 4);
    let names: Vec<(u32, u32)> = analysis::references(src, binding, true)
        .into_iter()
        .map(|r| (r.start.line, r.start.character))
        .collect();
    let use_r = at(src, "r + 1", 0, 0);
    assert!(
        names.contains(&(binding.line, binding.character))
            && names.contains(&(use_r.line, use_r.character)),
        "{names:?}"
    );
    let def = analysis::definition(src, use_r).expect("a definition");
    assert_eq!(def.start, binding);

    let w = at(src, "Some(w)", 0, 5);
    let refs = analysis::references(src, w, true);
    assert!(
        refs.len() >= 3,
        "the binding, the guard and the arm: {refs:?}"
    );
    assert!(refs
        .iter()
        .all(|r| r.end.character - r.start.character == 1));

    let x = at(src, "|x|", 0, 1);
    let refs = analysis::references(src, x, true);
    assert_eq!(refs.len(), 2, "{refs:?}");
    if let Some(d) = analysis::definition(src, at(src, "x + 1", 0, 0)) {
        assert_eq!(d.start, x);
    }
}

/// B-2026-11-20-57: a document symbol's selection range was its whole
/// item, where the protocol asks for the name.
#[test]
fn feature_sweep_a_symbol_selects_its_name() {
    let src = "#[derive(Debug)]\nstruct Point { x: i64 }\nfn area() -> i64 { 0 }\nenum Color { Red }\ntrait Tr { fn t(self) -> i64; }\n";
    let syms = analysis::document_symbols(src);
    assert_eq!(syms.len(), 4, "{syms:#?}");
    for s in &syms {
        let r = s.selection_range;
        assert_eq!(r.start.line, r.end.line);
        let line = src.lines().nth(r.start.line as usize).unwrap();
        let text = &line[r.start.character as usize..r.end.character as usize];
        assert_eq!(text, s.name);
        assert!(s.selection_range.start >= s.range.start && s.selection_range.end <= s.range.end);
    }
}

/// B-2026-11-20-58: hover on `let _ = ..` gave the enclosing
/// block's type, and on a closure that writes a capture `Fn(..)`, where it
/// is a `MutFn` (§9.6); either written back as the binding's type was
/// refused.
#[test]
fn feature_sweep_hover_wildcard_and_mut_fn() {
    let src = "fn main() {
    let mut total = 0;
    let add = |n: i64| { total += n; };
    let get = |n: i64| n + 1;
    let _ = get(2);
    add(1);
    println(total);
}
";
    assert_eq!(hover_text(src, at(src, "let _", 0, 4)), None);
    let add = hover_text(src, at(src, "add =", 0, 0)).expect("hover");
    assert!(add.contains("MutFn(i64)"), "{add}");
    let get = hover_text(src, at(src, "get =", 0, 0)).expect("hover");
    assert!(
        get.contains("Fn(i64) -> i64") && !get.contains("MutFn"),
        "{get}"
    );
}

/// B-2026-11-20-59: signature help gave nothing for a library type's
/// method or function the four collection pages do not list (`tx.send`,
/// `Duration.ms`), a number type's `parse`, a library module's function
/// (`sleep`, `log.init`), and a call of a function-typed value.
#[test]
fn feature_sweep_signature_help_reaches_the_library_and_function_values() {
    let src = "import std.time.{sleep, Duration};
import std.log;
fn main() {
    let (tx, rx) = channel(2);
    let _ = tx.send(1);
    sleep(Duration.ms(1));
    let n = i64.parse(\"12\").unwrap_or(0);
    log.init(log.text_backend(log.Stream.Stdout));
    let f = |x: i64| x + 1;
    println(f\"{n} {rx.recv()} {f(2)}\");
}
";
    for (needle, want) in [
        ("send(", "fn send(self, value: own T)"),
        ("ms(", "fn ms(ms: i64) -> Duration"),
        ("sleep(", "fn sleep(d: Duration)"),
        ("parse(", "fn parse(s: Str) -> Result[i64, ParseError]"),
        ("init(", "fn init[B: Backend](backend: own B"),
        ("text_backend(", "fn text_backend("),
        ("f(2", "f: Fn(i64) -> i64"),
    ] {
        let p = at(src, needle, 0, needle.find('(').unwrap() + 1);
        let help = analysis::signature_help(src, p)
            .unwrap_or_else(|| panic!("no signature help in {needle}"));
        let label = &help.signatures[0].label;
        assert!(label.starts_with(want), "{needle}: {label}");
    }
}

/// B-2026-11-20-56: a field's name in a struct literal went to the
/// struct (whose resolution spans the literal); once a binding inside it
/// stopped going there, the field's name needs its own answer: the field's
/// declaration.
#[test]
fn feature_sweep_a_field_name_in_a_literal_goes_to_the_field() {
    let src =
        "struct R { id: i64 }\nfn main() {\n    let r = R { id: 7 };\n    println(r.id);\n}\n";
    let def = analysis::definition(src, at(src, "id: 7", 0, 0)).expect("a definition");
    assert_eq!(def.start, at(src, "id: i64", 0, 0));
}

/// B-2026-11-20-57: a library function the buffer imports was an
/// outline entry, with its library file's span.
#[test]
fn feature_sweep_the_outline_holds_the_buffer_s_items_only() {
    let src = "import std.io.{read_to_string};\nfn main() {\n    println(read_to_string(\"x\").unwrap_or(\"\".to_string()));\n}\n";
    let names: Vec<String> = analysis::document_symbols(src)
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["main"]);
}

/// B-2026-11-20-58: a program's type named like one the compiler
/// spells (`Field`, a reflection record) is checked under a name of the
/// same length (`Fiel_`), and hover showed that name.
#[test]
fn feature_sweep_hover_names_a_renamed_type_as_written() {
    let src = "struct Field { kind: i64 }\nfn main() {\n    let f = Field { kind: 1 };\n    let v = vec![f];\n    println(v.len());\n}\n";
    let f = hover_text(src, at(src, "let f", 0, 4)).expect("hover");
    assert!(f.contains("Field") && !f.contains("Fiel_"), "{f}");
    let v = hover_text(src, at(src, "let v", 0, 4)).expect("hover");
    assert!(v.contains("Vec[Field]"), "{v}");
}

/// B-2026-11-20-60: a binding destructured from a place the function does
/// not own (`let (r, k) = h.pe` with `h` a bare parameter), and a `let`
/// that copies it, hovered `R` where it is a `ref R`; `let m: R = r` is
/// refused (E0313).
#[test]
fn feature_sweep_hover_a_view_binding_is_a_ref() {
    let src = "struct R { name: String }
struct H { pe: (R, i64) }
fn hh(h: H) -> i64 {
    let (r, k) = h.pe;
    let m = r;
    m.name.len() + k
}
fn main() {
    let r = R { name: \"abc\".to_string() };
    let x = H { pe: (r, 100) };
    println(hh(x));
}
";
    for needle in ["(r, k)", "m = r"] {
        let h = hover_text(
            src,
            at(src, needle, 0, usize::from(needle.starts_with('('))),
        )
        .expect("hover");
        assert!(h.contains("ref R"), "{needle}: {h}");
    }
    let r = hover_text(src, at(src, "let r", 0, 4)).expect("hover");
    assert!(r.contains("\nR\n") && !r.contains("ref"), "{r}");
}
