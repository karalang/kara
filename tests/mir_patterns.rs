//! `let` patterns on the MIR pipeline: what a destructuring `let` moves out
//! of its scrutinee, and what it only borrows (core semantics §4.6).

use karac::mir::interp::Outcome;

fn run(src: &str) -> Result<(String, i32), String> {
    let r = karac::mir::lower::run_source(src)?;
    match (&r.outcome, r.exit_code()) {
        (_, Some(code)) => Ok((r.output, code)),
        (Outcome::Error(e), None) => Err(format!("run: {e}\n--- output so far\n{}", r.output)),
        _ => Err("no exit code".into()),
    }
}

fn assert_runs(src: &str, want: &str) {
    match run(src) {
        Ok((out, 0)) if out == want => {}
        other => panic!("want exit 0 and\n{want}--- got {other:?}"),
    }
}

/// A `ref` sub-pattern borrows the part where it is. Over a local whose
/// type runs a `Drop` body, a pattern that only borrows or copies moves
/// nothing, so the local is whole and dies at its scope's end, after the
/// borrow's last use.
#[test]
fn a_ref_sub_pattern_borrows_a_drop_local_in_place() {
    assert_runs(
        r#"
#[derive(Clone)]
struct R { id: i64 }
struct W { r: R, n: i64 }
impl Drop for W { fn drop(mut ref self) { println(f"drop W{self.n}") } }
fn take(w: own W) -> R {
    let W { r: ref r, n } = w;
    println(f"n {n}");
    r.clone()
}
fn main() {
    let r = take(W { r: R { id: 3 }, n: 4 });
    println(r.id);
}
"#,
        "n 4\ndrop W4\n3\n",
    );
}

/// Without a `Drop` body the same pattern binds a reference into the
/// local's part, and a plain binding still moves its part out.
#[test]
fn a_ref_sub_pattern_and_a_move_in_one_let() {
    assert_runs(
        r#"
struct P { a: String, b: String }
fn main() {
    let p = P { a: "x", b: "y" };
    let P { a: ref a, b } = p;
    println(f"{a} {b}");
}
"#,
        "x y\n",
    );
}

/// §9.1/§9.5 (10-10): a place that drops before the `TaskGroup` holding the
/// task is captured by move. The MIR typecheck records which, for the
/// builder, as `karac check` does.
#[test]
fn the_mir_typecheck_records_task_captures_that_move() {
    let src = "fn total(v: ref Vec[i64]) -> i64 { v.len() }\n\
               fn main() {\n\
                   let early: Vec[i64] = [4];\n\
                   let mut g = TaskGroup.new();\n\
                   let data: Vec[i64] = [1, 2, 3];\n\
                   let h = g.spawn(|| total(data) + total(early));\n\
                   println(h.join());\n\
               }\n";
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let tc = karac::typecheck_with_library_source(&parsed.program, &resolved);
    let moved: Vec<&(String, String)> = tc.core_task_moved_captures.values().flatten().collect();
    assert_eq!(
        moved,
        vec![&("data".to_string(), "g".to_string())],
        "only `data` drops before `g`"
    );
}

/// B-2026-10-14-31: the payload of a bare `None` scrutinee in `if let` and
/// `while let` takes its type from the body, as a `match` arm's does. It
/// used to stay an unbound param that typed HIR read as `()`, so `c + b`
/// built invalid MIR ("binary operands of different types i64 and ()").
#[test]
fn if_let_on_a_bare_none_types_its_payload_from_the_body() {
    assert_runs(
        r#"
fn main() {
    let c = 5;
    let mut best = 0;
    if let Some(below) = None {
        best = c + below;
    }
    if let Some(b) = None {
        let z: i64 = b;
        best = z;
    }
    let o = if let Some(b) = None { c + b } else { 1 };
    while let Some(b) = None {
        best = c + b;
    }
    println(f"{best} {o}");
}
"#,
        "0 1\n",
    );
}

/// B-2026-10-14-25: a `mut` marker on an argument already rooted at a
/// `mut ref` binding (a field of one, a forwarded parameter, a `mut ref`
/// local from `or_insert`) is the v1 spelling and lends the same place.
#[test]
fn a_marked_forward_of_a_mut_ref_writes_the_same_place() {
    assert_runs(
        r#"
struct Session {
    scores: Vec[i64],
}

struct Node {
    n: i64,
    kids: Map[i64, Node],
}

fn sort_in_place(xs: mut Slice[i64]) {
    xs.sort();
}

fn bump(n: mut ref i64) {
    n += 1;
}

fn twice(n: mut ref i64) {
    bump(mut n);
    bump(n);
}

fn refresh(s: mut ref Session) {
    sort_in_place(mut s.scores);
}

fn grow(node: mut ref Node, depth: i64) {
    if depth == 0 {
        return;
    }
    node.n += 1;
    let child = node.kids.entry(depth).or_insert(Node { n: 0, kids: Map.new() });
    grow(mut child, depth - 1);
}

fn main() {
    let mut s = Session { scores: vec![3, 1, 2] };
    refresh(mut s);
    let mut k = 1;
    twice(mut k);
    let mut root = Node { n: 0, kids: Map.new() };
    grow(mut root, 3);
    let mut v = vec![1, 2];
    bump(mut v[0]);
    println(f"{s.scores} {k} {root.n} {root.kids.len()} {v}");
}
"#,
        "[1, 2, 3] 3 1 1 [2, 2]\n",
    );
}

/// B-2026-10-14-33: `-a` and `~a` on a user type call its `impl Neg` /
/// `impl Not`, the receiver passed as the impl takes it (`own self` moves a
/// non-`Copy` operand, a bare `self` borrows).
#[test]
fn user_neg_and_not_impls_are_called() {
    assert_runs(
        r#"
#[derive(Copy, Clone)]
struct V2 {
    x: i64,
    y: i64,
}

impl Neg for V2 {
    type Output = V2;
    fn neg(own self) -> Self.Output {
        V2 { x: -self.x, y: -self.y }
    }
}

struct Mask {
    bits: i64,
    name: String,
}

impl Not for Mask {
    fn not(self) -> Mask {
        Mask { bits: ~self.bits, name: self.name.clone() }
    }
}

struct Tag {
    s: String,
}

impl Neg for Tag {
    fn neg(own self) -> Tag {
        Tag { s: self.s + "!" }
    }
}

fn main() {
    let a = V2 { x: 1, y: -2 };
    let b = -a;
    let m = Mask { bits: 5, name: "m" };
    let n = ~m;
    let t = Tag { s: "hi" };
    let u = -t;
    println(f"{b.x} {b.y} {a.x} {n.bits} {m.name} {u.s}");
}
"#,
        "-1 2 1 -6 m hi!\n",
    );
}
