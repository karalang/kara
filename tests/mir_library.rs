//! The library's Kāra methods on the MIR pipeline (redesign A2).
//!
//! `Option` and `Result` methods are Kāra source in
//! `runtime/stdlib/source/`, appended to the program by the MIR pipeline and
//! typed through their bodies. Each program here runs on the MIR interpreter
//! and is held to the output the legacy interpreter prints for it.

use karac::mir::interp::Outcome;

/// Build `src` through the MIR pipeline and run it: the output and exit
/// code, or the stage that refused it.
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

#[test]
fn option_and_result_methods_run_from_their_kara_bodies() {
    let src = r#"
fn parse_pos(s: String) -> Result[i64, String] {
    if s == "x" { Err(f"bad {s}") } else { Ok(s.len()) }
}

fn main() {
    let a: Option[i64] = Some(3);
    let b: Option[i64] = None;
    println(f"{a.is_some()} {b.is_none()}");
    println(f"{a.unwrap_or(7)} {b.unwrap_or(7)} {b.unwrap_or_else(|| 9)}");
    let c = a.map(|x| x * 2);
    println(f"{c.unwrap()} {b.map_or(-1, |x| x + 1)}");
    let d = a.and_then(|x| if x > 1 { Some(x.to_string()) } else { None });
    println(f"{d.unwrap()}");
    let e: Option[String] = None;
    println(f"{e.or(Some("z".to_string())).unwrap()}");
    println(f"{a.filter(|x| x > 5).is_none()}");
    let r = a.ok_or("none".to_string());
    println(f"{r.is_ok()}");
    let p = parse_pos("abc".to_string());
    let q = parse_pos("x".to_string());
    println(f"{p.is_ok()} {q.is_err()} {p.unwrap()} {parse_pos("x".to_string()).unwrap_or(0)}");
    let m = q.map_err(|e| e.len());
    println(f"{m.unwrap_err()} {parse_pos("abc".to_string()).map(|n| n * 10).unwrap()}");
    let s = parse_pos("hey".to_string()).and_then(|n| parse_pos(n.to_string()));
    println(f"{s.ok().unwrap()} {parse_pos("x".to_string()).err().unwrap()}");
    let t = parse_pos("x".to_string()).unwrap_or_else(|e| e.len() * 100);
    println(f"{t}");
}
"#;
    assert_runs(
        src,
        "true true\n3 7 9\n6 -1\n3\nz\ntrue\ntrue\ntrue true 3 0\n5 30\n1 bad x\n500\n",
    );
}

/// `unwrap` and `expect` on the empty case panic, which exits 101 with what
/// was printed before it.
#[test]
fn unwrap_on_none_panics_from_the_kara_body() {
    let src = r#"
fn main() {
    let a: Option[i64] = None;
    println("before");
    println(f"{a.expect("never")}");
}
"#;
    assert_eq!(run(src), Ok(("before\n".to_string(), 101)));
}

/// `Option[T]` is `Copy` when `T` is (core semantics §1), so a by-value
/// method leaves an `Option[i64]` usable.
#[test]
fn an_option_of_a_copy_type_is_copied_into_a_by_value_method() {
    let src = r#"
fn main() {
    let a: Option[i64] = Some(4);
    let x = a.unwrap_or(0);
    let y = a.map(|v| v + 1).unwrap_or(0);
    println(f"{x} {y} {a.is_some()}");
}
"#;
    assert_runs(src, "4 5 true\n");
}

/// A program's own `impl Option[T]` is found although the stdlib's
/// `impl Option[Ordering]` already took `impl#0` for that target.
#[test]
fn a_programs_own_impl_on_a_library_type_is_found() {
    let src = r#"
impl[T] Option[T] {
    fn get_or(own self, d: own T) -> T {
        match self {
            Some(x) => x,
            None => d,
        }
    }
}

fn main() {
    let a: Option[i64] = Some(3);
    let b: Option[String] = None;
    println(f"{a.get_or(7)} {b.get_or("dflt".to_string())}");
}
"#;
    assert_runs(src, "3 dflt\n");
}

/// A generic body calls its function-typed parameter, in a free function
/// and in a method.
#[test]
fn a_generic_body_calls_its_closure_parameter() {
    let src = r#"
struct W[T] { v: T }
impl[T] W[T] {
    fn apply[U](self, f: OnceFn(T) -> U) -> U {
        f(self.v)
    }
}
fn mapit[T, U](o: Option[T], f: OnceFn(T) -> U) -> Option[U] {
    match o {
        Some(x) => Some(f(x)),
        None => None,
    }
}
fn main() {
    let w = W { v: 4 };
    println(f"{w.apply(|x| x + 1)}");
    match mapit(Some(2), |x| x * 3) {
        Some(y) => println(f"{y}"),
        None => {}
    }
}
"#;
    assert_runs(src, "5\n6\n");
}

