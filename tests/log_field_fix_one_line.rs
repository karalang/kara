//! B-2026-11-20-92: log.md § Fields gives a field whose value is `Display`
//! but not `Serialize` a fix-it that logs it as text, `f"{v}"`. The fix
//! wrote the expression as `karac fmt` writes a statement, so a struct
//! literal came out over three lines inside the interpolation. It keeps it
//! on one line.

use std::process::Command;

#[test]
fn a_struct_literal_log_field_is_fixed_on_one_line() {
    let dir = std::env::temp_dir().join(format!("karac-log-fix-one-line-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(
        &path,
        "import std.log.{info};\nstruct P { v: i64, w: i64 }\nimpl Display for P { fn to_string(ref self) -> String { f\"P{self.v}\" } }\nfn main() {\n    let p = P { v: 1, w: 0 };\n    info(\"x\", p: p);\n    info(\"y\", q: P { v: 2, w: 3 });\n}\n",
    )
    .unwrap();
    let fix = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("fix")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(fixed.contains("info(\"x\", p: f\"{p}\");"), "{fixed}");
    assert!(
        fixed.contains("info(\"y\", q: f\"{P { v: 2, w: 3 }}\");"),
        "{fixed}"
    );
    let run = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-run")
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&run.stderr),
        "{\"level\":\"info\",\"msg\":\"x\",\"p\":\"P1\",\"task_id\":null}\n{\"level\":\"info\",\"msg\":\"y\",\"q\":\"P2\",\"task_id\":null}\n"
    );
}
