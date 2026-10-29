//! A `String` literal borrows its static bytes (capacity 0) wherever the
//! `String` goes, not only into a local that is merely read
//! (B-2026-10-28-42): `return "One"` costs no allocation. The bytes sit in
//! read-only memory, so the compiled run below, which sends literals out
//! through every route a `String` can take and then mutates each one every
//! way the type allows, would fault on any path that wrote a borrowed
//! literal in place instead of copying it out first. It must print what the
//! MIR interpreter prints.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"struct Named {
    label: String,
    n: i64,
}

fn word(n: i64) -> String {
    if n == 1 {
        return "One";
    }
    if n == 2 {
        return "Two";
    }
    "Many"
}

fn grow(s: mut ref String) {
    s.push_str("!");
    s.push('?');
}

fn consume(s: own String) -> String {
    let mut t = s;
    t.push_str("-taken");
    t
}

fn main() {
    let mut a = word(1);
    a.push('x');
    a.push_str("yz");
    println(a);

    let mut b = word(2);
    b.clear();
    b.push_str("fresh");
    println(b);

    let mut c = word(3);
    c.truncate(2);
    c.push('a');
    println(c);

    let mut d = word(1);
    let last = d.pop();
    d.push_str("ce");
    println(f"{d} {last}");

    let mut e = word(2);
    grow(mut e);
    println(e);
    println(consume(word(3)));

    let mut nm = Named { label: "field", n: 1 };
    nm.label.push_str("-grown");
    println(f"{nm.label} {nm.n}");

    let mut v: Vec[String] = Vec.new();
    v.push(word(1));
    v.push("lit");
    v.push(word(2));
    v[1].push_str("-elem");
    v.sort();
    println(f"{v[0]} {v[1]} {v[2]}");

    let o: Option[String] = Some("opt");
    if let Some(s) = o {
        let mut t = s;
        t.push('!');
        println(t);
    }

    let mut tp = ("left", 5);
    tp.0.push_str("+");
    println(f"{tp.0} {tp.1}");

    let mut m: Map[i64, String] = Map.new();
    m.insert(1, "mapped");
    let mut got = m.get(1).unwrap().clone();
    got.push_str("-clone");
    println(got);

    let base = word(1);
    let mut cl = base.clone();
    cl.push('c');
    println(f"{base} {cl}");

    let mut acc = word(3);
    let mut i = 0;
    while i < 5 {
        acc = acc + "-" + word(i % 3);
        i = i + 1;
    }
    println(acc);
}
"#;

const WANT: &str = "Onexyz\nfresh\nMaa\nOnce Some(e)\nTwo!?\nMany-taken\nfield-grown 1\n\
One Two lit-elem\nopt!\nleft+ 5\nmapped-clone\nOne Onec\nMany-Many-One-Two-Many-One\n";

fn ir(src: &str) -> String {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR")
}

fn compiled(src: &str) -> Option<String> {
    let ir = ir(src);
    let dir = std::env::temp_dir().join(format!(
        "karac-mir-escaping-literals-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 2).expect("object");
    common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ))?;
    let out = Command::new(&exe).output().expect("run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).unwrap())
}

/// A returned literal is a borrowed static, not a copy: `word` makes no
/// runtime call at all.
#[test]
fn mir_escaping_literal_return_allocates_nothing() {
    let module = ir(r#"fn word(n: i64) -> String {
    if n == 1 {
        return "One";
    }
    "Many"
}

fn main() {
    println(word(1));
}
"#);
    assert!(
        !module.contains("call void @karac_mir_string_from("),
        "{module}"
    );
}

#[test]
fn mir_escaping_literals_survive_every_mutation_on_both_backends() {
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, WANT);
    if let Some(out) = compiled(SRC) {
        assert_eq!(out, WANT);
    }
}
