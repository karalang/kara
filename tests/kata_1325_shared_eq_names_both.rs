//! B-2026-11-24-38: `a === b` on two handles of a `shared struct`,
//! JavaScript's identity test, became a structural comparison under
//! `karac fix`: the `===` habit mends to `==`, and the refusal that
//! followed applied `#[derive(PartialEq)]`, so two distinct handles with
//! equal fields compared equal. On a handle the source does not say which
//! reading is meant, so the refusal now names both, `a.ref_eq(b)` and the
//! derive, and applies neither (design.md § Shared types). Kata 1325's
//! levels arm.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

const NODE: &str = "shared struct Node {
    mut val: i32,
}

";

#[test]
fn kata_1325_eq_on_shared_handles_names_identity_and_derive() {
    let body = |cmp_b: &str, cmp_c: &str| {
        format!(
            "{NODE}fn main() {{
    let a = Node {{ val: 1 }};
    let b = Node {{ val: 1 }};
    let c = a;
    if {cmp_b} {{
        println(\"same b\");
    }}
    if {cmp_c} {{
        println(\"same c\");
    }}
}}
"
        )
    };
    let path = std::env::temp_dir().join(format!(
        "karac-kata-1325-shared-eq-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, body("a === b", "a === c")).unwrap();
    let file = path.to_str().unwrap();
    karac(&["fix", file]);
    let text = std::fs::read_to_string(&path).unwrap();
    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    assert_eq!(text, body("a == b", "a == c"));
    assert!(!check.status.success(), "{err}");
    assert!(
        err.contains("`a.ref_eq(b)` asks whether the two are one value"),
        "{err}"
    );
    assert!(
        err.contains("`#[derive(PartialEq)]` on 'Node' compares their fields"),
        "{err}"
    );

    std::fs::write(&path, body("a.ref_eq(b)", "a.ref_eq(c)")).unwrap();
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "same c\n");
}
