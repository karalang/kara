//! The MIR interpreter keeps a boxed field's value (`next` in
//! `struct ListNode { next: Option[ListNode] }`, docs/core-semantics.md
//! and `layout::boxed_field`) in a heap slot of its own, as compiled code
//! keeps it in a box (B-2026-11-14-61). A reference into a plain
//! recursive value was an address walked down from the root, a field and
//! a variant step per node, so following a list through a `ref` cursor or
//! a recursion was quadratic: 17 s for 16000 nodes, where compiled code
//! takes milliseconds. These pin that the walk is linear and that every
//! way of building, reading, moving, cloning, comparing, hashing and
//! dropping such a value still gives the same answers, with nothing
//! leaked at exit (the interpreter's leak check fails the run).

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const LIST: &str = r#"
#[derive(PartialEq, Eq, Clone, Hash)]
struct ListNode { val: i64, next: Option[ListNode] }

fn build(vals: own Vec[i64]) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    for v in vals.iter().rev() {
        head = Some(ListNode { val: v, next: head });
    }
    head
}

fn count(n: ref Option[ListNode]) -> i64 {
    match n {
        Some(x) => 1 + count(x.next),
        None => 0,
    }
}

fn sum_owned(n: own Option[ListNode]) -> i64 {
    match n {
        Some(x) => x.val + sum_owned(x.next),
        None => 0,
    }
}

fn reverse(head: own Option[ListNode]) -> Option[ListNode] {
    let mut prev: Option[ListNode] = None;
    let mut cur = head;
    while let Some(node) = cur {
        let mut node = node;
        cur = node.next.take();
        node.next = prev;
        prev = Some(node);
    }
    prev
}

fn push_back(head: mut ref Option[ListNode], v: i64) {
    let mut cur = head;
    while let Some(node) = cur {
        cur = mut ref node.next;
    }
    *cur = Some(ListNode { val: v, next: None });
}

fn show(n: ref Option[ListNode]) -> String {
    let mut out = "";
    let mut cur = n;
    while let Some(node) = cur {
        out = out + f"{node.val} ";
        cur = ref node.next;
    }
    out
}

fn main() {
    let a = build([1, 2, 3, 4].to_vec());
    let b = a.clone();
    println(a == b);
    println(count(a));
    println(show(a));
    let r = reverse(b);
    println(show(r));
    let mut c = build([7].to_vec());
    push_back(mut c, 8);
    push_back(mut c, 9);
    println(show(c));
    println(c == a);
    let mut m: Map[ListNode, i64] = Map.new();
    if let Some(n) = a.clone() { m.insert(n, 5); }
    if let Some(n) = a.clone() { println(m.get(n)); }
    let mut s: Set[ListNode] = Set.new();
    s.insert(ListNode { val: 3, next: Some(ListNode { val: 4, next: None }) });
    println(s.contains(ListNode { val: 3, next: Some(ListNode { val: 4, next: None }) }));
    println(s.contains(ListNode { val: 3, next: None }));
    let mut vs: Vec[Option[ListNode]] = Vec.new();
    vs.push(build([3, 1].to_vec()));
    vs.push(r);
    println(vs.len());
    if let Some(n) = c {
        let mut n = n;
        let tail = n.next;
        println(show(tail));
        n.next = build([42].to_vec());
        println(n.val);
        println(count(n.next));
    }
    println(sum_owned(a));
    let mut d = build([5, 6, 7].to_vec());
    if let Some(n) = mut ref d {
        if let Some(m2) = mut ref n.next {
            m2.val = 60;
        }
    }
    println(show(d));
    let moved = mem.replace(mut d, None);
    println(show(moved));
    println(show(d));
}
"#;

const LIST_WANT: &str = "true\n4\n1 2 3 4 \n4 3 2 1 \n7 8 9 \nfalse\nSome(5)\ntrue\nfalse\n2\n\
                         8 9 \n7\n1\n10\n5 60 7 \n5 60 7 \n\n";

const TREE: &str = r#"
#[derive(Clone, PartialEq)]
enum Expr { Lit(i64), Add(Expr, Expr), Neg(Expr) }

