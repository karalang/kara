//! B-2026-11-25-40: design.md § Method calls, "Method calls and UFCS are
//! the same call", for a built-in type's own methods. `i64.abs(-3)`,
//! `Vec.len(v)` and `char.is_alphabetic(c)` were refused "no associated
//! function" (E0236) because UFCS knew only the methods an impl declares;
//! the call is now the receiver form (`method_ufcs::builtin_type_has_method`),
//! with a bare literal receiver taking the named type (`i32.wrapping_add(
//! 2147483647, 1)` wraps at 32 bits). Named as a value, the method is the
//! library's `method_value_{T}_{m}` (runtime/stdlib/source/method_values.kara),
//! generic over a container's elements; a library container's impl method
//! (`Vec.first`) lowers as a value; and a value lent straight to a `Fn`
//! parameter (`run(i64.abs, -5)`) is built in a temporary.

use std::path::PathBuf;
use std::process::Command;

const UFCS: &str = "fn main() {
    println(i64.abs(-3));
    println(i64.pow(2, 10));
    println(i32.wrapping_add(2147483647, 1));
    println(i8.signum(-7));
    println(u8.is_ascii_digit(49u8));
    println(u64.count_ones(255u64));
    println(usize.max(3, 9));
    println(f64.sqrt(16.0));
    println(f32.floor(2.5));
    println(f64.max(1.5, 2.5));
    println(char.is_alphabetic('x'));
    println(char.to_ascii_uppercase('q'));
    println(bool.to_string(true));
    let s = \"héllo\".to_string();
    println(String.len(s));
    println(String.contains(s, \"ll\"));
    let t: Str = \"abc\";
    println(Str.starts_with(t, \"ab\"));
    let mut v = [1, 2, 3];
    Vec.push(mut v, 4);
    println(Vec.len(v));
    println(Vec.contains(v, 4));
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1, 2);
    println(Map.contains_key(m, 1));
    let mut st: Set[i64] = Set.new();
    st.insert(5);
    println(Set.len(st));
    let mut sm: SortedMap[i64, i64] = SortedMap.new();
    sm.insert(3, 4);
    println(SortedMap.len(sm));
    let mut ss: SortedSet[i64] = SortedSet.new();
    ss.insert(3);
    ss.insert(1);
    println(SortedSet.min(ss));
}
";

const UFCS_EXPECTED: &str = "3
1024
-2147483648
-1
true
8
9
4.0
2.0
2.5
true
Q
true
6
true
true
4
true
true
1
1
Some(1)
";

const VALUES: &str = "fn run(f: Fn(i64) -> i64, x: i64) -> i64 {
    f(x)
}

fn run_generic[F: Fn(i64) -> i64](f: F, x: i64) -> i64 {
    f(x)
}

fn main() {
    let p = i64.pow;
    println(p(3, 4));
    let w = i32.wrapping_add;
    println(w(2147483647i32, 1i32));
    let c = u64.count_ones;
    println(c(7u64));
    let d = u8.is_ascii_digit;
    println(d(55u8));
    let mx = f64.max;
    println(mx(1.0, 3.0));
    let has = String.contains;
    println(has(\"abc\".to_string(), \"bc\"));
    let st = Str.starts_with;
    println(st(\"abc\", \"x\"));
    let lens: Vec[i64] = [[1], [2, 3]].iter().map(Vec.len).collect();
    println(lens);
    let first = Vec.first;
    println(first([4, 5]));
    let mut s: Set[i64] = Set.new();
    s.insert(2);
    let sl = Set.len;
    println(sl(s));
    let mut ss: SortedSet[i64] = SortedSet.new();
    ss.insert(8);
    ss.insert(6);
    let smin = SortedSet.min;
    println(smin(ss));
    println(run(i64.abs, -5));
    println(run_generic(i64.signum, -5));
}
";

const VALUES_EXPECTED: &str = "81
-2147483648
3
true
3.0
true
false
[1, 2]
Some(4)
1
Some(6)
5
-1
";

fn program(name: &str, src: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("karac-builtin-ufcs-{name}-{}", std::process::id()));
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

fn interprets(name: &str, src: &str, expected: &str) {
    let file = program(name, src);
    let f = file.to_str().unwrap();
    let (ok, out) = karac(&["check", f]);
    assert!(ok, "{out}");
    let (ok, out) = karac(&["__mir-run", f]);
    assert!(ok, "{out}");
    assert_eq!(out, expected);
}

#[test]
fn builtin_ufcs_calls_every_family_interpreted() {
    interprets("ufcs-interp", UFCS, UFCS_EXPECTED);
}

#[test]
fn builtin_method_values_every_family_interpreted() {
    interprets("values-interp", VALUES, VALUES_EXPECTED);
}

#[cfg(feature = "llvm")]
#[test]
fn builtin_ufcs_calls_every_family_compiled() {
    let file = program("ufcs-run", UFCS);
    let (ok, out) = karac(&["run", file.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert_eq!(out, UFCS_EXPECTED);
}

#[cfg(feature = "llvm")]
#[test]
fn builtin_method_values_every_family_compiled() {
    let file = program("values-run", VALUES);
    let (ok, out) = karac(&["run", file.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert_eq!(out, VALUES_EXPECTED);
}

#[test]
fn builtin_ufcs_receiver_of_another_type_is_refused() {
    let file = program(
        "mismatch",
        "fn main() {\n    let n: i32 = 3;\n    println(i64.abs(n));\n}\n",
    );
    let (ok, out) = karac(&["check", file.to_str().unwrap()]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("`i64.abs` takes a `i64` receiver, found `i32`"),
        "{out}"
    );
}

#[test]
fn builtin_method_with_no_function_value_names_the_closure() {
    // `sort_by` (it takes a closure) and `floor` (B-2026-11-26-70) have no
    // wrapper, hand-written or generated: the refusal is about the method,
    // not "no associated constant" (the probe's wording). `i64.checked_add`
    // and `Vec.windows`, refused here before, are values since
    // B-2026-11-26-17 (tests/method_value_families_2026_11_26.rs).
    let file = program(
        "no-value",
        "fn main() {\n    let f = Vec.sort_by;\n    let g = SortedMap.floor;\n}\n",
    );
    let (ok, out) = karac(&["check", file.to_str().unwrap()]);
    assert!(!ok, "{out}");
    assert!(!out.contains("associated constant"), "{out}");
    assert!(
        out.contains("`Vec.sort_by` names a method of `Vec`"),
        "{out}"
    );
    assert!(
        out.contains("`SortedMap.floor` names a method of `SortedMap`"),
        "{out}"
    );
}

#[test]
fn builtin_associated_functions_keep_the_type_receiver_path() {
    // `Vec.new()` / `String.with_capacity(n)` are no methods: UFCS leaves them.
    let file = program(
        "assoc",
        "fn main() {\n    let mut v: Vec[i64] = Vec.new();\n    v.push(1);\n    let s = String.with_capacity(4);\n    println(f\"{v.len()} {s.len()}\");\n}\n",
    );
    let (ok, out) = karac(&["__mir-run", file.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert_eq!(out, "1 0\n");
}
