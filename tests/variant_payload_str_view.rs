//! A `String` or `ref String` payload at a `Str` field of a variant is
//! viewed, as a call argument at a `Str` parameter is
//! (B-2026-11-25-82): `Some(ref original)` handed to an `Option[Str]`
//! passed the checker, then failed the MIR validator ("field 0 of Option
//! has the wrong type") on both backends, so the program did not run.

use std::process::Command;

const SRC: &str = r#"
enum Tag { Name(Str), Pair(Str, i64) }

fn show(p: Option[Str]) {
    match p {
        Some(s) => println(f"some {s}"),
        None => println("none"),
    }
}

fn tag(t: Tag) -> String {
    match t {
        Tag.Name(s) => f"name {s}",
        Tag.Pair(s, n) => f"pair {s} {n}",
    }
}

fn main() {
    let original = "abc".to_string();
    show(Some(ref original));
    let x: Option[Str] = Some(ref original);
    show(x);
    let r = ref original;
    let y: Option[Str] = Some(r);
    show(y);
    println(tag(Tag.Name(ref original)));
    println(tag(Tag.Pair(ref original, 2)));
    let lit: Option[Str] = Some("lit");
    show(lit);
    println(original);
}
"#;

const WANT: &str = "some abc\nsome abc\nsome abc\nname abc\npair abc 2\nsome lit\nabc\n";

fn run(mode: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "karac-variant-str-view-{mode}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(mode)
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{mode}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn interp_a_ref_string_payload_is_a_str_view() {
    assert_eq!(run("__mir-run"), WANT);
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_a_ref_string_payload_is_a_str_view() {
    assert_eq!(run("__mir-llvm-run"), WANT);
}
