//! A `;` missing before the `}` that closes a block is reported with its
//! insertion, and the parser goes on as if it were there: an assignment
//! ending each arm of an `if .. else` gets one fix per arm and no cascade,
//! so one `karac fix` run repairs the statement form and the match-arm
//! form alike (B-2026-10-28-31).

#[test]
fn each_arm_missing_its_semicolon_gets_its_own_fix() {
    let src = "fn main() {\n    let mut a = false;\n    let mut b = false;\n    let x = 3;\n\
               \x20   if x > 2 { a = true } else { b = true }\n\
               \x20   match x {\n        3 => if x > 1 { a = true } else { b = true },\n        _ => {}\n    }\n\
               \x20   println(f\"{a} {b}\");\n}\n";
    let result = karac::parse(src);
    let at: Vec<(usize, usize)> = result
        .errors
        .iter()
        .map(|e| (e.span.line, e.span.column))
        .collect();
    assert_eq!(
        at,
        [(5, 25), (5, 43), (7, 34), (7, 52)],
        "{:?}",
        result.errors
    );
    let mut edits: Vec<_> = result.fix_edits.values().cloned().collect();
    assert_eq!(edits.len(), 4);
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut out = src.to_string();
    for e in edits {
        out.replace_range(e.offset..e.offset + e.length, &e.replacement);
    }
    assert!(
        out.contains("if x > 2 { a = true; } else { b = true; }"),
        "{out}"
    );
    let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "true false\n");
}
