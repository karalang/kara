//! B-2026-11-17-135: moving a non-`Copy` field out of a `shared struct`
//! through an OWNED root (`h: own Holder`, or a `let` of one) reported
//! `borrow_projection_copy` ("`h.name` is reached through `h`, which does
//! not own it"), blaming a borrow the program does not have. The place is
//! borrowed because the refcounted box lends its fields, which is what
//! E_SHARED_FIELD_MOVE says, so the check now reports that, once, with the
//! same `.clone()` fix.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn codes(tag: &str, src: &str) -> (Vec<String>, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-shared-owned-root-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap();
    let out = karac(&["check", "--output=json", p]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut found = Vec::new();
    for part in text.split("\"code\":\"").skip(1) {
        found.push(part.split('"').next().unwrap().to_string());
    }
    karac(&["fix", p]);
    let fixed = std::fs::read_to_string(&f).unwrap();
    let check = karac(&["check", p]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        check.status.success(),
        "{fixed}\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    (found, fixed)
}

#[test]
fn a_field_moved_out_of_an_own_shared_parameter_names_the_box() {
    let (found, fixed) = codes(
        "param",
        "shared struct Holder { name: String }\n\nfn take(h: own Holder) -> String {\n    h.name\n}\n\nfn main() { println(take(Holder { name: \"a\".to_string() })); }\n",
    );
    assert_eq!(found, ["E_SHARED_FIELD_MOVE"], "{found:?}");
    assert!(fixed.contains("    h.name.clone()\n"), "{fixed}");
}

#[test]
fn a_field_moved_out_of_a_shared_local_names_the_box_once() {
    let (found, fixed) = codes(
        "local",
        "#[derive(Clone)]\nstruct R { s: String }\nshared struct Sh { r: R, name: String }\n\nfn main() {\n    let b = Sh { r: R { s: \"x\" }, name: \"a\" };\n    let n = b.name;\n    let r = b.r;\n    println(n);\n    println(r.s);\n}\n",
    );
    assert_eq!(
        found,
        ["E_SHARED_FIELD_MOVE", "E_SHARED_FIELD_MOVE"],
        "{found:?}"
    );
    assert!(fixed.contains("let n = b.name.clone();"), "{fixed}");
}