/// A variant built inside a generic impl takes the type its context needs,
/// not one spelled with the enum's own parameter names: `O2.N` here is an
/// `O2[E]`, though `O2` declares its parameter as `T` and the impl has a `T`
/// of its own.
#[test]
fn a_variant_in_a_generic_impl_takes_its_contexts_type() {
    let src = r#"
enum R2[T, E] { A(T), B(E) }
enum O2[T] { S(T), N }
impl[T, E] R2[T, E] {
    fn err2(own self) -> O2[E] {
        match self {
            A(_) => O2.N,
            B(e) => O2.S(e),
        }
    }
}
fn main() {
    let r: R2[i64, String] = R2.B("e".to_string());
    match r.err2() {
        S(v) => println(v),
        N => println("none"),
    }
    let s: R2[i64, String] = R2.A(1);
    match s.err2() {
        S(v) => println(v),
        N => println("none"),
    }
}
"#;
    assert_runs(src, "e\nnone\n");
}

/// A baked stdlib trait impl re-stated as a library source runs on the MIR
/// pipeline: `AllocError` prints through its written-out `Display`, by
/// interpolation and by `to_string`, not as the derived variant name.
#[test]
fn alloc_error_displays_through_its_library_impl() {
    let src = r#"
fn main() {
    println(f"{AllocError.CapacityOverflow}");
    let e = AllocError.OutOfMemory { requested_bytes: 64 };
    println(f"{e}");
    println(e.to_string());
}
"#;
    assert_runs(
        src,
        "capacity overflow\nout of memory: 64 bytes\nout of memory: 64 bytes\n",
    );
}

/// `Ordering`'s predicates run from their library bodies, on a computed
/// ordering and on a unit variant named as the receiver
/// (`Ordering.Less.is_lt()` parses as a path call).
#[test]
fn ordering_predicates_run_from_their_library_bodies() {
    let src = r#"
fn main() {
    let o = 3.cmp(5);
    println(f"{o.is_lt()} {o.is_ge()} {5.cmp(5).is_le()} {5.cmp(5).is_eq()}");
    println(f"{Ordering.Less.is_lt()} {Ordering.Greater.is_le()} {Ordering.Equal.is_ge()}");
}
"#;
    assert_runs(src, "true false true true\ntrue false true\n");
}

/// `PriorityQueue` is already Kāra, so its baked file is the library
/// source: smallest first by default, largest first from `max_first`, and
/// `from` heapifies.
#[test]
fn priority_queue_runs_from_its_library_body() {
    let src = r#"
fn main() {
    let mut q: PriorityQueue[i64] = PriorityQueue.new();
    q.push(5);
    q.push(1);
    q.push(3);
    println(f"{q.len()} {q.peek().unwrap()}");
    while let Some(x) = q.pop() {
        print(f"{x} ");
    }
    println("");
    let mut m: PriorityQueue[String] = PriorityQueue.max_first();
    m.push("b".to_string());
    m.push("c".to_string());
    m.push("a".to_string());
    println(f"{m.len()} {m.pop().unwrap()} {m.pop().unwrap()}");
    let h = PriorityQueue.from([4, 9, 2, 7]);
    println(f"{h.into_sorted_vec()}");
}
"#;
    assert_runs(src, "3 1\n1 3 5 \n3 c b\n[2, 4, 7, 9]\n");
}

/// The ordering operators on an `Ord` type call its `cmp`: a hand-written
/// body, or the order a derive gives (fields in declaration order, enum
/// variants by declaration order and then their payloads, tuples part by
/// part). A `PriorityQueue` of a derived-`Ord` struct pops in that order.
#[test]
fn ordering_operators_on_ord_types_compare_through_cmp() {
    let src = r#"
#[derive(Eq, PartialEq, Ord, PartialOrd)]
struct Task { pri: i64, name: String }

#[derive(Eq, PartialEq, Ord, PartialOrd)]
enum Shape { Dot, Line(i64), Box(i64, String) }

struct Rev { k: i64 }
impl PartialEq for Rev { fn eq(ref self, other: ref Rev) -> bool { self.k == other.k } }
impl Eq for Rev {}
impl PartialOrd for Rev { fn partial_cmp(ref self, other: ref Rev) -> Option[Ordering] { Some(self.cmp(other)) } }
impl Ord for Rev { fn cmp(ref self, other: ref Rev) -> Ordering { other.k.cmp(self.k) } }

fn main() {
    let a = Task { pri: 1, name: "a".to_string() };
    let b = Task { pri: 1, name: "b".to_string() };
    let c = Task { pri: 0, name: "z".to_string() };
    println(f"{a < b} {a > b} {a <= b} {a.cmp(b).is_lt()} {c < a} {a >= a}");
    let s1 = Shape.Line(3);
    let s2 = Shape.Box(1, "x".to_string());
    let s3 = Shape.Box(1, "y".to_string());
    println(f"{Shape.Dot < s1} {s1 < s2} {s2 < s3} {s3 > s2} {s1 <= Shape.Line(3)}");
    let t1 = (1, "b".to_string());
    let t2 = (1, "a".to_string());
    println(f"{t1.cmp(t2).is_gt()} {t1 > t2}");
    println(f"{Rev { k: 1 } < Rev { k: 2 }} {Rev { k: 1 } >= Rev { k: 2 }}");
    let mut q: PriorityQueue[Task] = PriorityQueue.new();
    q.push(b);
    q.push(c);
    q.push(a);
    while let Some(t) = q.pop() {
        print(f"{t.pri}{t.name} ");
    }
    println("");
}
"#;
    assert_runs(
        src,
        "true false true true true true\ntrue true true true true\ntrue true\nfalse true\n0z 1a 1b \n",
    );
}

