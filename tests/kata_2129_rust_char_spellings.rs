//! Kata 2129's Rust template capitalizes a word the way Rust's answers
//! usually do: `let mut cs = lower.chars(); match cs.next() { Some(f) =>
//! f.to_uppercase().collect::<String>() + cs.as_str(), .. }`, and its
//! bytes arm pushed `char::from_u32(b as u32).unwrap()`. Each met a gap:
//! a char's `to_uppercase()` is a `char` here, so `.collect()` had no
//! method and no fix; `StrChars` had no `as_str`; and `char.from_u32` was
//! named but not rewritten. The full mapping Rust collects is
//! `c.to_string().to_uppercase()`, `char.try_from(n).ok()` is the
//! `Option` Rust's `from_u32` returns, and `as_str` is the text not yet
//! read, as Rust's `Chars::as_str`.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// Writes `src`, runs `karac fix` on it, and returns the fixed text.
fn fixed(tag: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-k2129-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let file = path.to_str().unwrap();
    let fix = karac(&["fix", file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let check = karac(&["check", file]);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        check.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let _ = std::fs::remove_file(&path);
    text
}

fn run(text: &str) -> String {
    karac::mir::lower::run_source(text)
        .expect("MIR interpreter")
        .output
}

#[test]
fn kata_2129_char_from_u32_becomes_try_from_ok() {
    let text = fixed(
        "from-u32",
        "fn main() {
    let b: u8 = 104;
    let c = char.from_u32(b as u32).unwrap();
    let d = char.from_u32(55296);
    println(f\"{c} {d.is_none()}\");
}
",
    );
    assert!(
        text.contains("char.try_from(b as u32).ok().unwrap()"),
        "{text}"
    );
    assert!(text.contains("char.try_from(55296).ok()"), "{text}");
    assert_eq!(run(&text), "h true\n");
}

#[test]
fn kata_2129_collected_char_case_becomes_string_case() {
    let text = fixed(
        "collect",
        "fn main() {
    let f = 'ß';
    let g = 'q';
    let up: String = f.to_uppercase().collect();
    let low: String = g.to_lowercase().collect();
    println(f\"{up} {low} {up.len()}\");
}
",
    );
    assert!(text.contains("f.to_string().to_uppercase()"), "{text}");
    assert!(text.contains("g.to_string().to_lowercase()"), "{text}");
    assert_eq!(run(&text), "SS q 2\n");
}

#[test]
fn kata_2129_chars_as_str_is_the_rest() {
    let src = "fn main() {
    let s = \"héllo\".to_string();
    let mut cs = s.chars();
    let first = cs.next().unwrap();
    println(f\"{first} {cs.as_str()}\");
    cs.next_back();
    println(cs.as_str());
    let v: Str = \"ab\";
    let mut vs = v.chars();
    vs.next();
    vs.next();
    println(f\"[{vs.as_str()}]\");
}
";
    assert_eq!(run(src), "h éllo\néll\n[]\n");
}
