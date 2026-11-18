//! B-2026-11-18-17: `.raw()` on an owned non-`Copy` distinct value gives up
//! the base (design.md § Distinct Types), but `karac check` did not count it
//! as a move: a later use of the wrapper checked clean and was refused only
//! by the MIR move check, with no source position. Its help line now says
//! the `.raw()` gave the wrapper up rather than advise dropping an `own`.
//!
//! B-2026-11-18-58: `w.n.raw()` through a bare (borrowing) `w` was typed as
//! the owned base and refused as a move out of a borrowed place (E0516);
//! like `n.raw()` of a borrowed `n`, it is a borrow of the base.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, backend: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-distinct-raw-move-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(backend)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn backends() -> &'static [&'static str] {
    if cfg!(feature = "llvm") {
        &["check", "__mir-run", "__mir-llvm-run"]
    } else {
        &["check", "__mir-run"]
    }
}

fn runs(tag: &str, src: &str, want: &str) {
    for b in backends() {
        let out = karac(tag, b, src);
        let want = if *b == "check" { "" } else { want };
        assert_eq!(
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).as_ref()
            ),
            (Some(0), want),
            "{tag} on {b}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// `karac check` refuses with E0500 at the reuse, naming the `.raw()`.
fn refused_at_check(tag: &str, src: &str, at: &str, moved_at: &str) -> String {
    let out = karac(tag, "check", src);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert_ne!(out.status.code(), Some(0), "{tag}: {err}");
    assert!(err.contains("error[E0500]"), "{tag}: {err}");
    assert!(err.contains(at), "{tag}: wants {at}: {err}");
    assert!(err.contains(moved_at), "{tag}: wants {moved_at}: {err}");
    assert!(
        err.contains("`.raw()` on an owned value gives up 'n'"),
        "{tag}: {err}"
    );
    assert!(!err.contains("drop `own`"), "{tag}: {err}");
    err
}

#[test]
fn raw_of_an_owned_wrapper_moves_it_at_check() {
    refused_at_check(
        "m1",
        "distinct type Name = String;
fn main() { let n = Name(\"abc\".to_string()); let s: String = n.raw(); println(s); println(n.raw()); }
",
        ":2:91:",
        "moved at line 2:62",
    );
    refused_at_check(
        "m2",
        "distinct type Name = String;
fn take(n: own Name) { println(n.raw()); }
fn main() { let n = Name(\"abc\".to_string()); let s: String = n.raw(); println(s); take(n); }
",
        ":3:88:",
        "moved at line 3:62",
    );
    refused_at_check(
        "m3",
        "distinct type Name = String;
fn main() {
    let n = Name(\"abc\".to_string());
    let flag = \"x\".len() == 1;
    if flag { let s: String = n.raw(); println(s); }
    println(n.raw());
}
",
        ":6:13:",
        "moved at line 5:31 on some path",
    );
}

#[test]
fn raw_of_a_cloneable_wrapper_offers_a_copy() {
    let err = refused_at_check(
        "clone",
        "#[derive(Clone)]
distinct type Name = String;
fn main() { let n = Name(\"abc\".to_string()); let s: String = n.raw(); println(s); println(n.raw()); }
",
        ":3:91:",
        "moved at line 3:62",
    );
    assert!(err.contains("`n.clone().raw()`"), "{err}");
}

#[test]
fn raw_of_a_borrowed_or_copy_wrapper_stays_a_read() {
    runs(
        "borrowed",
        "distinct type Name = String;
fn show(n: Name) { println(n.raw()); println(n.raw()); }
fn len_of(n: Name) -> i64 { n.raw().len() }
fn main() { let n = Name(\"abc\".to_string()); show(n); show(n); println(len_of(n)); }
",
        "abc\nabc\nabc\nabc\n3\n",
    );
    runs(
        "copy",
        "#[derive(Clone, Copy)]
distinct type Id = i64;
fn main() { let i = Id(4); let a = i.raw(); let b = i.raw(); println(a + b); println(i.raw()); }
",
        "8\n4\n",
    );
}

#[test]
fn raw_of_a_field_through_a_bare_parameter_borrows() {
    runs(
        "field",
        "distinct type Name = String;
struct W { n: Name }
fn peek(w: W) -> i64 { w.n.raw().len() }
fn keep(w: W) -> i64 { let r = w.n.raw(); r.len() }
fn main() {
    let w = W { n: Name(\"abcd\".to_string()) };
    println(peek(w));
    println(keep(w));
    let s: String = w.n.raw();
    println(s);
}
",
        "4\n4\nabcd\n",
    );
}

#[test]
fn raw_of_a_generic_wrapper_moves_only_a_non_copy_base() {
    runs(
        "generic",
        "distinct type Wrap[T] = T;
distinct type Ids[T] = Vec[T];
fn main() {
    let w = Wrap(5);
    let a = w.raw();
    println(a + w.raw());
    let s = Wrap(\"hi\".to_string());
    let t: String = s.raw();
    println(t);
    let v = Ids([1, 2]);
    let x: Vec[i64] = v.raw();
    println(x.len());
}
",
        "10\nhi\n2\n",
    );
    refused_at_check(
        "generic-moved",
        "distinct type Wrap[T] = T;
fn main() { let n = Wrap(\"hi\".to_string()); let t: String = n.raw(); println(t); println(n.raw()); }
",
        ":2:90:",
        "moved at line 2:61",
    );
}
