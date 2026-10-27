//! A `--target=wasm_wasi` build keeps every loop in order under the default
//! `KARAC_AUTO_PAR`: the wasm pool runs one worker, where tasks only cost
//! (kata 28's brute force ran 3.1x slower as tasks there). The same source
//! built for the host runs its loop as tasks. B-2026-10-26-17.
#![cfg(feature = "llvm")]

const SRC: &str = r#"fn count(v: ref Vec[i64], k: i64) -> i64 {
    let mut s = 0;
    let mut i = 0;
    while i < v.len() {
        if v[i] % 7 == k {
            s = s + 1;
        }
        i = i + 1;
    }
    s
}

fn main() {
    let v: Vec[i64] = Vec.filled(1000, 3);
    let mut sum = 0;
    for k in 0..100 {
        sum = sum + count(v, k % 7);
    }
    println(sum);
}
"#;

/// The auto-par report of building `SRC`, with `target` when given.
fn report(target: Option<&str>) -> String {
    let dir = std::env::temp_dir().join(format!(
        "karac-auto-par-wasm-{}-{}",
        target.unwrap_or("host"),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env_remove("KARAC_AUTO_PAR")
        .env("KARAC_AUTO_PAR_DEBUG", "1")
        .arg("build");
    if let Some(t) = target {
        c.arg(format!("--target={t}"));
    }
    let out = c
        .arg(file.to_str().unwrap())
        .arg("-o")
        .arg(dir.join("main.out").to_str().unwrap())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    // The report comes before the link, so a missing runtime archive does
    // not hide it.
    assert!(
        out.status.success() || err.contains("Linker failed") || err.contains("not found"),
        "does not build: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    err
}

#[test]
fn mir_auto_par_keeps_wasm_loops_in_order() {
    let host = report(None);
    assert!(host.contains("auto-par: loops as tasks: main@"), "{host}");
    let wasm = report(Some("wasm_wasi"));
    assert!(!wasm.contains("auto-par:"), "{wasm}");
}
