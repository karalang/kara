//! Kata 2030's string arm reads the top of its stack with
//! `out.chars().last()`. `StrChars` took `Iterator.last`'s default, which
//! walks the whole text, so each read was O(n) and the arm quadratic. It
//! now reads from the back through `next_back`, as Rust's `Chars::last`
//! does, on a `String` and on a `Str` view, and after the front has been
//! consumed.

use std::process::{Command, Stdio};

fn run(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-kata-2030-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), format!("{src}\n")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", "cell.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn kata_2030_chars_last_reads_from_the_back() {
    let src = r#"fn main() {
    let s = String.from("héllo wörld");
    println(s.chars().last());
    let mut it = s.chars();
    it.next();
    println(it.last());
    println(String.new().chars().last());
    let one = String.from("é");
    let mut j = one.chars();
    j.next();
    println(j.last());
    let v: Str = s[0..6];
    println(v.chars().last());
    let mut w = v.chars();
    w.next_back();
    println(w.last());
}"#;
    assert_eq!(
        run("last", src),
        "Some(d)\nSome(d)\nNone\nNone\nSome(o)\nSome(l)\n"
    );
}

#[test]
fn kata_2030_chars_last_matches_the_walk() {
    // `last` agrees with a full walk over every prefix of a mixed text
    let src = r#"fn main() {
    let s = String.from("aé€𝄞b");
    let mut agree = 0;
    for end in [0, 1, 3, 6, 10, 11] {
        let v: Str = s[0..end];
        let mut walked: Option[char] = None;
        for c in v.chars() {
            walked = Some(c);
        }
        if v.chars().last() == walked {
            agree += 1;
        }
    }
    println(agree);
}"#;
    assert_eq!(run("walk", src), "6\n");
}
