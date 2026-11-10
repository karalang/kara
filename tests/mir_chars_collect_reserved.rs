//! `s.chars().collect()` into a `Vec[char]` reserves the text's byte
//! length, a ceiling on its chars. When that reservation covers the whole
//! text the walk runs in a copy that pushes without testing the capacity
//! (`Vec.__push_reserved`), as Rust's sized collect does: kata 484's
//! collect of 20 million chars went from 14 instructions a char to 11
//! (B-2026-11-09-47). The pins: an empty text, multi-byte chars of every
//! width, a collect pushed onto afterwards, and a `for` over chars beside
//! it, on the interpreter and the built binary; and the built fill's
//! fast copy carries no growth call.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const CHARS: &str = r#"fn letters(s: String) -> Vec[char] {
    let v: Vec[char] = s.chars().collect();
    v
}
fn main() {
    let a = letters("".to_string());
    let b = letters("héllo wörld ✓ 𝄞!".to_string());
    let c: Vec[char] = "xyz".chars().collect();
    let mut d = letters("ab".to_string());
    d.push('c');
    d.push('d');
    println(f"{a.len()} {b.len()} {b} {c} {d}");
    let mut n = 0;
    for ch in "añb".chars() {
        if ch != 'b' {
            n += 1;
        }
    }
    println(f"{n}");
}
"#;

const WANT: &str =
    "0 16 [h, é, l, l, o,  , w, ö, r, l, d,  , ✓,  , 𝄞, !] [x, y, z] [a, b, c, d]\n2\n";

#[test]
fn mir_chars_collect_pushes_into_its_reservation() {
    let d = std::env::temp_dir().join(format!("karac-chars-reserved-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let file = d.join("main.kara");
    std::fs::write(&file, CHARS).unwrap();
    let file = file.to_str().unwrap();
    let interp = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", file])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&interp.stdout),
        WANT,
        "{}",
        String::from_utf8_lossy(&interp.stderr)
    );
    let exe = d.join("main");
    let ll = d.join("pre.ll");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_AUTO_PAR", "0")
        .env("KARAC_MIR_LL", &ll)
        .args(["build", file, "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    })
    .is_none()
    {
        return;
    }
    let ran = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&ran.stdout), WANT);
    // `letters` walks its text twice, once per copy, and only the copy for
    // a reservation that falls short can grow the vector.
    let mut ir = String::new();
    for p in [ll.clone(), d.join("pre.ll.0")] {
        if let Ok(m) = std::fs::read_to_string(p) {
            ir.push_str(&m);
        }
    }
    let body: String = ir
        .split("\ndefine ")
        .find(|f| {
            f.lines()
                .next()
                .is_some_and(|l| l.contains("@kara.letters("))
        })
        .unwrap_or_else(|| panic!("no `letters` in the IR"))
        .to_string();
    // The chunk runs on to the next `define`, so count calls only: the
    // declarations that follow `letters` name the same symbols.
    let calls = |sym: &str| {
        body.lines()
            .filter(|l| l.contains("call") && l.contains(sym))
            .count()
    };
    let walks = calls("@karac_mir_char_at_byte(");
    let grows = calls("@grow1");
    assert_eq!(walks, 2, "{body}");
    assert_eq!(grows, 1, "{body}");
    let _ = std::fs::remove_dir_all(&d);
}
