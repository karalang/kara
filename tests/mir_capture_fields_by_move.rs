//! B-2026-11-14-20: a closure that captures a struct by move and uses it only
//! through its fields moves just those fields, as a `ref` or `mut ref`
//! capture borrows just its fields (core semantics §3.6, §9.1, design.md
//! § Captures). It moved the whole binding, so another field used after
//! it passed check and failed every run lane with the MIR move check's
//! `use of _1.1, which may have been moved`; and a returned closure kept
//! the whole struct alive, dropping `b` and `c` only with the closure.
//! Each program runs on the interpreter, compiled and under ASan.

use std::process::{Command, Stdio};

fn run(sub: &str, tag: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-cap-fields-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([sub, "cell.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn lanes(tag: &str, src: &str, want: &str) {
    let (ok, out, err) = run("__mir-run", tag, src);
    assert!(ok, "{tag} interpreted: {err}");
    assert_eq!(out, want, "{tag} interpreted: {err}");
    if !cfg!(feature = "llvm") {
        return;
    }
    for sub in ["__mir-llvm-run", "__mir-llvm-asan"] {
        let (ok, out, err) = run(sub, tag, src);
        if err.contains("Linker failed")
            && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none()
        {
            eprintln!("skipping {sub}: no runtime archive");
            continue;
        }
        assert!(ok, "{tag} {sub}: {err}");
        assert_eq!(out, want, "{tag} {sub}: {err}");
    }
}

#[test]
fn fn_value_capture_then_field_moved() {
    lanes(
        "x1",
        r#"struct T { n: i64 }
impl Drop for T { fn drop(mut ref self) { println(f"drop {self.n}"); } }
struct S { a: T, b: T, c: T }
fn eat(t: own T) { println(f"eat {t.n}"); }
fn mk(s: own S) -> Fn() -> i64 {
  let f = || s.a.n;
  eat(s.b);
  f
}
fn main() {
  let s = S { a: T { n: 1 }, b: T { n: 2 }, c: T { n: 3 } };
  let f = mk(s);
  println("made");
  println(f());
}
"#,
        "eat 2\ndrop 2\ndrop 3\nmade\n1\ndrop 1\n",
    );
}

#[test]
fn local_capture_then_field_moved() {
    lanes(
        "x2",
        r#"struct T { n: i64 }
impl Drop for T { fn drop(mut ref self) { println(f"drop {self.n}"); } }
struct S { a: T, b: T, c: T }
fn eat(t: own T) { println(f"eat {t.n}"); }
fn main() {
  let s = S { a: T { n: 1 }, b: T { n: 2 }, c: T { n: 3 } };
  let f = || eat(s.a);
  eat(s.b);
  f();
  println("end");
}
"#,
        "eat 2\ndrop 2\neat 1\ndrop 1\nend\ndrop 3\n",
    );
}

#[test]
fn stored_capture_then_field_read() {
    lanes(
        "x3",
        r#"struct T { n: i64 }
impl Drop for T { fn drop(mut ref self) { println(f"drop {self.n}"); } }
struct S { a: T, b: T, c: T }
fn main() {
  let s = S { a: T { n: 1 }, b: T { n: 2 }, c: T { n: 3 } };
  let mut v: Vec[Fn() -> i64] = Vec.new();
  v.push(|| s.a.n);
  println(s.b.n);
  println((v[0])());
  println("end");
}
"#,
        "2\n1\nend\ndrop 1\ndrop 3\ndrop 2\n",
    );
}

#[test]
fn returned_capture_drops_the_rest() {
    lanes(
        "k17_partial_capture_drop_rest",
        r#"// expect: run
// asan
//> drop 3
//> drop 2
//> made
//> 1
//> drop 1
//> end
struct T { n: i64 }
impl Drop for T { fn drop(mut ref self) { println(f"drop {self.n}"); } }
struct S { a: T, b: T, c: T }
fn mk(s: own S) -> Fn() -> i64 { || s.a.n }
fn main() {
  {
    let s = S { a: T { n: 1 }, b: T { n: 2 }, c: T { n: 3 } };
    let f = mk(s);
    println("made");
    println(f());
  }
  println("end");
}
"#,
        "drop 3\ndrop 2\nmade\n1\ndrop 1\nend\n",
    );
}
