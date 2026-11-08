//! B-2026-11-08-16 — a `usize` (or `isize`) size expression where an `i64`
//! is wanted, as after a Rust template's `let n = n as usize;`, gets
//! `as i64` as a machine fix (the operand parenthesized unless it is a
//! postfix form); before, the message named the edit and `karac fix` had
//! none. The fixed program checks. A use whose `usize` root is itself
//! fixed to `i64` in the same pass gets no cast.

use std::process::Command;

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn file(tag: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-usize-size-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

/// The message `karac check` gives, then the program `karac fix` leaves,
/// which must check.
fn check_then_fix(tag: &str, src: &str, message: &str, fixed: &str) {
    let path = file(tag, src);
    let p = path.to_str().unwrap();
    let (ok, out) = karac(&["check", p]);
    assert!(
        !ok && out.contains(message),
        "{tag}: want `{message}`:\n{out}"
    );
    for _ in 0..3 {
        karac(&["fix", p]);
    }
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, fixed, "{tag}: karac fix");
    let (ok, out) = karac(&["check", p]);
    assert!(ok, "{tag}: the fixed program checks:\n{out}");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_usize_size_argument_is_fixed_with_as_i64() {
    // A program that declares FFI items keeps its `as usize` casts (one may
    // be what a foreign function is given), so the use is where the edit
    // goes.
    check_then_fix(
        "capacity",
        "/// # Safety\n/// `abs` is libc's.\nunsafe extern \"C\" {\n    fn abs(x: i32) -> i32;\n}\nfn sums(x: f64) -> i64 {\n    let n = x as usize;\n    let mut v: Vec[i64] = Vec.with_capacity(n * (n + 1) / 2);\n    let w: Vec[i64] = Vec.with_capacity(n);\n    v.push(1);\n    v.len() + w.len()\n}\nfn main() { println(sums(4.0)); }\n",
        "implicit coercion from 'usize' to 'i64' would change sign",
        "/// # Safety\n/// `abs` is libc's.\nunsafe extern \"C\" {\n    fn abs(x: i32) -> i32;\n}\nfn sums(x: f64) -> i64 {\n    let n = x as usize;\n    let mut v: Vec[i64] = Vec.with_capacity((n * (n + 1) / 2) as i64);\n    let w: Vec[i64] = Vec.with_capacity(n as i64);\n    v.push(1);\n    v.len() + w.len()\n}\nfn main() { println(sums(4.0)); }\n",
    );
}

/// The Rust template's own shape, `let n = n as usize;` over an `i32`: the
/// cast is fixed to `as i64`, so the uses need no cast of their own and get
/// none (each would be left as a redundant `as i64` in parentheses).
#[test]
fn a_usize_use_takes_no_cast_when_its_root_is_fixed() {
    check_then_fix(
        "root",
        "fn sums(n: i32) -> i64 {\n    let n = n as usize;\n    let mut v: Vec[i64] = Vec.with_capacity(n * (n + 1) / 2);\n    let m = n;\n    let w: Vec[i64] = Vec.with_capacity(m);\n    v.push(1);\n    v.len() + w.len()\n}\nfn main() { println(sums(4)); }\n",
        "implicit coercion from 'usize' to 'i64' would change sign",
        "fn sums(n: i32) -> i64 {\n    let n = n as i64;\n    let mut v: Vec[i64] = Vec.with_capacity(n * (n + 1) / 2);\n    let m = n;\n    let w: Vec[i64] = Vec.with_capacity(m);\n    v.push(1);\n    v.len() + w.len()\n}\nfn main() { println(sums(4)); }\n",
    );
    check_then_fix(
        "float",
        "fn sums(x: f64) -> i64 {\n    let n = x as usize;\n    let v: Vec[i64] = Vec.with_capacity(n * 2);\n    v.len()\n}\nfn main() { println(sums(4.0)); }\n",
        "implicit coercion from 'usize' to 'i64' would change sign",
        "fn sums(x: f64) -> i64 {\n    let n = x as i64;\n    let v: Vec[i64] = Vec.with_capacity(n * 2);\n    v.len()\n}\nfn main() { println(sums(4.0)); }\n",
    );
    check_then_fix(
        "param",
        "fn sums(n: usize) -> i64 {\n    let v: Vec[i64] = Vec.with_capacity(n * 2);\n    v.len()\n}\nfn main() { println(sums(4)); }\n",
        "implicit coercion from 'usize' to 'i64' would change sign",
        "fn sums(n: i64) -> i64 {\n    let v: Vec[i64] = Vec.with_capacity(n * 2);\n    v.len()\n}\nfn main() { println(sums(4)); }\n",
    );
}
