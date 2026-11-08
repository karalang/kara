//! A `shared` handle, and an `Option` of one, is released in line at its
//! drop site: the count's decrement and zero test there, with only the
//! box's last release (`last.shared T`) out of line. And a `weak` slot read
//! as a value is upgraded once, into a temporary the read then moves: it
//! took one count in the upgrade and a second in a clone of the fresh
//! temporary, and gave one back through the drop glue (B-2026-11-08-9,
//! kata 510: 154M calls to the self-recursive `drop.Option[shared Node]`).
//! The pins: both backends print the program's values and its drops in
//! the same order, and `climb`'s IR calls neither a clone nor drop glue.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}"); } }
shared struct Node {
    r: R,
    mut next: Option[Node],
    mut parent: weak Node,
}
fn climb(node: Node) -> i64 {
    let mut up = node.parent;
    let mut n = 0;
    while let Some(p) = up {
        n = n * 10 + p.r.id;
        up = p.parent;
    }
    n
}
fn main() {
    let a = Node { r: R { id: 1 }, next: None, parent: None };
    let b = Node { r: R { id: 2 }, next: None, parent: None };
    let c = Node { r: R { id: 3 }, next: None, parent: None };
    b.parent = a;
    c.parent = b;
    a.next = Some(b);
    println(climb(c));
    let kept = c.parent;
    println(match kept { Some(k) => k.r.id, None => 0 });
    b.next = Some(c);
    println("dropping a");
    let up = c.parent;
    println(match up { Some(u) => u.r.id, None => 0 });
}
"#;

/// `climb(c)` reads 2 then 1; `kept` and `up` each upgrade `c.parent`
/// (2); the boxes are freed when `c`, `b` and `a` go out of scope, in
/// that order, since `a` holds `b` holds `c`.
const EXPECTED: &str = "21\n2\ndropping a\n2\nd3\nd2\nd1\n";

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "0");
    c
}

#[test]
fn shared_handles_release_in_line_and_upgrades_move() {
    let d = std::env::temp_dir().join(format!("karac-shared-release-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let file = d.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    for backend in ["__mir-run", "__mir-llvm-run"] {
        let out = karac()
            .args([backend, file.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            EXPECTED,
            "{backend}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let exe = d.join("main");
    let ll = d.join("main.ll");
    let built = karac()
        .env("KARAC_MIR_LL", &ll)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    })
    .is_none()
    {
        return;
    }
    let ran = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&ran.stdout), EXPECTED);
    let ir = std::fs::read_to_string(&ll).unwrap();
    let climb: Vec<&str> = ir
        .lines()
        .skip_while(|l| !(l.starts_with("define") && l.contains("@kara.climb(")))
        .take_while(|l| *l != "}")
        .collect();
    assert!(!climb.is_empty(), "no climb in the IR");
    let calls = |name: &str| climb.iter().filter(|l| l.contains(name)).count();
    assert_eq!(
        calls("@\"clone.Option[shared Node]\""),
        0,
        "{}",
        climb.join("\n")
    );
    assert_eq!(
        calls("@\"drop.Option[shared Node]\""),
        0,
        "{}",
        climb.join("\n")
    );
    assert_eq!(calls("@\"drop.shared Node\""), 0, "{}", climb.join("\n"));
    assert!(calls("@\"last.shared Node\"") > 0, "{}", climb.join("\n"));
    let _ = std::fs::remove_dir_all(&d);
}
