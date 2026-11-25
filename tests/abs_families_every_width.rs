//! B-2026-11-25-47 (library/core-types.md, Absolute value): the `abs`
//! overflow families `checked_abs`, `wrapping_abs`, `saturating_abs` and
//! `overflowing_abs`, and `unsigned_abs`, exist on every signed width, as
//! Rust's, and on no unsigned one. Pinned on the MIR interpreter at `MIN`,
//! `MIN + 1`, a negative, zero and `MAX` of each width; the compiled lanes
//! run the same program as the conformance cell
//! `conformance/library/core-types/abs_families_at_every_width`.

fn karac() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn run_interp(name: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-abs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let file = dir.join("main.kara");
    std::fs::write(&file, src).expect("write source");
    let out = karac()
        .args(["run", "--interp"])
        .arg(&file)
        .output()
        .expect("run karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn abs_families_match_rust_at_every_signed_width() {
    let src = "fn main() {\n\
        let a: i8 = i8.MIN; println(a.checked_abs()); println(a.wrapping_abs()); println(a.saturating_abs()); println(a.overflowing_abs()); println(a.unsigned_abs());\n\
        let b: i16 = i16.MIN + 1; println(b.checked_abs()); println(b.overflowing_abs()); println(b.unsigned_abs());\n\
        let c: i32 = -5; println(c.checked_abs()); println(c.saturating_abs()); println(c.unsigned_abs());\n\
        let d: i64 = i64.MIN; println(d.checked_abs()); println(d.saturating_abs()); println(d.unsigned_abs());\n\
        let e: i128 = i128.MIN; println(e.checked_abs()); println(e.wrapping_abs()); println(e.overflowing_abs()); println(e.unsigned_abs());\n\
        let f: isize = isize.MAX; println(f.checked_abs()); println(f.wrapping_abs());\n\
        let g: i64 = 0; println(g.overflowing_abs());\n\
    }\n";
    let (ok, out, err) = run_interp("widths", src);
    assert!(ok, "interp failed: {err}");
    // From Rust's own methods (rustc 1.x, `iN::checked_abs` and friends).
    let want = "None\n-128\n127\n(-128, true)\n128\n\
        Some(32767)\n(32767, false)\n32767\n\
        Some(5)\n5\n5\n\
        None\n9223372036854775807\n9223372036854775808\n\
        None\n-170141183460469231731687303715884105728\n(-170141183460469231731687303715884105728, true)\n170141183460469231731687303715884105728\n\
        Some(9223372036854775807)\n9223372036854775807\n\
        (0, false)\n";
    assert_eq!(out, want);
}

#[test]
fn abs_families_are_signed_only() {
    for (m, t) in [
        ("checked_abs", "u8"),
        ("wrapping_abs", "u64"),
        ("unsigned_abs", "u32"),
    ] {
        let src = format!("fn main() {{ let u: {t} = 3; println(u.{m}()); }}\n");
        let (ok, _, err) = run_interp(m, &src);
        assert!(!ok, "{m} on {t} was accepted");
        assert!(
            err.contains(&format!("no method '{m}' on type '{t}'")),
            "{m} on {t}: {err}"
        );
    }
}
