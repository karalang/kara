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
    fn get_or(self, d: T) -> T {
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
    fn err2(self) -> O2[E] {
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
