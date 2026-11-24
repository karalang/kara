//! A closure literal handed to a generic bare `F` parameter borrows its
//! captures (core semantics §9.1, §9.3): a bare parameter is a borrow and
//! not escaping, so the callee can only call it (B-2026-11-19-72). The MIR
//! builder used to lower it as an escaping closure, moving every capture in:
//! a captured `Drop` value was dropped at the end of the call statement, a
//! later use of it was refused by the move check after `karac check` had
//! accepted it, and a counter the closure wrote was a copy.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-generic-fn-borrow-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn runs_as(tag: &str, src: &str, want: &str) {
    let check = karac(tag, &["check"], src);
    assert!(
        check.status.success(),
        "{tag}: check: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for b in backends {
        let out = karac(tag, &[b], src);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{tag} on {b}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{tag} on {b}: {:?}", out.status);
    }
}

const R: &str = "struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f\"d{self.id}\"); } }
fn call[F: Fn() -> i64](f: F) -> i64 { f() }
";

#[test]
fn generic_bare_fn_param_closure_keeps_its_drop_capture_to_scope_end() {
    let src = format!(
        "{R}fn main() {{ let r = R {{ id: 5 }}; let x = call(|| r.id); println(x); println(\"end\"); }}\n"
    );
    runs_as("drop", &src, "5\nend\nd5\n");
}

#[test]
fn generic_bare_fn_param_closure_capture_is_usable_after_the_call() {
    let src = format!(
        "{R}fn main() {{ let r = R {{ id: 5 }}; let x = call(|| r.id); println(x); println(r.id); println(\"end\"); }}\n"
    );
    runs_as("later-use", &src, "5\n5\nend\nd5\n");
}

#[test]
fn generic_bare_fn_param_closure_over_a_string_leaves_it_owned() {
    let src = "fn twice[F: Fn() -> i64](f: F) -> i64 { f() + f() }
fn main() {
    let s = \"abc\".to_string();
    let k = twice(|| s.len());
    let t = s;
    println(t);
    println(k);
}
";
    runs_as("string", src, "abc\n6\n");
}

/// A `MutFn` called through a bare (shared) `F` is refused since
/// B-2026-11-19-76; through `own F` the writes reach the caller's counter.
#[test]
fn generic_own_mut_fn_param_closure_writes_the_callers_counter() {
    let src = "fn call[F: MutFn() -> i64](f: own F) -> i64 { let mut f = f; f() + f() }
fn main() { let mut n = 0; println(call(|| { n += 1; n })); println(n); }
";
    runs_as("counter", src, "3\n2\n");
}
