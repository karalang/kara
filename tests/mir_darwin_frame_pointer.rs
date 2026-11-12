//! B-2026-11-12-10: Apple's ABIs require every frame to keep a frame record
//! linked through the frame pointer (`x29` on arm64, `rbp` on x86_64),
//! which `sample`, Instruments and crash reports walk without unwind
//! tables. karac emitted none on either Apple triple: a function saved
//! `x29` and never pointed it at its own record, so a backtrace through
//! Kāra code skipped every Kāra frame. Each defined function now carries
//! the triple's `frame-pointer` attribute (arm64 `non-leaf`, x86_64 `all`,
//! rustc's and clang's defaults); other triples are unchanged.
//!
//! The IR is the copy `KARAC_MIR_LL` keeps of a cross build. The link is
//! `true`, so neither the target's linker nor its runtime archive is needed.

use std::path::Path;
use std::process::Command;

const PROGRAM: &str = "fn depth(n: i64) -> i64 {\n    if n == 0 {\n        return 0;\n    }\n    1 + depth(n - 1)\n}\n\nfn main() {\n    println(f\"{depth(3)}\");\n}\n";

/// The IR of `PROGRAM` built for `triple` (the host's when `None`).
fn ir_for(tag: &str, triple: Option<&str>) -> Option<String> {
    let dir = std::env::temp_dir().join(format!("karac-frame-ptr-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("fp.kara"), PROGRAM).unwrap();
    let ll = dir.join("fp.ll");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.current_dir(&dir)
        .arg("build")
        .arg("fp.kara")
        .env("KARAC_MIR_LL", &ll)
        .env_remove("KARAC_MIR_FRAME_POINTER");
    if let Some(triple) = triple {
        cmd.arg(format!("--target={triple}"))
            .env("KARAC_LINKER", "true")
            .env(
                "KARAC_RUNTIME",
                Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
            );
    }
    let out = cmd.output().unwrap();
    let ir = std::fs::read_to_string(&ll).ok();
    let _ = std::fs::remove_dir_all(&dir);
    // The copy is written before the link, so only a failed code
    // generation leaves none.
    assert!(
        ir.is_some(),
        "{tag}: no IR written: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    ir
}

/// The `frame-pointer` value of `@kara.depth`'s attribute group.
fn depth_frame_pointer(ir: &str) -> Option<String> {
    let def = ir
        .lines()
        .find(|l| l.starts_with("define") && l.contains("@kara.depth("))?;
    let group = def.rsplit('#').next()?.split_whitespace().next()?;
    let attrs = ir
        .lines()
        .find(|l| l.starts_with(&format!("attributes #{group} ")))?;
    let at = attrs.find("\"frame-pointer\"=\"")? + "\"frame-pointer\"=\"".len();
    Some(attrs[at..].split('"').next()?.to_string())
}

#[test]
fn darwin_frame_pointer_is_non_leaf_on_arm64_and_all_on_x86_64() {
    if !cfg!(feature = "llvm") || !cfg!(unix) {
        return;
    }
    let arm = ir_for("arm64", Some("aarch64-apple-darwin")).unwrap();
    assert_eq!(
        depth_frame_pointer(&arm).as_deref(),
        Some("non-leaf"),
        "{arm}"
    );
    let x86 = ir_for("x86", Some("x86_64-apple-darwin")).unwrap();
    assert_eq!(depth_frame_pointer(&x86).as_deref(), Some("all"), "{x86}");
}

#[test]
fn linux_build_names_no_frame_pointer() {
    if !cfg!(feature = "llvm") || !cfg!(unix) {
        return;
    }
    let linux = ir_for("linux", Some("x86_64-unknown-linux-gnu")).unwrap();
    assert!(linux.contains("@kara.depth("), "{linux}");
    assert_eq!(depth_frame_pointer(&linux), None, "{linux}");
}
