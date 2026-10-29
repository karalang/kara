//! v2 core §4.6 (ruled 2026-10-29, B-2026-10-28-25): a `match`, `if` or
//! block whose every result that does not diverge is a reference is a
//! reference, so `let r = match rx { Some(ref r) => r, None => return -1 };`
//! binds a `ref` and moves nothing, for a payload with no `.clone()` too.
//! `karac check` refused it as a move out of a borrow. The `ref` it binds
//! cannot be moved out or written through, and a `match` that mixes
//! references and owned values is still a move out of a borrow (§3.7).
//! The runnable pin is corpus/core/ok_match_result_is_a_ref.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-refmatch-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn refused(name: &str, src: &str, want: &str) {
    let (ok, err) = check(name, src);
    assert!(!ok, "{name}: check must refuse");
    assert!(err.contains(want), "{name}: {err}");
}

const MATCHER: &str = "struct Matcher { prefix: String }\n";

#[test]
fn a_match_of_ref_bindings_over_a_regex_is_a_ref() {
    let src = "import std.regex.{Regex};

fn count_hits(keys: Vec[String], pat: Str, on: bool) -> i64 {
    let rx = if on { Regex.compile(pat).ok() } else { None };
    let mut total = 0;
    for k in keys {
        let r = match rx {
            Some(ref r) => r,
            None => return -1,
        };
        if r.is_match(k) {
            total += 1;
        }
    }
    total
}

fn main() {
    let keys: Vec[String] = [\"ab\".to_string(), \"b\".to_string()].to_vec();
    println(count_hits(keys, \"^a\", true));
}
";
    let (ok, err) = check("regex", src);
    assert!(ok, "{err}");
}

#[test]
fn a_match_of_a_ref_and_an_owned_value_still_moves_out_of_a_borrow() {
    let src = format!(
        "{MATCHER}fn width(m: Option[Matcher]) -> i64 {{
    let r = match m {{
        Some(ref r) => r,
        None => Matcher {{ prefix: \"\".to_string() }},
    }};
    r.prefix.len()
}}
fn main() {{ println(width(None)); }}
"
    );
    refused("mixed", &src, "the other arms of this `match` are an owned");
}

#[test]
fn the_ref_a_match_binds_cannot_be_moved_out_or_written_through() {
    let body = |stmt: &str| {
        format!(
            "{MATCHER}fn main() {{
    let m = Some(Matcher {{ prefix: \"a\".to_string() }});
    let mut v: Vec[Matcher] = Vec.new();
    let mut r = match m {{
        Some(ref r) => r,
        None => return,
    }};
    {stmt}
    println(v.len());
}}
"
        )
    };
    refused("push", &body("v.push(r);"), "cannot move 'r'");
    refused("annotated", &body("let k: Matcher = r;"), "cannot move 'r'");
    refused(
        "write",
        &body("r.prefix = \"x\".to_string();"),
        "cannot modify it (core-semantics.md §5.9)",
    );
}

#[test]
fn an_if_and_a_block_of_references_are_references() {
    let src = format!(
        "{MATCHER}fn pick(a: Matcher, b: Matcher, first: bool) -> i64 {{
    let m = if first {{ a }} else {{ b }};
    let n = {{
        let x = m;
        x
    }};
    n.prefix.len()
}}
fn main() {{
    let a = Matcher {{ prefix: \"xy\".to_string() }};
    let b = Matcher {{ prefix: \"z\".to_string() }};
    println(pick(a, b, true) + pick(a, b, false));
    println(a.prefix);
}}
"
    );
    let (ok, err) = check("if_block", &src);
    assert!(ok, "{err}");
    assert_eq!(
        karac::mir::lower::run_source(&src)
            .expect("MIR interpreter")
            .output,
        "3\nxy\n"
    );
}

#[test]
fn a_copy_payload_bound_through_a_match_is_still_a_copy() {
    let src = "fn bump(o: Option[i64]) -> i64 {
    let mut n = match o {
        Some(ref n) => n,
        None => 0,
    };
    n += 1;
    n
}
fn main() { println(bump(Some(4))); }
";
    let (ok, err) = check("copy", src);
    assert!(ok, "{err}");
    assert_eq!(
        karac::mir::lower::run_source(src)
            .expect("MIR interpreter")
            .output,
        "5\n"
    );
}
