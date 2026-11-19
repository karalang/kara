//! A bound that writes no arguments names its trait at the trait's defaults
//! (B-2026-11-19-66). With `trait Combine[Rhs = Self]` and a type carrying
//! both `impl Combine[i64]` and `impl Combine`, a call under `T: Combine`
//! ran whichever impl came first: with `Combine[i64]` first, compiled code
//! read the `M` it was handed as the `i64` (printing 6 for 2 + 3) and the
//! interpreter stopped on `Mul` of an integer and a struct. The checker read
//! `T.Output` from whichever impl came last, so with `impl Combine` first it
//! refused `let r: M = total(..)` and typed `total(..) + 1` as an `i64`
//! addition on an `M`. Each program runs on the interpreter and, with the
//! backend, compiled.

use std::path::PathBuf;
use std::process::{Command, Output};

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-defaulted-trait-param-{}-{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path: PathBuf = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .output()
        .unwrap()
}

fn backends() -> &'static [&'static str] {
    if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    }
}

fn runs_as(tag: &str, src: &str, want: &str) {
    for b in backends() {
        let out = karac(tag, &[b], src);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{tag} on {b}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.status.success(),
            "{tag} on {b} exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const TRAIT: &str = "trait Combine[Rhs = Self] { type Output; fn combine(own self, rhs: own Rhs) -> Self.Output; }\n\
     struct M { c: i64 }\n";
const AT_I64: &str = "impl Combine[i64] for M { type Output = i64; fn combine(own self, rhs: own i64) -> i64 { self.c * rhs } }\n";
const AT_SELF: &str = "impl Combine for M { type Output = M; fn combine(own self, rhs: own M) -> M { M { c: self.c + rhs.c } } }\n";
const AT_M: &str = "impl Combine[M] for M { type Output = M; fn combine(own self, rhs: own M) -> M { M { c: self.c + rhs.c } } }\n";
const TOTAL: &str = "fn total[T: Combine](a: own T, b: own T) -> T.Output { a.combine(b) }\n";

#[test]
fn a_bare_bound_runs_the_impl_at_the_defaults_declared_second() {
    runs_as(
        "second",
        &format!("{TRAIT}{AT_I64}{AT_SELF}{TOTAL}fn main() {{ println(total(M {{ c: 2 }}, M {{ c: 3 }}).c); }}\n"),
        "5\n",
    );
}

#[test]
fn a_bare_bound_reads_the_output_at_the_defaults_declared_first() {
    runs_as(
        "first",
        &format!("{TRAIT}{AT_SELF}{AT_I64}{TOTAL}fn main() {{ let r: M = total(M {{ c: 2 }}, M {{ c: 3 }}); println(r.c); }}\n"),
        "5\n",
    );
}

#[test]
fn a_bound_written_self_runs_the_impl_that_writes_none() {
    runs_as(
        "self-arg",
        &format!(
            "{TRAIT}{AT_I64}{AT_SELF}fn total[T: Combine[T]](a: own T, b: own T) -> T.Output {{ a.combine(b) }}\n\
             fn main() {{ println(total(M {{ c: 2 }}, M {{ c: 3 }}).c); }}\n"
        ),
        "5\n",
    );
}

#[test]
fn a_bare_bound_runs_the_impl_that_writes_the_default() {
    runs_as(
        "written-default",
        &format!("{TRAIT}{AT_I64}{AT_M}{TOTAL}fn main() {{ println(total(M {{ c: 2 }}, M {{ c: 3 }}).c); }}\n"),
        "5\n",
    );
}

#[test]
fn a_bound_with_arguments_still_runs_the_impl_it_names() {
    runs_as(
        "named",
        &format!(
            "{TRAIT}{AT_SELF}{AT_I64}fn total[T: Combine[i64]](a: own T, b: i64) -> T.Output {{ a.combine(b) }}\n\
             fn main() {{ println(total(M {{ c: 2 }}, 3)); }}\n"
        ),
        "6\n",
    );
}

#[test]
fn a_bare_bound_output_is_not_the_other_impls() {
    let src = format!(
        "{TRAIT}{AT_SELF}{AT_I64}{TOTAL}fn main() {{ let r = total(M {{ c: 2 }}, M {{ c: 3 }}); println(r + 1); }}\n"
    );
    let out = karac("not-i64", &["check"], &src);
    assert!(!out.status.success(), "check accepted an M + 1");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("does not implement trait Add"), "{err}");
}
