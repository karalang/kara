//! B-2026-11-20-67: a binding a pattern takes out of a borrowed place
//! (`let (r, k) = h.pe` with `h` a bare parameter) is a view, a `ref R` whose
//! type reads as `R` (core-semantics.md §5.1). `let m: ref R = r` copies that
//! view, so neither `karac check` nor the MIR pipeline's own typecheck may
//! refuse it with E0200 ("a type annotation does not borrow"). An owned
//! local is still refused: there the annotation would move it.

use std::path::PathBuf;
use std::process::{Command, Output};

fn karac(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr)
}

fn scratch(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-viewannot-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

const FIELD_VIEW: &str = "struct R { name: String }
struct H { pe: (R, i64) }
fn hh(h: H) -> i64 {
    let (r, k) = h.pe;
    let m: ref R = r;
    m.name.len() + k
}
fn g(p: (R, i64)) -> i64 {
    let (r, k) = p;
    let m: ref R = r;
    m.name.len() + k
}
fn main() {
    println(hh(H { pe: (R { name: \"ab\".to_string() }, 1) }));
    println(g((R { name: \"abc\".to_string() }, 1)));
}
";

#[test]
fn view_binding_ref_annotation_is_accepted_and_runs() {
    let f = scratch("field", FIELD_VIEW);
    let p = f.to_str().unwrap();
    let check = karac(&["check", p]);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["run", "--interp", p]);
    assert!(run.status.success(), "{}", text(&run));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "3\n4\n");
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
}

#[test]
fn view_binding_ref_annotation_still_refuses_an_owned_local() {
    let src = "struct R { name: String }
fn f() -> i64 {
    let r = R { name: \"a\".to_string() };
    let m: ref R = r;
    m.name.len()
}
fn main() { println(f()); }
";
    let f = scratch("owned", src);
    let check = karac(&["check", f.to_str().unwrap()]);
    assert!(!check.status.success());
    assert!(
        text(&check).contains("a type annotation does not borrow"),
        "{}",
        text(&check)
    );
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
}

#[test]
fn view_binding_ref_annotation_still_refuses_a_move_out_of_the_view() {
    let src = "struct R { name: String }
struct H { pe: (R, i64) }
fn hh(h: H) -> i64 {
    let (r, k) = h.pe;
    let n: R = r;
    n.name.len() + k
}
fn main() { println(hh(H { pe: (R { name: \"a\".to_string() }, 1) })); }
";
    let f = scratch("move", src);
    let check = karac(&["check", f.to_str().unwrap()]);
    assert!(!check.status.success());
    assert!(text(&check).contains("cannot move 'r'"), "{}", text(&check));
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
}
