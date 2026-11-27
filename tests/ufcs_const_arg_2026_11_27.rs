//! A type-qualified call on a type with `const` parameters binds them from
//! the receiver's arguments: `Buf[i64, 3].fill(7)` with `impl[T, const N:
//! i64] Buf[T, N] { fn fill(x: T) -> Array[T, N] }` returns an `Array[i64,
//! 3]`. The const argument was dropped, so the call's type was `Array[i64,
//! N]` and `let a: Array[i64, 3] = Buf[i64, 3].fill(7)` was refused E0200;
//! `Buf[i64, M]` inside `impl[const M: i64]` was E0236 "no method"
//! (B-2026-11-27-9).

mod common;

use std::process::Command;

fn karac(args: &[&str], name: &str, src: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-ufcs-const-arg-{name}-{}-{}",
        args[0].trim_start_matches('_'),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// `karac check` accepts `src`, and the interpreter (and, with the backend,
/// the compiled program) prints `want` and exits `code`.
fn checks_and_runs(name: &str, src: &str, want: &str, code: i32) {
    let check = karac(&["check"], name, src);
    assert!(
        check.status.success(),
        "{name}: check: {}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for cmd in backends {
        let out = karac(&[cmd], name, src);
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        if *cmd != "__mir-run"
            && common::link_or_skip(if out.status.code() == Some(code) {
                Ok(())
            } else {
                Err(err.clone())
            })
            .is_none()
        {
            continue;
        }
        assert_eq!(out.status.code(), Some(code), "{name}: {cmd}: {err}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{name}: {cmd}\n{src}"
        );
    }
}

#[test]
fn ufcs_const_arg_binds_a_literal_length() {
    checks_and_runs(
        "literal",
        "struct Buf[T, const N: i64] { a: Array[T, N] }\nimpl[T: Clone, const N: i64] Buf[T, N] {\n    fn size() -> i64 { N }\n    fn fill(x: T) -> Array[T, N] { Array.from_fn(N, |_| x.clone()) }\n    fn make(x: T) -> Result[Array[T, N], String] { Ok(Array.from_fn(N, |_| x.clone())) }\n}\nfn main() {\n    println(Buf[i64, 4].size());\n    let a: Array[i64, 3] = Buf[i64, 3].fill(7);\n    println(a[2]);\n    let b: Result[Array[String, 2], String] = Buf[String, 2].make(\"q\");\n    println(b.unwrap()[1]);\n}\n",
        "4\n7\nq\n",
        0,
    );
}

#[test]
fn ufcs_const_arg_binds_an_enclosing_const_parameter() {
    checks_and_runs(
        "enclosing",
        "struct Buf[T, const N: i64] { a: Array[T, N] }\nimpl[T: Clone, const N: i64] Buf[T, N] {\n    fn fill(x: T) -> Array[T, N] { Array.from_fn(N, |_| x.clone()) }\n}\nstruct Two[const M: i64] { n: i64 }\nimpl[const M: i64] Two[M] {\n    fn twice(ref self) -> i64 { let a = Buf[i64, M].fill(self.n); a.len() * 2 }\n}\nfn main() { let t: Two[5] = Two { n: 1 }; println(t.twice()); }\n",
        "10\n",
        0,
    );
}

#[test]
fn ufcs_const_arg_a_different_length_is_still_refused() {
    let out = karac(
        &["check"],
        "mismatch",
        "struct Buf[T, const N: i64] { a: Array[T, N] }\nimpl[T: Clone, const N: i64] Buf[T, N] {\n    fn fill(x: T) -> Array[T, N] { Array.from_fn(N, |_| x.clone()) }\n}\nfn main() {\n    let a: Array[i64, 4] = Buf[i64, 3].fill(7);\n    println(a[0]);\n}\n",
    );
    let t = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "accepted:\n{t}");
    assert!(
        t.contains("expected 'Array[i64, 4]', found 'Array[i64, 3]'"),
        "{t}"
    );
}
