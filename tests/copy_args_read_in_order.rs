//! A `Copy` argument lent to a bare parameter and written by a later
//! argument is a copy read at its own position (core semantics §4.1,
//! §5.8, §8.1): a generic `pr(c.n, bump(mut c))` at `T = i64`
//! (B-2026-11-01-35) and a whole `Array[i64, 2]` in `sh(a, set(mut a))`
//! (B-2026-11-01-38) print what the non-generic call and a `Copy` struct
//! already did. A non-`Copy` argument in the same spot is still a borrow
//! the later write conflicts with.

use std::path::PathBuf;
use std::process::{Command, Output};

fn program(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-copy-args-in-order-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .current_dir(path.parent().unwrap())
        .env("KARAC_AUTO_PAR", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn all(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// `karac check` passes and the MIR interpreter prints `expected`; under
/// `llvm`, `karac run` prints it too.
fn checks_and_prints(tag: &str, src: &str, expected: &str) {
    let path = program(tag, src);
    let out = karac(&["check"], &path);
    assert!(out.status.success(), "{}", all(&out));
    let mut cmds = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        cmds.push("run");
    }
    for cmd in cmds {
        let out = karac(&[cmd], &path);
        assert!(out.status.success(), "{cmd}: {}", all(&out));
        assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "{cmd}");
    }
}

#[test]
fn a_generic_copy_argument_is_read_before_a_later_write() {
    checks_and_prints(
        "generic",
        "struct C { n: i64 }\n\
         fn bump(c: mut ref C) -> i64 { c.n += 10; c.n }\n\
         fn pr[T: Display](a: T, b: T) -> String { f\"{a} {b}\" }\n\
         fn main() { let mut c = C { n: 1 }; println(pr(c.n, bump(mut c))); println(c.n); }\n",
        "1 11\n11\n",
    );
}

#[test]
fn a_whole_copy_array_argument_is_read_before_a_later_write() {
    checks_and_prints(
        "array",
        "fn sh(a: Array[i64, 2], q: i64) -> String { f\"{a[0]} {q}\" }\n\
         fn set(a: mut ref Array[i64, 2]) -> i64 { a[0] = 9; 9 }\n\
         fn main() {\n    let mut a: Array[i64, 2] = [1, 2];\n    println(sh(a, set(mut a)));\n    \
         println(sh(a, { a[0] = 4; 4 }));\n    println(a[0]);\n}\n",
        "1 9\n9 4\n4\n",
    );
}

#[test]
fn a_non_copy_argument_stays_lent_across_a_later_write() {
    let path = program(
        "vec",
        "fn first(v: Vec[i64], q: i64) -> i64 { v[0] + q }\n\
         fn grow(v: mut ref Vec[i64]) -> i64 { v.push(5); 5 }\n\
         fn main() { let mut v: Vec[i64] = [1, 2].to_vec(); println(first(v, grow(mut v))); }\n",
    );
    let out = karac(&["check"], &path);
    assert!(!out.status.success(), "{}", all(&out));
    assert!(all(&out).contains("E0516"), "{}", all(&out));
}
