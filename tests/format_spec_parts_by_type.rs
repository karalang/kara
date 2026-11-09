//! B-2026-11-08-74: which format-spec parts apply to which values
//! (design.md §3 Format specifiers, "Which parts apply to which values").
//! Fill, align and width pad any value's `Display` text, so a float without
//! a precision, a `bool` and a `char` take them; `+` and `0` are for
//! numbers, `type` for integers, `.precision` for floats, and a part that
//! does not apply is still an error at the hole.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-format-spec-parts-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(args)
        .arg("m.kara")
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

const PADDED: &str = r#"fn main() {
    let x = 0.1;
    let y = 1.5;
    let z = 0.0;
    let nan = z / z;
    let inf = 1.0 / z;
    let flag = true;
    let c = 'x';
    let big = 1e16;
    let h: f32 = 0.1;
    let neg = -0.0;
    println(f"[{x:>8}][{y:+}][{nan:>5}][{inf:<5}][{-inf:^6}]");
    println(f"[{flag:>6}][{c:^3}][{flag:*<7}][{c:-^5}]");
    println(f"[{big:>8}][{h:>6}][{h:+}][{neg:+}][{y:08}][{x:<6}|]");
    println(f"[{-y:+08}][{2.0:>5}][{nan:+}][{x:.3}]");
}
"#;

const WANT: &str = "[     0.1][+1.5][  NaN][inf  ][ -inf ]
[  true][ x ][true***][--x--]
[   1e+16][   0.1][+0.1][-0.0][000001.5][0.1   |]
[-00001.5][  2.0][NaN][0.100]
";

// A type of the program's own and a generic parameter pad their `Display`
// text the same way.
const OWN: &str = r#"struct P { x: i64 }
impl Display for P {
    fn to_string(ref self) -> String { f"P{self.x}" }
}
fn show[T: Display](t: T) -> String { f"[{t:>5}]" }
fn main() {
    let p = P { x: 7 };
    println(f"[{p:>6}][{p:*<5}]");
    println(show(3.5));
    println(show(true));
}
"#;

#[test]
fn width_fill_and_align_pad_every_values_display_text() {
    let check = karac("check", &["check"], PADDED);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run", "run"]
    } else {
        &["__mir-run"]
    };
    for b in backends {
        for (name, src, want) in [
            ("padded", PADDED, WANT),
            ("own", OWN, "[    P7][P7***]\n[  3.5]\n[ true]\n"),
        ] {
            let tag = format!("run-{name}-{}", b.trim_start_matches('_'));
            let out = karac(&tag, &[b], src);
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                want,
                "{b} {name}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

#[cfg(feature = "llvm")]
#[test]
fn the_built_binary_pads_the_same() {
    let dir = std::env::temp_dir().join(format!(
        "karac-format-spec-parts-build-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), PADDED).unwrap();
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["build", "m.kara", "-o", "m.bin"])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    if !built.status.success() {
        let err = String::from_utf8_lossy(&built.stderr).into_owned();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            err.contains("archive")
                && std::env::var("KARAC_REQUIRE_RUNTIME_ARCHIVE").as_deref() != Ok("1"),
            "{err}"
        );
        eprintln!("skipped the built leg: {err}");
        return;
    }
    let out = Command::new(dir.join("m.bin"))
        .stdin(Stdio::null())
        .output()
        .expect("run");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(String::from_utf8_lossy(&out.stdout), WANT);
}

#[test]
fn a_part_that_does_not_apply_is_an_error_at_the_hole() {
    let src = "fn main() {
    let flag = true;
    let c = 'x';
    let x = 1.5;
    let n = 3;
    let s = \"ab\";
    println(f\"{flag:+}\");
    println(f\"{c:05}\");
    println(f\"{flag:.2}\");
    println(f\"{c:x}\");
    println(f\"{x:x}\");
    println(f\"{n:.2}\");
    println(f\"{s:.1}\");
}
";
    let out = karac("refused", &["check"], src);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    for (line, needle) in [
        (
            7,
            "the sign flag (`+`), which is for numbers, is not valid for a `bool`",
        ),
        (
            8,
            "zero-pad (`0`), which is for numbers, is not valid for a `char`",
        ),
        (
            9,
            "precision (`.N`), which is for floats, is not valid for a `bool`",
        ),
        (
            10,
            "a radix type (x/X/o/b), which is for integers, is not valid for a `char`",
        ),
        (11, "a radix type (x/X/o) is not valid for a float"),
        (12, "precision (`.N`) is not valid for an integer"),
        (13, "precision (`.N`) on a string is not in v1"),
    ] {
        assert!(
            err.lines()
                .any(|l| l.contains(&format!(":{line}:")) && l.contains(needle)),
            "line {line}: {needle}\n{err}"
        );
    }
}
