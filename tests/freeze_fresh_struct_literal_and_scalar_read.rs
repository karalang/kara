//! B-2026-11-19-16 (a remainder of B-2026-11-17-27): `freeze r` needs `r`
//! to be the only live name for its value (design.md §11, Frozen
//! handles). Two uses that make no second name were counted as if they
//! did: pushing a struct literal whose fields hold no handle
//! (`Node { val: 2, kids: Vec.new() }`, as fresh as `leaf(2)`), and
//! reading a `Copy` field of the source (`println(r.val)`). Moving a
//! handle-holding field out, or pushing a literal that holds another
//! handle, still costs the source its uniqueness.

use std::process::{Command, Stdio};

fn karac(name: &str, cmd: &str, src: &str) -> std::process::Output {
    let path = std::env::temp_dir().join(format!("karac-b1916-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

const HEAD: &str = "shared struct Node { val: i64, mut kids: Vec[Node] }
fn rd(n: frozen Node) -> i64 { n.val + n.kids.len() }
";

fn accepted_and_runs(name: &str, main: &str, want: &str) {
    let src = format!("{HEAD}{main}");
    let check = karac(name, "check", &src);
    assert!(
        check.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let run = karac(name, "__mir-run", &src);
    assert_eq!(String::from_utf8_lossy(&run.stdout), want, "{name}");
}

fn refused(name: &str, main: &str) {
    let src = format!("{HEAD}{main}");
    let check = karac(name, "check", &src);
    let err = String::from_utf8_lossy(&check.stderr);
    assert!(
        err.contains("error[E0512]") && err.contains("its source is used before the `freeze`"),
        "{name}: {err}"
    );
}

#[test]
fn freeze_after_pushing_a_handle_free_struct_literal() {
    accepted_and_runs(
        "lit",
        "fn main() { let r = Node { val: 1, kids: Vec.new() }; \
         r.kids.push(Node { val: 2, kids: Vec.new() }); let g = freeze r; println(rd(g)); }\n",
        "2\n",
    );
}

#[test]
fn freeze_after_a_scalar_read_of_the_source() {
    accepted_and_runs(
        "scalar",
        "fn main() { let r = Node { val: 1, kids: Vec.new() }; println(r.val); \
         let g = freeze r; println(rd(g)); }\n",
        "1\n1\n",
    );
}

#[test]
fn freeze_after_pushing_a_literal_that_holds_another_handle_is_refused() {
    refused(
        "holds",
        "fn main() { let r = Node { val: 1, kids: Vec.new() }; \
         let o = Node { val: 5, kids: Vec.new() }; \
         r.kids.push(Node { val: 2, kids: [o].to_vec() }); let g = freeze r; println(rd(g)); }\n",
    );
}
