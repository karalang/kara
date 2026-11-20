//! A tuple or array that holds an erased function value moves, as the
//! function value does (design.md § Escaping function values): `karac
//! check` judged it by its parts, all `Copy` to the structural rule, and
//! passed a reuse after `let s = a;` that the build's move check refuses
//! (B-2026-11-20-99). So does one holding a capture-free closure literal or
//! a function name: stored there, it is a value of a function type
//! (B-2026-11-20-102).

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-erased-agg-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const MAKE: &str = "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n";

#[test]
fn a_tuple_holding_an_erased_fn_moves() {
    let src = format!(
        "{MAKE}fn main() {{ let a = (make(10), 0); let s = a; println((a.0)(1)); println((s.0)(2)); }}\n"
    );
    let (ok, text) = check("tuple", &src);
    assert!(!ok, "{text}");
    assert!(
        text.contains("error[E0500]") && text.contains("value 'a' moved here"),
        "{text}"
    );
}

#[test]
fn an_array_holding_an_erased_fn_moves() {
    let src = format!(
        "{MAKE}fn main() {{ let a: Array[Fn(i64) -> i64, 1] = [make(10)]; let s = a; \
         println((a[0])(1)); println((s[0])(2)); }}\n"
    );
    let (ok, text) = check("array", &src);
    assert!(!ok, "{text}");
    assert!(text.contains("error[E0500]"), "{text}");
}

/// A capture-free closure literal is `Copy` as itself, and a function name
/// copies as a bare value, but stored in a tuple either is a value of a
/// function type, which moves (design.md § Escaping function values). The
/// build's move check refused this while check passed it (B-2026-11-20-102).
#[test]
fn a_tuple_of_a_capture_free_closure_and_a_fn_name_moves_too() {
    let src = "fn dbl(x: i64) -> i64 { x * 2 }\n\
               fn main() { let a = (|x: i64| x + 1, dbl); let s = a; println((a.0)(1)); println((s.1)(2)); }\n";
    let (ok, text) = check("stored", src);
    assert!(!ok, "{text}");
    assert!(text.contains("error[E0500]"), "{text}");
}

#[test]
fn a_bare_capture_free_closure_still_copies() {
    let src = "fn main() { let g = |x: i64| x + 1; let h = g; println(g(1)); println(h(2)); }\n";
    let (ok, text) = check("bare", src);
    assert!(ok, "{text}");
}
