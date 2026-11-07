//! A part of a `shared` value held in a local (`m.a` with `m` a handle) is
//! a `ref` scrutinee (core semantics §4.6): a plain binding of a non-`Copy`
//! part is a `ref` into it, and only moving that binding is the §3.7 error.
//! The MIR builder treated the place as owned, so every `match`, `if let`,
//! `while let`, `let`-`else` or `let` destructuring of such a field was
//! refused as a move out of a `shared` value, by `karac check` and every
//! backend alike; a borrowed parameter's field (reached through a `ref`)
//! already worked (B-2026-11-07-12).

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[test]
fn a_local_handles_field_binds_refs_in_every_pattern_form() {
    let src = "shared struct S { mut a: Option[String], mut b: String, pair: (String, i64) }

fn close(cur: own Option[S], out: mut ref Vec[S]) {
    let Some(m) = cur else {
        return;
    };
    match m.a {
        Some(i) => {
            m.b = i.clone();
            out.push(m);
        }
        None => println(\"none\"),
    }
}

fn main() {
    let m = S { a: Some(\"x\".to_string()), b: String.new(), pair: (\"p\".to_string(), 4) };
    match m.a {
        Some(i) if i.len() > 0 => println(f\"guard {i}\"),
        _ => {}
    }
    if let Some(i) = m.a {
        println(f\"if let {i}\");
    }
    let Some(j) = m.a else {
        return;
    };
    println(f\"let else {j}\");
    match m.a {
        Some(mut ref i) => i.push_str(\"y\"),
        None => {}
    }
    let (s, k) = m.pair;
    println(f\"{s} {k}\");
    while let Some(i) = m.a {
        println(f\"while {i}\");
        m.a = None;
    }
    let mut out: Vec[S] = Vec.new();
    close(Some(S { a: Some(\"z\".to_string()), b: String.new(), pair: (\"q\".to_string(), 1) }), mut out);
    println(f\"{out.len()} {out[0].b}\");
}
";
    assert_eq!(
        run(src),
        "guard x\nif let x\nlet else x\np 4\nwhile xy\n1 z\n"
    );
}

#[test]
fn moving_a_binding_of_a_local_handles_field_is_still_refused() {
    let src = "shared struct S { a: Option[String] }

fn main() {
    let m = S { a: Some(\"x\".to_string()) };
    let mut v: Vec[String] = Vec.new();
    match m.a {
        Some(i) => v.push(i),
        None => {}
    }
    println(v.len());
}
";
    let Err(err) = karac::mir::lower::run_source(src) else {
        panic!("moving a ref binding is refused");
    };
    assert!(err.contains("7:27"), "{err}");
}
