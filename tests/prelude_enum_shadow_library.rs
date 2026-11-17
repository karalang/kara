//! A program's own type named like a prelude enum (`struct Option`,
//! `struct Result`) shadows the prelude's for the program alone
//! (design.md § The prelude: prelude names are not reserved). Library code
//! lowered in the same unit keeps its `Option` and `Result`, so its
//! `Some(..)` / `Ok(..)` patterns still match (B-2026-11-17-37). Both
//! backends must print what the program says, as a project whose module
//! declares the type must under `karac run`.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const OPTION: &str = r#"struct Option { v: i64 }
fn main() {
    let o = Option { v: 3 };
    println(o.v);
    let mut m: Map[String, i64] = Map.new();
    m.insert("a", 5);
    match m.get("a") { Some(v) => println(f"{v}"), None => println("none") }
    println([4, 6].first().unwrap());
}
"#;

const RESULT: &str = r#"struct Result { v: i64 }
fn main() {
    let r = Result { v: 3 };
    println(r.v);
    println(i64.parse("12").unwrap_or(0));
}
"#;

fn run_both(name: &str, src: &str, want: &str) {
    let dir = std::env::temp_dir().join(format!(
        "karac-prelude-shadow-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let mut ran = 0;
    for cmd in ["__mir-run", "__mir-llvm-run"] {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg(cmd)
            .arg(&file)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        if cmd == "__mir-llvm-run" && stderr.contains("undefined reference") {
            if common::link_or_skip(Err(stderr)).is_none() {
                continue;
            }
            unreachable!("link_or_skip passes only a successful link");
        }
        assert!(out.status.success(), "{cmd}: {stderr}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{cmd}");
        ran += 1;
    }
    assert!(ran >= 1, "neither backend ran");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_program_struct_option_leaves_the_librarys_option_alone() {
    run_both("option", OPTION, "3\n5\n4\n");
}

#[test]
fn a_program_struct_result_leaves_the_librarys_result_alone() {
    run_both("result", RESULT, "3\n12\n");
}

#[test]
fn an_imported_struct_option_leaves_the_librarys_option_alone() {
    let dir = std::env::temp_dir().join(format!("karac-prelude-shadow-pkg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"shadow\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/m.kara"), "pub struct Option { pub v: i64 }\n").unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import m.Option;\nfn main() {\n    let o = Option { v: 3 };\n    println(o.v);\n    println([4, 6].first().unwrap());\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("run")
        .current_dir(&dir)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if stderr.contains("undefined reference") && common::link_or_skip(Err(stderr.clone())).is_none()
    {
        return;
    }
    assert!(out.status.success(), "karac run: {stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "3\n4\n");
    let _ = std::fs::remove_dir_all(&dir);
}