/// `Option[Ordering]`'s predicates (what `partial_cmp` returns) run from
/// the library source that re-states the baked impl.
#[test]
fn option_ordering_predicates_run_from_their_library_bodies() {
    let src = r#"
fn main() {
    let a: Option[Ordering] = Some(Ordering.Less);
    let n: Option[Ordering] = None;
    println(f"{a.is_lt()} {a.is_le()} {a.is_gt()} {a.is_ge()} {a.is_eq()}");
    println(f"{n.is_lt()} {n.is_le()} {n.is_ge()} {n.is_eq()}");
}
"#;
    assert_runs(
        src,
        "true true false false false\nfalse false false false\n",
    );
}

/// The `F64` / `F32` wrappers compare their field by total order, every NaN
/// as one and last: `-0.0 < 0.0`, a NaN equals itself, and as `Map` keys two
/// NaNs are one key.
#[test]
fn the_float_wrappers_compare_by_total_order() {
    let src = r#"
fn main() {
    let z = 0.0;
    let a = F64.from(-z);
    let b = F64.from(z);
    let n = F64.from(z / z);
    println(f"{a < b} {a == b} {n == n} {b < n} {a.cmp(b).is_lt()}");
    let mut m: Map[F64, i64] = Map.new();
    m.insert(n, 1);
    m.insert(F64.from(z / z), 2);
    println(f"{m.len()}");
    let mut q: PriorityQueue[F32] = PriorityQueue.new();
    q.push(F32.from(2.5));
    q.push(F32.from(-1.0));
    q.push(F32.from(0.5));
    while let Some(x) = q.pop() {
        print(f"{x.value} ");
    }
    println("");
}
"#;
    assert_runs(src, "true false true true true\n1\n-1 0.5 2.5 \n");
}

/// The library source is generic over `T`; a program's own type named `T`
/// does not stand in for that parameter inside the library's bodies.
#[test]
fn a_program_s_type_named_t_does_not_shadow_the_library_s_parameter() {
    assert_runs(
        r#"
shared struct T { v: i64 }
fn main() {
    let v = vec![1, 2, 2];
    let s: Set[i64] = v.iter().collect();
    let t = T { v: s.len() };
    println(f"{t.v}");
}
"#,
        "2\n",
    );
}

/// `sum` calls `V.default()`; a program's own `struct V` or variant `V`
/// does not stand in for that parameter.
#[test]
fn a_program_s_v_does_not_shadow_the_library_s_parameter() {
    assert_runs(
        r#"
struct V { id: i64 }
enum E { V(i64) }
fn main() {
    let v = vec![1, 2, 3];
    let s: i64 = v.iter().sum();
    let x = V { id: s };
    match E.V(x.id) { E.V(n) => println(f"{n}") }
}
"#,
        "6\n",
    );
}

/// `Map.get` lends the value (`Option[ref V]`): a `Copy` value reads out
/// through `??`, a value with a `Drop` body is borrowed rather than
/// cloned, and `.cloned()` copies a `String` out.
#[test]
fn map_get_lends_the_value() {
    assert_runs(
        r#"
struct G { n: i64 }
impl Drop for G { fn drop(mut ref self) { println(f"drop {self.n}"); } }
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("a".to_string(), 3);
    let c = m.get("a".to_string()) ?? 0;
    let d = m.get("z".to_string()) ?? 7;
    match m.get("a".to_string()) { Some(v) => println(f"{v + c + d}"), None => println("none") }
    let mut g: Map[i64, G] = Map.new();
    g.insert(1, G { n: 5 });
    if let Some(x) = g.get(1) { println(f"got {x.n}"); }
    let names: Map[i64, String] = Map.new();
    let s = names.get(1).cloned() ?? "dflt".to_string();
    println(s);
    let idx: i64 = m.get("a".to_string()).unwrap();
    if let Some(k) = m.get("a".to_string()) { println(f"{idx} {idx.min(k)} {10.max(k)}"); }
}
"#,
        "13\ngot 5\ndflt\n3 3 10\ndrop 5\n",
    );
}

