//! B-2026-11-08-39 — E0411 (a target does not provide a resource the call
//! chain uses) cites the section that states the rule, design.md
//! § Effect-driven target gating, and names binding a provider as a remedy
//! only for a resource a provider can be bound for: none stands in for
//! `ProcessTable`.

use std::process::Command;

fn check(tag: &str, src: &str, target: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-target-gate-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([
            "check",
            path.to_str().unwrap(),
            &format!("--targets={target}"),
        ])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_process_on_wasm_names_no_provider_and_the_gating_section() {
    let out = check(
        "process",
        "import std.process.{Command};\nfn run_it() -> i64 { match Command.new(\"echo\").arg(\"x\").output() { Ok(o) => o.stdout.len(), Err(_) => -1 } }\nfn main() { println(run_it()); }\n",
        "wasm_wasi",
    );
    assert!(
        out.contains("does not provide resource 'ProcessTable'")
            && out.contains("main → run_it → Command.output")
            && out.contains("(design.md § Effect-driven target gating)"),
        "{out}"
    );
    assert!(!out.contains("bind a provider"), "{out}");
    assert!(!out.contains("Cross-target Compilation"), "{out}");
}

#[test]
fn a_file_read_in_the_browser_names_its_provider() {
    let out = check(
        "fs",
        "fn load() -> bool { fs.read_to_string(\"a.txt\").is_ok() }\nfn main() { println(load()); }\n",
        "wasm_browser",
    );
    assert!(
        out.contains("does not provide resource 'FileSystem'")
            && out.contains("bind a provider for 'FileSystem'")
            && out.contains("(design.md § Effect-driven target gating)"),
        "{out}"
    );
}
