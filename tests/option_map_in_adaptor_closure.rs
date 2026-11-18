//! B-2026-11-18-78: since B-2026-11-15-32 a closure whose body is
//! `x.map(|u| ... capture ...)` was read as returning the inner closure,
//! because `map` is a lazy adaptor's name, so the capture moved into it
//! (core semantics §9.3) and the outer closure became once-only: E0235 in
//! an `Iterator.map` slot, and E0500 at a later use of the capture. On an
//! `Option` or `Result` receiver `map` runs the closure at once and returns
//! a value, so nothing is returned and the capture is only read. Found by
//! apps/cli/mini_timeseries.

use std::process::Command;

const SRC: &str = r#"struct T { v: Vec[Option[i64]], tz: Option[String] }
fn shift(tz: Option[String], u: i64) -> i64 { if tz.is_some() { u + 10 } else { u } }
fn half(r: own Result[i64, String], k: String) -> Result[i64, String] { r.map(|u| u + k.len()) }
fn main() {
    let t = T { v: [Some(1), None, Some(3)].to_vec(), tz: Some("ab".to_string()) };
    let out: Vec[Option[i64]] = t.v.iter().map(|x| x.map(|u| shift(t.tz, u))).collect();
    println(f"{out} {t.tz}");
    let k = "abc".to_string();
    let rs: Vec[Result[i64, String]] = [Ok(1), Err("e".to_string())].to_vec();
    let got: Vec[Result[i64, String]] = rs.iter().map(|r| r.clone().map(|u| u + k.len())).collect();
    println(f"{got} {k} {half(Ok(2), k)}");
}"#;

fn run(backend: &str) -> String {
    let path =
        std::env::temp_dir().join(format!("karac-b78-{backend}-{}.kara", std::process::id()));
    std::fs::write(&path, SRC).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(backend)
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "{backend}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const WANT: &str = "[Some(11), None, Some(13)] Some(ab)\n[Ok(4), Err(e)] abc Ok(5)\n";

#[test]
fn option_map_inside_an_iterator_map_closure_only_reads_its_capture() {
    assert_eq!(run("__mir-run"), WANT);
    #[cfg(feature = "llvm")]
    assert_eq!(run("__mir-llvm-run"), WANT);
}
