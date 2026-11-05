//! Two `par` branches that each apply `?` to a `Result[D, E]` whose `D` has a
//! printing `Drop` drop nothing inside the branches, so they do not conflict
//! (B-2026-11-05-5). The residual drop of the `?` temporary switches on its
//! discriminant again after the `Err` arm took the payload, and its `Ok` arm,
//! which drops a `D` that path never holds, charged `writes(Stdout)` to each
//! branch. Branches that do drop a `D` still conflict.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

const HEAD: &str = "struct D { t: String }
impl Drop for D { fn drop(mut ref self) { println(f\"drop {self.t}\"); } }
";

fn write(name: &str, body: &str) -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("karac-par-try-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, format!("{HEAD}{body}")).unwrap();
    path
}

#[test]
fn par_branches_applying_try_to_an_effectful_drop_do_not_conflict() {
    let ok = write(
        "ok",
        "fn mk(t: own String, ok: bool) -> Result[D, String] { if ok { Ok(D { t: t }) } else { Err(t) } }
fn p(x: bool) -> Result[i64, String] { let (a, b) = par { mk(\"a\".to_string(), true)?, mk(\"b\".to_string(), x)? }; Ok(1) }
fn main() { println(p(true).is_ok()); println(p(false).is_ok()); }
",
    );
    let bad = write(
        "bad",
        "fn mk2(t: own String) -> D { D { t: t } }
fn q() -> i64 { let (a, b) = par { { let d = mk2(\"a\".to_string()); 1 }, { let e = mk2(\"b\".to_string()); 2 } }; a + b }
fn main() { println(q()); }
",
    );
    let check = karac(&["check", ok.to_str().unwrap()]);
    let run = karac(&["__mir-run", ok.to_str().unwrap()]);
    let refused = karac(&["check", bad.to_str().unwrap()]);
    let _ = std::fs::remove_file(&ok);
    let _ = std::fs::remove_file(&bad);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "drop b\ndrop a\ntrue\ndrop a\nfalse\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let err = String::from_utf8_lossy(&refused.stderr);
    assert!(
        err.contains(":4:30: branches 1 and 2 of the `par` conflict: `writes(Stdout)`"),
        "{err}"
    );
}
