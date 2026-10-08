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
