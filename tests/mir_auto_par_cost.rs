//! The size half of the auto-par cost gate (M4 item 13): under
//! `KARAC_AUTO_PAR=1` a loop of straight-line iterations, a loop inside
//! another that makes a literal handful of trips, and a run of statements
//! that each make one straight-line pass stay in order, where a loop whose
//! iterations loop runs as tasks. `KARAC_AUTO_PAR_SIZE=0` turns the size
//! half down to the gate it replaced. The output is the sequential
//! program's either way, on the MIR interpreter and on the built binary.
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

const SRC: &str = r#"fn pass(v: ref Vec[i64]) -> i64 {
    let mut s = 0;
    for x in v.iter() {
        s = (s + *x) % 1000003;
    }
    s
}

fn two(v: ref Vec[i64]) -> i64 { // two
    let a = pass(v); // group
    let b = pass(v);
    a * 3 + b
}

fn main() {
    let mut dp: Vec[i64] = Vec.new();
    for i in 0..5000 { // fill
        dp.push(i % 7);
    }
    let t = two(dp);
    let mut out: Vec[i64] = Vec.new();
    for x in 0..40 { // pixels
        for c in 0..3 { // channels
            let mut s = 0;
            for d in 0..3 {
                s = s + (x + c) * d;
            }
            out.push(s);
        }
    }
    let mut sum = 0;
    for r in 0..200 { // rounds
        sum += pass(out) + r;
    }
    println(f"{dp.len()} {t} {out.len()} {sum}");
}
"#;

/// The line of `SRC` that carries `// marker`.
fn line(marker: &str) -> usize {
    SRC.lines()
        .position(|l| l.ends_with(&format!("// {marker}")))
        .expect("no such marker")
        + 1
}

fn karac(auto: bool, size: bool) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_HASH_SEED", "1");
    if auto {
        c.env("KARAC_AUTO_PAR", "1")
            .env("KARAC_AUTO_PAR_DEBUG", "1");
    } else {
        c.env("KARAC_AUTO_PAR", "0");
    }
    if size {
        c.env_remove("KARAC_AUTO_PAR_SIZE");
    } else {
        c.env("KARAC_AUTO_PAR_SIZE", "0");
    }
    c
}

fn source(tag: &str) -> (PathBuf, PathBuf) {
    let dir =
        std::env::temp_dir().join(format!("karac-auto-par-cost-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    (dir, file)
}

fn interp_run(tag: &str, auto: bool, size: bool) -> Output {
    let (dir, file) = source(tag);
    let out = karac(auto, size)
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// `karac build --mir` and the binary run, or `None` when the runtime
/// archive does not link; the build's stderr comes back with it.
fn build_and_run(tag: &str, auto: bool, size: bool) -> Option<(Output, String)> {
    let (dir, file) = source(tag);
    let exe = dir.join("main");
    let built = karac(auto, size)
        .args([
            "build",
            "--mir",
            file.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    assert!(
        built.status.success() || err.contains("Linker failed"),
        "{tag} does not build: {err}"
    );
    common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err.clone())
    })?;
    let out = Command::new(&exe).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    Some((out, err))
}

fn stdout(o: &Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout.clone()).unwrap()
}

/// The sites an auto-par report lists after `prefix`, sorted.
fn listed(report: &str, prefix: &str) -> Vec<String> {
    let mut v: Vec<String> = report
        .lines()
        .find_map(|l| l.strip_prefix(prefix))
        .map(|l| l.split(", ").map(String::from).collect())
        .unwrap_or_default();
    v.sort();
    v
}

#[test]
fn mir_auto_par_small_work_stays_in_order() {
    let want = interp_run("seq", false, true);
    let want = stdout(&want);
    assert_eq!(want.lines().count(), 1, "{want}");
    let par = interp_run("par", true, true);
    assert_eq!(stdout(&par), want);
    let report = String::from_utf8_lossy(&par.stderr).to_string();
    // The pixel loop's iterations loop, so it runs as tasks, its push in
    // the channels loop folded in order; until B-2026-10-26-14 the builder
    // missed a reduction inside an inner loop and kept it in order.
    let mut tasked = vec![
        format!("main@{}", line("pixels")),
        format!("main@{}", line("rounds")),
    ];
    tasked.sort();
    assert_eq!(
        listed(&report, "auto-par: loops as tasks: "),
        tasked,
        "{report}"
    );
    assert!(
        listed(&report, "auto-par: statements as tasks: ").is_empty(),
        "{report}"
    );
    for (at, why) in [
        (
            format!("main@{}", line("fill")),
            "its body neither loops nor calls a body that does",
        ),
        (
            format!("main@{}", line("channels")),
            "it is inside a loop and makes 3 trips",
        ),
        (
            format!("two@{}", line("two")),
            "0 of its 2 statements nest loops or recurse",
        ),
    ] {
        let want = format!("auto-par: in order: {at}: Cost(\"{why}\")");
        assert!(report.lines().any(|l| l == want), "{want} in {report}");
    }
    // With the size half turned down, the fill and the two passes run as
    // tasks too (the channels loop runs inside the pixel loop's tasks), and
    // the output stays the same.
    let free = interp_run("free", true, false);
    assert_eq!(stdout(&free), want);
    let report = String::from_utf8_lossy(&free.stderr).to_string();
    let loops = listed(&report, "auto-par: loops as tasks: ");
    for at in ["fill", "pixels", "rounds"] {
        let at = format!("main@{}", line(at));
        assert!(loops.contains(&at), "{at} in {report}");
    }
    let group = format!("two@{}", line("group"));
    assert!(
        listed(&report, "auto-par: statements as tasks: ").contains(&group),
        "{group} in {report}"
    );
    if let (Some((seq, _)), Some((par, built)), Some((free, _))) = (
        build_and_run("seq-b", false, true),
        build_and_run("par-b", true, true),
        build_and_run("free-b", true, false),
    ) {
        assert_eq!(stdout(&seq), want);
        assert_eq!(stdout(&par), want);
        assert_eq!(stdout(&free), want);
        assert_eq!(listed(&built, "auto-par: loops as tasks: "), tasked);
    }
}
