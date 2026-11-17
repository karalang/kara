//! What a compiled program reports when it ends abnormally, where the
//! corpus (which compares stdout and the exit code only) cannot see it:
//!
//! - a `#[track_caller]` or library body whose only abort is a native's (an
//!   index's bounds, a string cut inside a char) reports its caller's place
//!   (design.md §10, B-2026-11-16-318);
//! - a shift amount wider than the value is range-checked in its own width
//!   (design.md "Division, remainder and shifts", B-2026-11-16-322);
//! - a recursion deeper than the 1 GiB stack `main` runs on exhausts it and
//!   aborts with the runtime's message (core-semantics §10.1,
//!   B-2026-11-16-319).
//!
//! Each case also runs on the MIR interpreter, which the compiled report
//! must match.

use std::process::{Command, Stdio};

/// The interpreter's exit code, stdout and stderr for `src` as `m.kara`.
fn interpreted(tag: &str, src: &str) -> (Option<i32>, String, String) {
    let dir = scratch(&format!("i-{tag}"), src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["__mir-run", "m.kara"])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn scratch(tag: &str, src: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("karac-compiled-panic-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), src).unwrap();
    dir
}

const TRACKED_INDEX: &str = "#[track_caller]
fn at(v: Vec[i64], i: i64) -> i64 {
    v[i]
}
fn main() {
    let v = [1, 2, 3].to_vec();
    println(at(v, 1));
    println(at(v, 7));
}
";
const TRACKED_INDEX_REPORT: &str = "runtime error: index out of bounds\n  at m.kara:8:13\n";

const SPLIT_INSIDE_A_CHAR: &str = "fn main() {
    let s = \"h\u{e9}llo\".to_string();
    let (a, b) = s.split_at(2);
    println(f\"{a}{b}\");
}
";
const SPLIT_REPORT: &str = "runtime error: byte index 2 is not a char boundary\n  at m.kara:3:18\n";

const WIDE_SHIFT: &str = "fn main() {
    let x: i32 = 1;
    let a: i128 = 1 << 64;
    println(\"before\");
    println(x << a);
}
";
const NARROW_VALUE_SHIFT: &str = "fn main() {
    let x: u8 = 3;
    let a: i32 = 256;
    println(\"before\");
    println(x >> a);
}
";
const SHIFT_REPORT: &str = "runtime error: shift amount out of range\n  at m.kara:5:13\n";

#[test]
fn interpreted_track_caller_index_reports_the_call() {
    let (code, out, err) = interpreted("tci", TRACKED_INDEX);
    assert_eq!((code, out.as_str()), (Some(101), "2\n"), "{err}");
    assert!(err.starts_with(TRACKED_INDEX_REPORT), "{err}");
}

#[test]
fn interpreted_split_at_inside_a_char_reports_the_call() {
    let (code, _, err) = interpreted("split", SPLIT_INSIDE_A_CHAR);
    assert_eq!(code, Some(101), "{err}");
    assert!(err.starts_with(SPLIT_REPORT), "{err}");
}

#[test]
fn interpreted_wide_shift_amounts_trap() {
    for (tag, src) in [("wide", WIDE_SHIFT), ("narrow", NARROW_VALUE_SHIFT)] {
        let (code, out, err) = interpreted(tag, src);
        assert_eq!(
            (code, out.as_str()),
            (Some(101), "before\n"),
            "{tag}: {err}"
        );
        assert!(err.starts_with(SHIFT_REPORT), "{tag}: {err}");
    }
}

#[cfg(feature = "llvm")]
mod compiled {
    use super::*;

    /// The built program's exit code, stdout and stderr, or `None` when the
    /// build fails for want of a runtime archive (the skip the rest of the
    /// E2E suite makes; `KARAC_REQUIRE_RUNTIME_ARCHIVE=1` forbids it).
    fn compiled(tag: &str, src: &str) -> Option<(Option<i32>, String, String)> {
        let dir = scratch(&format!("c-{tag}"), src);
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
            return None;
        }
        let out = Command::new(dir.join("m.bin"))
            .current_dir(&dir)
            .stdin(Stdio::null())
            .output()
            .expect("run");
        let _ = std::fs::remove_dir_all(&dir);
        Some((
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ))
    }

    #[test]
    fn compiled_track_caller_index_reports_the_call() {
        let Some((code, out, err)) = compiled("tci", TRACKED_INDEX) else {
            return;
        };
        assert_eq!((code, out.as_str()), (Some(101), "2\n"), "{err}");
        assert!(err.starts_with(TRACKED_INDEX_REPORT), "{err}");
    }

    #[test]
    fn compiled_split_at_inside_a_char_reports_the_call() {
        let Some((code, _, err)) = compiled("split", SPLIT_INSIDE_A_CHAR) else {
            return;
        };
        assert_eq!(code, Some(101), "{err}");
        assert!(err.starts_with(SPLIT_REPORT), "{err}");
    }

    #[test]
    fn compiled_wide_shift_amounts_trap() {
        for (tag, src) in [("wide", WIDE_SHIFT), ("narrow", NARROW_VALUE_SHIFT)] {
            let Some((code, out, err)) = compiled(tag, src) else {
                return;
            };
            assert_eq!(
                (code, out.as_str()),
                (Some(101), "before\n"),
                "{tag}: {err}"
            );
            assert!(err.starts_with(SHIFT_REPORT), "{tag}: {err}");
        }
    }

    /// The pin `corpus/fixtures/mir_library/deep_recursion_aborts_as_a_stack_overflow`
    /// on the compiled lane: 200M frames overflow the 1 GiB stack `main`
    /// runs on, the flushed line is kept, and the process aborts with the
    /// runtime's message.
    #[test]
    fn compiled_unfoldable_recursion_overflows_the_stack() {
        let src = "fn down(n: i64) -> i64 {
    if n <= 0 {
        return 0;
    }
    let sub = down(n - 1);
    return sub % 1000003 + n % 7;
}
fn main() {
    println(\"before\");
    stdout.flush();
    println(f\"{down(200000000)}\");
}
";
        let Some((code, out, err)) = compiled("stack", src) else {
            return;
        };
        assert_eq!((code, out.as_str()), (Some(134), "before\n"), "{err}");
        assert!(
            err.starts_with("fatal runtime error: stack overflow\n"),
            "{err}"
        );
    }
}
