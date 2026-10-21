//! `String.normalize(form)` on the v2 backends (B-2026-10-21-23): the MIR
//! interpreter and, compiled, against the opt-in unicode archive, both
//! through `icu_normalizer`. The expected output is docs/library/strings.md's
//! and design.md § Strings (Equality)'s: `==` compares bytes, and two
//! spellings of one text are equal once both are normalized to one form.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "karac-mir-normalize-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The program's output on the compiled backend at `opt`; `None` when the
/// runtime archive is absent.
fn build_and_run(src: &str, opt: u8) -> Option<String> {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::codegen::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = Scratch::new();
    let obj = dir.path().join("main.o");
    let exe = dir.path().join("main");
    karac::codegen::mir_aot::compile_object(&ir, &obj, opt).expect("object");
    common::link_or_skip(karac::codegen::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ))?;
    let out = Command::new(&exe).output().expect("run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).expect("utf-8"))
}

/// Every form, on a `String` and on a `Str`, an empty text, and a result
/// longer and shorter than its input.
const FORMS: &str = r#"fn show(s: Str) -> String {
    let mut out = String.new();
    for c in s.chars() {
        out.push_str(f"{c as u32} ");
    }
    out
}
fn main() {
    let a = "caf\u{e9}";
    let b = "cafe\u{301}";
    println(f"{a == b}");
    println(f"{a.normalize(NormalizationForm.Nfc) == b.normalize(NormalizationForm.Nfc)}");
    println(f"{a.normalize(NormalizationForm.Nfd) == b.normalize(NormalizationForm.Nfd)}");
    println(f"{a.normalize(NormalizationForm.Nfd).len()} {b.normalize(NormalizationForm.Nfc).len()}");
    let lig = "\u{fb01}x".to_string();
    println(show(lig.normalize(NormalizationForm.Nfc).as_str()));
    println(show(lig.normalize(NormalizationForm.Nfkc).as_str()));
    println(show(lig.normalize(NormalizationForm.Nfkd).as_str()));
    let v: Str = "e\u{301}";
    println(show(v.normalize(NormalizationForm.Nfc).as_str()));
    println(f"[{"".normalize(NormalizationForm.Nfc)}]");
}
"#;

const WANT_FORMS: &str = "false\ntrue\ntrue\n6 5\n64257 120 \n102 105 120 \n\
                          102 105 120 \n233 \n[]\n";

#[test]
fn normalize_agrees_on_both_backends() {
    let interp = karac::mir::lower::run_source(FORMS).expect("MIR interpreter");
    assert_eq!(interp.output, WANT_FORMS, "on the interpreter");
    for opt in [0, 2] {
        if let Some(out) = build_and_run(FORMS, opt) {
            assert_eq!(out, WANT_FORMS, "compiled at -O{opt}");
        }
    }
}

/// The compiled program has no leak, double free or use-after-free under
/// the ASan lane.
#[test]
fn normalize_is_clean_under_asan() {
    let dir = Scratch::new();
    let src = dir.path().join("forms.kara");
    std::fs::write(&src, FORMS).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-llvm-asan")
        .arg(&src)
        .output()
        .expect("karac");
    let err = String::from_utf8_lossy(&out.stderr);
    if err.contains("not found") || err.contains("fsanitize") {
        eprintln!("skipping: no ASan toolchain or runtime archive: {err}");
        return;
    }
    assert!(!err.contains("Sanitizer"), "{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), WANT_FORMS, "{err}");
}
