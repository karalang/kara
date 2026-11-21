//! Found writing kata 2780 (Minimum Index of a Valid Split).
//!
//! B-2026-11-20-147: `now > left.get(k) ?? 0`. `??` binds loosest of all
//! (design.md § `?` and `??`), so this is `(now > left.get(k)) ?? 0`: it
//! failed "cannot compare 'i64' and 'Option[ref i64]'" and then "'??'
//! requires an `Option` or a `Result` on the left, found 'bool'" whose
//! help was to drop the fallback, the wrong edit. The `??` now names the
//! precedence once, and `karac fix` writes `now > (left.get(k) ?? 0)`.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn karac(cmd: &str, src: &str) -> (String, String) {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "karac-coalesce-cmp-left-{}-{n}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([cmd, path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let after = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    (
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        after,
    )
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

const WRITTEN: &str = "fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1, 5);
    let now = 3;
    let o: Option[i64] = None;
    if now > m.get(1) ?? 0 {
        println(\"above\");
    }
    println(f\"{now < m.get(1) ?? 0} {now == o ?? 3}\");
}
";

const FIXED: &str = "fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1, 5);
    let now = 3;
    let o: Option[i64] = None;
    if now > (m.get(1) ?? 0) {
        println(\"above\");
    }
    println(f\"{now < (m.get(1) ?? 0)} {now == (o ?? 3)}\");
}
";

#[test]
fn comparison_before_nil_coalesce_names_precedence() {
    let (out, _) = karac("check", WRITTEN);
    assert!(
        out.contains(":6:8: `??` binds more loosely than `>`: `a > b ?? c` is `(a > b) ?? c`, which compares the wrapper itself; write `a > (b ?? c)` to compare the value it holds"),
        "{out}"
    );
    assert!(out.contains("`??` binds more loosely than `<`"), "{out}");
    assert!(out.contains("`??` binds more loosely than `==`"), "{out}");
    assert!(!out.contains("cannot compare"), "{out}");
    assert!(!out.contains("drop the '??"), "{out}");
    assert!(out.contains("3 error(s) found"), "{out}");
}

#[test]
fn comparison_before_nil_coalesce_fix_parenthesizes() {
    let (_, after) = karac("fix", WRITTEN);
    assert_eq!(after, FIXED);
    let (out, _) = karac("check", FIXED);
    assert!(out.contains("All checks passed"), "{out}");
    assert_eq!(run(FIXED), "true true\n");
}

#[test]
fn bool_compared_then_coalesced_still_drops_fallback() {
    // A comparison of two plain values under `??` is still a `bool`, and the
    // old help stands.
    let (out, _) = karac(
        "check",
        "fn main() {\n    let a = 1;\n    println(f\"{a > 2 ?? true}\");\n}\n",
    );
    assert!(out.contains("drop the '?? ...'"), "{out}");
}
