//! A match on a place reached from a `mut ref` through fields alone
//! (`match c.s` with `c: mut ref C`, `self.s` in a `mut ref self`
//! method) binds a non-`Copy` part as a `mut ref` on the MIR pipeline,
//! as `karac check` does (core semantics §4.6, B-2026-10-24-15): it can be
//! passed on as `mut buf`. Through a shared `ref` it stays a `ref`.

use karac::mir::interp::Outcome;

fn run(src: &str) -> Result<(String, i32), String> {
    let r = karac::mir::lower::run_source(src)?;
    match (&r.outcome, r.exit_code()) {
        (_, Some(code)) => Ok((r.output, code)),
        (Outcome::Error(e), None) => Err(format!("run: {e}\n--- output so far\n{}", r.output)),
        _ => Err("no exit code".into()),
    }
}

/// A variant's, an `Option`'s and a tuple's part, one and two fields
/// deep, and in a `mut ref self` method, each passed on as `mut`.
#[test]
fn a_part_reached_through_mut_refs_binds_mut_ref() {
    let src = r#"
enum S {
    B(Vec[u8]),
    N,
}
struct C {
    s: S,
    o: Option[Vec[u8]],
    t: (i64, Vec[u8]),
}
struct W {
    c: C,
}
fn add(out: mut ref Vec[u8], x: u8) { out.push(x); }
fn put(c: mut ref C) {
    match c.s {
        S.B(mut ref buf) => add(mut buf, 7),
        S.N => {}
    }
    match c.o {
        Some(buf) => add(mut buf, 8),
        None => {}
    }
    match c.t {
        (_, v) => add(mut v, 9),
    }
}
fn deep(w: mut ref W) {
    match w.c.s {
        S.B(buf) => add(mut buf, 1),
        S.N => {}
    }
}
impl C {
    fn fill(mut ref self) {
        match self.s {
            S.B(buf) => add(mut buf, 2),
            S.N => {}
        }
    }
}
fn main() {
    let mut w = W { c: C { s: S.B(Vec.new()), o: Some(Vec.new()), t: (0, Vec.new()) } };
    put(mut w.c);
    deep(mut w);
    w.c.fill();
    match w.c.s {
        S.B(buf) => println(f"{buf.len()} {buf[0]} {buf[2]}"),
        S.N => println("none"),
    }
    match w.c.o {
        Some(v) => println(f"{v.len()} {v[0]}"),
        None => println("none"),
    }
    println(w.c.t.1.len());
}
"#;
    match run(src) {
        Ok((out, 0)) if out == "3 7 2\n1 8\n1\n" => {}
        other => panic!("want exit 0 and 3 7 2, 1 8, 1; got {other:?}"),
    }
}

/// The same part reached through a shared `ref` binds a `ref`, which is
/// not passed as `mut`.
#[test]
fn a_part_reached_through_a_shared_ref_stays_ref() {
    let src = r#"
enum S { B(Vec[u8]), N }
struct C { s: S }
fn add(out: mut ref Vec[u8], x: u8) { out.push(x); }
fn put(c: ref C) {
    match c.s {
        S.B(buf) => add(mut buf, 7),
        S.N => {}
    }
}
fn main() {
    let c = C { s: S.B(Vec.new()) };
    put(c);
}
"#;
    match run(src) {
        Err(e) if e.contains("expected 'mut ref Vec[u8]', found 'ref Vec[u8]'") => {}
        other => panic!("want the refusal, got {other:?}"),
    }
}
