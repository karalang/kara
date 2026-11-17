//! B-2026-11-16-313, -314: `--target=wasm32-wasip1` is the triple
//! `wasm_wasi` builds, so it builds a wasm module; every other wasm triple
//! (the threaded one included) is refused naming `--target=wasm_wasi`,
//! where it was linked as a native triple and failed in `wasm-ld` on
//! `crt1.o` and `-lpthread`. `karac build --help` marks `wasm_browser`
//! Not in 0.8, as the build refuses it.

use std::path::PathBuf;
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-wasm-triples-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("h.kara"), "fn main() {\n    println(\"hi\");\n}\n").unwrap();
    dir
}

fn karac(dir: &PathBuf, args: &[&str]) -> (Option<i32>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    (
        out.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn wasm32_wasip1_builds_as_wasm_wasi() {
    let dir = scratch("wasip1");
    for flag in [
        &["--target=wasm32-wasip1"][..],
        &["--target", "wasm32-wasip1"][..],
    ] {
        let mut args = vec!["build", "h.kara"];
        args.extend_from_slice(flag);
        let (_, out) = karac(&dir, &args);
        // With the backend the module is built (or the wasm archive is
        // named as missing); without it the check says where the module
        // would go. Either way it is the wasm build, not a native link.
        assert!(
            out.contains("h.wasm")
                || dir.join("h.wasm").exists()
                || out.contains("libkarac_runtime_wasm"),
            "{flag:?}: {out}"
        );
        assert!(!out.contains("crt1.o"), "{flag:?}: {out}");
        assert!(!out.contains("lpthread"), "{flag:?}: {out}");
        assert!(!out.contains("is a wasm triple"), "{flag:?}: {out}");
        let _ = std::fs::remove_file(dir.join("h.wasm"));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn other_wasm_triples_are_refused_naming_wasm_wasi() {
    let dir = scratch("others");
    for (triple, threads) in [
        ("wasm32-wasip1-threads", true),
        ("wasm32-unknown-unknown", false),
    ] {
        let (code, out) = karac(&dir, &["build", "h.kara", &format!("--target={triple}")]);
        assert_eq!(code, Some(1), "{triple}: {out}");
        assert!(
            out.contains(&format!("`{triple}` is a wasm triple")),
            "{triple}: {out}"
        );
        assert!(out.contains("--target=wasm_wasi"), "{triple}: {out}");
        assert_eq!(out.contains("Threaded wasm"), threads, "{triple}: {out}");
        assert!(!out.contains("crt1.o"), "{triple}: {out}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_help_marks_wasm_browser_not_in_0_8() {
    let dir = scratch("help");
    let (code, out) = karac(&dir, &["build", "--help"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("wasm_browser is Not in\n"), "{out}");
    assert!(
        out.contains("`--target=wasm_browser` (the `.js` glue"),
        "{out}"
    );
    assert!(
        out.contains("is Not in 0.8: `karac build` and `karac\n"),
        "{out}"
    );
    assert!(!out.contains("`--target=wasm_browser` emits"), "{out}");
    assert!(
        !out.contains("`--target=wasm_browser` builds the same"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