/// `Option.as_ref` lends the payload, and a match on a place reached
/// through `ref self` binds a part that is not `Copy` as a `ref`.
#[test]
fn as_ref_lends_the_payload() {
    assert_runs(
        r#"
struct Box2 { name: Option[String], n: Option[i64] }
impl Box2 {
    fn name_len(ref self) -> i64 {
        match self.name { Some(s) => s.len(), None => 0 }
    }
}
fn main() {
    let b = Box2 { name: Some("abc".to_string()), n: Some(4) };
    match b.name.as_ref() { Some(s) => println(f"{s}"), None => println("none") }
    match b.n.as_ref() { Some(k) => println(f"{k}"), None => println("none") }
    println(f"{b.name_len()}");
    let mut o: Option[String] = Some("x".to_string());
    let t = o.take();
    println(f"{t.is_some()} {o.is_none()}");
    let old = o.replace("y".to_string());
    println(f"{old.is_none()} {o.unwrap()}");
}
"#,
        "abc\n4\n3\ntrue true\ntrue y\n",
    );
}

/// `VecDeque.front` / `back` borrow the element `pop_front` / `pop_back`
/// would return (docs/library/collections.md), and check types them so.
#[test]
fn a_deque_lends_its_front_and_back() {
    assert_runs(
        r#"
fn main() {
    let mut q: VecDeque[(i64, i64)] = VecDeque.new();
    q.push_back((1, 5));
    q.push_back((2, 9));
    match q.front() { Some(e) => println(e.1), None => println("none") }
    match q.back() { Some(e) => { if e.1 > 3 { println(e.0) } } None => {} }
    q.pop_front();
    q.pop_front();
    match q.front() { Some(_) => println("some"), None => println("none") }
}
"#,
        "5\n2\nnone\n",
    );
}

/// `Vec.is_empty`, `reverse` and `swap_remove` are Kāra bodies over `len`,
/// `swap` and `pop` (redesign A2).
#[test]
fn vec_reverse_and_swap_remove_are_written_in_kara() {
    assert_runs(
        r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("a"); v.push("b"); v.push("c"); v.push("d");
    v.reverse();
    println(v.join(","));
    let x = v.swap_remove(0);
    println(f"{x} {v.join(",")} {v.is_empty()}");
    let mut e: Vec[i64] = Vec.new();
    e.reverse();
    println(e.is_empty());
}
"#,
        "d,c,b,a\nd a,c,b false\ntrue\n",
    );
}

#[test]
fn vec_first_and_contains_are_written_in_kara() {
    // `contains` is bounded on `T: PartialEq`, which a hand-written impl
    // satisfies as the derive does, and `==` runs that impl: `tag` is not
    // compared.
    assert_runs(
        r#"
struct P { x: i64, tag: i64 }
impl PartialEq for P { fn eq(ref self, other: ref P) -> bool { self.x == other.x } }
fn same[T: PartialEq](a: T, b: T) -> bool { a == b }
fn main() {
    let v = vec![3, 1, 4];
    println(f"{v.first()} {v.last()} {v.contains(4)} {v.contains(9)}");
    let e: Vec[i64] = Vec.new();
    println(f"{e.first()} {e.last()} {e.contains(1)}");
    let s = vec!["a".to_string(), "b".to_string()];
    let b = "b".to_string();
    println(f"{s.contains(b)} {s.last().unwrap()}");
    let ps = vec![P { x: 1, tag: 0 }, P { x: 2, tag: 0 }];
    println(f"{ps.contains(P { x: 2, tag: 7 })} {ps.first().unwrap().x}");
    let a = P { x: 1, tag: 5 };
    let b = P { x: 1, tag: 9 };
    println(f"{a == b} {a != b} {same(a, b)}");
}
"#,
        "Some(3) Some(4) true false\nNone None false\ntrue b\ntrue 1\ntrue false true\n",
    );
}

