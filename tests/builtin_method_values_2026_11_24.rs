//! B-2026-11-24-7: a built-in type's method named as a value, as design.md
//! § Function values gives every method (`T.m` is a function whose first
//! parameter is the receiver). `i64.abs` was "no associated constant",
//! `Str.to_string` and `String.len` "no associated function", and
//! `String.trim`, which the library defines, checked as the library impl's
//! method and then failed to lower. Each now names the library's
//! `method_value_{T}_{m}` (runtime/stdlib/source/method_values.kara), so it
//! behaves as a function of that signature written by hand.

use std::path::PathBuf;
use std::process::Command;

const PROGRAM: &str = "fn main() {
    let n: Vec[i64] = [-1, 2, -3].iter().map(i64.abs).collect();
    println(n[0] + n[2]);
    let o: Vec[i64] = [-4, 5].into_iter().map(i64.abs).collect();
    println(o[0]);
    let parts: Vec[String] = \"a,bb\".split(\",\").map(Str.to_string).collect();
    println(parts[1]);
    let v: Vec[String] = [\"xyz\".to_string(), \" q \".to_string()];
    let lens: Vec[i64] = v.iter().map(String.len).collect();
    println(lens[0] + lens[1]);
    let up: Vec[String] = v.iter().map(String.to_uppercase).collect();
    println(up[0]);
    let t: Vec[Str] = v.iter().map(String.trim).collect();
    println(t[1]);
    let words: Vec[Str] = [\" a\", \"b \"];
    let tw: Vec[Str] = words.iter().map(Str.trim).collect();
    println(f\"{tw[0]}{tw[1]}\");
    let f: Fn(i64) -> i64 = i64.abs;
    println(f(-9));
    let g = f64.sqrt;
    println(g(16.0));
    let letters: Vec[char] = \"a1b\".chars().filter(char.is_alphabetic).collect();
    println(letters.len());
    let s: Vec[String] = [1u8, 2u8].iter().map(u8.to_string).collect();
    println(s[0] + s[1]);
    println(String.len(\"abc\".to_string()));
}
";

const EXPECTED: &str = "4\n4\nbb\n6\nXYZ\nq\nab\n9\n4.0\n2\n12\n3\n";

fn program(name: &str, src: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("karac-method-values-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    file
}

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn builtin_method_values_check_and_interpret() {
    let file = program("interp", PROGRAM);
    let f = file.to_str().unwrap();
    let (ok, out) = karac(&["check", f]);
    assert!(ok, "{out}");
    let (ok, out) = karac(&["__mir-run", f]);
    assert!(ok, "{out}");
    assert_eq!(out, EXPECTED);
}

#[cfg(feature = "llvm")]
#[test]
fn builtin_method_values_run_compiled() {
    let file = program("run", PROGRAM);
    let (ok, out) = karac(&["run", file.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert_eq!(out, EXPECTED);
}

#[test]
fn an_unsigned_abs_named_as_a_value_is_still_refused_by_the_checker() {
    // `u64` has no `abs` (method_numeric.rs), so there is no function to
    // name: the checker refuses it, and nothing reaches the MIR builder.
    let file = program(
        "uabs",
        "fn main() {\n    let v: Vec[u64] = [1u64].iter().map(u64.abs).collect();\n    println(v.len());\n}\n",
    );
    let (ok, out) = karac(&["check", file.to_str().unwrap()]);
    assert!(!ok, "{out}");
    assert!(out.contains("abs"), "{out}");
    assert!(!out.contains("not lowered"), "{out}");
}
