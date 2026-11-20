//! A type error inside a `comptime` block is `karac check`'s too. Check
//! folded the block and re-checked the program, replacing the pass that
//! found the error, so it answered "All checks passed" for a program the
//! build refuses at that error (B-2026-11-20-101).

use std::process::Command;

fn karac(cmd: &str, name: &str, src: &str) -> (bool, String) {
    let dir =
        std::env::temp_dir().join(format!("karac-comptime-err-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

#[test]
fn check_refuses_a_reflection_call_of_the_wrong_arity_as_the_build_does() {
    let src = "struct P { x: i64 }\nfn main() { let _ = comptime { P.derives() }; }\n";
    let (ok, text) = karac("check", "arity", src);
    assert!(!ok, "{text}");
    assert!(text.contains("error[E0202]"), "{text}");
    assert!(
        text.contains("reflection method `derives` takes exactly one argument"),
        "{text}"
    );
    let (ok, text) = karac("__mir-run", "arity", src);
    assert!(!ok, "{text}");
    assert!(text.contains("takes exactly one argument"), "{text}");
}

#[test]
fn check_still_accepts_a_well_typed_comptime_block() {
    let src = "fn main() { let n = comptime { 6 * 7 }; println(n); }\n";
    let (ok, text) = karac("check", "ok", src);
    assert!(ok, "{text}");
    let (ok, text) = karac("__mir-run", "ok", src);
    assert!(ok, "{text}");
    assert!(text.contains("42"), "{text}");
}
