//! Two diagnostics about function values (core semantics §1.1, §9.6;
//! design.md § Escaping function values).
//!
//! - A value of function type is move-only, so a `.to_vec()` or `.clone()`
//!   of a collection of them is refused by `karac check`. It passed check
//!   and then failed at run ("clone of Fn(i64) -> i64 needs its Clone
//!   body"), B-2026-11-03-3.
//! - `move |..|` named `own |..|` as the replacement, a prefix §9.6 removed;
//!   the error now says to delete `move`, with the edit (B-2026-11-03-4).

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> (Output, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "karac-fn-value-clone-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (out, path)
}

/// `karac check --output=json`: each diagnostic's line, column, message and
/// replacement text, if any.
fn check(tag: &str, src: &str) -> Vec<(u64, u64, String, Option<String>)> {
    let (out, path) = karac(tag, &["check", "--output=json"], src);
    let _ = std::fs::remove_file(path);
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{tag}: {e}"));
    json["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .map(|d| {
            (
                d["line"].as_u64().unwrap_or(0),
                d["column"].as_u64().unwrap_or(0),
                d["message"].as_str().unwrap_or("").to_string(),
                d["replacement"]["text"].as_str().map(str::to_string),
            )
        })
        .collect()
}

fn runs(tag: &str, src: &str, want: &str) {
    let (out, path) = karac(tag, &["__mir-run"], src);
    let _ = std::fs::remove_file(path);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        want,
        "{tag}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_collection_of_function_values_does_not_clone() {
    for (tag, src, at, what, hint) in [
        (
            "to-vec",
            "fn main() {\n    let fs: Vec[Fn(i64) -> i64] = [|x: i64| x + 1].to_vec();\n    println(fs[0](2));\n}\n",
            (2, 35),
            "`.to_vec()` on 'Vec[Fn(i64) -> i64]'",
            "build the `Vec` with `vec![..]` or `push` instead",
        ),
        (
            "clone",
            "fn main() {\n    let fs: Vec[Fn(i64) -> i64] = vec![|x: i64| x + 1];\n    let gs = fs.clone();\n    println(gs[0](2));\n}\n",
            (3, 14),
            "`.clone()` on 'Vec[Fn(i64) -> i64]'",
            "wrap the function in a `shared struct`",
        ),
    ] {
        let found = check(tag, src);
        assert_eq!(found.len(), 1, "{tag}: {found:?}");
        let (line, col, msg, _) = &found[0];
        assert_eq!((*line, *col), at, "{tag}: {msg}");
        assert!(
            msg.contains(what)
                && msg.contains("'Fn(i64) -> i64' is move-only and has no `.clone()`")
                && msg.contains(hint),
            "{tag}: {msg}"
        );
    }
    // The ways the hint names build the same `Vec`, and run.
    let built = "fn main() {\n    let fs: Vec[Fn(i64) -> i64] = vec![|x: i64| x + 1];\n    \
                 let mut gs: Vec[Fn(i64) -> i64] = Vec.new();\n    gs.push(|x: i64| x * 2);\n    \
                 println(fs[0](2) + gs[0](2));\n}\n";
    assert!(check("built", built).is_empty());
    runs("built", built, "7\n");
}

#[test]
fn a_move_closure_is_told_to_drop_move() {
    let src = "fn main() {\n    let f = move |x: i64| x + 1;\n    println(f(2));\n}\n";
    let found = check("move", src);
    assert_eq!(found.len(), 1, "{found:?}");
    let (line, col, msg, fix) = &found[0];
    assert_eq!((*line, *col), (2, 13), "{msg}");
    assert!(
        msg.contains("the `move` keyword is not used in Kāra: delete it")
            && msg.contains("inferred from its body")
            && !msg.contains("own |"),
        "{msg}"
    );
    assert_eq!(fix.as_deref(), Some(""), "{msg}");
    // `karac fix` applies it, and what is left checks and runs.
    let (_, path) = karac("move-fix", &["fix"], src);
    let fixed = std::fs::read_to_string(&path).expect("read");
    let _ = std::fs::remove_file(path);
    assert_eq!(
        fixed,
        "fn main() {\n    let f = |x: i64| x + 1;\n    println(f(2));\n}\n"
    );
    assert!(check("move-fixed", &fixed).is_empty());
    runs("move-fixed", &fixed, "3\n");
}

/// A method call on a function value checked to an error with no
/// diagnostic, and MIR then said "a type that failed to check is not
/// lowered yet" (B-2026-11-03-5). `clone` is told the move-only rule;
/// any other method that a function value has none.
#[test]
fn a_method_on_a_function_value_is_named() {
    for (tag, call, needle) in [
        (
            "fn-clone",
            "f.clone()",
            "a value of function type 'Fn(i64) -> i64' is move-only; it cannot be cloned",
        ),
        (
            "fn-other",
            "f.foo()",
            "no method 'foo' on function type 'Fn(i64) -> i64': a function value has no methods",
        ),
    ] {
        let src = format!(
            "fn main() {{\n    let f: Fn(i64) -> i64 = |x: i64| x + 1;\n    let g = {call};\n    println(g(2));\n}}\n"
        );
        let found = check(tag, &src);
        assert_eq!(found.len(), 1, "{tag}: {found:?}");
        let (line, col, msg, _) = &found[0];
        assert_eq!((*line, *col), (3, 13), "{tag}: {msg}");
        assert!(
            msg.contains(needle) && !msg.contains("not lowered"),
            "{tag}: {msg}"
        );
    }
    // The copy the clone hint names: a closure literal with only `Copy`
    // captures is copied by assignment (core semantics §1.1).
    let copied =
        "fn main() {\n    let f = |x: i64| x + 1;\n    let g = f;\n    println(f(1) + g(2));\n}\n";
    assert!(check("copied", copied).is_empty());
    runs("copied", copied, "5\n");
}
