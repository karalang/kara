//! `karac run --example NAME` runs a project example
//! (`examples/NAME/kara.toml`) as its package, the way `karac run` does in
//! its own directory: every module and the package's types are there
//! (design.md §4: `examples/` is a separate root, each program run with
//! `karac run --example NAME`; B-2026-11-16-282). It compiled
//! `src/main.kara` alone, so a type from a sibling module was unknown.

use std::process::Command;

#[test]
fn a_project_example_runs_with_its_modules() {
    let root = std::env::temp_dir().join(format!("karac-run-example-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let proj = root.join("examples").join("shapes");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(
        proj.join("kara.toml"),
        "[package]\nname = \"shapes\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\n",
    )
    .unwrap();
    std::fs::write(
        proj.join("src/geometry.kara"),
        "pub struct Rect { pub w: i64, pub h: i64 }\n\
         pub trait Area { fn area(ref self) -> i64; }\n\
         impl Area for Rect { fn area(ref self) -> i64 { self.w * self.h } }\n",
    )
    .unwrap();
    std::fs::write(
        proj.join("src/main.kara"),
        "import geometry.{Rect, Area};\n\
         fn main() {\n    let r = Rect { w: 3, h: 4 };\n    println(r.area());\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["run", "--example", "shapes"])
        .current_dir(&root)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&root);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "karac run --example: {stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "12\n");
}
