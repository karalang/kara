//! Kata 2196 (B-2026-11-15-37): `return m.get(k);` from a function
//! returning `Option[Node]` was refused with "expected 'Option[Node]',
//! found 'Option[ref Node]'" and no fix-it, so `karac fix` left the
//! natural lookup broken. The mismatch now names the view and offers
//! `.cloned()` when the item is `Clone`; a non-`Clone` item gets no edit,
//! since `.cloned()` would only trade one error for another.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(tag: &str, src: &str, args: &[&str]) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-2196-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), format!("{src}\n")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(Path::new(&dir))
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let fixed = std::fs::read_to_string(dir.join("cell.kara")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        fixed,
    )
}

const LOOKUP: &str = "struct Node {\n    v: i64,\n}\nfn find(m: Map[i64, Node], k: i64) -> Option[Node] {\n    return m.get(k);\n}\nfn main() {\n    let mut m: Map[i64, Node] = Map.new();\n    m.insert(1, Node { v: 5 });\n    match find(m, 1) {\n        Some(n) => println(f\"{n.v}\"),\n        None => println(\"none\"),\n    }\n}";

#[test]
fn kata_2196_option_ref_return_offers_cloned() {
    let src = format!("#[derive(Clone)]\n{LOOKUP}");
    let (ok, out, _) = karac("json", &src, &["check", "--output=json"]);
    assert!(!ok, "{out}");
    assert!(out.contains("\"text\":\".cloned()\""), "{out}");
    let (ok, out, fixed) = karac("fix", &src, &["fix"]);
    assert!(ok, "{out}");
    // A later pass drops the `return` (B-2026-11-15-131).
    assert!(fixed.contains("    m.get(k).cloned()\n"), "{fixed}");
}

#[test]
fn kata_2196_option_ref_cloned_fix_runs() {
    let src = format!("#[derive(Clone)]\n{LOOKUP}").replace("m.get(k);", "m.get(k).cloned();");
    let (ok, out, _) = karac("run", &src, &["__mir-run"]);
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "5", "{out}");
}

#[test]
fn kata_2196_option_ref_without_clone_gets_no_edit() {
    let (ok, out, _) = karac("noclone", LOOKUP, &["check", "--output=json"]);
    assert!(!ok, "{out}");
    assert!(out.contains("Option[ref Node]"), "{out}");
    assert!(!out.contains(".cloned()"), "{out}");
}
