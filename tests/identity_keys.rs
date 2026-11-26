//! `Identity[T]` (std.hash): a `Map` or `Set` keyed by a handle's identity
//! (design.md § Shared types "Identity keys", collections.md § Hashing,
//! B-2026-11-21-55). A deep copy of a graph of `shared` nodes maps each
//! original to its copy; before this it needed an id field in the user's
//! type or an O(n^2) search with `ref_eq` (kata 1485's question).

use std::process::Command;

#[cfg(feature = "llvm")]
mod common;

fn karac(name: &str, src: &str, cmd: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-identity-keys-{name}-{}-{}",
        cmd.trim_start_matches('_'),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The program's stdout on every backend this build has, after `karac
/// check` accepted it.
fn runs_everywhere(name: &str, src: &str) -> Vec<String> {
    let (ok, out, err) = karac(name, src, "check");
    assert!(ok, "{name}: check: {out}{err}");
    let mut outs = Vec::new();
    for cmd in ["__mir-run", "__mir-llvm-run", "__mir-llvm-asan"] {
        if cmd != "__mir-run" && !cfg!(feature = "llvm") {
            continue;
        }
        let (ok, out, err) = karac(name, src, cmd);
        #[cfg(feature = "llvm")]
        if cmd != "__mir-run"
            && common::link_or_skip(if ok { Ok(()) } else { Err(err.clone()) }).is_none()
        {
            continue;
        }
        assert!(ok, "{name}: {cmd}: {err}");
        outs.push(out);
    }
    outs
}

fn refused(name: &str, src: &str) -> String {
    let (ok, out, err) = karac(name, src, "check");
    assert!(!ok, "{name}: check accepted it:\n{src}");
    format!("{out}{err}")
}

const DEEP_COPY: &str = r#"import std.hash.Identity;

shared struct Node {
    val: i64,
    mut next: Option[Node],
    mut random: weak Node,
}

fn copy_list(head: Option[Node]) -> Option[Node] {
    let mut copies: Map[Identity[Node], Node] = Map.new();
    let mut cur = head;
    while let Some(n) = cur {
        copies.insert(Identity.of(n), Node { val: n.val, next: None, random: None });
        cur = n.next;
    }
    cur = head;
    while let Some(n) = cur {
        let c = copies[n.identity()];
        if let Some(x) = n.next {
            c.next = Some(copies[Identity.of(x)]);
        }
        if let Some(r) = n.random {
            c.random = copies[Identity.of(r)];
        }
        cur = n.next;
    }
    match head {
        Some(h) => Some(copies[Identity.of(h)]),
        None => None,
    }
}

fn show(head: Option[Node]) -> String {
    let mut seen: Set[Identity[Node]] = Set.new();
    let mut out = "";
    let mut cur = head;
    while let Some(n) = cur {
        seen.insert(n.identity());
        let r = match n.random {
            Some(r) => f"{r.val}",
            None => "-",
        };
        out = f"{out}[{n.val} {r}]";
        cur = n.next;
    }
    f"{out} {seen.len()}"
}

fn main() {
    let vals = [7, 13, 11, 10, 1];
    let rnd = [-1, 0, 4, 2, 0];
    let mut nodes: Vec[Node] = Vec.new();
    for v in vals {
        nodes.push(Node { val: v, next: None, random: None });
    }
    for i in 0..nodes.len() {
        if i + 1 < nodes.len() {
            nodes[i].next = Some(nodes[i + 1]);
        }
        if rnd[i] >= 0 {
            nodes[i].random = nodes[rnd[i]];
        }
    }
    let head = Some(nodes[0]);
    let copy = copy_list(head);
    println(show(head));
    println(show(copy));
    let mut shared_nodes = 0;
    let mut a = head;
    let mut b = copy;
    while let (Some(x), Some(y)) = (a, b) {
        if x.ref_eq(y) {
            shared_nodes += 1;
        }
        a = x.next;
        b = y.next;
    }
    let k = Identity.of(nodes[2]);
    println(f"{shared_nodes} {k == nodes[2].identity()} {k == Identity.of(nodes[3])} {k.get().val} {k.clone() == k}");
}
"#;

#[test]
fn an_identity_map_deep_copies_a_list_with_random_pointers() {
    for out in runs_everywhere("deep-copy", DEEP_COPY) {
        assert_eq!(
            out,
            "[7 -][13 7][11 1][10 11][1 7] 5\n[7 -][13 7][11 1][10 11][1 7] 5\n0 true false 11 true\n"
        );
    }
}

const DROPS: &str = r#"import std.hash.Identity;

shared struct Node {
    val: i64,
}

impl Drop for Node {
    fn drop(mut ref self) {
        println(f"drop {self.val}");
    }
}

fn main() {
    {
        let mut m: Map[Identity[Node], i64] = Map.new();
        let mut s: Set[Identity[Node]] = Set.new();
        let mut i = 0;
        while i < 3 {
            let n = Node { val: i };
            m.insert(Identity.of(n), i * 10);
            s.insert(n.identity());
            i += 1;
        }
        println(f"{m.len()} {s.len()}");
    }
    println("end");
}
"#;

#[test]
fn dropping_an_identity_table_releases_its_handles() {
    for out in runs_everywhere("drops", DROPS) {
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.first(), Some(&"3 3"), "{out}");
        assert_eq!(lines.last(), Some(&"end"), "{out}");
        let mut drops: Vec<&str> = lines[1..lines.len() - 1].to_vec();
        drops.sort();
        assert_eq!(drops, ["drop 0", "drop 1", "drop 2"], "{out}");
    }
}