#[test]
fn sorted_ranges_are_half_open_and_inclusive_ranges_closed() {
    // docs/library/collections.md: `range(from, to)` gives the elements (or
    // entries) in `[from, to)`, ascending, and `range_inclusive` `[from, to]`.
    assert_runs(
        r#"
fn main() {
    let mut s: SortedSet[i64] = SortedSet.new();
    for x in [9, 2, 7, 4, 5, 11] { s.insert(x); }
    let mut out = "".to_string();
    for x in s.range(4, 9) { out.push_str(f"{x} "); }
    println(out);
    let mut w: SortedSet[String] = SortedSet.new();
    w.insert("pear".to_string()); w.insert("apple".to_string()); w.insert("fig".to_string());
    println(f"{w.range("b".to_string(), "g".to_string())} {s.range(20, 30).len()}");
    println(f"{s.range_inclusive(4, 9)} {s.range(9, 4).len()}");
    let mut m: SortedMap[i64, String] = SortedMap.new();
    m.insert(3, "c".to_string()); m.insert(1, "a".to_string()); m.insert(5, "e".to_string());
    println(f"{m.range(1, 5)} {m.range_inclusive(1, 5).len()}");
}
"#,
        "4 5 7 \n[fig] 0\n[4, 5, 7, 9] 0\n[(1, a), (3, c)] 3\n",
    );
}

/// B-2026-10-12-4: a removed entry leaves a hole in an insertion-ordered
/// table instead of moving every later entry down. Every read (iteration,
/// `keys`, `values`, display, `clone`, set operations, re-insertion, `clear`)
/// sees the entries in the order it saw before the change, with the holes
/// still open (a third removed) and after they close (two thirds removed).
#[test]
fn a_table_reads_the_same_after_removes_leave_holes() {
    let src = r#"
fn show(m: ref Map[i64, String]) {
    let mut line = String.new();
    for (k, v) in m {
        line.push_str(f"{k}={v} ");
    }
    println(f"[{m.len()}] {line}");
}
fn main() {
    more();
    let mut m: Map[i64, String] = Map.new();
    let mut i: i64 = 0;
    while i < 40 {
        m.insert(i, f"v{i}");
        i = i + 1;
    }
    i = 0;
    while i < 40 {
        if i % 3 == 0 { m.remove(i); }
        i = i + 1;
    }
    show(m);
    m.insert(2, "back");
    m.insert(7, "x");
    show(m);
    println(f"{m.get(7)} {m.get(2)} {m.contains_key(4)} {m.contains_key(5)}");
    let ks = m.keys();
    println(f"{ks}");
    let vs = m.values();
    println(f"{vs}");
    println(f"{m}");
    let c = m.clone();
    println(f"{c} {c.len()}");
    i = 0;
    while i < 40 { m.remove(i); i = i + 1; }
    show(m);
    m.insert(100, "h");
    show(m);
    let mut s: Set[String] = Set.new();
    i = 0;
    while i < 30 { s.insert(f"s{i}"); i = i + 1; }
    i = 0;
    while i < 30 { if i % 4 == 1 { s.remove(f"s{i}"); } i = i + 1; }
    println(f"{s} {s.len()}");
    let mut t: Set[String] = Set.new();
    t.insert("s4"); t.insert("zz");
    println(f"{s.union(t)} {s.intersection(t)} {s.difference(t)}");
    for x in s { println(x); }
    let mut e: Map[String, i64] = Map.new();
    e.insert("a", 1); e.insert("b", 2); e.insert("c", 3);
    e.remove("a");
    let r = e.entry("a").or_insert(9);
    println(f"{r}");
    *e.entry("b").or_insert(0) += 10;
    println(f"{e}");
    for (k, v) in e { println(f"{k}:{v}"); }
}
fn more() {
    let mut a: Map[i64, i64] = Map.new();
    let mut b: Map[i64, i64] = Map.new();
    let mut i: i64 = 0;
    while i < 10 { a.insert(i, i * i); i = i + 1; }
    i = 0;
    while i < 10 { if i != 4 { b.insert(i, i * i); } i = i + 1; }
    a.remove(4);
    println(f"{a} {b} {a.len()}");
    let mut s: Set[i64] = Set.new();
    i = 0;
    while i < 10 { s.insert(i); i = i + 1; }
    s.remove(0); s.remove(5);
    println(f"{s} {s.len()} {s.is_empty()} {s.contains(5)} {s.contains(6)}");
    a.clear();
    println(f"{a} {a.len()}");
    a.insert(3, 3);
    println(f"{a}");
}
"#;
    assert_runs(
        src,
        r#"{0: 0, 1: 1, 2: 4, 3: 9, 5: 25, 6: 36, 7: 49, 8: 64, 9: 81} {0: 0, 1: 1, 2: 4, 3: 9, 5: 25, 6: 36, 7: 49, 8: 64, 9: 81} 9
{1, 2, 3, 4, 6, 7, 8, 9} 8 false false true
{} 0
{3: 3}
[26] 1=v1 2=v2 4=v4 5=v5 7=v7 8=v8 10=v10 11=v11 13=v13 14=v14 16=v16 17=v17 19=v19 20=v20 22=v22 23=v23 25=v25 26=v26 28=v28 29=v29 31=v31 32=v32 34=v34 35=v35 37=v37 38=v38 
[26] 1=v1 2=back 4=v4 5=v5 7=x 8=v8 10=v10 11=v11 13=v13 14=v14 16=v16 17=v17 19=v19 20=v20 22=v22 23=v23 25=v25 26=v26 28=v28 29=v29 31=v31 32=v32 34=v34 35=v35 37=v37 38=v38 
Some(x) Some(back) true true
[1, 2, 4, 5, 7, 8, 10, 11, 13, 14, 16, 17, 19, 20, 22, 23, 25, 26, 28, 29, 31, 32, 34, 35, 37, 38]
[v1, back, v4, v5, x, v8, v10, v11, v13, v14, v16, v17, v19, v20, v22, v23, v25, v26, v28, v29, v31, v32, v34, v35, v37, v38]
{1: v1, 2: back, 4: v4, 5: v5, 7: x, 8: v8, 10: v10, 11: v11, 13: v13, 14: v14, 16: v16, 17: v17, 19: v19, 20: v20, 22: v22, 23: v23, 25: v25, 26: v26, 28: v28, 29: v29, 31: v31, 32: v32, 34: v34, 35: v35, 37: v37, 38: v38}
{1: v1, 2: back, 4: v4, 5: v5, 7: x, 8: v8, 10: v10, 11: v11, 13: v13, 14: v14, 16: v16, 17: v17, 19: v19, 20: v20, 22: v22, 23: v23, 25: v25, 26: v26, 28: v28, 29: v29, 31: v31, 32: v32, 34: v34, 35: v35, 37: v37, 38: v38} 26
[0] 
[1] 100=h 
{s0, s2, s3, s4, s6, s7, s8, s10, s11, s12, s14, s15, s16, s18, s19, s20, s22, s23, s24, s26, s27, s28} 22
{s0, s2, s3, s4, s6, s7, s8, s10, s11, s12, s14, s15, s16, s18, s19, s20, s22, s23, s24, s26, s27, s28, zz} {s4} {s0, s2, s3, s6, s7, s8, s10, s11, s12, s14, s15, s16, s18, s19, s20, s22, s23, s24, s26, s27, s28}
s0
s2
s3
s4
s6
s7
s8
s10
s11
s12
s14
s15
s16
s18
s19
s20
s22
s23
s24
s26
s27
s28
9
{b: 12, c: 3, a: 9}
b:12
c:3
a:9
"#,
    );
}

