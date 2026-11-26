//! B-2026-11-26-17: a built-in type's method named as a value with no
//! hand-written wrapper (`i64.checked_add`, `Vec.windows`,
//! `SortedSet.range`) gets one generated from its family
//! (`karac::method_value_gen`), for the methods the program names that
//! way. These pin a sample of each family the row named on both backends,
//! then sweep every method the method tables give a built-in type: each
//! generated wrapper checks, each scalar one is a value a program can name,
//! and the methods left without one are exactly the listed ones.

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-method-value-families-{name}-{}",
        std::process::id()
    ));
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

const FAMILIES: &str = "fn apply(f: Fn(i64, i64) -> Option[i64], a: i64, b: i64) -> Option[i64] {
    f(a, b)
}

fn main() {
    let ca = i64.checked_add;
    println(ca(1, 2));
    println(ca(i64.MAX, 1));
    let cm = u8.checked_mul;
    println(cm(16u8, 16u8));
    let cp = i32.checked_pow;
    println(cp(2, 10));
    println(apply(i64.checked_mul, 3, 4));
    let v: Vec[i64] = [1, 2, 3, 4];
    let w = Vec.windows;
    for s in w(v, 3) {
        println(s[0]);
    }
    let c = Vec.chunks;
    let parts = c(v, 3);
    println(f\"{parts.len()} {parts[1].len()}\");
    let mut ss: SortedSet[i64] = SortedSet.new();
    ss.insert(9);
    ss.insert(2);
    ss.insert(7);
    let r = SortedSet.range;
    let mut n = 0;
    for x in r(ss, 3, 10) {
        n += x;
    }
    println(n);
    let ri = SortedSet.range_inclusive;
    let mut n = 0;
    for x in ri(ss, 2, 7) {
        n += x;
    }
    println(n);
    let mut m: SortedMap[i64, String] = SortedMap.new();
    m.insert(9, \"c\".to_string());
    m.insert(1, \"a\".to_string());
    m.insert(5, \"b\".to_string());
    let mr = SortedMap.range;
    for (k, v) in mr(m, 0, 6) {
        println(f\"{k}={v}\");
    }
    let mri = SortedMap.range_inclusive;
    for (k, v) in mri(m, 5, 9) {
        println(f\"{k}={v}\");
    }
}
";

// Worked by hand: the windows of 3 over [1, 2, 3, 4] start at 1 and 2; its
// chunks of 3 are [1, 2, 3] and [4]; [3, 10) of {2, 7, 9} sums to 16 and
// [2, 7] to 9.
const FAMILIES_EXPECTED: &str = "Some(3)
None
None
Some(1024)
Some(12)
1
2
2 1
16
9
1=a
5=b
5=b
9=c
";

#[test]
fn method_value_families_interpreted() {
    let file = program("families-interp", FAMILIES);
    let f = file.to_str().unwrap();
    let (ok, out) = karac(&["check", f]);
    assert!(ok, "{out}");
    let (ok, out) = karac(&["__mir-run", f]);
    assert!(ok, "{out}");
    assert_eq!(out, FAMILIES_EXPECTED);
}

#[cfg(feature = "llvm")]
#[test]
fn method_value_families_compiled() {
    let file = program("families-run", FAMILIES);
    let (ok, out) = karac(&["run", file.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert_eq!(out, FAMILIES_EXPECTED);
}

const HAND_WRITTEN: &str = include_str!("../runtime/stdlib/source/method_values.kara");

fn hand_written(ty: &str, m: &str) -> bool {
    let name = format!("fn method_value_{ty}_{m}");
    HAND_WRITTEN.contains(&format!("{name}(")) || HAND_WRITTEN.contains(&format!("{name}["))
}

/// Every method the tables give a built-in type with no hand-written
/// wrapper: `(type, method, generated source)`.
fn sweep() -> Vec<(&'static str, &'static str, Option<String>)> {
    let mut out = Vec::new();
    for ty in karac::method_value_gen::TYPES {
        for m in karac::typechecker::builtin_type_methods(ty) {
            if !hand_written(ty, m) {
                out.push((*ty, m, karac::method_value_gen::wrapper_source(ty, m)));
            }
        }
    }
    out
}

#[test]
fn every_generated_method_value_wrapper_checks() {
    let mut src: String = sweep().into_iter().filter_map(|(_, _, s)| s).collect();
    src.push_str("\nfn main() {}\n");
    let file = program("sweep-wrappers", &src);
    let (ok, out) = karac(&["check", file.to_str().unwrap()]);
    assert!(ok, "{out}");
}

/// A program naming each generated wrapper that is not generic as a value,
/// through the library path a program takes.
fn scalar_values() -> String {
    let mut body = String::new();
    for (i, (ty, m, src)) in sweep().into_iter().enumerate() {
        let Some(src) = src else { continue };
        if src.contains(&format!("fn method_value_{ty}_{m}[")) {
            continue;
        }
        body.push_str(&format!("    let _v{i} = {ty}.{m};\n"));
    }
    format!("fn main() {{\n{body}    println(\"ok\");\n}}\n")
}

#[test]
fn every_scalar_family_method_is_a_value_interpreted() {
    let file = program("sweep-values-interp", &scalar_values());
    let f = file.to_str().unwrap();
    let (ok, out) = karac(&["check", f]);
    assert!(ok, "{out}");
    let (ok, out) = karac(&["__mir-run", f]);
    assert!(ok, "{out}");
    assert_eq!(out, "ok\n");
}

#[cfg(feature = "llvm")]
#[test]
fn every_scalar_family_method_is_a_value_compiled() {
    let file = program("sweep-values-run", &scalar_values());
    let (ok, out) = karac(&["run", file.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert_eq!(out, "ok\n");
}

#[test]
fn methods_with_no_generated_value_are_the_listed_ones() {
    let left: Vec<String> = sweep()
        .into_iter()
        .filter(|(_, _, s)| s.is_none())
        .map(|(ty, m, _)| format!("{ty}.{m}"))
        .collect();
    // Each is refused as a value with the closure to write. A new table
    // method lands here until a family covers it.
    let expected: &[&str] = &[
        // A value by another path: the comparator (B-2026-10-21-30) and the
        // library impl's `first` (B-2026-11-25-40).
        "f32.total_cmp",
        "f64.total_cmp",
        "Vec.first",
        // Rust's `Hash.hash` takes a hasher, which no family spells.
        "i8.hash",
        "i16.hash",
        "i32.hash",
        "i64.hash",
        "i128.hash",
        "isize.hash",
        "u8.hash",
        "u16.hash",
        "u32.hash",
        "u64.hash",
        "u128.hash",
        "usize.hash",
        "char.hash",
        // Refused as calls too: a 128-bit `isqrt` (core-types.md).
        "i128.isqrt",
        "u128.isqrt",
        // A pointer-sized integer's byte count is the target's.
        "isize.to_be_bytes",
        "isize.to_le_bytes",
        "isize.to_ne_bytes",
        "usize.to_be_bytes",
        "usize.to_le_bytes",
        "usize.to_ne_bytes",
        // An element-dependent result: `Vec[U]` for a sequence of
        // sequences, `String` for text (collections.md).
        "Vec.concat",
        // A length the call site's expected type gives.
        "Vec.try_into_array",
        // An `unsafe fn`, called inside `unsafe { }`.
        "Vec.get_unchecked",
    ];
    let mut want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
    let mut got = left.clone();
    want.sort();
    got.sort();
    assert_eq!(got, want, "uncovered methods changed: {left:?}");
}

#[test]
fn a_method_with_no_family_is_still_refused_with_the_closure() {
    let file = program("no-family", "fn main() {\n    let f = Vec.concat;\n}\n");
    let (ok, out) = karac(&["check", file.to_str().unwrap()]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("`Vec.concat` names a method of `Vec`, which has no function value yet"),
        "{out}"
    );
}
