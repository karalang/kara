//! B-2026-11-12-15: `s.push_str(x.repeat(n))` built the repeat as a
//! `String` of its own (a runtime call, an allocation and a copy per
//! doubling) and freed it once appended: kata 1138's four
//! `path.push_str("U".repeat(k))` per letter ran 7.0G instructions to
//! Rust's 4.6G. Of a literal, or an owned `String` local other than `s`,
//! it is now the native `String.__push_repeat`, which appends the copies
//! in place. The answers are the separate repeat's on every backend,
//! including `s += ..`, a field receiver, a `ref` count, a negative
//! count, multi-byte text and `t.push_str(t.repeat(2))`, which stays apart.

use std::process::{Command, Stdio};

const SRC: &str = r#"struct Board {
    path: String,
}

fn main() {
    let mut s = String.new();
    for k in -1..5 {
        s.push_str("U".repeat(k));
        s.push_str("!");
    }
    println(s);
    let mut t = "<";
    t.push_str("ab".repeat(3));
    t.push_str("é".repeat(2));
    t += "xy".repeat(2);
    t += "".repeat(7);
    let pad = "-=".to_string();
    t.push_str(pad.repeat(2));
    t.push_str(t.repeat(2));
    println(f"{t} {t.len()}");
    let mut b = Board { path: String.new() };
    let ks = [3, 0, 2];
    for k in ks.iter() {
        b.path.push_str("R".repeat(k));
        b.path += "D".repeat(*k);
    }
    println(b.path);
    let mut big = "[";
    big.push_str("abc".repeat(1000));
    big.push_str("z".repeat(4097));
    println(f"{big.len()} {big[0..7]} {big[2998..3004]} {big[big.len() - 3..]}");
}
"#;

const WANT: &str = r#"!!U!UU!UUU!UUUU!
<abababééxyxy-=-=<abababééxyxy-=-=<abababééxyxy-=-= 57
RRRDDDRRDD
7098 [abcabc abczzz zzz
"#;

fn run(sub: &str, tag: &str, dump: bool) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-push-repeat-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), SRC).unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    if dump {
        c.env("KARAC_MIR_DUMP", "elaborated");
    }
    let out = c
        .args([sub, "cell.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn push_str_of_repeat_interpreted() {
    let (out, err) = run("__mir-run", "interp", true);
    assert_eq!(out, WANT, "{err}");
    // The literal, `+=`, local and field forms append in place; the
    // receiver's own repeat does not.
    assert!(
        err.contains("String.__push_repeat"),
        "no fused append:
{err}"
    );
    assert!(
        err.contains("String.repeat("),
        "the self-repeat was fused:
{err}"
    );
}

#[test]
fn push_str_of_repeat_compiled() {
    if !cfg!(feature = "llvm") {
        return;
    }
    let (out, err) = run("__mir-llvm-run", "llvm", false);
    if err.contains("Linker failed") && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none()
    {
        eprintln!("skipping: no runtime archive");
        return;
    }
    assert_eq!(out, WANT, "{err}");
}
