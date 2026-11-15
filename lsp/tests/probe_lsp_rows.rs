//! The probe's kara-lsp rows: signature help in `println` showed the
//! buffer's first function (LS1, B-2026-11-15-121) and found nothing inside
//! an f-string's `{..}` (LS5, B-2026-11-15-125); renaming a struct field
//! missed its accesses (LS2, B-2026-11-15-122); rename took a keyword and
//! gave no reason for a non-name (LS3, B-2026-11-15-123); and references to
//! a struct gave a literal's whole range (LS4, B-2026-11-15-124).

use kara_lsp::analysis;
use lsp_types::Position;

fn at(src: &str) -> (String, Position) {
    let i = src.find('|').expect("a cursor");
    let text = format!("{}{}", &src[..i], &src[i + 1..]);
    let line = src[..i].matches('\n').count() as u32;
    let col = (i - src[..i].rfind('\n').map_or(0, |n| n + 1)) as u32;
    (text, Position::new(line, col))
}

fn label(src: &str) -> Option<String> {
    let (text, pos) = at(src);
    analysis::signature_help(&text, pos).map(|h| h.signatures[0].label.clone())
}

fn rename(src: &str, new_name: &str) -> Result<Option<String>, String> {
    let (text, pos) = at(src);
    let Some(edits) = analysis::rename(&text, pos, new_name)? else {
        return Ok(None);
    };
    let index = analysis::LineIndex::new(&text);
    let mut spans: Vec<(usize, usize, String)> = edits
        .iter()
        .map(|e| {
            (
                index.offset(e.range.start),
                index.offset(e.range.end),
                e.new_text.clone(),
            )
        })
        .collect();
    spans.sort();
    let mut out = text.clone();
    for (s, e, t) in spans.into_iter().rev() {
        out.replace_range(s..e, &t);
    }
    Ok(Some(out))
}

fn refs(src: &str) -> Vec<String> {
    let (text, pos) = at(src);
    let index = analysis::LineIndex::new(&text);
    analysis::references(&text, pos, false)
        .into_iter()
        .map(|r| text[index.offset(r.start)..index.offset(r.end)].to_string())
        .collect()
}

#[test]
fn println_shows_its_own_signature_not_the_first_function() {
    let got = label(
        "fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() {\n    let r = add(1, 2);\n    println(|f\"{r}\");\n}\n",
    );
    assert!(got.as_deref().is_none_or(|l| !l.contains("add")), "{got:?}");
    let got = label("fn main() {\n    let x = 1;\n    println( |x );\n}\n");
    assert!(
        got.as_deref().is_none_or(|l| !l.contains("main")),
        "{got:?}"
    );
}

#[test]
fn signature_help_inside_an_f_string_hole() {
    let got = label(
        "fn helper(n: i64) -> i64 { n + 1 }\nfn main() {\n    let m: Map[Str, i64] = Map.new();\n    println(f\"{m.len()} \\\"{helper(|2)}\");\n}\n",
    );
    assert_eq!(got.as_deref(), Some("fn helper(n: i64) -> i64"));
    // In the string's text, outside its holes, there is no call.
    assert_eq!(label("fn main() {\n    println(f\"a(|b {1}\");\n}\n"), None);
}

const FIELDS: &str = "struct P { x: i64 }
impl P {
    fn get(self) -> i64 { self.x }
}
fn helper(n: i64) -> i64 { n + 1 }
fn main() {
    let x = 2;
    let p = P { x: helper(1) };
    let q = P { x };
    let P { x: a } = p;
    let P { x } = q;
    let y = p.x + a + x;
    println(f\"{helper(p.x)} {y} {q.get()}\");
}
";

#[test]
fn renaming_a_field_renames_its_accesses_literals_and_patterns() {
    let src = FIELDS.replacen("struct P { x", "struct P { |x", 1);
    let got = rename(&src, "z").unwrap().unwrap();
    assert_eq!(
        got,
        "struct P { z: i64 }
impl P {
    fn get(self) -> i64 { self.z }
}
fn helper(n: i64) -> i64 { n + 1 }
fn main() {
    let x = 2;
    let p = P { z: helper(1) };
    let q = P { z: x };
    let P { z: a } = p;
    let P { z: x } = q;
    let y = p.z + a + x;
    println(f\"{helper(p.z)} {y} {q.get()}\");
}
"
    );
    // From an access, the same edits.
    let from_use = FIELDS.replacen("let y = p.x", "let y = p.|x", 1);
    assert_eq!(rename(&from_use, "z").unwrap().unwrap(), got);
}

#[test]
fn renaming_a_local_named_by_a_shorthand_keeps_the_field() {
    let src = FIELDS.replacen("let x = 2", "let |x = 2", 1);
    let got = rename(&src, "w").unwrap().unwrap();
    assert!(got.contains("let w = 2;"), "{got}");
    assert!(got.contains("let q = P { x: w };"), "{got}");
}

#[test]
fn rename_refuses_a_keyword_or_a_non_name_and_says_why() {
    let src = "fn he|lper(n: i64) -> i64 { n + 1 }\nfn main() {\n    let y = helper(1);\n}\n";
    let err = rename(src, "fn").unwrap_err();
    assert!(err.contains("keyword"), "{err}");
    let err = rename(src, "1 bad").unwrap_err();
    assert!(err.contains("not a name"), "{err}");
    assert_eq!(
        rename(src, "assist").unwrap().as_deref(),
        Some("fn assist(n: i64) -> i64 { n + 1 }\nfn main() {\n    let y = assist(1);\n}\n")
    );
}

#[test]
fn references_to_a_struct_are_its_name_alone() {
    let src = FIELDS.replacen("struct P", "struct |P", 1);
    let got = refs(&src);
    assert!(!got.is_empty());
    assert!(got.iter().all(|r| r == "P"), "{got:?}");
}

#[test]
fn references_to_a_field_are_its_uses() {
    let src = FIELDS.replacen("let y = p.x", "let y = p.|x", 1);
    assert_eq!(refs(&src), ["x"; 7]);
}
