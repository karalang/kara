//! The fourth round of the corpus fix-coverage sweep (review thread,
//! 2026-11-16): `partition` over an array literal's `iter()`, and text given
//! where its bytes are wanted. Each test runs `karac fix` and checks the
//! program it leaves, then runs it.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

/// Writes `src`, runs `karac fix`, and returns the fixed text, the output of
/// `karac check` on it and the stdout of `karac run`.
fn fixed(tag: &str, src: &str) -> (String, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-fixcov4-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap();
    let fix = karac(&["fix", p]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let after = std::fs::read_to_string(&f).unwrap();
    let check = karac(&["check", p]);
    let text = String::from_utf8_lossy(&check.stdout).into_owned()
        + &String::from_utf8_lossy(&check.stderr);
    let run = karac(&["run", p]);
    let out = String::from_utf8_lossy(&run.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    (after, text, out)
}

/// B-2026-11-16-250: an array literal's element type is still a type
/// variable when `partition` is checked, so the `.cloned()` fix that a `Vec`
/// receiver gets was not offered.
#[test]
fn partition_over_an_array_literal_takes_the_cloned_fix() {
    let (after, check, out) = fixed(
        "partarr",
        "fn main() {
    let v = [1, 2, 3, 4, 5, 6, 7];
    let (evens, odds): (Vec[i64], Vec[i64]) = v.iter().partition(|x| x % 2 == 0);
    println(f\"{evens.len()}:{odds.len()}\");
}
",
    );
    assert!(
        after.contains("v.iter().cloned().partition(|x| x % 2 == 0)"),
        "{after}"
    );
    assert!(check.contains("All checks passed"), "{check}");
    assert_eq!(out, "3:4\n");
}

/// B-2026-11-16-251: a `String` or `Str` where a `Slice[u8]` is wanted reads
/// its bytes with `.bytes()`.
#[test]
fn text_where_bytes_are_wanted_reads_its_bytes() {
    let (after, check, out) = fixed(
        "bytes",
        "fn count(b: Slice[u8]) -> i64 { b.len() }
struct P { name: String }
fn main() {
    let t = \"abc\";
    let s = \"wxyz\".to_string();
    let p = P { name: \"hello\".to_string() };
    println(f\"{count(t)} {count(s)} {count(p.name)}\");
}
",
    );
    assert!(after.contains("count(t.bytes())"), "{after}");
    assert!(after.contains("count(s.bytes())"), "{after}");
    assert!(after.contains("count(p.name.bytes())"), "{after}");
    assert!(check.contains("All checks passed"), "{check}");
    assert_eq!(out, "3 4 5\n");
}

/// B-2026-11-16-251: a child's stdin takes bytes; `write(text)` gets the
/// same fix.
#[test]
fn text_written_to_a_child_stdin_reads_its_bytes() {
    let (after, check, out) = fixed(
        "stdin",
        "import std.process.{Command, Stdio};
fn main() {
    let text = \"hi\\n\".to_string();
    let child = Command.new(\"cat\").stdin(Stdio.piped()).spawn().unwrap();
    match child.stdin() {
        Some(w) => {
            let _ = w.write(text);
            let _ = w.close();
        }
        None => {}
    }
    let _ = child.wait();
}
",
    );
    assert!(after.contains("w.write(text.bytes())"), "{after}");
    assert!(check.contains("All checks passed"), "{check}");
    assert_eq!(out, "hi\n");
}

/// B-2026-11-16-252: a `par` block written as statements whose tail computes
/// from the `let`s' values: the branches' tuple binds their patterns, and the
/// tail becomes a statement of its own after the block.
#[test]
fn a_par_block_tail_over_its_lets_moves_after_the_comma_form() {
    let (after, check, out) = fixed(
        "partail",
        "struct Point { x: i64, y: i64 }
fn twice(n: i64) -> i64 { n * 2 }
fn main() {
    let p = Point { x: 10, y: 20 };
    let total = par {
        let a = twice(p.x);
        let b = twice(p.y);
        a + b
    };
    println(total);
}
",
    );
    assert!(after.contains("let (a, b) = par {"), "{after}");
    assert!(after.contains("    let total = a + b;\n"), "{after}");
    assert!(check.contains("All checks passed"), "{check}");
    assert_eq!(out, "60\n");
}