/// B-2026-10-12-8: the copies `String.bytes` hands out a view of are freed
/// once nothing refers to them. Copies of different strings still viewed
/// (from a local, a `Vec`, a struct field, a caller's frame) read their own
/// bytes after the sweeps that 900 calls on dropped 2,000-byte strings set
/// off; a sweep that freed them would hand their slots to later copies.
#[test]
fn bytes_views_in_use_outlive_the_sweeps_of_unused_ones() {
    let src = r#"
struct Holder {
    b: Slice[u8],
}

fn text(n: i64, c: char) -> String {
    let mut s = String.new();
    for _ in 0..n {
        s.push(c);
    }
    return s;
}

fn tail(b: Slice[u8]) -> i64 {
    return (b.len() as i64) * 1000 + (b[b.len() - 1] as i64);
}

// Copies of 300 different strings of 2,000 bytes or so, each dropped at
// once: enough to set off sweeps.
fn churn(rounds: i64) -> i64 {
    let mut t: i64 = 0;
    for r in 0..rounds {
        let s = text(2000 + r % 7, if r % 2 == 0 { 'm' } else { 'n' });
        let c = s.bytes();
        t += c.len() as i64;
    }
    return t;
}

fn churn_holding(b: Slice[u8], rounds: i64) -> i64 {
    let t = churn(rounds);
    return t + tail(b);
}

fn main() {
    let strs: Vec[String] = vec![text(10, 'a'), text(20, 'b'), text(30, 'c'), text(40, 'd')];
    let first = strs[0].bytes();
    let mut kept: Vec[Slice[u8]] = Vec.new();
    kept.push(strs[1].bytes());
    kept.push(strs[2].bytes());
    let h = Holder { b: strs[3].bytes() };
    let mut total = churn(300);
    total += churn_holding(strs[2].bytes(), 300);
    total += churn(300);
    let mut tails = String.new();
    for k in kept {
        tails.push_str(f"{tail(k)} ");
    }
    println(f"{total} {tail(first)} {tails}{tail(h.b)}");
}
"#;
    assert_runs(src, "1832790 10097 20098 30099 40100\n");
}

