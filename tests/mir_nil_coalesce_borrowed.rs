//! `??` on a borrowed `Option` or `Result` (a bare `for` element, a bare
//! parameter under D5, a `ref` parameter) yields its payload by value
//! (core-semantics §3.7). A `Copy` payload is copied out of the borrow; a
//! non-`Copy` one is refused by the check at the operand with `.clone()`
//! as the fix. Before, the `for` element passed the check and failed the
//! build (B-2026-10-29-10), and the parameter was refused as "not an
//! `Option`" with the help to drop the `??` (B-2026-10-28-32). Outputs are
//! worked out by hand.
//!
//! With `--features llvm` each program is also compiled, at -O0 and -O2,
//! and must print the same.

#[cfg(feature = "llvm")]
mod common;

use karac::mir::interp::Outcome;

/// The interpreter prints `want` and finishes with nothing leaked, and so
/// does the program compiled.
fn runs(src: &str, want: &str) {
    let run = karac::mir::lower::run_source(src).expect("MIR");
    assert!(
        matches!(run.outcome, Outcome::Returned(_)),
        "{:?}\n{}",
        run.outcome,
        run.output
    );
    assert_eq!(run.output, want);
    #[cfg(feature = "llvm")]
    for opt in [0, 2] {
        if let Some((ok, out)) = compiled::build_and_run(src, opt) {
            assert!(ok, "at -O{opt}: {out}");
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[cfg(feature = "llvm")]
mod compiled {
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// What the program prints compiled at `opt`, and whether it exited
    /// cleanly; `None` when there is no runtime archive to link.
    pub fn build_and_run(src: &str, opt: u8) -> Option<(bool, String)> {
        static N: AtomicUsize = AtomicUsize::new(0);
        let lowered = karac::mir::lower::build_source(src).expect("MIR");
        karac::mir::lower::compiled_check(&lowered).expect("checks");
        let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("karac-nil-borrow-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let (obj, exe) = (dir.join("main.o"), dir.join("main"));
        karac::backend::mir_aot::compile_object(&ir, &obj, opt).expect("object");
        let linked = super::common::link_or_skip(karac::backend::driver::link_executable(
            &obj.to_string_lossy(),
            &exe.to_string_lossy(),
        ));
        let out = linked.map(|()| Command::new(&exe).output().expect("run"));
        let _ = std::fs::remove_dir_all(&dir);
        let out = out?;
        Some((
            out.status.success(),
            String::from_utf8(out.stdout).expect("utf-8"),
        ))
    }
}

/// Writes `src` to a scratch file named for `tag` and runs `karac` with
/// `args` on it; the file's text after the run and the command's stderr.
fn karac(tag: &str, args: &[&str], src: &str) -> (bool, String, String) {
    let path = std::env::temp_dir().join(format!(
        "karac-nil-borrow-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .output()
        .expect("karac");
    let text = std::fs::read_to_string(&path).expect("read");
    let _ = std::fs::remove_file(&path);
    (
        out.status.success(),
        text,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const FOR_ELEMENT: &str = r#"fn main() {
    let v: Vec[Option[String]] = vec![Some("a".to_string()), None];
    let mut out: Vec[String] = Vec.new();
    for n in v {
        out.push(n ?? String.new());
    }
    println(out.join(","));
}
"#;

const BARE_PARAMETER: &str = r#"fn label(prefix: Option[String], n: i64) -> String {
    let p = prefix ?? "x".to_string();
    f"{p}{n}"
}
fn main() {
    let p: Option[String] = Some("k".to_string());
    println(label(p, 1));
    println(label(None, 2));
}
"#;

/// A bare `for` element over a `Vec[Option[String]]` and a bare
/// `Option[String]` parameter are refused at the operand, and `karac fix`
/// writes the `.clone()` that makes them check.
#[test]
fn nil_coalesce_refuses_a_borrowed_non_copy_payload() {
    for (tag, src, at, fixed) in [
        (
            "for",
            FOR_ELEMENT,
            ":5:18: '??' on a borrowed 'Option[String]' would move its 'String' out of the borrow",
            "out.push(n.clone() ?? String.new());",
        ),
        (
            "param",
            BARE_PARAMETER,
            ":2:13: '??' on a borrowed 'Option[String]' would move its 'String' out of the borrow",
            "let p = prefix.clone() ?? \"x\".to_string();",
        ),
    ] {
        let (ok, _, err) = karac(tag, &["check"], src);
        assert!(!ok, "{tag}: checked");
        assert!(err.contains(at), "{tag}: {err}");
        assert!(!err.contains("needs no fallback"), "{tag}: {err}");
        let (_, text, err) = karac(tag, &["fix"], src);
        assert!(text.contains(fixed), "{tag}: {err}\n{text}");
    }
}

/// The fixed programs run.
#[test]
fn nil_coalesce_runs_on_a_cloned_operand() {
    runs(&FOR_ELEMENT.replace("n ?? ", "n.clone() ?? "), "a,\n");
    runs(
        &BARE_PARAMETER.replace("prefix ?? ", "prefix.clone() ?? "),
        "k1\nx2\n",
    );
}

/// A borrowed wrapper whose payload is `Copy` copies it out: a bare
/// `Result[i64, String]` parameter, a `ref Option[i64]` read through `*`,
/// and a `for` element over a `Vec[Option[i64]]`.
#[test]
fn nil_coalesce_copies_a_copy_payload_out_of_a_borrow() {
    runs(
        r#"fn total(r: Result[i64, String], o: ref Option[i64]) -> i64 {
    (r ?? 0) + (*o ?? 1)
}
fn main() {
    let r: Result[i64, String] = Err("e".to_string());
    let o: Option[i64] = None;
    println(f"{total(r, o)}");
    let w: Vec[Option[i64]] = vec![Some(3), None];
    let mut s = 0;
    for x in w { s += x ?? 10; }
    println(f"{s}");
}
"#,
        "1\n13\n",
    );
}
