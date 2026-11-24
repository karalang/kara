//! `karac explain <file>.<Type>` for a `shared` or `sync` type: the fields
//! that carry a run-time borrow flag and the total per instance, as
//! design.md § Shared types says it reports (B-2026-11-23-75). The fields
//! come from the borrow-flag pass's own predicates: a struct field only
//! when written through a handle and neither `Copy` nor counted, an enum
//! payload only when some body writes it.

use std::process::Command;

fn explain(src: &str, target: &str, json: bool) -> String {
    let dir = std::env::temp_dir().join(format!(
        "karac-explain-type-flags-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.kara"), src).unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.current_dir(&dir).args(["explain", target]);
    if json {
        c.arg("--format=json");
    }
    let out = c.output().unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&dir);
    text
}

const SRC: &str = "shared struct Inbox {
    name: String,
    mut items: Vec[String],
    mut count: i64,
    mut peer: Option[Inbox],
}
shared enum Node { Leaf(i64), Branch(Vec[Node], String) }
shared struct Cell[T] { mut v: T }
struct Plain { mut x: Vec[i64] }
fn grow(n: Node) { match n { Node.Branch(mut ref kids, _) => kids.clear(), _ => {} } }
fn main() {
    let i = Inbox { name: \"a\".to_string(), items: [], count: 0, peer: None };
    i.items.push(\"x\".to_string());
    println(i.name);
}
";

#[test]
fn explain_type_flags_names_the_flagged_struct_fields_and_the_total() {
    let out = explain(SRC, "a.kara.Inbox", false);
    assert!(out.contains("a `shared struct`"), "{out}");
    assert!(
        out.contains("Borrow flags: 1 (1 byte per instance before padding)"),
        "{out}"
    );
    assert!(out.contains("  items: flagged\n"), "{out}");
    assert!(out.contains("  name: no flag (not `mut`)"), "{out}");
    assert!(out.contains("  count: no flag (`Copy`)"), "{out}");
    assert!(
        out.contains("  peer: no flag (a counted handle or handle aggregate)"),
        "{out}"
    );
}

#[test]
fn explain_type_flags_counts_only_the_enum_payloads_a_body_writes() {
    let out = explain(SRC, "a.kara.Node", false);
    assert!(out.contains("a `shared enum`"), "{out}");
    assert!(out.contains("Borrow flags: 1 "), "{out}");
    assert!(out.contains("  Branch.0: flagged"), "{out}");
    assert!(
        out.contains("  Leaf.0: no flag (no body writes it)"),
        "{out}"
    );
}

#[test]
fn explain_type_flags_reports_an_unused_generic_type_by_parameter() {
    let out = explain(SRC, "a.kara.Cell", false);
    assert!(out.contains("Borrow flags: 1 "), "{out}");
    assert!(out.contains("  v: flagged (typed by a parameter"), "{out}");
}

#[test]
fn explain_type_flags_says_a_plain_type_has_none_and_functions_still_explain() {
    let out = explain(SRC, "a.kara.Plain", false);
    assert!(out.contains("Borrow flags: none"), "{out}");
    let f = explain(SRC, "a.kara.main", false);
    assert!(f.contains("karac explain — item: main"), "{f}");
}

#[test]
fn explain_type_flags_json() {
    let out = explain(SRC, "a.kara.Inbox", true);
    assert!(
        out.contains("\"kind\":\"type\",\"item\":\"Inbox\",\"file\":\"a.kara\",\"keyword\":\"shared\",\"borrow_flags\":1,\"flagged\":[{\"field\":\"items\",\"by_parameter\":false}]"),
        "{out}"
    );
}
