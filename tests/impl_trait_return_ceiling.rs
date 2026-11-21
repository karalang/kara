//! B-2026-11-20-120 — design.md § `impl Trait`: a return-position
//! `impl Trait` that declares a ceiling holds the hidden type's methods to
//! it. `-> (impl Iterator[Item = i64] with reads(Clock))` whose `map`
//! closure printed was accepted and printed: lowering put the witness in
//! the existential's place, erasing the clause, and nothing read it. The
//! closures the returned value carries are now held to the ceiling.

use std::process::Command;

fn check(tag: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-impl-ceiling-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn doubled(ceiling: &str, closure: &str) -> String {
    format!(
        "fn doubled(xs: own Vec[i64]) -> (impl Iterator[Item = i64] with {ceiling}) {{
    xs.into_iter().map({closure})
}}

fn main() {{
    let s: i64 = doubled([1, 2, 3]).sum();
    println(f\"{{s}}\");
}}
"
    )
}

#[test]
fn a_closure_past_the_impl_trait_ceiling_is_refused() {
    let (ok, out) = check(
        "past",
        &doubled(
            "reads(Clock)",
            "|x| {\n        println(f\"{x}\");\n        x * 2\n    }",
        ),
    );
    assert!(!ok, "{out}");
    assert!(out.contains("has the ceiling [reads(Clock)]"), "{out}");
    assert!(out.contains("performs writes(Stdout)"), "{out}");
}

#[test]
fn a_closure_within_the_impl_trait_ceiling_is_accepted() {
    let (ok, out) = check(
        "within",
        &doubled(
            "writes(Stdout)",
            "|x| {\n        println(f\"{x}\");\n        x * 2\n    }",
        ),
    );
    assert!(ok, "{out}");
}

/// `allocates(Heap)` and `panics` need no room in a ceiling, as in a
/// signature: a closure that only computes is within any ceiling.
#[test]
fn a_closure_with_no_effect_is_within_any_ceiling() {
    let (ok, out) = check("pure", &doubled("reads(Clock)", "|x| x * 2"));
    assert!(ok, "{out}");
}

/// A closure the body calls before it returns is the function's own
/// effect, not the hidden type's: only what the returned value carries is
/// held to the ceiling.
#[test]
fn a_closure_run_inside_the_body_is_not_held_to_the_ceiling() {
    let src = "fn doubled(xs: own Vec[i64]) -> (impl Iterator[Item = i64] with reads(Clock)) {
    xs.iter().for_each(|x| println(f\"{x}\"));
    xs.into_iter().map(|x| x * 2)
}

fn main() {
    let s: i64 = doubled([1, 2, 3]).sum();
    println(f\"{s}\");
}
";
    let (ok, out) = check("body", src);
    assert!(ok, "{out}");
}

/// A `return` value is held to the ceiling as the tail is.
#[test]
fn a_returned_closure_past_the_ceiling_is_refused() {
    let src = "fn doubled(xs: own Vec[i64]) -> (impl Iterator[Item = i64] with reads(Clock)) {
    return xs.into_iter().map(|x| {
        println(f\"{x}\");
        x * 2
    });
}

fn main() {
    let s: i64 = doubled([1, 2, 3]).sum();
    println(f\"{s}\");
}
";
    let (ok, out) = check("ret", src);
    assert!(!ok, "{out}");
    assert!(out.contains("performs writes(Stdout)"), "{out}");
}
