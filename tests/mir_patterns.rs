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
    let moved: Vec<&(String, String, bool)> =
        tc.core_task_moved_captures.values().flatten().collect();
    assert_eq!(
        moved,
        vec![&("data".to_string(), "g".to_string(), false)],
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
    bump(mut n);
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

// B-2026-10-14-32: a move of a non-`Copy` field out of a `ref` or out of a
// `shared` value is refused at its source line (core semantics §3.7), as
// `karac check` refuses it, not by the MIR validator naming MIR locals.
#[test]
fn a_move_out_of_a_borrowed_or_shared_place_is_refused_at_its_line() {
    let src = r#"shared struct Inner { word: String }
impl Inner { fn get(ref self) -> String { self.word } }
fn give(i: ref Inner) -> String { let w = i.word; w }
shared struct S { v: i64, t: String }
fn main() {
    let a = Inner { word: "hi" };
    let s = S { v: 1, t: "t" };
    let j = |q: S| q.t;
    println(f"{a.get()} {give(a)} {j(s)}");
}
"#;
    let err = match karac::mir::lower::build_source(src) {
        Ok(_) => panic!("built"),
        Err(e) => e,
    };
    assert!(!err.contains("invalid MIR"), "{err}");
    for want in [
        "2:43: cannot move a non-`Copy` value out of a borrowed place",
        "3:43: cannot move a non-`Copy` value out of a borrowed place",
        "8:20: cannot move a non-`Copy` value out of a `shared` value",
    ] {
        assert!(err.contains(want), "{want}\n{err}");
    }
}

// B-2026-10-14-40: `??` reads a `ref` of a `Copy` `Option` / `Result`
// through the reference, as `*v ?? 0` does.
#[test]
fn coalesce_reads_a_ref_of_a_copy_wrapper() {
    assert_runs(
        r#"fn f(o: mut ref Option[i64]) -> i64 { o ?? 5 }
fn main() {
    let values: Vec[Option[i64]] = vec![Some(3), None, Some(4)];
    let mut sum = 0;
    for v in values.iter() { sum += v ?? 0; }
    let r = ref values[0];
    sum += r ?? 0;
    let rs: Vec[Result[i64, i64]] = vec![Ok(7), Err(1)];
    for x in rs.iter() { sum += x ?? 100; }
    let mut o: Option[i64] = None;
    sum += f(mut o);
    println(f"{sum}");
}
"#,
        "122\n",
    );
}

// B-2026-10-14-37: `c[k]` on a user type is the place its `index` returns,
// `c[k] = v` and `c[k] += v` write through `index_mut`.
#[test]
fn user_index_impls_are_called() {
    assert_runs(
        r#"struct Grid { w: i64, cells: Vec[i64] }
impl Index[(i64, i64)] for Grid {
    type Output = i64;
    fn index(self, idx: (i64, i64)) -> ref i64 {
        let (r, c) = idx;
        ref self.cells[r * self.w + c]
    }
}
impl IndexMut[(i64, i64)] for Grid {
    type Output = i64;
    fn index_mut(mut ref self, idx: (i64, i64)) -> mut ref i64 {
        let (r, c) = idx;
        self.cells[r * self.w + c]
    }
}
struct Names { all: Vec[String] }
impl Index[i64] for Names {
    type Output = String;
    fn index(self, i: i64) -> ref String { ref self.all[i] }
}
struct Stack[T] { items: Vec[T] }
impl[T] Index[i64] for Stack[T] {
    type Output = T;
    fn index(self, i: i64) -> ref T { ref self.items[self.items.len() as i64 - 1 - i] }
}
fn main() {
    let mut g = Grid { w: 2, cells: vec![1, 2, 3, 4] };
    g[1, 0] = 30;
    g[0, 1] += 5;
    println(f"{g[1, 0]} {g[0, 1]} {g[(1, 1)]}");
    let n = Names { all: vec!["ada".to_string(), "bob".to_string()] };
    println(f"{n[1]} {n[0].len()}");
    let c = n[0].clone();
    println(c);
    let s = Stack { items: vec![1, 2, 3] };
    let t = Stack { items: vec!["a".to_string(), "b".to_string()] };
    println(f"{s[0]} {s[2]} {t[0]} {t[1]}");
}
"#,
        "30 7 4\nbob 3\nada\n3 1 b a\n",
    );
}

/// B-2026-10-15-25: `|v: own T|` parses and runs; the annotation sets the
/// parameter's mode.
#[test]
fn a_closure_with_an_own_parameter_runs() {
    assert_runs(
        r#"
fn main() {
    let f: Fn(own Vec[i64]) -> i64 = |v: own Vec[i64]| v.len();
    let v: Vec[i64] = Vec.new();
    println(f(v));
    let g = |w: own Vec[i64], k: i64| w.len() + k;
    let u: Vec[i64] = vec![1];
    println(g(u, 10));
}
"#,
        "0\n11\n",
    );
}

/// B-2026-10-15-29: a struct declared after an enum with a variant of the
/// same name is not a duplicate; both are usable.
#[test]
fn a_struct_after_an_enum_with_its_name_as_a_variant_runs() {
    assert_runs(
        r#"
enum Block { Heading(i64), Rule }
struct Heading { level: i64 }
enum Shape { Circle(Circle), Dot }
struct Circle { r: i64 }
fn main() {
    let h = Heading { level: 3 };
    match Block.Heading(2) {
        Block.Heading(n) => println(f"{n} {h.level}"),
        Block.Rule => println("rule"),
    }
    match Shape.Circle(Circle { r: 4 }) {
        Shape.Circle(c) => println(c.r),
        Shape.Dot => println("dot"),
    }
}
"#,
        "2 3\n4\n",
    );
}

/// B-2026-10-15-49: a variant pattern spelled through a type alias names
/// the target's variant. `OldE.A` matched every value (a bare name binding)
/// and `OldE.B(n)` was refused.
#[test]
fn a_variant_pattern_through_a_type_alias_tests_the_variant() {
    assert_runs(
        r#"
enum NetE { A, B(i32) }
type OldE = NetE;
fn main() {
    let v: NetE = NetE.B(7);
    match v {
        OldE.A => println("a"),
        OldE.B(n) => println(f"b {n}"),
    }
    let w: OldE = NetE.A;
    match w {
        OldE.B(n) => println(f"b {n}"),
        OldE.A => println("a"),
    }
}
"#,
        "b 7\na\n",
    );
}

/// `mut ref place` borrows a field or an element for writing, and the
/// writes land in the place (core semantics §5.1, B-2026-10-15-14).
#[test]
fn a_mut_ref_place_writes_through_to_the_place() {
    assert_runs(
        r#"
struct U { refused: i64, down_for: i64 }
struct Pl { members: Vec[U] }
struct Proxy { pools: Vec[Pl], hits: i64 }
impl Pl {
    fn at(mut ref self, i: i64) -> mut ref U { mut ref self.members[i] }
}
fn main() {
    let mut proxy = Proxy { pools: vec![Pl { members: vec![U { refused: 0, down_for: 0 }, U { refused: 0, down_for: 0 }] }], hits: 0 };
    let pool = mut ref proxy.pools[0];
    let up = mut ref pool.members[1];
    up.refused += 1;
    up.down_for = 3;
    let u0 = pool.at(0);
    u0.refused += 7;
    let h = mut ref proxy.hits;
    h += 2;
    println(f"{proxy.pools[0].members[1].refused} {proxy.pools[0].members[1].down_for} {proxy.pools[0].members[0].refused} {proxy.hits}");
}
"#,
        "1 3 7 2\n",
    );
}

/// A value enum carries another value enum as a payload, a unit-only one
/// or one with a heap payload, and patterns reach into it
/// (B-2026-10-06-86).
#[test]
fn a_value_enum_carries_value_enum_payloads() {
    assert_runs(
        r#"
enum Kind { Round, Square, Curly }
enum Tok { Open(Kind), Close(Kind) }
enum V { Nil, Int(i64), Text(String) }
enum Place { Global(String), Field(V, V, String) }
fn kind_of(t: ref Tok) -> i64 {
    match t {
        Tok.Open(Kind.Round) | Tok.Close(Kind.Round) => 1,
        Tok.Open(Kind.Square) | Tok.Close(Kind.Square) => 2,
        _ => 3,
    }
}
fn main() {
    let ts = vec![Tok.Open(Kind.Square), Tok.Close(Kind.Curly), Tok.Open(Kind.Round)];
    let mut s = 0;
    for t in ts.iter() { s = s * 10 + kind_of(t); }
    println(f"{s}");
    let ps = vec![Place.Field(V.Text("k".to_string()), V.Int(4), "f".to_string()), Place.Global("g".to_string())];
    for p in ps.iter() {
        match p {
            Place.Field(V.Text(a), V.Int(b), c) => println(f"{a} {b} {c}"),
            Place.Field(_, _, c) => println(f"other {c}"),
            Place.Global(n) => println(n),
        }
    }
}
"#,
        "231\nk 4 f\ng\n",
    );
}

/// A literal arm compares at the scrutinee's width: an upper-half unsigned
/// constant (`u64::MAX`, which the literal holds as a 128-bit `-1`) and a
/// negative signed one match their own arm (B-2026-10-22-37).
#[test]
fn literal_arms_match_at_the_scrutinees_width() {
    assert_runs(
        r#"
fn main() {
    let b: u64 = 18446744073709551615u64;
    let s = match b {
        18446744073709551615u64 => "u64 max",
        _ => "u64 other",
    };
    println(s);
    let c: u32 = 4294967295u32;
    let t = match c {
        4294967295u32 => "u32 max",
        _ => "u32 other",
    };
    println(t);
    let n: i64 = -1;
    let u = match n {
        -1 => "minus one",
        _ => "i64 other",
    };
    println(u);
    let m: i8 = -128;
    let v = match m {
        -128 => "i8 min",
        _ => "i8 other",
    };
    println(v);
    let h: u128 = 340282366920938463463374607431768211455u128;
    let w = match h {
        340282366920938463463374607431768211455u128 => "u128 max",
        _ => "u128 other",
    };
    println(w);
}
"#,
        "u64 max\nu32 max\nminus one\ni8 min\nu128 max\n",
    );
}
