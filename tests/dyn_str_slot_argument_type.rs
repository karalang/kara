//! A `Str` slot through `dyn` or a bound, `x: X` at `dyn Cv[Str]` or
//! `T: Cv[Str]`, takes its argument as the slot does, by the argument's
//! own type: a literal (or an `if` of literals) checked as the view is
//! passed as one, and a `String` place or temporary is viewed and the view
//! lent. The lowering assumed a literal arrived as its `String` constant,
//! so a literal typed `Str` printed 784 for 3 compiled, and the checker
//! refused a `String` and an `if` of literals there (B-2026-11-20-13).
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const CV: &str = "trait Cv[T] { fn cv(self, x: T) -> i64; }
trait Ln { fn ln(self, x: Str) -> i64; }
struct A { }
impl Cv[Str] for A { fn cv(self, x: Str) -> i64 { x.len() } }
impl Ln for A { fn ln(self, x: Str) -> i64 { x.len() } }
";

/// The program's stdout from `karac <cmd> main.kara`, after `karac check`
/// accepted it; `None` when the compiled backend cannot link.
fn checked_and_run(name: &str, src: &str, cmd: &str) -> Option<String> {
    let dir = std::env::temp_dir().join(format!(
        "karac-dyn-str-slot-{name}-{cmd}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let check = Command::new(karac)
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{name}: check: {}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let out = Command::new(karac).arg(cmd).arg(&file).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if cmd != "__mir-run" {
        common::link_or_skip(if out.status.success() {
            Ok(())
        } else {
            Err(err.clone())
        })?;
    }
    assert!(out.status.success(), "{name}: {cmd}: {err}");
    Some(String::from_utf8(out.stdout).unwrap())
}

fn runs_everywhere(name: &str, main: &str, want: &str) {
    let src = format!("{CV}{main}\n");
    for cmd in ["__mir-run", "__mir-llvm-run", "__mir-llvm-asan"] {
        if let Some(out) = checked_and_run(name, &src, cmd) {
            assert_eq!(out, want, "{name}: {cmd}\n{src}");
        }
    }
}

#[test]
fn dyn_str_slot_takes_a_literal_as_the_view() {
    runs_everywhere(
        "lit",
        "fn main() { let v: Vec[dyn Cv[Str]] = [A { }]; let mut t = 0; \
         for e in v.iter() { t += e.cv(\"xyz\"); } println(t); }",
        "3\n",
    );
}

#[test]
fn dyn_str_slot_takes_an_if_of_literals() {
    runs_everywhere(
        "if",
        "fn main() { let e: dyn Cv[Str] = A { }; \
         println(e.cv(if true { \"xyz\" } else { \"ab\" })); }",
        "3\n",
    );
}

#[test]
fn dyn_str_slot_views_a_string_place_and_a_temporary() {
    runs_everywhere(
        "string",
        "fn main() { let e: dyn Cv[Str] = A { }; let s = \"abcd\".to_string(); \
         println(e.cv(s) + e.cv(s.clone())); println(s); }",
        "8\nabcd\n",
    );
}

#[test]
fn bounded_str_slot_views_a_string_and_takes_an_if_of_literals() {
    runs_everywhere(
        "bound",
        "fn h[T: Cv[Str]](t: T) -> i64 { let s = \"abcd\".to_string(); \
         t.cv(s) + t.cv(s.clone()) + t.cv(\"xyz\") + t.cv(if true { \"xy\" } else { \"ab\" }) }\n\
         fn main() { println(h(A { })); }",
        "13\n",
    );
}

#[test]
fn written_str_slot_still_takes_every_shape() {
    runs_everywhere(
        "written",
        "fn g[T: Ln](t: T) -> i64 { t.ln(if true { \"xy\" } else { \"ab\" }) }\n\
         fn main() { let e: dyn Ln = A { }; let s = \"abcd\".to_string(); \
         println(e.ln(\"xyz\") + e.ln(s) + e.ln(s.clone()) + g(A { })); }",
        "13\n",
    );
}
