//! The call-site `mut` marker at a method's `mut ref` parameter and the
//! `let mut` rule around it (design.md § Call-site `mut` markers):
//!
//! - B-2026-11-10-22: a `mut` marker on a bare `shared` binding lends the
//!   binding itself, which the callee can rebind, so it needs `let mut`.
//! - B-2026-11-09-101: a receiver write the stdlib table did not list
//!   (`VecDeque.push_back`) needs `let mut` too.
//! - B-2026-11-09-100: an unmarked argument at a method's `mut ref`
//!   parameter is deprecated in 0.8.x, with a fix that inserts `mut `; a
//!   forwarded `mut ref` / `mut Slice` needs none.

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-method-mut-marker-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

fn deprecated_lines(out: &str) -> usize {
    out.lines()
        .filter(|l| l.starts_with("warning[deprecated]") && l.contains("takes the call-site"))
        .count()
}

/// Refused, then `karac fix` makes it check clean and print `want`.
fn refused_then_fixed(name: &str, src: &str, needle: &str, fixed: &str, want: &str) {
    let path = scratch(name, src);
    let check = karac(&["check"], &path);
    assert!(!check.status.success(), "{}", text(&check));
    assert!(text(&check).contains(needle), "{}", text(&check));
    let fix = karac(&["fix"], &path);
    assert!(fix.status.success(), "{}", text(&fix));
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.contains(fixed), "{after}");
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(String::from_utf8_lossy(&run.stdout), want, "{}", text(&run));
}

#[test]
fn a_shared_let_lent_as_mut_is_refused_and_fixed_with_let_mut() {
    refused_then_fixed(
        "shared_lent.kara",
        "shared struct T { v: i64 }
fn advance(t: mut ref T) { let n = T { v: t.v + 1 }; *t = n; }
fn main() {
    let tail = T { v: 1 };
    advance(mut tail);
    println(f\"{tail.v}\");
}
",
        "cannot pass `tail` as `mut`",
        "let mut tail = T",
        "2\n",
    );
}

#[test]
fn a_field_of_a_shared_let_lent_as_mut_still_needs_no_let_mut() {
    let path = scratch(
        "shared_field.kara",
        "shared struct T { mut v: i64 }
fn bump(x: mut ref i64) { *x += 1; }
fn main() {
    let t = T { v: 1 };
    bump(mut t.v);
    println(f\"{t.v}\");
}
",
    );
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "2\n",
        "{}",
        text(&run)
    );
}

#[test]
fn a_vecdeque_push_back_on_a_plain_let_is_refused_and_fixed() {
    refused_then_fixed(
        "vecdeque.kara",
        "fn main() {
    let w: VecDeque[i64] = VecDeque.new();
    w.push_back(1);
    println(f\"{w.len()}\");
}
",
        "cannot call `push_back` on `w`",
        "let mut w: VecDeque[i64]",
        "1\n",
    );
}

#[test]
fn a_reading_library_method_on_a_plain_let_stays_accepted() {
    let path = scratch(
        "reader.kara",
        "fn main() {
    let mut w: VecDeque[i64] = VecDeque.new();
    w.push_back(4);
    let r = w;
    let v = Vec[3, 1, 2];
    println(f\"{r.len()} {r.front().unwrap()} {v.len()} {v.contains(1)}\");
}
",
    );
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
}

const MARKER: &str = "struct R { n: i64 }
impl R {
    fn fill(ref self, buf: mut ref String) { buf.push_str(\"x\"); }
    fn fill_slice(ref self, s: mut Slice[i64]) { s[0] = 9; }
}
fn fwd(r: ref R, b: mut ref String) { r.fill(b); }
fn main() {
    let r = R { n: 1 };
    let mut s = String.new();
    r.fill(s);
    r.fill(mut s);
    fwd(r, mut s);
    let mut v = Vec[1, 2, 3];
    r.fill_slice(v[0..2]);
    println(f\"{s} {v[0]}\");
}
";

#[test]
fn an_unmarked_method_mut_ref_argument_is_deprecated_and_fixed() {
    let path = scratch("marker.kara", MARKER);
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    // `r.fill(s)` and `r.fill_slice(v[0..2])`; the forwarded `b` in `fwd`
    // and the marked call are not warned.
    assert_eq!(deprecated_lines(&text(&check)), 2, "{}", text(&check));
    let before = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&before.stdout),
        "xxx 9\n",
        "{}",
        text(&before)
    );
    let fix = karac(&["fix"], &path);
    assert!(fix.status.success(), "{}", text(&fix));
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(
        src.contains("    r.fill(mut s);\n    r.fill(mut s);"),
        "{src}"
    );
    assert!(src.contains("r.fill_slice(mut v[0..2])"), "{src}");
    assert!(src.contains("{ r.fill(b); }"), "{src}");
    let check = karac(&["check"], &path);
    assert_eq!(deprecated_lines(&text(&check)), 0, "{}", text(&check));
    let after = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&after.stdout),
        "xxx 9\n",
        "{}",
        text(&after)
    );
}

#[test]
fn allow_deprecated_silences_the_unmarked_method_argument() {
    let path = scratch(
        "marker_allowed.kara",
        &MARKER.replace("fn main() {", "#[allow(deprecated)]\nfn main() {"),
    );
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    assert_eq!(deprecated_lines(&text(&check)), 0, "{}", text(&check));
}
