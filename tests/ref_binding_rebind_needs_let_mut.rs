//! A `let` binding holding a reference handle is rebound only with `let
//! mut`: plain `=` on an identifier never writes through a reference
//! (`up = 5` on a `mut ref i64` is E0200), so `up = mut ref b` rebinds the
//! `let` and is E0506 as `let x = a; x = b;` is (core semantics §5.1: "`let
//! mut` would only let it be rebound"; B-2026-11-27-50). The same holds for
//! a `ref` binding and a `shared` value. A write through the handle still
//! needs no `mut`.

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

fn file(src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-ref-binding-rebind-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    (dir, f)
}

/// `karac check` on `src`: whether it passed, and its output.
fn check(src: &str) -> (bool, String) {
    let (dir, f) = file(src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The program's stdout under `cmd`, after `karac check` accepted it;
/// `None` when a compiled backend cannot link.
fn run(src: &str, cmd: &str) -> Option<String> {
    let (ok, out) = check(src);
    assert!(ok, "check:\n{out}\n{src}");
    let (dir, f) = file(src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if cmd != "__mir-run" {
        common::link_or_skip(if out.status.success() {
            Ok(())
        } else {
            Err(err.clone())
        })?;
    }
    assert!(out.status.success(), "{cmd}: {err}\n{src}");
    Some(String::from_utf8(out.stdout).unwrap())
}

fn runs_everywhere(src: &str, want: &str) {
    let mut lanes = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        lanes.extend(["__mir-llvm-run", "__mir-llvm-asan"]);
    }
    for cmd in lanes {
        if let Some(out) = run(src, cmd) {
            assert_eq!(out, want, "{cmd}\n{src}");
        }
    }
}

/// `src` is refused with E0506 naming `name`.
fn refused_rebind(src: &str, name: &str) {
    let (ok, out) = check(src);
    assert!(!ok, "{src}\n{out}");
    assert!(
        out.contains("E0506") && out.contains(&format!("cannot reassign `{name}`")),
        "{src}\n{out}"
    );
}

#[test]
fn a_reference_handle_let_is_not_rebound_without_mut() {
    refused_rebind(
        "fn main() { let mut a = 1; let mut b = 2; let up = mut ref a; up = mut ref b; \
         *up += 1; println(f\"{a} {b}\"); }\n",
        "up",
    );
    refused_rebind(
        "fn main() { let a = 1; let b = 2; let r = ref a; r = ref b; println(*r); }\n",
        "r",
    );
    refused_rebind(
        "shared struct S { n: i64 }\n\
         fn main() { let s = S { n: 1 }; let t = S { n: 2 }; s = t; println(s.n); }\n",
        "s",
    );
}

#[test]
fn let_mut_rebinds_a_reference_handle() {
    runs_everywhere(
        "fn main() { let mut a = 1; let mut b = 2; let mut up = mut ref a; up = mut ref b; \
         *up += 1; println(f\"{a} {b}\"); }\n",
        "1 3\n",
    );
    runs_everywhere(
        "shared struct S { n: i64 }\n\
         fn main() { let mut s = S { n: 1 }; let t = S { n: 2 }; s = t; println(s.n); }\n",
        "2\n",
    );
}

#[test]
fn a_write_through_a_reference_handle_needs_no_mut() {
    runs_everywhere(
        "struct P { hits: i64 }\n\
         shared struct S { mut n: i64 }\n\
         fn main() { let mut a = 1; let up = mut ref a; *up += 1; *up = *up * 10; \
         let mut p = P { hits: 0 }; let q = mut ref p; q.hits += 1; \
         let s = S { n: 1 }; s.n = 5; println(f\"{a} {p.hits} {s.n}\"); }\n",
        "20 1 5\n",
    );
}

#[test]
fn the_rebind_fix_adds_let_mut() {
    let src = "fn main() { let mut a = 1; let mut b = 2; let up = mut ref a; up = mut ref b; \
               *up += 1; println(f\"{a} {b}\"); }\n";
    let (dir, f) = file(src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("fix")
        .arg(&f)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let fixed = std::fs::read_to_string(&f).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(fixed.contains("let mut up = mut ref a;"), "{fixed}");
    runs_everywhere(&fixed, "1 3\n");
}

/// The corpus shapes a handle-wide refusal broke: `m = m + 1` inside `lock
/// m` writes the locked value, and `old = v` on a `get_mut` result writes
/// the entry (kata 2353). A value of the referent's type is a write, not a
/// rebind.
#[test]
fn a_value_assigned_to_a_handle_is_a_write_not_a_rebind() {
    runs_everywhere(
        "import std.sync.{Mutex};\n\
         fn main() { let m = Mutex.new(0); lock m { m = m + 1; } lock m { println(m); } }\n",
        "1\n",
    );
    runs_everywhere(
        "fn main() { let mut info: Map[String, (String, i64)] = Map.new(); \
         info.insert(\"a\".to_string(), (\"x\".to_string(), 1)); \
         let (c, old) = info.get_mut(\"a\".to_string()).unwrap(); old = 7; println(c); \
         println(info[\"a\".to_string()].1); }\n",
        "x\n7\n",
    );
}
