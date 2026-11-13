//! B-2026-11-13-77: a view stored where its borrow is not kept (an escaping
//! closure, a `shared` value, a channel) was reported as "a view of a
//! temporary value", though the program names what it views (`t`, `t.s`),
//! and a view stored in a `shared` field printed its type with MIR's `str`
//! where the program writes `Str`.

use std::process::{Command, Stdio};

const T: &str = "struct T { s: String }\n\
fn mkt() -> T { T { s: \"abc\".to_string() } }\n";

fn check(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-view-names-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "cell.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success(), "{name}: accepted");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn unkept_view_names_the_local_it_views() {
    let err = check(
        "local",
        "shared struct H { v: Str }\nfn main() { let t = \"abc\".to_string(); let h = H { v: t.as_str() }; println(\"ok\"); }\n",
    );
    assert!(
        err.contains("a view of `t` is stored in an escaping closure"),
        "{err}"
    );
    assert!(!err.contains("temporary value"), "{err}");
}

#[test]
fn unkept_view_names_the_field_it_views() {
    let err = check(
        "field",
        &format!(
            "{T}struct H {{ f: Fn() -> Str }}\nfn main() {{\n    let h = {{ let t = mkt(); let x = t.s.as_str(); H {{ f: || x }} }};\n    println((h.f)());\n}}\n"
        ),
    );
    assert!(err.contains("a view of `t.s` is stored"), "{err}");
}

#[test]
fn view_in_shared_field_prints_str_as_written() {
    let err = check(
        "shared_field",
        &format!(
            "{T}shared struct H {{ mut v: Option[Str] }}\nfn main() {{\n    let h = H {{ v: None }};\n    {{ let t = mkt(); h.v = Some(t.s.as_str()); }}\n    println(h.v.unwrap());\n}}\n"
        ),
    );
    assert!(
        err.contains("a view (`Option[Str]`) is stored in `h.v`"),
        "{err}"
    );
}