const SYNC_TASK: &str = r#"import std.sync.{Atomic, TaskGroup};
import std.hash.Identity;

sync struct Counter {
    hits: Atomic[i64],
}

fn main() {
    let a = Counter { hits: Atomic.new(0) };
    let b = Counter { hits: Atomic.new(0) };
    let mut seen: Set[Identity[Counter]] = Set.new();
    seen.insert(Identity.of(a));
    seen.insert(a.identity());
    let k = Identity.of(b);
    let g = TaskGroup.new();
    let t = g.spawn(|| {
        let c = k.get();
        c.hits.fetch_add(5);
        c.hits.load()
    });
    let got = t.join();
    println(f"{seen.len()} {seen.contains(Identity.of(a))} {seen.contains(Identity.of(b))} {got} {b.hits.load()}");
}
"#;

#[test]
fn a_sync_identity_key_crosses_a_task() {
    for out in runs_everywhere("sync-task", SYNC_TASK) {
        assert_eq!(out, "1 true false 5 5\n");
    }
}

#[test]
fn a_shared_identity_key_does_not_cross_a_task() {
    let err = refused(
        "shared-task",
        r#"import std.sync.{TaskGroup};
import std.hash.Identity;

shared struct Node {
    val: i64,
}

fn main() {
    let a = Node { val: 1 };
    let k = Identity.of(a);
    let g = TaskGroup.new();
    let t = g.spawn(|| k.get().val);
    println(t.join());
}
"#,
    );
    assert!(err.contains("E_NOT_CROSS_TASK"), "{err}");
    assert!(err.contains("Identity[Node]"), "{err}");
}

#[test]
fn identity_of_a_value_is_refused() {
    let err = refused(
        "value",
        r#"import std.hash.Identity;

struct Plain {
    val: i64,
}

fn main() {
    let a = Plain { val: 1 };
    let k = Identity.of(a);
    println(k.get().val);
}
"#,
    );
    assert!(err.contains("`Plain` is not a handle"), "{err}");
}

#[test]
fn a_programs_own_identity_method_wins() {
    let src = r#"shared struct Node {
    val: i64,
}

impl Node {
    fn identity(ref self) -> i64 {
        self.val * 2
    }
}

fn main() {
    let a = Node { val: 21 };
    println(a.identity());
}
"#;
    for out in runs_everywhere("own-method", src) {
        assert_eq!(out, "42\n");
    }
}