/// B-2026-10-12-10: a `VecDeque` keeps empty slots before its front, so
/// `push_front` and `pop_front` are O(1) on average. Every read (`len`,
/// `front`, `back`, `get`, indexing, iteration, `clone`, display, an owned
/// walk) sees the elements it saw when the deque was a plain `Vec`.
#[test]
fn a_deque_reads_the_same_with_room_before_its_front() {
    let src = r#"
fn show(d: ref VecDeque[String]) {
    let mut line = String.new();
    for x in d {
        line.push_str(f"{x} ");
    }
    println(f"[{d.len()}] {line}");
}

fn main() {
    let mut d: VecDeque[String] = VecDeque.new();
    for i in 0..5 {
        d.push_front(f"f{i}");
        d.push_back(f"b{i}");
    }
    show(d);
    println(f"{d.front()} {d.back()} {d.get(3)} {d.get(10)} {d[0]} {d[9]}");
    for _ in 0..3 {
        let x = d.pop_front();
        println(f"{x}");
    }
    show(d);
    let c = d.clone();
    show(c);
    for i in 0..30 {
        d.push_back(f"x{i}");
        if i % 2 == 0 {
            d.pop_front();
        }
    }
    show(d);
    while d.len() > 2 {
        d.pop_front();
    }
    d.push_front("again");
    show(d);
    println(f"{d.pop_back()} {d.pop_back()} {d.pop_back()} {d.pop_back()} {d.is_empty()}");
    d.push_front("solo");
    show(d);
    let mut q: VecDeque[i64] = VecDeque.new();
    let mut total: i64 = 0;
    for i in 0..1000 {
        q.push_back(i);
        if i % 3 == 0 {
            total += q.pop_front().unwrap();
        }
    }
    println(f"{total} {q.len()} {q.front()} {q.back()}");
    let mut owned = String.new();
    for x in c {
        owned.push_str(x);
    }
    println(owned);
}
"#;
    assert_runs(
        src,
        r#"[10] f4 f3 f2 f1 f0 b0 b1 b2 b3 b4 
Some(f4) Some(b4) Some(f1) None f4 b4
Some(f4)
Some(f3)
Some(f2)
[7] f1 f0 b0 b1 b2 b3 b4 
[7] f1 f0 b0 b1 b2 b3 b4 
[22] x8 x9 x10 x11 x12 x13 x14 x15 x16 x17 x18 x19 x20 x21 x22 x23 x24 x25 x26 x27 x28 x29 
[3] again x28 x29 
Some(x29) Some(x28) Some(again) None true
[1] solo 
55611 666 Some(334) Some(999)
f1f0b0b1b2b3b4
"#,
    );
}

/// `Option.get_or_insert` / `get_or_insert_with` are Kāra bodies that hand
/// out a `mut ref` to the stored value: a `match self` in a `mut ref self`
/// method binds `mut ref` parts, and `*self = Some(v)` writes the receiver.
/// The value given to `get_or_insert` drops when `self` was already `Some`.
#[test]
fn get_or_insert_hands_out_a_mut_ref_to_the_stored_value() {
    assert_runs(
        r#"
fn main() {
    let mut a: Option[i64] = None;
    let x = a.get_or_insert(5);
    *x = *x + 1;
    println(f"{a.unwrap()}");
    let mut b: Option[String] = Some("keep".to_string());
    let y = b.get_or_insert("drop".to_string());
    y.push_str("!");
    println(f"{b.unwrap()}");
    let mut c: Option[Vec[i64]] = None;
    c.get_or_insert_with(|| Vec.new()).push(3);
    c.get_or_insert_with(|| Vec.new()).push(4);
    println(f"{c.unwrap().len()}");
}
"#,
        "6\nkeep!\n2\n",
    );
}

/// B-2026-10-12-9: `*` on a `Copy` part of a borrowed item reads it, as
/// `karac check` accepts: a destructured `Vec[(String, f64)]` iter item, a
/// `Set` iter item, an undestructured `SortedMap` item and a `floor` entry.
#[test]
fn deref_of_a_copy_part_of_a_borrowed_item_reads_it() {
    assert_runs(
        r#"
fn main() {
    let v: Vec[(String, f64)] = vec![("a".to_string(), 1.5), ("b".to_string(), 2.0)];
    let mut t = 0.0;
    for (n, w) in v.iter() {
        t = t + *w;
    }
    println(f"{t}");
    let mut s: Set[i64] = Set.new();
    s.insert(3);
    s.insert(4);
    let mut u = 0;
    for k in s.iter() {
        u += *k;
    }
    println(f"{u}");
    let mut m: SortedMap[i64, i64] = SortedMap.new();
    m.insert(1, 10);
    m.insert(5, 50);
    for p in m.iter() {
        println(f"{*p.0} {*p.1}");
    }
    if let Some((k, x)) = m.floor(4) {
        println(f"{*k} {*x}");
    }
}
"#,
        "3.5\n7\n1 10\n5 50\n1 10\n",
    );
}

