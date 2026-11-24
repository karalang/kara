//! `with_provider[R](p, || Ok(v))` checked against a `Result` (a function's
//! tail, an annotated `let`, the operand of `?`): the closure's `Ok(v)`
//! leaves the `Err` side open, the call was left untyped, and MIR refused it
//! with "a type that failed to check is not lowered yet" (E0516), a valid
//! program and the escape case alike (B-2026-11-23-20). The call now takes
//! the open side from what it is checked against.

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Every call gets a directory of its own, whatever names two tests pass.
static CALL: AtomicUsize = AtomicUsize::new(0);

fn karac(name: &str, src: &str, cmd: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-wp-ok-closure-{name}-{cmd}-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// `karac check` accepts `src`, and the interpreter and, built with the
/// backend, the compiled program and its ASan build print `want`.
fn runs_everywhere(name: &str, src: &str, want: &str) {
    let check = karac(name, src, "check");
    assert!(
        check.status.success(),
        "{name}: check: {}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let cmds: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run", "__mir-llvm-asan"]
    } else {
        &["__mir-run"]
    };
    for cmd in cmds {
        let out = karac(name, src, cmd);
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        // Only a failed link may skip: a refusal, a runtime error or a
        // sanitizer report is the program's own.
        if *cmd != "__mir-run" {
            let linked = common::link_or_skip(if out.status.success() {
                Ok(())
            } else {
                Err(err.clone())
            });
            if linked.is_none() {
                continue;
            }
        }
        assert!(out.status.success(), "{name}: {cmd}: {err}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name}: {cmd}");
    }
}

/// `karac check` refuses `src` with `code`, and its message holds `needle`.
fn refused_with(name: &str, src: &str, code: &str, needle: &str) {
    let out = karac(name, src, "check");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{name}: accepted");
    assert!(text.contains(&format!("error[{code}]")), "{name}: {text}");
    assert!(text.contains(needle), "{name}: {text}");
}

const HEAD: &str = "effect resource Db: Store;
trait Store {
    fn get(ref self, k: i64) -> i64;
}
struct M {
    v: i64,
}
impl Store for M {
    fn get(ref self, k: i64) -> i64 {
        self.v + k
    }
}
fn mk(v: i64) -> M {
    M { v: v }
}
";

/// The fn-tail shape with no escape, an annotated `let`, a closure value in
/// `Ok`, the operand of `?`, and `Err` / `None` tails.
#[test]
fn a_with_provider_closure_returning_ok_checks_and_runs() {
    runs_everywhere(
        "ok-shapes",
        &format!(
            "{HEAD}
fn tail() -> Result[i64, String] {{
    with_provider[Db](mk(1), || Ok(Db.get(1)))
}}
fn value() -> Result[Fn() -> i64, String] {{
    with_provider[Db](mk(1), || Ok(|| 3))
}}
fn question() -> Result[i64, String] {{
    let n = with_provider[Db](mk(1), || Ok(Db.get(2)))?;
    Ok(n + 10)
}}
fn failed() -> Result[i64, String] {{
    with_provider[Db](mk(1), || Err(\"bad\".to_string()))
}}
fn nothing() -> Option[i64] {{
    with_provider[Db](mk(1), || None)
}}
fn main() {{
    let r: Result[i64, String] = with_provider[Db](mk(5), || Ok(Db.get(1)));
    match r {{
        Ok(v) => println(v),
        Err(e) => println(e),
    }}
    match tail() {{
        Ok(v) => println(v),
        Err(e) => println(e),
    }}
    match value() {{
        Ok(f) => println(f()),
        Err(e) => println(e),
    }}
    match question() {{
        Ok(v) => println(v),
        Err(e) => println(e),
    }}
    match failed() {{
        Ok(v) => println(v),
        Err(e) => println(e),
    }}
    println(nothing().is_none());
}}
"
        ),
        "6\n2\n3\n13\nbad\ntrue\n",
    );
}

/// The row's program: a closure that uses the provider, returned in `Ok`,
/// is refused with E0600 alone, as in `Some(..)`.
#[test]
fn a_provider_closure_escaping_in_ok_gets_e0600_alone() {
    let src = format!(
        "{HEAD}
fn mkr() -> Result[Fn() -> i64, String] {{
    with_provider[Db](mk(1), || Ok(|| Db.get(1)))
}}
fn mks() -> Option[Fn() -> i64] {{
    with_provider[Db](mk(1), || Some(|| Db.get(1)))
}}
fn main() {{
    match mkr() {{
        Ok(f) => println(f()),
        Err(e) => println(e),
    }}
    match mks() {{
        Some(f) => println(f()),
        None => println(\"n\"),
    }}
}}
"
    );
    let out = karac("escape", &src, "check");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{text}");
    assert_eq!(text.matches("error[E0600]").count(), 2, "{text}");
    assert!(!text.contains("E0516"), "{text}");
    assert!(!text.contains("not lowered yet"), "{text}");
}

/// A closure whose payload disagrees with the slot is a type error at its
/// tail, not MIR's untyped call.
#[test]
fn a_with_provider_closure_giving_the_wrong_payload_is_a_type_error() {
    refused_with(
        "wrong-payload",
        &format!(
            "{HEAD}
fn main() {{
    let r: Result[i64, String] = with_provider[Db](mk(1), || Ok(\"s\"));
    println(r.is_ok());
}}
"
        ),
        "E0200",
        "expected 'Result[i64, String]', found 'Result[String, _]': the closure `with_provider` runs gives this",
    );
}
