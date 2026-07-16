//! Differential BEHAVIORAL oracle for the self-hosted codegen emitter
//! (`selfhost/src/codegen.kara`, Phase 12 Codegen port).
//!
//! Unlike the front-end oracles (which diff a canonical render byte-for-byte),
//! LLVM IR text is not reproducible character-for-character — SSA value
//! numbering and block labels are construction-order artifacts. So the gate
//! here is PROGRAM-OUTPUT parity: for each source program, emit IR with the
//! Kāra emitter, run that IR via `karac_jit_runner`, and assert its stdout +
//! exit status match the seed's `karac run` on the same source.
//!
//! Slice 1 surface: a `main` of `println("literal")` statements.
//!
//! Requires `--features llvm` (the JIT runner + codegen). Skips benignly if the
//! selfhost driver can't link (no runtime archive), never on a compiler panic.
#![cfg(feature = "llvm")]

use std::path::PathBuf;
use std::process::Command;

/// Programs whose emitted IR must run identically to `karac run`.
const CORPUS: &[&str] = &[
    "fn main() { println(\"hi\") }",
    "fn main() { println(\"hello\"); println(\"world\") }",
    "fn main() { println(\"\") }",
    "fn main() { println(\"a b c\") }",
    "fn main() { println(\"tab\tand\tspaces\") }",
    "fn main() { println(\"unicode: \u{e9}\u{2192}\") }",
    "fn main() { }",
    "fn main() { println(\"one\"); println(\"two\"); println(\"three\") }",
    // Slice 2: integer literals + arithmetic, formatted via `.to_string()`.
    "fn main() { println((2 + 3).to_string()) }",
    "fn main() { println(42.to_string()) }",
    "fn main() { println((10 - 4).to_string()) }",
    "fn main() { println((6 * 7).to_string()) }",
    "fn main() { println((2 + 3 * 4).to_string()) }",
    "fn main() { println((0 - 5).to_string()) }",
    "fn main() { println(\"n = \"); println((1 + 1).to_string()) }",
    // Slice 3: let bindings, local reads, assignment, shadowing.
    "fn main() { let x = 5; println(x.to_string()) }",
    "fn main() { let x = 2; let y = 3; println((x + y).to_string()) }",
    "fn main() { let x = 2; let y = x * 10; println((y + x).to_string()) }",
    "fn main() { let mut x = 1; x = x + 41; println(x.to_string()) }",
    "fn main() { let x = 1; let x = x + 1; println(x.to_string()) }",
    "fn main() { let mut a = 10; a = a - 3; a = a * 2; println(a.to_string()) }",
];

const ENTRY: &str = ";;;KARA_ENTRY;;;";

fn kara_str_lit(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Build the selfhost modules + a driver that emits IR (separated by `ENTRY`)
/// for every corpus program, run it, and return the raw stdout — or `None` on a
/// benign link skip.
fn build_and_emit_all() -> Option<String> {
    let tmp = std::env::temp_dir().join(format!("karac-selfhost-codegen-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        "[package]\nname = \"cg\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let selfhost_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("selfhost/src");
    for f in [
        "span.kara",
        "token.kara",
        "lexer.kara",
        "ast.kara",
        "parser.kara",
        "codegen.kara",
    ] {
        std::fs::copy(selfhost_src.join(f), tmp.join("src").join(f))
            .unwrap_or_else(|e| panic!("copy selfhost module {f}: {e}"));
    }

    let mut driver = String::from(
        "import parser.parse_program;\n\
         import codegen.emit_program;\n\
         \n\
         fn dump(src: String) with panics {\n\
         \x20   println(\";;;KARA_ENTRY;;;\");\n\
         \x20   print(emit_program(parse_program(src)));\n\
         }\n\
         fn main() {\n",
    );
    for input in CORPUS {
        driver.push_str(&format!("    dump(\"{}\");\n", kara_str_lit(input)));
    }
    driver.push_str("}\n");
    std::fs::write(tmp.join("src").join("main.kara"), &driver).unwrap();

    let build = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&tmp)
        .args(["build"])
        .env_remove("KARAC_RUNTIME")
        .output()
        .expect("spawn karac build");
    let berr = String::from_utf8_lossy(&build.stderr);
    let bin = tmp.join("cg");
    if !bin.exists() {
        let crashed = berr.contains("panicked at") || build.status.code().is_none();
        let compile_err = crashed
            || berr.contains("error[")
            || berr.contains("codegen failed")
            || berr.contains("parse error")
            || berr.contains("Module verification failed");
        assert!(
            !compile_err,
            "self-hosted emitter FAILED TO COMPILE (port regression):\n{berr}\n\
             --- driver ---\n{driver}"
        );
        eprintln!("skip: selfhost codegen oracle — driver did not link:\n{berr}");
        let _ = std::fs::remove_dir_all(&tmp);
        return None;
    }
    let run = Command::new(&bin).output().expect("run emitter driver");
    assert!(
        run.status.success(),
        "emitter driver exited nonzero:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let out = String::from_utf8_lossy(&run.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&tmp);
    Some(out)
}

/// Run LLVM IR text through `karac_jit_runner`, returning (stdout, exit code).
fn run_ir(ir: &str) -> (String, i32) {
    let tmp = std::env::temp_dir().join(format!("karac-cg-ir-{}.ll", std::process::id()));
    std::fs::write(&tmp, ir).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac_jit_runner"))
        .arg(&tmp)
        .output()
        .expect("spawn karac_jit_runner");
    let _ = std::fs::remove_file(&tmp);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// Run a source program through the seed's `karac run`, returning (stdout, code).
fn seed_run(src: &str) -> (String, i32) {
    let tmp = std::env::temp_dir().join(format!("karac-cg-seed-{}.kara", std::process::id()));
    std::fs::write(&tmp, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["run", tmp.to_str().unwrap()])
        .output()
        .expect("spawn karac run");
    let _ = std::fs::remove_file(&tmp);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn selfhost_codegen_matches_seed_run() {
    let Some(all) = build_and_emit_all() else {
        return;
    };
    // Split the driver's stdout into per-program IR blocks.
    let blocks: Vec<&str> = all.split(ENTRY).skip(1).collect();
    assert_eq!(
        blocks.len(),
        CORPUS.len(),
        "expected {} IR blocks, got {}",
        CORPUS.len(),
        blocks.len()
    );
    for (i, (src, ir)) in CORPUS.iter().zip(blocks.iter()).enumerate() {
        let ir = ir.trim_start_matches('\n');
        let (kara_out, kara_code) = run_ir(ir);
        let (seed_out, seed_code) = seed_run(src);
        assert_eq!(
            kara_out, seed_out,
            "stdout mismatch at corpus[{i}] ({src:?}):\n  Kāra-emitted: {kara_out:?}\n  \
             seed run:     {seed_out:?}\n--- emitted IR ---\n{ir}"
        );
        assert_eq!(
            kara_code, seed_code,
            "exit-code mismatch at corpus[{i}] ({src:?}): Kāra {kara_code} vs seed {seed_code}"
        );
    }
}
