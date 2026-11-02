//! A panic's message and the string-slicing natives keep std's panic hook
//! out of a MIR binary (B-2026-11-02-24). `karac_mir_panic_set` held the
//! message in a `String` and switched the abort to its full report, and
//! `karac_mir_str_slice` / `karac_mir_str_view` formatted their messages
//! and made lossy text through `format!` and `String`: each kept a Rust
//! panic reachable, and with it the hook and its backtrace symbolizer, so
//! kata 38's and kata 282's in-order binaries were 312 KiB where other
//! katas' are 52-69 KB. Each program here is built and run, and its exit
//! code, stdout and stderr must equal the MIR interpreter's.
#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-panic-free-messages-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn karac(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "0")
        .env_remove("KARAC_RUNTIME")
        .env_remove("KARAC_FORCE_FULL_RUNTIME")
        .current_dir(dir);
    c
}

/// The interpreter's run of `src` and the built binary's, or `None`
/// (after saying so) when the runtime archive is missing and the run does
/// not require it.
fn both(tag: &str, src: &str) -> Option<(Output, Output, PathBuf)> {
    let dir = scratch(tag);
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let interp = karac(&dir)
        .args(["__mir-run", "main.kara"])
        .output()
        .unwrap();
    let exe = dir.join("main.out");
    let built = karac(&dir)
        .args(["build", "main.kara", "-o"])
        .arg(&exe)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).into_owned();
    if !built.status.success()
        && err.contains("not found")
        && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none()
    {
        eprintln!("skipping: no runtime archive: {err}");
        return None;
    }
    assert!(built.status.success(), "{err}");
    let ran = Command::new(&exe).current_dir(&dir).output().unwrap();
    Some((interp, ran, exe))
}

fn links_panic_hook(exe: &Path) -> bool {
    let bytes = std::fs::read(exe).unwrap();
    bytes.windows(14).any(|w| w == b"RUST_BACKTRACE")
}

/// Builds and runs `src`, which must agree with the interpreter, exit with
/// `code`, write `stderr` and link no panic hook.
fn check(tag: &str, src: &str, code: i32, stderr: &str) {
    let Some((interp, ran, exe)) = both(tag, src) else {
        return;
    };
    assert_eq!(interp.status.code(), Some(code), "{interp:?}");
    assert_eq!(ran.status.code(), Some(code), "{ran:?}");
    assert_eq!(ran.stdout, interp.stdout, "stdout");
    assert_eq!(String::from_utf8_lossy(&ran.stderr), stderr);
    assert_eq!(ran.stderr, interp.stderr, "stderr");
    assert!(!links_panic_hook(&exe), "std's panic hook is linked");
}

/// Kata 38's shape: an `unwrap` that may fire, `chars()` and formatted
/// integers, run to its end.
#[test]
fn mir_unwrap_that_may_fire_links_no_panic_hook() {
    let src = r#"fn digit_char(n: i64) -> char {
    char.try_from((n + 48) as u32).unwrap()
}
fn say(s: String) -> String {
    let mut out: String = "";
    let mut run: char = '0';
    let mut len = 0;
    for c in s.chars() {
        if len != 0 and c == run {
            len = len + 1;
        } else {
            if len != 0 {
                out.push(digit_char(len));
                out.push(run);
            }
            run = c;
            len = 1;
        }
    }
    if len != 0 {
        out.push(digit_char(len));
        out.push(run);
    }
    out
}
fn main() {
    let mut acc = 0;
    let mut k = 0;
    while k < 50 {
        let mut cur: String = f"{k + 1}";
        let mut step = 0;
        while step < 6 {
            cur = say(cur);
            step = step + 1;
        }
        for c in cur.chars() {
            acc = (acc * 31 + (c as i64)) % 1000000007;
        }
        k = k + 1;
    }
    println(f"{acc}");
}
"#;
    check("say", src, 0, "");
}

#[test]
fn mir_unwrap_on_err_reports_without_the_panic_hook() {
    let src = r#"fn get(n: i64) -> Result[i64, String] {
    if n > 3 { Err(f"too big {n}") } else { Ok(n) }
}
fn main() {
    let mut i: i64 = 0;
    while i < 10 {
        println(f"{get(i).unwrap()}");
        i = i + 1;
    }
}
"#;
    check(
        "unwrap",
        src,
        101,
        "runtime error: called unwrap() on Err\n  at main.kara:7:20\nError return trace:\n  main.kara:7:20\n",
    );
}

#[test]
fn mir_formatted_panic_reports_without_the_panic_hook() {
    let src = r#"fn main() {
    let mut i: i64 = 0;
    while i < 10 {
        if i == 7 {
            panic(f"boom {i}");
        }
        i = i + 1;
    }
}
"#;
    check(
        "panic",
        src,
        101,
        "runtime error: boom 7\n  at main.kara:5:13\nError return trace:\n  main.kara:5:13\n",
    );
}

/// A `substring` cut inside a code point: the message quotes the text as
/// `{:?}` does, a single quote unescaped and the rest escaped.
#[test]
fn mir_substring_cut_reports_without_the_panic_hook() {
    let src = "fn main() {\n    let s: String = f\"a'b\\\"c\\\\d\\ne{1}\u{e9}\";\n    let n = s.len();\n    println(f\"{n}\");\n    let t = s.substring(0, n - 1);\n    println(t);\n}\n";
    check(
        "substring",
        src,
        101,
        "runtime error: E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY: byte range 0..11 does not fall on UTF-8 char boundaries of \"a'b\\\"c\\\\d\\ne1\u{e9}\"\n  at main.kara:5:13\nError return trace:\n  main.kara:5:13\n",
    );
}

/// `slice` cut inside a code point makes the text lossy; an index range
/// past the end panics.
#[test]
fn mir_slice_and_index_range_report_without_the_panic_hook() {
    let src = "fn main() {\n    let s: String = f\"h\u{e9}llo{1}\";\n    let k = s.len() - 5;\n    let t = s.slice(1, k);\n    println(f\"{t.len()} {t}\");\n    let v = s[0..k + 9];\n    println(v);\n}\n";
    check(
        "slice",
        src,
        101,
        "runtime error: string slice bounds 0..11 out of range (len 7)\n  at main.kara:6:13\nError return trace:\n  main.kara:6:13\n",
    );
}
