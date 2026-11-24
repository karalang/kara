//! B-2026-11-24-17: a qualified receiver may name a `dyn` type
//! (`Option[dyn Shape].size_of()`, `Vec[dyn Shape].new()`, design.md
//! § Generics). The brackets were parsed as an index, and `dyn` refused as
//! "a reserved keyword and cannot be used as an identifier".

use std::process::Command;

#[test]
fn a_qualified_receiver_names_a_dyn_type() {
    let dir = std::env::temp_dir().join(format!("karac-dyn-receiver-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(
        &file,
        "trait Shape { fn area(self) -> i64; }\n\
         struct Sq { s: i64 }\n\
         impl Shape for Sq { fn area(self) -> i64 { self.s } }\n\
         fn main() {\n\
         \x20   println(Option[dyn Shape].size_of());\n\
         \x20   let mut v = Vec[dyn Shape].new();\n\
         \x20   v.push(Sq { s: 2 });\n\
         \x20   println(v[0].area());\n\
         \x20   let w = Vec[i64].new();\n\
         \x20   println(w.len());\n\
         }\n",
    )
    .unwrap();
    let f = file.to_str().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", f])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", f])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "16\n2\n0\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