fn eval(e: ref Expr) -> i64 {
    match e {
        Expr.Lit(n) => n,
        Expr.Add(a, b) => eval(a) + eval(b),
        Expr.Neg(a) => 0 - eval(a),
    }
}

fn simplify(e: own Expr) -> Expr {
    match e {
        Expr.Neg(Expr.Neg(x)) => simplify(x),
        Expr.Neg(other) => Expr.Neg(simplify(other)),
        Expr.Add(a, b) => Expr.Add(simplify(a), simplify(b)),
        lit => lit,
    }
}

struct Tree { key: i64, left: Option[Tree], right: Option[Tree] }

fn insert(t: mut ref Option[Tree], k: i64) {
    match t {
        Some(n) => if k < n.key { insert(mut n.left, k) } else { insert(mut n.right, k) },
        None => *t = Some(Tree { key: k, left: None, right: None }),
    }
}

fn inorder(t: ref Option[Tree], out: mut ref Vec[i64]) {
    if let Some(n) = t {
        inorder(n.left, mut out);
        out.push(n.key);
        inorder(n.right, mut out);
    }
}

fn main() {
    let e = Expr.Add(Expr.Lit(3), Expr.Neg(Expr.Neg(Expr.Lit(4))));
    println(eval(e));
    let f = e.clone();
    let s = simplify(e);
    println(eval(s));
    println(s == f);
    let mut t: Option[Tree] = None;
    for k in [5, 3, 8, 1, 4, 9, 7].to_vec() { insert(mut t, k); }
    let mut out: Vec[i64] = Vec.new();
    inorder(t, mut out);
    println(f"{out}");
    if let Some(n) = t {
        let l = n.left;
        let mut o2: Vec[i64] = Vec.new();
        inorder(l, mut o2);
        println(f"{o2}");
    }
}
"#;

const TREE_WANT: &str = "7\n7\nfalse\n[1, 3, 4, 5, 7, 8, 9]\n[1, 3, 4]\n";

/// Kata 2130's walk: build a list of `n` nodes, then sum it through a
/// `ref` cursor and through a recursion over the borrowed list.
const WALK: &str = r#"
struct ListNode { val: i32, next: Option[ListNode] }

fn build(n: i64) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = n;
    while i > 0 {
        head = Some(ListNode { val: (i % 1000) as i32, next: head });
        i = i - 1;
    }
    head
}

fn count(n: ref Option[ListNode]) -> i64 {
    match n {
        Some(x) => 1 + count(x.next),
        None => 0,
    }
}

fn main() {
    let head = build(NODES);
    let mut total = 0;
    let mut cur = ref head;
    while let Some(node) = cur {
        total = total + node.val as i64;
        cur = ref node.next;
    }
    println(total);
    println(count(head));
}
"#;

fn run(mode: &str, src: &str) -> (String, Duration) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-boxed-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let at = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(mode)
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let took = at.elapsed();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{mode}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (String::from_utf8_lossy(&out.stdout).into_owned(), took)
}

#[test]
fn interp_boxed_fields_list_operations() {
    assert_eq!(run("__mir-run", LIST).0, LIST_WANT);
}

#[test]
fn interp_boxed_fields_trees() {
    assert_eq!(run("__mir-run", TREE).0, TREE_WANT);
}

/// 100000 nodes, the kata's largest input. Linear, the walk takes well
/// under a second even in a debug build; quadratic, it took 1299 s.
#[test]
fn interp_walks_a_long_plain_list_in_linear_time() {
    let (out, took) = run("__mir-run", &WALK.replace("NODES", "100000"));
    assert_eq!(out, "49950000\n100000\n");
    assert!(
        took < Duration::from_secs(60),
        "100000-node walk took {took:?}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_boxed_fields_agree_with_the_interpreter() {
    assert_eq!(run("__mir-llvm-run", LIST).0, LIST_WANT);
    assert_eq!(run("__mir-llvm-run", TREE).0, TREE_WANT);
    assert_eq!(
        run("__mir-llvm-run", &WALK.replace("NODES", "100000")).0,
        "49950000\n100000\n"
    );
}