/// B-2026-10-12-9: a bare `for` over a collection borrows it (core semantics
/// §4.6), so `*v` reads a `Copy` item, whole or destructured.
#[test]
fn deref_of_a_copy_item_of_a_bare_for_reads_it() {
    assert_runs(
        r#"
fn total(vals: Vec[i64]) -> i64 {
    let mut t = 0;
    for v in vals {
        t += *v;
    }
    t
}

fn main() {
    let mut counts: Map[char, i64] = Map.new();
    counts.insert('a', 2);
    counts.insert('b', 3);
    let mut s = 0;
    for (_, c) in counts {
        s += *c;
    }
    println(f"{s} {total(vec![1, 2, 3])}");
}
"#,
        "5 6\n",
    );
}

#[test]
fn a_mutating_method_on_a_range_view_writes_through_it() {
    // B-2026-10-12-24.
    assert_runs(
        r#"
struct S { v: Vec[i64] }

fn flip(v: mut ref Vec[i64]) {
    v[1..3].reverse();
}

fn main() {
    let mut v = vec![1, 2, 3, 4];
    v[0..2].reverse();
    flip(mut v);
    println(f"{v[0]} {v[1]} {v[2]} {v[3]}");
    v[1..4].sort();
    v[0..2].fill(9);
    v[2..4].swap(0, 1);
    println(f"{v[0]} {v[1]} {v[2]} {v[3]}");
    let mut s = S { v: vec![5, 4, 3] };
    s.v[0..2].sort_by(|a, b| a.cmp(b));
    println(f"{s.v[0]} {s.v[1]} {s.v[2]}");
}
"#,
        "2 3 1 4\n9 9 4 3\n4 5 3\n",
    );
}

#[test]
fn an_index_read_of_an_option_of_a_shared_value_retains_it() {
    // B-2026-10-12-26.
    assert_runs(
        r#"
shared struct Node {
    val: i64,
}

fn total(n: Option[Node]) -> i64 {
    if let Some(x) = n { x.val } else { 0 }
}

fn pair_total(p: (Node, i64)) -> i64 {
    p.0.val + p.1
}

fn main() {
    let options: Vec[Option[Node]] = vec![Some(Node { val: 2 }), None];
    let pairs: Vec[(Node, i64)] = vec![(Node { val: 3 }, 4)];
    let o = options[0];
    println(f"{total(o)} {total(options[1])} {total(options[0])}");
    println(f"{pair_total(pairs[0])} {pairs[0].0.val}");
}
"#,
        "2 0 2\n7 3\n",
    );
}

#[test]
fn expect_takes_a_str_message() {
    // B-2026-10-12-31: docs/library/core-types.md gives `expect(msg: Str)`;
    // a literal, a `String` and an f-string all pass as the view.
    assert_runs(
        r#"
fn main() {
    let a: Option[i64] = Some(3);
    let b: Result[i64, String] = Ok(4);
    let b2: Result[i64, String] = Ok(5);
    let name = "ctx".to_string();
    println(f"{a.expect("x")} {b.expect("y")} {a.expect(f"msg {name}")} {b2.expect(name)}");
}
"#,
        "3 4 3 5\n",
    );
    match run("fn main() {\n    let c: Option[i64] = None;\n    let n = 7;\n    let v = c.expect(f\"gone {n}\");\n    println(f\"{v}\");\n}\n") {
        Ok((out, code)) => assert!(code != 0 && out.is_empty(), "expect on None must panic: {code} {out}"),
        Err(e) => assert!(e.contains("gone 7"), "expect on None must panic with its message: {e}"),
    }
}

#[test]
fn a_floor_entry_taken_out_with_expect_reads_through_deref() {
    // B-2026-10-12-28: `floor` / `ceiling` hand out `Option[(ref K, ref V)]`
    // (docs/library/collections.md), so after `.expect()` / `.unwrap()` the
    // items read with `*`, as the `Some((k, v))` pattern forms already do.
    assert_runs(
        r#"
fn main() {
    let mut m: SortedMap[i64, i64] = SortedMap.new();
    m.insert(3, 30);
    m.insert(8, 80);
    let (k, v) = m.floor(4).expect("an entry");
    let entry = m.ceiling(4).expect("an entry");
    let w = m.floor(9).unwrap();
    let (top, _) = m.floor(i64.MAX).expect("a top");
    let top = *top;
    println(f"{*k} {*v} {*entry.0} {*entry.1} {*w.0} {top}");
}
"#,
        "3 30 8 80 8 8\n",
    );
}
