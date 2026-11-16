//! Typecheck rows from the probe's format fuzzer (probe-core/fmx), each
//! program run on the interpreter and, under `--features llvm`, compiled.
//!
//! - FX1: a generic struct literal holding `None` inside `Some`
//!   (`Some(G { v: None })` at `Option[G[Option[i64]]]`) overflowed the
//!   checker's stack: the `T` of `None`'s `Option[T]` was taken for `G`'s
//!   own `T`, binding `?T` to `Option[?T]`.
//! - FX5: an empty `[]` one level inside a tuple inside `Some` / `Ok`, or in
//!   a nested tuple, was refused as "a type that failed to check".
//! - FX3: `.clone()` on `Result[H, String]` with a `#[derive(Clone)]` `H`
//!   was "no method clone".

mod common;

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-fx-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    file
}

/// `src` checks clean and prints `want` on the interpreter and, with the
/// backend built, compiled.
fn runs_everywhere(name: &str, src: &str, want: &str) {
    let file = program(name, src);
    let check = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{name}: check: {}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let mut cmds = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        cmds.push("__mir-llvm-run");
    }
    for cmd in cmds {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg(cmd)
            .arg(&file)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        if cmd == "__mir-llvm-run"
            && stderr.contains("undefined reference")
            && common::link_or_skip(Err(stderr.clone())).is_none()
        {
            continue;
        }
        assert!(out.status.success(), "{name}: {cmd}: {stderr}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name}: {cmd}");
    }
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn fx1_none_in_a_generic_struct_inside_some_checks() {
    runs_everywhere(
        "fx1",
        r#"struct G[T] { v: T }
struct P[T] { a: T, b: Option[T] }
fn main() {
    let s: Option[G[Option[i64]]] = Some(G { v: None });
    match s { Some(g) => println(g.v.is_none()), None => println("none") }
    let u: Option[G[Option[String]]] = Some(G { v: None });
    println(u.is_some());
    let p: P[Option[i64]] = P { a: None, b: None };
    println(p.a.is_none());
    let q: Option[P[Option[i64]]] = Some(P { a: Some(1), b: None });
    println(q.unwrap().a.unwrap());
}
"#,
        "true\ntrue\ntrue\n1\n",
    );
}

#[test]
fn fx5_empty_list_under_tuples_and_constructors_takes_its_slot() {
    runs_everywhere(
        "fx5",
        r#"fn main() {
    let a: Option[(Vec[i64], i32)] = Some(([], 0i32));
    println(a.unwrap().0.len());
    let b: Option[Result[Vec[i64], String]] = Some(Ok([]));
    println(b.unwrap().unwrap().len());
    let c: ((i8, Vec[i64]), u8) = ((1i8, []), 2u8);
    println(f"{c.0.0} {c.0.1.len()} {c.1}");
    let mut d: Result[(Vec[String], i64), String] = Ok(([], 4));
    match d { Ok(mut ref t) => t.0.push("x".to_string()), Err(_) => {} }
    println(d.unwrap().0.len());
}
"#,
        "0\n0\n1 0 2\n1\n",
    );
}

#[test]
fn fx3_result_with_a_derived_clone_half_clones_deeply() {
    runs_everywhere(
        "fx3",
        r#"#[derive(Clone)]
struct H { n: i64, s: String, v: Vec[String] }
fn make(ok: bool) -> Result[H, String] {
    if ok { Ok(H { n: 1, s: "hello".to_string(), v: ["a".to_string(), "b".to_string()] }) } else { Err("bad".to_string()) }
}
fn main() {
    let r = make(true);
    let mut c = r.clone();
    match c { Ok(mut ref h) => { h.s.push_str("!"); h.v.push("c".to_string()); }, Err(_) => {} }
    match r { Ok(h) => println(f"{h.n} {h.s} {h.v.len()}"), Err(e) => println(e) }
    match c { Ok(h) => println(f"{h.n} {h.s} {h.v.len()}"), Err(e) => println(e) }
    let e = make(false);
    let e2 = e.clone();
    println(e2.is_err());
    let xs: Vec[Result[H, String]] = [make(true), make(false)];
    let y = xs[0].clone();
    println(y.is_ok());
    let o: Result[Option[String], i64] = Ok(Some("z".to_string()));
    let mut o2 = o.clone();
    match o2 { Ok(Some(mut ref s)) => s.push_str("!"), _ => {} }
    println(f"{o.unwrap().unwrap()} {o2.unwrap().unwrap()}");
    let mut m: Map[i64, i64] = Map.new();
    m[1] = 2;
    let rm: Result[Map[i64, i64], i64] = Ok(m);
    let mut rm2 = rm.clone();
    match rm2 { Ok(mut ref mm) => mm[3] = 4, Err(_) => {} }
    println(f"{rm.unwrap().len()} {rm2.unwrap().len()}");
}
"#,
        "1 hello 2\n1 hello! 3\ntrue\ntrue\nz z!\n1 2\n",
    );
}
