//! A trait default body copied into several impls is checked once per copy,
//! and the copies share the trait's spans, so one mistake in the body gave
//! one identical diagnostic per impl. `karac check` reports each once
//! (B-2026-10-28-13).

use std::process::Command;

fn check(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-copydedup-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success(), "check must refuse:\n{src}");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

const KEEPS: &str = "struct Keep {}
impl Keep { fn take(ref self, s: own String) -> i64 { return s.len(); } }
struct Hold {}
impl Hold { fn take(ref self, s: own String) -> i64 { return s.len() * 2; } }
";

#[test]
fn a_use_after_a_move_in_a_default_body_is_reported_once_for_two_impls() {
    let src = format!(
        "{KEEPS}trait Use[T] {{
    fn tool(ref self) -> T;
    fn run(ref self) -> i64 {{
        let t = self.tool();
        let s = \"abc\";
        let n = t.take(s);
        return n + s.len();
    }}
}}
struct A {{}}
impl Use[Keep] for A {{ fn tool(ref self) -> Keep {{ return Keep {{}}; }} }}
struct B {{}}
impl Use[Hold] for B {{ fn tool(ref self) -> Hold {{ return Hold {{}}; }} }}
fn main() {{ println(A {{}}.run()); println(B {{}}.run()); }}
"
    );
    let err = check("uam", &src);
    let want = "main.kara:11:20: value 's' moved here, used again here (moved at line 10:24)";
    assert_eq!(err.matches(want).count(), 1, "{err}");
    assert!(err.contains("1 error(s) found."), "{err}");
}

#[test]
fn an_element_move_in_a_default_body_is_reported_once_for_two_impls() {
    let src = format!(
        "{KEEPS}trait Use[T, E] {{
    fn tool(ref self) -> T;
    fn items(ref self) -> Vec[E];
    fn run(ref self) -> i64 {{
        let t = self.tool();
        let v = self.items();
        return t.take(v[0]);
    }}
}}
struct A {{}}
impl Use[Keep, String] for A {{
    fn tool(ref self) -> Keep {{ return Keep {{}}; }}
    fn items(ref self) -> Vec[String] {{ return [\"abc\".to_string()].to_vec(); }}
}}
struct B {{}}
impl Use[Hold, String] for B {{
    fn tool(ref self) -> Hold {{ return Hold {{}}; }}
    fn items(ref self) -> Vec[String] {{ return [\"abc\".to_string()].to_vec(); }}
}}
fn main() {{ println(A {{}}.run()); println(B {{}}.run()); }}
"
    );
    let err = check("index", &src);
    let want = "main.kara:11:23: error[E_INDEX_MOVE_NON_COPY]";
    assert_eq!(err.matches(want).count(), 1, "{err}");
    assert!(err.contains("1 error(s) found."), "{err}");
}
