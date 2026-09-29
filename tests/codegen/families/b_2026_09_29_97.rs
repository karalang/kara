//! B-2026-09-29-97: a struct with an i128 field as an enum payload lost its high word.

use super::*;

/// B-2026-09-29-97 — a struct with an `i128` field as an enum payload: the pack
/// kept only the field's low word (`{ lo, 3, 0 }` for `Lit { value: 10^20, k: 3 }`),
/// so the unpack read `value` from `(lo, 3)` and `k` as 0.
#[test]
fn e2e_enum_payload_struct_with_an_i128_field_keeps_both_words() {
    let Some(out) = run_program(
        r#"struct Lit {
    value: i128,
    k: i64,
}

enum E {
    Int(Lit),
    Other,
}

fn main() {
    println("start")
    let e = E.Int(Lit { value: 100000000000000000000i128, k: 3 });
    match e {
        Int(l) => println(l.value.to_string() + " " + l.k.to_string()),
        Other => println("o"),
    }
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"start
100000000000000000000 3
"#,
        "b_2026_09_29_97_sp1"
    );
}

/// B-2026-09-29-97 — the same field one level down (`Outer { lit: Lit }`), and
/// under a `shared enum`: the unpack's multi-word fallback `insertvalue`d a
/// bare `i64` into the `i128` slot and failed module verification.
#[test]
fn e2e_i128_payload_struct_through_nested_and_shared_enums() {
    let Some(out) = run_program(
        r#"struct Lit {
    value: i128,
    suffix: String,
}

struct Outer {
    lit: Lit,
    k: i64,
}

enum E {
    Int(Lit),
    Nest(Outer),
    Other,
}

shared enum Se {
    Int(Lit),
    Leaf,
}

fn show(e: E) -> String {
    match e {
        Int(l) => l.value.to_string() + " " + l.suffix,
        Nest(o) => {
            let l = o.lit;
            l.value.to_string() + " " + l.suffix + " " + o.k.to_string()
        }
        Other => "o".to_string(),
    }
}

fn shs(e: Se) -> String {
    match e {
        Int(l) => l.value.to_string() + " " + l.suffix,
        Leaf => "leaf".to_string(),
    }
}

fn main() {
    println("start")
    let big: i128 = 100000000000000000000i128;
    println(show(E.Int(Lit { value: big, suffix: "i128".to_string() })))
    println(show(E.Nest(Outer { lit: Lit { value: big + 1, suffix: "n".to_string() }, k: 7 })))
    println(show(E.Int(Lit { value: -5, suffix: "neg".to_string() })))
    println(shs(Se.Int(Lit { value: big + 2, suffix: "sh".to_string() })))
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"start
100000000000000000000 i128
100000000000000000001 n 7
-5 neg
100000000000000000002 sh
"#,
        "b_2026_09_29_97_sp"
    );
}

/// B-2026-09-29-97 — the self-hosted parser's shape: an `i128` token payload read
/// out of a `Vec` element's field and rebuilt into a `shared enum` node's struct
/// payload, plus `Option[Lit]`.
#[test]
fn e2e_i128_token_payload_rebuilt_into_a_shared_ast_node() {
    let Some(out) = run_program(
        r#"struct Sp {
    offset: i64,
    length: i64,
}

struct Lit {
    value: i128,
    suffix: String,
    span: Sp,
}

shared enum Ex {
    Int(Lit),
    Neg(Ex),
    Leaf,
}

enum Tok {
    Integer(i128, String),
    Word(String),
}

struct Spanned {
    token: Tok,
    at: i64,
}

fn render(e: Ex) -> String {
    match e {
        Int(l) => {
            let Lit { value, suffix, span } = l;
            value.to_string() + suffix + "@" + span.offset.to_string()
        }
        Neg(inner) => "-(" + render(inner) + ")",
        Leaf => "leaf".to_string(),
    }
}

fn main() {
    println("start")
    let mut toks: Vec[Spanned] = Vec.new();
    toks.push(Spanned { token: Tok.Integer(100000000000000000000i128, "i128".to_string()), at: 0 });
    toks.push(Spanned { token: Tok.Word("w".to_string()), at: 1 });
    toks.push(Spanned { token: Tok.Integer(-170141183460469231731687303715884105727i128 - 1, "".to_string()), at: 2 });
    let mut exprs: Vec[Ex] = Vec.new();
    let mut i = 0;
    while i < toks.len() {
        let e = match toks[i].token {
            Integer(v, sfx) => Ex.Int(Lit { value: v, suffix: sfx, span: Sp { offset: i as i64, length: 1 } }),
            Word(w) => Ex.Leaf,
        };
        exprs.push(e);
        i += 1;
    }
    for e in exprs {
        println(render(Ex.Neg(e)))
    }
    let o: Option[Lit] = Some(Lit { value: 18446744073709551616i128, suffix: "o".to_string(), span: Sp { offset: 9, length: 2 } });
    match o {
        Some(l) => println(l.value.to_string() + l.suffix),
        None => println("none"),
    }
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"start
-(100000000000000000000i128@0)
-(leaf)
-(-170141183460469231731687303715884105728@2)
18446744073709551616o
"#,
        "b_2026_09_29_97_sp2"
    );
}
