//! B-2026-11-19-111: an E0509 for an element of a `let`-bound
//! collection that a bare `for` walks (it borrows each element, core
//! semantics §4.5), moved into a value that outlives it, has a machine fix
//! when nothing names the collection after the loop: walk it with
//! `.into_iter()`, which moves the elements out (kata 1242's
//! `for next in links { queue.push_back(next) }`).

use std::process::{Command, Stdio};

const SRC: &str = "fn main() {
    let mut queue: Vec[String] = Vec.new();
    let links: Vec[String] = [\"a\".to_string(), \"b\".to_string()];
    for next in links {
        queue.push(next);
    }
    println(f\"{queue}\");
}
";

fn karac(dir: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
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

fn scratch(tag: &str, src: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("karac-for-into-iter-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    dir
}

#[test]
fn e0509_on_a_bare_for_element_fix_walks_the_collection_with_into_iter() {
    let dir = scratch("moved", SRC);
    let (ok, said) = karac(&dir, &["check", "main.kara"]);
    assert!(!ok, "{said}");
    assert!(said.contains("iterate with `.into_iter()`"), "{said}");
    let (ok, said) = karac(&dir, &["fix", "main.kara"]);
    assert!(ok, "{said}");
    let fixed = std::fs::read_to_string(dir.join("main.kara")).unwrap();
    assert!(fixed.contains("for next in links.into_iter() {"), "{fixed}");
    let (ok, said) = karac(&dir, &["__mir-run", "main.kara"]);
    assert!(ok, "{said}");
    assert_eq!(said, "[a, b]\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn e0509_on_a_bare_for_element_named_after_the_loop_is_not_walked_by_into_iter() {
    let src = SRC.replace(
        "println(f\"{queue}\");",
        "println(f\"{queue} {links.len()}\");",
    );
    let dir = scratch("named", &src);
    let (_, said) = karac(&dir, &["fix", "main.kara"]);
    let fixed = std::fs::read_to_string(dir.join("main.kara")).unwrap();
    assert!(!fixed.contains("links.into_iter()"), "{said}\n{fixed}");
    let _ = std::fs::remove_dir_all(&dir);
}
