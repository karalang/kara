//! B-2026-11-25-22: a cursor over a `shared struct` list started from a
//! borrow, `let mut at = start; .. let Some(n) = at.next else { break; };
//! at = n;`, passed `karac check` and stopped every lane with "invalid
//! MIR: assigning shared Node to _ of type ref shared Node": `at` is a
//! `ref Node`, and `n`, a handle read out of a field, is counted, not
//! borrowed. Check now refuses it, and `karac fix` declares `at` as the
//! handle (`let mut at: Node = start;`), which counts it. A bare parameter
//! and a view of a `for` item are borrows and stay allowed. Kata 1372's
//! differential.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn cell(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-kata-1372-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    dir
}

fn lanes() -> &'static [&'static str] {
    if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    }
}

const NODE: &str = "shared struct Node {\n    mut val: i32,\n    mut next: Option[Node],\n}\n\n";

/// Check refuses `src`, `karac fix` leaves `fixed`, which checks clean
/// and prints `want` on every lane.
fn mends(name: &str, src: &str, fixed: &str, want: &str) {
    let dir = cell(name, &format!("{NODE}{src}"));
    let (ok, json, _) = karac(&dir, &["check", "--output=json", "cell.kara"]);
    assert!(!ok, "{name}: check passed: {json}");
    assert!(json.contains("counted `Node` handle"), "{name}: {json}");
    karac(&dir, &["fix", "cell.kara"]);
    let text = std::fs::read_to_string(dir.join("cell.kara")).unwrap();
    assert_eq!(text, format!("{NODE}{fixed}"), "{name}");
    let (ok, json, _) = karac(&dir, &["check", "--output=json", "cell.kara"]);
    assert!(ok && json.contains("\"diagnostics\":[]"), "{name}: {json}");
    for lane in lanes() {
        let (_, out, err) = karac(&dir, &[lane, "cell.kara"]);
        assert_eq!(out, want, "{name} on {lane}: {err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `src` checks clean and prints `want` on every lane.
fn runs(name: &str, src: &str, want: &str) {
    let dir = cell(name, &format!("{NODE}{src}"));
    let (ok, json, _) = karac(&dir, &["check", "--output=json", "cell.kara"]);
    assert!(ok, "{name}: {json}");
    for lane in lanes() {
        let (_, out, err) = karac(&dir, &[lane, "cell.kara"]);
        assert_eq!(out, want, "{name} on {lane}: {err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn kata_1372_ref_cursor_takes_counted_handle_after_fix() {
    let body = |decl: &str| {
        format!(
            "fn main() {{
    let c = Node {{ val: 3, next: None }};
    let b = Node {{ val: 2, next: Some(c) }};
    let nodes: Vec[Node] = [b, c];
    let mut total = 0;
    for start in nodes.iter() {{
        let mut at{decl} = start;
        loop {{
            total += at.val;
            let Some(n) = at.next else {{
                break;
            }};
            at = n;
        }}
    }}
    println(f\"{{total}}\");
}}
"
        )
    };
    mends("let-else", &body(""), &body(": Node"), "8\n");
}

#[test]
fn kata_1372_ref_cursor_while_let_and_let_copy_are_fixed() {
    let body = |decl: &str| {
        format!(
            "fn main() {{
    let c = Node {{ val: 3, next: None }};
    let b = Node {{ val: 2, next: Some(c) }};
    let mut total = 0;
    let mut cur{decl} = ref b;
    while let Some(n) = cur.next {{
        total += n.val;
        cur = n;
    }}
    let d = b;
    let mut at{decl} = ref c;
    at = d;
    println(f\"{{total}} {{at.val}} {{cur.val}}\");
}}
"
        )
    };
    mends("while-let", &body(""), &body(": Node"), "3 2 3\n");
}

#[test]
fn kata_1372_ref_cursor_from_param_or_view_stays_a_borrow() {
    runs(
        "param",
        "fn walk(p: Node, q: Node) -> i32 {
    let mut at = ref p;
    let mut t: i32 = at.val;
    at = q;
    t += at.val;
    t
}

fn main() {
    let c = Node { val: 3, next: None };
    let b = Node { val: 2, next: Some(c) };
    println(f\"{walk(b, c)}\");
}
",
        "5\n",
    );
    runs(
        "view",
        "fn main() {
    let c = Node { val: 3, next: None };
    let b = Node { val: 2, next: Some(c) };
    let nodes: Vec[Node] = [b, c];
    let mut at = ref b;
    for n in nodes {
        at = n;
    }
    println(f\"{at.val}\");
}
",
        "3\n",
    );
}
