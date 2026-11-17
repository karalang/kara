//! Two gaps kata 1182 (Maximum Subarray Sum with One Deletion) found in
//! LeetCode's Rust solutions carried to Kāra.
//!
//! B-2026-11-17-90: a method's argument did not widen into its parameter's
//! type. design.md § Mixed-width operands says "At a binding or an
//! argument, the whole table applies", and a free function's argument
//! widened, but `best.max(a)` with `best: i64` and `a: i32` was refused
//! with "`max` expects an argument of type `i64`, got `i32`". Narrowing is
//! still refused.
//!
//! B-2026-11-17-91: in `fold((arr[0], 0, arr[0]), |(keep, del, best), x|
//! ..)` over `i32`s, the seed's `0` takes the `i64` default, and the
//! refusal at the use did not say why or offer the `0i32` suffix that
//! `let (keep, del) = (arr[0], 0)` gets. The closure's tuple parameter now
//! binds the seed's literals as that `let` does, and `karac fix` writes the
//! suffix.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> (Output, String) {
    let path =
        std::env::temp_dir().join(format!("karac-kata1182-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let after = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    (out, after)
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

#[test]
fn method_argument_widens_into_its_parameter_type() {
    let src = "fn f(a: i32, b: i64) -> i64 { b.max(a) }\n\
               fn g(a: i32, b: i64) -> i64 { b.min(a) }\n\
               fn h(a: f32, b: f64) -> f64 { b.max(a) }\n\
               fn k(a: u8, b: i16) -> i16 { b.max(a) }\n\
               fn main() { println(f\"{f(-3, -9)} {g(-3, -9)} {h(1.5, 0.5)} {k(200, -1)}\"); }\n";
    let (out, _) = karac("widen-check", &["check"], src);
    assert!(out.status.success(), "{}", text(&out));
    let (out, _) = karac("widen-run", &["__mir-run"], src);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "-3 -9 1.5 200\n");
}

#[test]
fn method_argument_never_narrows() {
    for (sig, want) in [
        ("fn f(a: i64, b: i32) -> i32 { b.max(a) }", "got `i64`"),
        ("fn f(a: u32, b: i32) -> i32 { b.max(a) }", "got `u32`"),
        ("fn f(a: i64, b: f64) -> f64 { b.max(a) }", "got `i64`"),
        ("fn f(a: f64, b: f32) -> f32 { b.min(a) }", "got `f64`"),
    ] {
        let src = format!("{sig}\nfn main() {{ }}\n");
        let (out, _) = karac("narrow", &["check"], &src);
        assert!(!out.status.success(), "{sig}: {}", text(&out));
        assert!(text(&out).contains(want), "{sig}: {}", text(&out));
    }
}

const CHAIN: &str = "fn best(arr: Vec[i32]) -> i32 {\n\
    arr.iter()\n\
        .skip(1)\n\
        .fold((arr[0], 0, arr[0]), |(keep, del, best), x| {\n\
            let del = (del + x).max(keep);\n\
            let keep = (keep + x).max(x);\n\
            (keep, del, best.max(keep).max(del))\n\
        })\n\
        .2\n\
}\n\
fn main() { println(f\"{best([1, -2, 0, 3])}\"); }\n";

#[test]
fn fold_seed_literal_refusal_names_the_suffix() {
    let (out, _) = karac("seed-check", &["check"], CHAIN);
    assert!(!out.status.success());
    let t = text(&out);
    assert!(
        t.contains("`del` is an 'i64' because the literal it is bound to took the default")
            && t.contains("write that literal `0i32`"),
        "{t}"
    );
}

#[test]
fn karac_fix_writes_the_fold_seed_suffix() {
    let (out, after) = karac("seed-fix", &["fix"], CHAIN);
    assert!(out.status.success(), "{}", text(&out));
    assert!(after.contains(".fold((arr[0], 0i32, arr[0])"), "{after}");
    let (out, _) = karac("seed-fixed-run", &["__mir-run"], &after);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "4\n");
}
