//! The 2026-11-22 conformance pass over design.md § 11 (Ownership and
//! sharing) and core-semantics.md §§ 3 to 6,
//! /mnt/project-files/conformance/design-11/:
//!
//! - a use of a `MutFn` local after a move reports E0500, the code §3.2
//!   gives every use after a move, not E0518 (B-2026-11-23-71);
//! - E_SHARED_FIELD_MOVE names the keyword the type was declared with,
//!   `shared` or `sync`, not the retired `shared`/`par` (B-2026-11-23-72);
//! - the borrow-flag conflict panic names where the borrow holding the flag
//!   was made, as § Shared types says ("the source spans of both accesses"):
//!   a `let r = ref a.v` holder used to be reported at the conflicting
//!   access instead (B-2026-11-23-73);
//! - a `mut` argument made from a bare parameter reports E0517 alone, without
//!   a bare `expected 'mut ref T', found 'ref T'` at the same span (B-2026-11-23-74).

use std::process::{Command, Output};

fn karac(tag: &str, src: &str, args: &[&str]) -> Output {
    let dir = std::env::temp_dir().join(format!("karac-d11-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(args)
        .arg("main.kara")
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)
}

#[test]
fn d11_a_moved_mut_fn_use_is_e0500() {
    let src = "fn main() {
    let mut n = 0;
    let mut f = || { n += 1; };
    let mut g = f;
    f();
    g();
    println(f\"{n}\");
}
";
    let out = karac("mutfn", src, &["check", "--output=json"]);
    let t = text(&out);
    assert!(t.contains("\"code\":\"E0500\""), "{t}");
    assert!(!t.contains("\"code\":\"E0518\""), "{t}");
    assert!(t.contains("a MutFn is never Copy"), "{t}");
}

#[test]
fn d11_a_shared_field_move_names_the_declared_keyword() {
    let shared = karac(
        "shared-move",
        "shared struct H { name: String }
fn main() { let h = H { name: \"x\".to_string() }; let s: String = h.name; println(s); }
",
        &["check"],
    );
    let t = text(&shared);
    assert!(
        t.contains("which is declared `shared`: the value is a counted handle"),
        "{t}"
    );
    let sync = karac(
        "sync-move",
        "sync struct H { name: String }
fn main() { let h = H { name: \"x\".to_string() }; let s: String = h.name; println(s); }
",
        &["check"],
    );
    let t = text(&sync);
    assert!(
        t.contains("which is declared `sync`: the value is a counted handle"),
        "{t}"
    );
    assert!(!t.contains("`par`"), "{t}");
}

const REF_HOLDER: &str = "shared struct C { mut v: Vec[i64] }
fn f(a: C, b: C) -> i64 {
    let r = ref a.v;
    b.v.push(9);
    r.len()
}
fn main() { let c = C { v: [1] }; println(f\"{f(c, c)}\"); }
";

#[test]
fn d11_the_flag_panic_names_where_a_ref_holder_was_made() {
    let out = karac("ref-holder", REF_HOLDER, &["__mir-run"]);
    assert_eq!(out.status.code(), Some(101), "{}", text(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "runtime error: already borrowed: `C.v` is borrowed through another handle at \
         main.kara:3:13 (core semantics §6.2)\n  at main.kara:4:5\n"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn d11_the_flag_panic_names_where_a_ref_holder_was_made_compiled() {
    let out = karac("ref-holder-jit", REF_HOLDER, &["__mir-llvm-run"]);
    assert_eq!(out.status.code(), Some(101), "{}", text(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "runtime error: already borrowed: `C.v` is borrowed through another handle at \
         main.kara:3:13 (core semantics §6.2)\n  at main.kara:4:5\n"
    );
}

#[test]
fn d11_a_mut_argument_from_a_bare_parameter_is_one_error() {
    let src = "fn add(v: mut ref Vec[i64]) { v.push(1); }
fn outer(v: Vec[i64]) { add(mut v); }
fn main() { let v: Vec[i64] = []; outer(v); }
";
    let out = karac("mut-arg", src, &["check"]);
    let t = text(&out);
    assert!(t.contains("error[E0517]"), "{t}");
    assert!(!t.contains("expected 'mut ref Vec[i64]'"), "{t}");
    assert!(t.contains("1 error(s) found."), "{t}");
}
