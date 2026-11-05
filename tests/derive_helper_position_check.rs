//! A derive helper where no derive reads it is refused, not dropped
//! (design.md § Attributes; B-2026-11-05-17 follow-up): `#[serde(...)]` is
//! read on a struct, an enum, a field or a variant, and the protobuf derive's
//! `#[karac::proto(...)]` / `#[karac::field(...)]` on a field.

use std::process::{Command, Stdio};

fn check(tag: &str, src: &str) -> (bool, String) {
    let path = std::env::temp_dir().join(format!("karac-dhp-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

#[test]
fn a_derive_helper_off_its_derive_s_items_is_refused() {
    for (tag, src) in [
        ("serde_fn", "#[serde(rename: \"x\")]\nfn f() {}\nfn main() { f(); }\n"),
        ("proto_fn", "#[karac::proto(sint64)]\nfn f() {}\nfn main() { f(); }\n"),
        (
            "field_struct",
            "#[karac::field(3)]\nstruct S { a: i64 }\nfn main() { let s = S { a: 1 }; println(s.a); }\n",
        ),
        (
            "serde_impl",
            "struct S { a: i64 }\n#[serde(skip)]\nimpl S { fn a(self) -> i64 { self.a } }\n\
             fn main() { let s = S { a: 1 }; println(s.a()); }\n",
        ),
    ] {
        let (ok, out) = check(tag, src);
        assert!(!ok, "{tag}: {out}");
        assert!(out.contains("E_HELPER_ATTRIBUTE_INVALID_POSITION"), "{tag}: {out}");
    }
}

#[test]
fn a_derive_helper_where_its_derive_reads_it_is_accepted() {
    let src = "#[derive(Serialize)]\n#[serde(tag: \"type\")]\n\
               enum E { #[serde(rename: \"a\")] A { #[serde(rename: \"xx\")] x: i64 }, B }\n\
               #[derive(Serialize)]\nstruct P { #[serde(skip)] a: i64, #[karac::field(3)] b: i64, \
               #[karac::proto(sint64)] c: i64 }\nfn main() { println(1); }\n";
    let (ok, out) = check("valid", src);
    assert!(ok, "{out}");
}
