//! B-2026-11-20-83, from kata 1265: a `shared` handle read through a
//! borrow could not become a `dyn` value. `match n.next { Some(m) =>
//! Some(m), .. }` into an `Option[dyn Show]` got E0313 from check, the
//! slot's `dyn` type missing the counted-handle exemption the value's own
//! type has; past it, and for a borrowed parameter returned or pushed as a
//! `dyn`, MIR made the `dyn` from the reference and the borrow check
//! refused it with E0509. The `dyn` is now made from a counted copy of the
//! handle, as the same code into an `Option[Node]` already was. Each
//! program checks, and runs on the interpreter, compiled and under ASan.

use std::process::{Command, Stdio};

fn run(sub: &str, tag: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-shared-into-dyn-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([sub, "cell.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn lanes(tag: &str, src: &str, want: &str) {
    let (ok, out, err) = run("check", tag, src);
    assert!(ok, "{tag} checked: {out}{err}");
    let (ok, out, err) = run("__mir-run", tag, src);
    assert!(ok, "{tag} interpreted: {err}");
    assert_eq!(out, want, "{tag} interpreted: {err}");
    if !cfg!(feature = "llvm") {
        return;
    }
    for sub in ["__mir-llvm-run", "__mir-llvm-asan"] {
        let (ok, out, err) = run(sub, tag, src);
        if err.contains("Linker failed")
            && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none()
        {
            eprintln!("skipping {sub}: no runtime archive");
            continue;
        }
        assert!(ok, "{tag} {sub}: {err}");
        assert_eq!(out, want, "{tag} {sub}: {err}");
    }
}

#[test]
fn a_shared_handle_out_of_a_borrow_becomes_a_dyn_value() {
    lanes(
        "match-if-let-param",
        r#"trait Show {
    fn show(self) -> i64;
}

shared struct Node {
    val: i64,
    next: Option[Node],
}

impl Show for Node {
    fn show(self) -> i64 {
        self.val
    }
}

fn a(n: Node) -> Option[dyn Show] {
    match n.next {
        Some(m) => Some(m),
        None => None,
    }
}

fn b(n: Node) -> dyn Show {
    let m = n.next.unwrap();
    m
}

fn c(n: Node) -> dyn Show {
    n
}

fn d(n: Node) -> Option[dyn Show] {
    if let Some(m) = n.next {
        return Some(m);
    }
    None
}

fn main() {
    let x = Node { val: 1, next: None };
    let y = Node { val: 2, next: Some(x) };
    println(f"{a(y).unwrap().show()} {b(y).show()} {c(y).show()} {d(y).unwrap().show()}");
}
"#,
        "1 1 2 1\n",
    );
}

#[test]
fn borrowed_shared_handles_go_into_a_vec_of_dyn() {
    lanes(
        "push-while-let",
        r#"trait Show {
    fn show(self) -> i64;
}

shared struct Node {
    val: i64,
    next: Option[Node],
}

impl Show for Node {
    fn show(self) -> i64 {
        self.val
    }
}

fn all(head: Node) -> Vec[dyn Show] {
    let mut out: Vec[dyn Show] = Vec.new();
    let first: Option[dyn Show] = Some(head);
    if first.is_none() {
        return out;
    }
    let mut at = head;
    out.push(head);
    while let Some(n) = at.next {
        out.push(n);
        at = n;
    }
    out
}

fn main() {
    let mut head: Option[Node] = None;
    for v in [4, 3, 2, 1] {
        head = Some(Node { val: v, next: head });
    }
    let h = head.unwrap();
    let got = all(h);
    let s: Vec[i64] = got.iter().map(|d| d.show()).collect();
    println(f"{s}");
}
"#,
        "[1, 2, 3, 4]\n",
    );
}
