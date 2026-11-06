//! The AOT pipeline folds a two-entry phi up to SimplifyCFG cost 6, not
//! LLVM's 4 (B-2026-11-04-37): kata 342's `n > 0 and (n & (n - 1)) == 0
//! and ...`, once InstCombine has made its middle test a `ctpop`, costs
//! more than 4 on aarch64, so its loop kept `n > 0` as a second
//! data-dependent branch per element that legacy's loop does not have.
//! The pin builds that loop for aarch64-apple-darwin (the object only: the
//! "linker" is a script that keeps it) at the default and at
//! `KARAC_MIR_PHI_FOLD_THRESHOLD=4`, and asserts the two objects differ.
#![cfg(all(feature = "llvm", unix))]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const SRC: &str = r#"fn is_power_of_four(n: i32) -> bool {
    return n > 0 and (n & (n - 1)) == 0 and (n & 0x55555555) != 0;
}
fn main() {
    let mut a: Vec[i32] = Vec.new();
    let mut s: i64 = 7;
    for _ in 0..1000 {
        s = (s * 1103515245 + 12345) % 2147483648;
        a.push((s - 1073741824) as i32);
    }
    let mut count = 0;
    for x in a {
        if is_power_of_four(x) {
            count += 1;
        }
    }
    println(f"{count}");
}
"#;

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-phi-fold-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The aarch64 object karac emits for `SRC`, or `None` when this LLVM
/// cannot target aarch64.
fn object(d: &Path, threshold: Option<&str>) -> Option<Vec<u8>> {
    let tag = threshold.unwrap_or("default");
    let file = d.join("main.kara");
    let keep = d.join(format!("{tag}.o"));
    let grab = d.join("grab.sh");
    std::fs::write(&file, SRC).unwrap();
    std::fs::write(
        &grab,
        "#!/bin/sh\nout=\"\"; prev=\"\"\nfor a in \"$@\"; do\n  case \"$a\" in *.o) cp \"$a\" \"$GRAB_TO\";; esac\n  [ \"$prev\" = \"-o\" ] && out=\"$a\"\n  prev=\"$a\"\ndone\n[ -n \"$out\" ] && : > \"$out\"\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&grab, std::fs::Permissions::from_mode(0o755)).unwrap();
    let runtime = d.join("fake.a");
    std::fs::write(&runtime, b"").unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    if let Some(t) = threshold {
        c.env("KARAC_MIR_PHI_FOLD_THRESHOLD", t);
    }
    let built = c
        .env("KARAC_AUTO_PAR", "0")
        .env("KARAC_LINKER", &grab)
        .env("KARAC_RUNTIME", &runtime)
        .env("GRAB_TO", &keep)
        .args([
            "build",
            "--target=aarch64-apple-darwin",
            file.to_str().unwrap(),
            "-o",
            d.join(tag).to_str().unwrap(),
        ])
        .output()
        .unwrap();
    if !built.status.success() {
        let why = String::from_utf8_lossy(&built.stderr);
        assert!(
            why.contains("aarch64") || why.contains("target"),
            "the cross build failed for another reason:\n{why}"
        );
        eprintln!("skipping: no aarch64 target in this LLVM:\n{why}");
        return None;
    }
    let o = std::fs::read(&keep).unwrap_or_else(|_| panic!("no object kept for {tag}"));
    assert!(!o.is_empty(), "{tag}: empty object");
    Some(o)
}

#[test]
fn a_ctpop_test_after_a_sign_test_folds_on_aarch64() {
    let d = dir();
    let Some(default) = object(&d, None) else {
        return;
    };
    let llvm = object(&d, Some("4")).unwrap();
    assert_ne!(
        default, llvm,
        "the fold threshold made no difference to the aarch64 object"
    );
    let _ = std::fs::remove_dir_all(&d);
}
