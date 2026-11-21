//! B-2026-11-21-59: a method whose parameter has a fixed integer type
//! (`char.from_digit`'s digit and radix, `to_digit`'s and `is_digit`'s
//! radix, the amount of a `wrapping_shl` or a `checked_pow`, `powi`'s
//! exponent) refused an `i64` argument
//! with a message naming the cast, `(cast with `as u32`)`, and no fix, where
//! `pow`'s exponent had one since kata 1067. Kata 1301's harness, a digit
//! drawn as `next_random(..) % 9 + 1`, found it. Each now takes the cast it
//! names: after a name, a call or a literal, and around anything looser. A
//! float argument keeps the plain refusal, since its cast would truncate.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn scratch(tag: &str, src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-castarg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    (dir, f)
}

fn fixed(tag: &str, src: &str, wants: &[&str], output: &str) {
    let (dir, f) = scratch(tag, src);
    let p = f.to_str().unwrap();
    let out = karac(&["fix", p]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = std::fs::read_to_string(&f).unwrap();
    for want in wants {
        assert!(after.contains(want), "missing {want:?} in\n{after}");
    }
    let check = karac(&["check", p]);
    assert!(
        check.status.success(),
        "{after}\n{}",
        String::from_utf8_lossy(&check.stdout)
    );
    let run = karac(&["__mir-run", p]);
    assert_eq!(String::from_utf8_lossy(&run.stdout), output, "{after}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn from_digit_takes_the_cast_on_its_digit_and_radix() {
    fixed(
        "fromdigit",
        "fn main() {
    let n: i64 = 22;
    let r: i64 = 10;
    println(f\"{char.from_digit(n % 9 + 1, r)} {char.from_digit(n, 16)}\");
}
",
        &[
            "char.from_digit((n % 9 + 1) as u32, r as u32)",
            "char.from_digit(n as u32, 16)",
        ],
        "Some(5) None\n",
    );
}

#[test]
fn a_radix_and_a_shift_amount_take_the_cast() {
    fixed(
        "radix",
        "fn main() {
    let base: i64 = 16;
    let k: i64 = 4;
    let x: u8 = 0b1001_0110;
    let b: i64 = 3;
    println(f\"{'f'.to_digit(base)} {'g'.is_digit(base + 1)} {x.wrapping_shl(k)} {b.checked_pow(k)}\");
}
",
        &[
            "'f'.to_digit(base as u32)",
            "'g'.is_digit((base + 1) as u32)",
            "x.wrapping_shl(k as u32)",
            "b.checked_pow(k as u32)",
        ],
        "Some(15) true 96 Some(81)\n",
    );
}

#[test]
fn powi_takes_an_i32_cast() {
    fixed(
        "powi",
        "fn main() {
    let k: i64 = 3;
    let x = 2.0;
    println(f\"{x.powi(k)} {x.powi(k - 1)}\");
}
",
        &["x.powi(k as i32)", "x.powi((k - 1) as i32)"],
        "8.0 4.0\n",
    );
}

#[test]
fn a_float_digit_keeps_the_plain_refusal() {
    let src = "fn main() {
    let d = 3.0;
    println(f\"{char.from_digit(d, 10)}\");
}
";
    let (dir, f) = scratch("float", src);
    let p = f.to_str().unwrap();
    let out = karac(&["fix", p]);
    assert!(!out.status.success(), "a float digit was fixed");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), src);
    let _ = std::fs::remove_dir_all(&dir);
}
