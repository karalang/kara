//! B-2026-11-11-21 — `format!` and `println!` whose argument holds a Rust
//! path (`String::from_utf8(v).unwrap_or(String::new())`, `i64::from(b)`)
//! had no f-string fix: a hole's `:` starts a format spec, so any `:` in an
//! argument refused the rewrite. A path's `::` is now written as Kāra's `.`
//! in the hole; a turbofish still has no hole form.

use std::process::Command;

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn fixed(tag: &str, src: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-format-path-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let p = path.to_str().unwrap();
    for _ in 0..2 {
        karac(&["fix", p]);
    }
    let text = std::fs::read_to_string(&path).unwrap();
    let (_, out) = karac(&["check", p]);
    let _ = std::fs::remove_dir_all(&dir);
    (text, out)
}

#[test]
fn a_format_argument_with_a_path_gets_the_f_string_fix() {
    let (text, out) = fixed(
        "paths",
        "fn main() {\n    let v: Vec[u8] = \"hi\".bytes().to_vec();\n    let s = format!(\"<{}>\", String::from_utf8(v.clone()).unwrap_or(String::new()));\n    println!(\"{} {}\", s, i64::from(v[0]));\n}\n",
    );
    assert_eq!(
        text,
        "fn main() {\n    let v: Vec[u8] = \"hi\".bytes().to_vec();\n    let s = f\"<{String.from_utf8(v.clone()).unwrap_or(String.new())}>\";\n    println(f\"{s} {i64.from(v[0])}\");\n}\n"
    );
    assert!(out.contains("All checks passed"), "{out}");
    let out = karac::mir::lower::run_source(&text).expect("runs").output;
    assert_eq!(out, "<hi> 104\n");
}

#[test]
fn a_turbofish_argument_still_has_no_hole_form() {
    let src = "fn main() {\n    let v = [1, 2].to_vec();\n    let s = format!(\"{}\", v.iter().collect::<Vec<i64>>().len());\n    println(s);\n}\n";
    let (text, out) = fixed("turbofish", src);
    assert!(text.contains("format!("), "{text}");
    assert!(out.contains("Kāra has no macros"), "{out}");
}
