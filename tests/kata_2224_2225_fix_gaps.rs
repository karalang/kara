//! Two `karac fix` gaps met carrying LeetCode templates (B-2026-11-17-141, B-2026-11-17-142).
//!
//! Kata 2224: a closure's Rust return type moved onto its binding was written
//! `Fn(String) -> ..`, but a bare closure parameter borrows, so the closure is
//! `Fn(ref String) -> ..`; the owned annotation moved the argument out of
//! every caller and the next fix round own-ified and cloned through them.
//! Kata 2225: a typed receiver naming the impl's own type
//! (`self: ref Tournament`) had no machine fix; only `Self` did.

use std::process::Command;

fn fix_and_run(name: &str, src: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.kara"), src).unwrap();
    let karac = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_karac"))
            .current_dir(&dir)
            .args(args)
            .output()
            .unwrap()
    };
    for _ in 0..3 {
        karac(&["fix", "a.kara"]);
    }
    let after = std::fs::read_to_string(dir.join("a.kara")).unwrap();
    let run = karac(&["run", "a.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(run.status.success(), "{run:?}\n{after}");
    (after, String::from_utf8_lossy(&run.stdout).into_owned())
}

#[test]
fn kata_2224_closure_binding_annotation_borrows_its_parameter() {
    let src = "fn convert(current: String, correct: String) -> i64 {\n\
               \x20   let digits = |time: String| -> Vec[i64] { time.chars().filter(|c| c.is_ascii_digit()).map(|c| (c as i64) - ('0' as i64)).collect() };\n\
               \x20   let a = digits(current);\n\
               \x20   let b = digits(correct);\n\
               \x20   b[0] - a[0]\n\
               }\n\
               fn main() {\n\
               \x20   let s = \"02:30\".to_string();\n\
               \x20   let t = \"14:35\".to_string();\n\
               \x20   println(f\"{convert(s, t)} {s} {t}\");\n\
               }\n";
    let (after, out) = fix_and_run("2224", src);
    assert!(
        after.contains("let digits: Fn(ref String) -> Vec[i64] = |time: String| {"),
        "{after}"
    );
    assert!(
        after.contains("fn convert(current: String, correct: String)"),
        "{after}"
    );
    assert!(!after.contains(".clone()"), "{after}");
    assert_eq!(out, "1 02:30 14:35\n");
}

#[test]
fn kata_2225_typed_receiver_naming_the_impl_type_is_fixed() {
    let src = "struct Tournament {\n    x: i64,\n}\n\
               impl Tournament {\n\
               \x20   fn get(self: ref Tournament) -> i64 {\n        self.x\n    }\n\
               \x20   fn bump(self: mut ref Tournament) {\n        self.x += 1;\n    }\n\
               }\n\
               fn main() {\n\
               \x20   let mut a = Tournament { x: 1 };\n\
               \x20   a.bump();\n\
               \x20   println(f\"{a.get()}\");\n\
               }\n";
    let (after, out) = fix_and_run("2225", src);
    assert!(after.contains("fn get(self) -> i64"), "{after}");
    assert!(after.contains("fn bump(mut ref self)"), "{after}");
    assert_eq!(out, "2\n");
}
