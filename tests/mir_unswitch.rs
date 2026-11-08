//! The AOT pipeline unswitches loops on a condition the loop never changes,
//! as rustc's O2 does and LLVM's own O2 does not (B-2026-11-08-3): kata
//! 492's `while area % width != 0 { width -= 1 }` tests `area == i64.MIN`
//! (half of the `MIN % -1` check) on every step, and kata 277's
//! verification loop re-asks `knows(cand, j)` once `cand` is the planted
//! celebrity. Unswitched, each loop gets a copy that leaves the test out.
//! The pin builds 492's loop (an area read from a Vec, so its range is
//! unknown) for aarch64-apple-darwin, the object only (the "linker" is a
//! script that keeps it; a native build links with the system's), by
//! default and at `KARAC_MIR_UNSWITCH=0`, and asserts the default object
//! carries the extra loop copy.
#![cfg(all(feature = "llvm", unix))]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const SRC: &str = r#"fn widest(area: i64) -> i64 {
    let mut width = (area as f64).sqrt() as i64;
    while area % width != 0 {
        width -= 1;
    }
    width
}
fn main() {
    let mut s: i64 = 7;
    let mut areas: Vec[i64] = Vec.new();
    for _ in 0..1000 {
        s = (s * 1103515245 + 12345) % 2147483648;
        areas.push(s % 100000 + 1);
    }
    let mut total: i64 = 0;
    for a in areas {
        total += widest(a);
    }
    println(f"{total}");
}
"#;

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-unswitch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The aarch64 object karac emits for `SRC`, with `KARAC_MIR_UNSWITCH` set
/// to `unswitch` when given, or `None` when this LLVM cannot target aarch64.
fn object(d: &Path, unswitch: Option<&str>) -> Option<Vec<u8>> {
    let tag = unswitch.unwrap_or("default");
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
    if let Some(u) = unswitch {
        c.env("KARAC_MIR_UNSWITCH", u);
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
fn a_loop_invariant_overflow_test_is_unswitched() {
    let d = dir();
    let Some(default) = object(&d, None) else {
        return;
    };
    let off = object(&d, Some("0")).unwrap();
    assert!(
        default.len() > off.len(),
        "unswitching added no loop copy: {} bytes against {} without it",
        default.len(),
        off.len()
    );
    let _ = std::fs::remove_dir_all(&d);
}
