//! A program's private type may reuse the name of a library type
//! (`trait Receiver`, `struct Sender`). The signature-visibility audit keys
//! types by name and walked the library's items too, so the library's
//! `pub fn channel(..) -> (Sender[T], Receiver[T])` read the program's
//! private type and every `karac run` and `karac build` of the program
//! stopped at "private type 'Receiver' leaks through return type of
//! 'channel'", a line of library source, while `karac check` passed
//! (B-2026-11-07-6). The audit still holds the program's own `pub`
//! signatures to its own types.

use std::process::Command;

#[test]
fn a_private_type_named_like_a_library_type_runs() {
    let src = "trait Receiver {
    fn receive(self, n: i64) -> i64;
}

struct Sender { k: i64 }

struct Dbl {}

impl Receiver for Dbl {
    fn receive(self, n: i64) -> i64 { n * 2 }
}

fn main() {
    let d = Dbl {};
    let s = Sender { k: 3 };
    println(f\"{d.receive(4)} {s.k}\");
}
";
    let out = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(out.output, "8 3\n");
}

#[test]
fn a_pub_fn_returning_a_private_type_named_like_a_library_type_is_still_refused() {
    let src = "struct Receiver { n: i64 }

pub fn make() -> Receiver {
    Receiver { n: 1 }
}

fn main() {
    println(f\"{make().n}\");
}
";
    let dir = std::env::temp_dir().join(format!("karac-libname-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains("private type 'Receiver' leaks through return type of 'make'"),
        "{err}"
    );
}
