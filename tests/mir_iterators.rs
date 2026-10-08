//! Iterators on the MIR pipeline (redesign A2): `for` over a type that
//! implements `Iterator`, and trait default methods built on `next`.

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

/// `for x in it` over a program's own `Iterator` calls its `next` until
/// `None`, directly and through a bound `I: Iterator[Item = i64]`.
#[test]
fn a_for_loop_calls_a_programs_own_next() {
    let src = r#"
struct Counter { n: i64, stop: i64 }
impl Iterator for Counter {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        if self.n >= self.stop { return None; }
        self.n = self.n + 1;
        Some(self.n)
    }
}
fn total[I: Iterator[Item = i64]](it: I) -> i64 {
    let mut t = 0;
    for x in it {
        t = t + x;
    }
    t
}
fn main() {
    let c = Counter { n: 0, stop: 4 };
    for x in c {
        println(f"{x}");
    }
    println(f"{total(Counter { n: 0, stop: 3 })}");
}
"#;
    assert_runs(src, "1\n2\n3\n4\n6\n");
}

/// Under `I: Iterator[Item = i64]`, `inner.next()` is an `Option[i64]`,
/// so its payload can be doubled.
#[test]
fn an_item_binding_on_a_bound_fixes_next_s_type() {
    let src = r#"
struct Counter { n: i64, stop: i64 }
impl Iterator for Counter {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        if self.n >= self.stop { return None; }
        self.n = self.n + 1;
        Some(self.n)
    }
}
struct Doubled[I] { inner: I }
impl[I: Iterator[Item = i64]] Iterator for Doubled[I] {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        match self.inner.next() {
            Some(x) => Some(x * 2),
            None => None,
        }
    }
}
fn main() {
    let mut c = Counter { n: 0, stop: 3 };
    let mut d = Doubled { inner: c };
    let mut t = 0;
    loop {
        match d.next() {
            Some(x) => { t = t + x; }
            None => break,
        }
    }
    println(f"{t}");
}
"#;
    assert_runs(src, "12\n");
}

/// A trait default method matching on `self.next()`. Checking it used to
/// loop forever: the scrutinee's `Option[Self.Item]` was freshened over and
/// over through its projection.
#[test]
fn a_trait_default_method_calls_the_implementors_next() {
    let src = r#"
trait It2 {
    type Item;
    fn nxt(mut ref self) -> Option[Self.Item];
    fn cnt(mut ref self) -> i64 {
        let mut n = 0;
        loop {
            match self.nxt() {
                Some(_) => { n = n + 1; }
                None => break,
            }
        }
        n
    }
}
struct Counter { n: i64, stop: i64 }
impl It2 for Counter {
    type Item = i64;
    fn nxt(mut ref self) -> Option[i64] {
        if self.n >= self.stop { return None; }
        self.n = self.n + 1;
        Some(self.n)
    }
}
fn main() {
    let mut c = Counter { n: 0, stop: 4 };
    println(f"{c.cnt()}");
}
"#;
    assert_runs(src, "4\n");
}

/// A trait default method with a parameter of its own, called on the
/// implementing type.
#[test]
fn a_trait_default_method_takes_a_plain_parameter() {
    let src = r#"
trait It2 {
    type Item;
    fn nxt(mut ref self) -> Option[Self.Item];
    fn skipn(mut ref self, k: i64) -> i64 {
        let mut n = 0;
        while n < k {
            match self.nxt() {
                Some(_) => { n = n + 1; }
                None => break,
            }
        }
        n
    }
}
struct Counter { n: i64, stop: i64 }
impl It2 for Counter {
    type Item = i64;
    fn nxt(mut ref self) -> Option[i64] {
        if self.n >= self.stop { return None; }
        self.n = self.n + 1;
        Some(self.n)
    }
}
fn main() {
    let mut c = Counter { n: 0, stop: 4 };
    println(f"{c.skipn(2)}");
}
"#;
    assert_runs(src, "2\n");
}
