//! `h.d ?? x`, with `h` a borrowed struct whose field `d` is an `Option` of a
//! `shared` handle (or of a handle aggregate), reads the payload as a count
//! (core-semantics §6.1) and leaves `h.d` usable, as a bare `d: Option[Node]`
//! parameter already did (B-2026-11-01-15). Check used to refuse it as a move
//! out of a borrow. A payload that is neither Copy nor counted (a `String`) is
//! still refused, with `.clone()` as the fix.

use std::process::Command;

fn interp_stdout(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-field-nc-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-run")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

const PROG: &str = r#"shared enum Node {
    Leaf(i64),
    Pair(Node, Node),
}

struct Holder {
    d: Option[Node],
    p: Option[(Node, i64)],
}

fn show(n: Node) -> String {
    match n {
        Node.Leaf(v) => f"{v}",
        _ => "pair",
    }
}

fn field(h: Holder) -> Node {
    h.d ?? Node.Leaf(-1)
}

fn pair(h: Holder) -> i64 {
    let (_, k) = h.p ?? (Node.Leaf(0), -1);
    k
}

fn main() {
    let h = Holder { d: Some(Node.Leaf(3)), p: Some((Node.Leaf(1), 7)) };
    println(show(field(h)));
    println(show(field(h)));
    println(f"{pair(h)}");
    let e = Holder { d: None, p: None };
    println(show(field(e)));
    println(f"{pair(e)}");
}
"#;

#[test]
fn nil_coalesce_counts_a_handle_field_through_a_borrow() {
    let diags = karac::check_source(PROG);
    assert!(
        diags.is_empty(),
        "{:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let (ok, out) = interp_stdout("run", PROG);
    assert!(ok, "{out}");
    assert_eq!(out, "3\n3\n7\n-1\n-1\n");
}

#[test]
fn nil_coalesce_still_refuses_a_moved_payload_field_through_a_borrow() {
    let src = "struct Holder {\n    s: Option[String],\n}\n\nfn field(h: Holder) -> String {\n    h.s ?? \"none\".to_string()\n}\n\nfn main() {\n    println(field(Holder { s: None }));\n}\n";
    let errors: Vec<String> = karac::check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("would move its 'String' out of the borrow"),
        "{errors:?}"
    );
}
