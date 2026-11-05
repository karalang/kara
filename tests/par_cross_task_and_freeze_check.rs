//! B-2026-11-05-6: E_NOT_CROSS_TASK on a value that contains a `shared`
//! handle names the value's type and the field path to the handle, and a
//! `freeze` stays allowed after a value built in place is pushed into a field
//! of its source (design.md §11, §13).

use std::process::{Command, Stdio};

fn check(tag: &str, src: &str) -> (bool, String) {
    let path = std::env::temp_dir().join(format!("karac-pcf-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

const CT: &str = "shared struct Ct { mut n: i64 }\n";

#[test]
fn a_value_containing_a_shared_handle_names_its_type_and_path() {
    let (ok, diag) = check(
        "nested",
        &format!(
            "{CT}struct In {{ cache: Ct }}\nstruct Srv {{ inner: In }}\nfn main() {{\n    \
             let s = Srv {{ inner: In {{ cache: Ct {{ n: 1 }} }} }};\n    par {{\n        \
             {{ println(s.inner.cache.n); }},\n        {{ println(2); }},\n    }};\n}}\n"
        ),
    );
    assert!(!ok, "{diag}");
    assert!(
        diag.contains("`s` (of type `Srv`, which contains the `shared` type `Ct`)"),
        "{diag}"
    );
    assert!(
        diag.contains("note: `Srv` contains the shared type `Ct` at field path inner.cache"),
        "{diag}"
    );
    let (ok, diag) = check(
        "vec",
        &format!(
            "{CT}fn main() {{\n    let v: Vec[Ct] = [Ct {{ n: 1 }}];\n    par {{\n        \
             {{ println(v.len()); }},\n        {{ println(2); }},\n    }};\n}}\n"
        ),
    );
    assert!(!ok, "{diag}");
    assert!(
        diag.contains("`v` (of type `Vec[Ct]`, which contains the `shared` type `Ct`)"),
        "{diag}"
    );
}

const ND: &str = "shared struct Nd { mut val: i64, mut kids: Vec[Nd] }\n";

#[test]
fn freeze_after_pushing_a_value_built_in_place_is_accepted() {
    for (tag, push) in [
        ("literal", "r.kids.push(Nd { val: 3, kids: [] });"),
        (
            "copy_local",
            "let k = 4;\n    r.kids.push(Nd { val: k, kids: [] });",
        ),
    ] {
        let (ok, diag) = check(
            tag,
            &format!(
                "{ND}fn main() {{\n    let r = Nd {{ val: 2, kids: [] }};\n    {push}\n    \
                 let g = freeze r;\n    println(g.kids.len());\n}}\n"
            ),
        );
        assert!(ok, "{tag}: {diag}");
    }
}

#[test]
fn freeze_after_pushing_an_existing_handle_is_refused() {
    let (ok, diag) = check(
        "alias",
        &format!(
            "{ND}fn main() {{\n    let r = Nd {{ val: 2, kids: [] }};\n    \
             let c = Nd {{ val: 3, kids: [] }};\n    r.kids.push(c);\n    let g = freeze r;\n    \
             c.val = 9;\n    println(g.kids[0].val);\n}}\n"
        ),
    );
    assert!(!ok, "{diag}");
    assert!(diag.contains("cannot be frozen yet"), "{diag}");
}
