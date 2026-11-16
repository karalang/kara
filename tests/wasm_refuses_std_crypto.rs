//! `std.crypto` is native code only the native runtime carries, so a wasm
//! target refuses its import at check, naming the module and the target,
//! instead of failing at wasm-ld (B-2026-11-16-197).

use std::process::{Command, Output};

fn karac_check(target: Option<&str>, src: &str, name: &str) -> Output {
    let dir = std::env::temp_dir().join(format!("karac-wasm-crypto-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.arg("check");
    if let Some(t) = target {
        cmd.arg(format!("--target={t}"));
    }
    cmd.arg(&path).output().unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

const SHA: &str = "import std.crypto.sha2.{sha256};
fn main() { println(sha256(\"abc\".bytes())[0]); }
";

const CT: &str = "import std.crypto.{constant_time_eq};
fn main() { println(constant_time_eq(\"a\".bytes(), \"a\".bytes())); }
";

#[test]
fn wasm_targets_refuse_a_std_crypto_import_by_name() {
    for target in ["wasm_wasi"] {
        for (name, src, module) in [
            ("sha.kara", SHA, "std.crypto.sha2"),
            ("ct.kara", CT, "std.crypto"),
        ] {
            let out = karac_check(Some(target), src, name);
            let t = text(&out);
            assert!(!out.status.success(), "{target} {name}: {t}");
            assert!(
                t.contains(&format!("`{module}` is not available on target `{target}`")),
                "{target} {name}: {t}"
            );
        }
    }
}

#[test]
fn a_native_check_still_accepts_std_crypto() {
    for (name, src) in [("sha_native.kara", SHA), ("ct_native.kara", CT)] {
        let out = karac_check(None, src, name);
        assert!(out.status.success(), "{name}: {}", text(&out));
    }
}
