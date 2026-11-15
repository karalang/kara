//! B-2026-11-15-33: `Option.as_mut()` was refused ("no method 'as_mut' on
//! type 'Option'") though `as_ref` exists; Rust's linked-list templates walk
//! `while let Some(node) = cur.as_mut()`. It is the writable twin of
//! `as_ref`: `fn as_mut(mut ref self) -> Option[mut ref T]`.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

#[test]
fn option_as_mut_writes_through_the_payload_and_walks_a_list() {
    let src = "struct Node {
    val: i64,
    next: Option[Node],
}

fn main() {
    let mut o: Option[i64] = Some(4);
    if let Some(x) = o.as_mut() {
        *x += 1;
    }
    println(f\"{o}\");
    let mut n: Option[i64] = None;
    if let Some(x) = n.as_mut() {
        *x += 1;
    }
    println(f\"{n}\");
    let mut head: Option[Node] = Some(Node { val: 1, next: Some(Node { val: 2, next: None }) });
    let mut cur = head.as_mut();
    while let Some(node) = cur {
        node.val *= 10;
        cur = node.next.as_mut();
    }
    let mut r = head.as_ref();
    while let Some(node) = r {
        println(node.val);
        r = node.next.as_ref();
    }
}
";
    let dir = std::env::temp_dir().join(format!("karac-asmut-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap();
    let check = karac(&["check", p]);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stdout)
    );
    let run = karac(&["run", "--interp", p]);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "Some(5)\nNone\n10\n20\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
