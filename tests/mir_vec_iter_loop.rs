//! `for x in v.iter()` and `for x in v.iter_mut()` over a `Vec` read `v`
//! by position, as `for x in v` does, instead of calling the library
//! `VecIter.next` once an element (B-2026-10-29-11).

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

fn built(src: &str, name: &str) -> String {
    let l = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::pretty::pretty_body(&l.program.bodies[name], &l.tys)
}

#[test]
fn a_vec_iter_loop_reads_by_position() {
    let src = r#"
fn total(v: ref Vec[i64]) -> i64 {
    let mut s = 0;
    for w in v.iter() {
        if w % 2 == 0 {
            continue;
        }
        if w > 7 {
            break;
        }
        s += w;
    }
    s
}

fn main() {
    let v: Vec[i64] = vec![1, 2, 3, 4, 5, 9, 7];
    println(total(v));
}
"#;
    assert_runs(src, "9\n");
    let body = built(src, "total");
    assert!(!body.contains("VecIter"), "{body}");
}

#[test]
fn a_vec_iter_mut_loop_writes_in_place_by_position() {
    let src = r#"
fn bump(v: mut ref Vec[i64]) {
    for w in v.iter_mut() {
        *w += 10;
    }
}

fn main() {
    let mut v: Vec[i64] = vec![1, 2, 3];
    bump(mut v);
    for x in v.iter_mut() {
        *x *= 2;
    }
    println(f"{v[0]} {v[1]} {v[2]}");
}
"#;
    assert_runs(src, "22 24 26\n");
    let body = built(src, "bump");
    assert!(!body.contains("VecIterMut"), "{body}");
}

#[test]
fn a_vec_iter_enumerate_loop_and_a_temporary_vec_read_by_position() {
    let src = r#"
struct Item { name: String, n: i64 }

fn make() -> Vec[String] {
    vec!["a".to_string(), "bb".to_string(), "ccc".to_string()]
}

fn main() {
    let items: Vec[Item] = vec![Item { name: "x".to_string(), n: 3 }, Item { name: "y".to_string(), n: 4 }];
    let mut out = String.new();
    for (i, it) in items.iter().enumerate() {
        out = out + f"{i}{it.name}{it.n} ";
    }
    for s in make().iter() {
        out = out + f"{s.len()}";
    }
    println(out);
}
"#;
    assert_runs(src, "0x3 1y4 123\n");
    let body = built(src, "main");
    assert!(!body.contains("VecIter"), "{body}");
}
