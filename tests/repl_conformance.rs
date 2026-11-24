//! The REPL cells under `conformance/repl/` (README there): each session's
//! stdout and stderr, and what `:save` writes and the saved file prints
//! (B-2026-11-24-11).

use std::path::Path;
use std::process::{Command, Stdio};

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn read(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok()
}

#[test]
fn repl_conformance_cells() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/repl");
    let mut cases: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "repl"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 10, "only {} cells found", cases.len());
    let mut failures = Vec::new();
    for case in &cases {
        let name = case.file_stem().unwrap().to_string_lossy().to_string();
        let work =
            std::env::temp_dir().join(format!("karac-repl-conf-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).unwrap();
        let args = read(&case.with_extension("args")).unwrap_or_default();
        let mut child = karac()
            .arg("repl")
            .args(args.split_whitespace())
            .current_dir(&work)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(read(case).unwrap().as_bytes()).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let want_out = read(&case.with_extension("out")).expect("a .out file");
        let want_err = read(&case.with_extension("err")).unwrap_or_default();
        if stdout != want_out {
            failures.push(format!(
                "{name}: stdout\n--- want\n{want_out}--- got\n{stdout}"
            ));
        }
        if stderr != want_err {
            failures.push(format!(
                "{name}: stderr\n--- want\n{want_err}--- got\n{stderr}"
            ));
        }
        if let Some(want_saved) = read(&case.with_extension("saved")) {
            let saved = read(&work.join("saved.kara")).unwrap_or_default();
            if saved != want_saved {
                failures.push(format!(
                    "{name}: saved file\n--- want\n{want_saved}--- got\n{saved}"
                ));
            }
            let run = karac()
                .args(["run", "saved.kara"])
                .current_dir(&work)
                .output()
                .unwrap();
            let got = String::from_utf8_lossy(&run.stdout).to_string()
                + &String::from_utf8_lossy(&run.stderr);
            let want = read(&case.with_extension("savedout")).unwrap_or_default();
            if got != want {
                failures.push(format!("{name}: saved run\n--- want\n{want}--- got\n{got}"));
            }
        }
        let _ = std::fs::remove_dir_all(&work);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
