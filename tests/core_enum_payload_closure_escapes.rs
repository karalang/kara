//! A closure bound by `let` and then stored in an enum payload
//! (`let c = |d| seen.n + d; let l = L.F(c);`) escapes, as one stored in a
//! struct field or a tuple does (core semantics §9.3: a field of function
//! type is escaping by nature), so it captures by move. The MIR builder's
//! escape walk for a `let`-bound closure did not count a variant
//! constructor, so the closure captured a `shared` handle by `ref` and the
//! borrow checker refused the program with E0509, "a view of a temporary
//! value is stored in an escaping closure" (B-2026-11-07-5). The same
//! closure written inline in the constructor, or stored in a struct field,
//! was accepted. A non-`Copy` capture used after the store is E0500, as
//! for a struct field.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-enumesc-{}-{name}", std::process::id()));
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
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_let_bound_closure_stored_in_an_enum_payload_captures_by_move() {
    for (name, body, want) in [
        (
            "reads_handle",
            "let c = |d: i64| seen.n + d;
    let l = L.F(c);",
            "8 8\n",
        ),
        (
            "writes_handle",
            "let c = |d: i64| {
        seen.n += d;
        seen.n
    };
    let l = L.F(c);",
            "8 13\n",
        ),
        (
            "result_payload",
            "let c = |d: i64| seen.n + d;
    let l = Link.Func(c);
    let l = match l {
        Link.Func(f) => L.F(f),
    };",
            "8 8\n",
        ),
    ] {
        let src = format!(
            "shared struct Counter {{ mut n: i64 }}

enum L {{ F(Fn(i64) -> i64) }}

enum Link {{ Func(Fn(i64) -> i64) }}

fn main() {{
    let seen = Counter {{ n: 3 }};
    {body}
    match l {{
        L.F(f) => println(f\"{{f(5)}} {{f(5)}}\"),
    }}
}}
"
        );
        let (ok, err) = check(name, &src);
        assert!(ok, "{name}: {err}");
        assert_eq!(
            karac::mir::lower::run_source(&src)
                .expect("MIR interpreter")
                .output,
            want,
            "{name}"
        );
    }
}

#[test]
fn a_capture_used_after_its_closure_went_into_an_enum_payload_is_e0500() {
    let (ok, err) = check(
        "moved",
        "enum L { F(Fn() -> i64) }

fn main() {
    let s = \"abc\".to_string();
    let c = || s.len();
    let l = L.F(c);
    println(s);
    match l {
        L.F(f) => println(f\"{f()}\"),
    }
}
",
    );
    assert!(!ok, "the store moves `s` into the closure");
    assert!(err.contains("escaping closure"), "{err}");
}
