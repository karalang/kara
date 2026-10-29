//! `+` chains (B-2026-10-28-42, part 2): `mir::concat` moves a left side
//! nothing reads again into its add, clearing its drop flag when it has
//! one, and fuses `(x + a) + b` into one add that grows once. Each shape
//! below is one the rewrite must either get right or leave alone: a
//! borrow that outlives the add (a closure's capture), a call between the
//! two adds that writes what the first borrowed, a left read again later.
//! The compiled run must print what the MIR interpreter prints, and both
//! what `WANT` says (from a Python mirror of `SRC`).
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"fn bump(s: mut ref String) -> String {
    s.push_str("!");
    s.clone()
}

fn name(n: i64) -> String {
    if n == 1 {
        return "One";
    }
    if n == 2 {
        return "Two";
    }
    "Many"
}

fn group(n: i64) -> String {
    if n < 3 {
        return name(n);
    }
    let h = name(n % 3) + " Hundred";
    let r = name(n / 3);
    if r == "" {
        return h;
    }
    return h + " " + r;
}

fn words(n: i64) -> String {
    let mut out = "";
    let mut rem = n;
    let mut scale = 0;
    while rem > 0 {
        let part = rem % 10;
        if part > 0 {
            let mut piece = group(part);
            if scale > 0 {
                piece = piece + " " + name(scale);
            }
            if out == "" {
                out = piece;
            } else {
                out = piece + " " + out;
            }
        }
        rem = rem / 10;
        scale = scale + 1;
    }
    return out;
}

fn mk() -> String {
    String.from("ab") + "c"
}

fn captured() {
    let a = mk();
    let f = || println(a);
    let b = a + "yyyyyyyyyyyyyyyyyyyyyyyyyyyyyy";
    f();
    println(b);
}

fn main() {
    captured();

    let mut m = String.from("x");
    let s = m + " " + bump(mut m);
    println(f"{s} | {m}");

    let left = String.from("left");
    let joined = left + "-" + name(2) + "-" + name(7) + "-" + left;
    println(f"{joined} {left}");

    println("" + "" + "" + "|");
    println(name(1) + name(2) + name(3) + "ünï" + "ç" + name(1).clone());

    let mut acc = "";
    let mut i = 0;
    while i < 5 {
        acc = acc + "," + i.to_string() + ";";
        i = i + 1;
    }
    println(acc);

    println(words(0));
    println(words(7));
    println(words(52));
    println(words(1203));
    println(words(98765));
}
"#;

const WANT: &str = "abc\nabcyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy\nx x! | x!\nleft-Two-Many-left left\n|\nOneTwoManyünïçOne\n,0;,1;,2;,3;,4;\n\nOne Hundred Two\nTwo Hundred One One Two\nOne Many Two Two Many Hundred One\nMany Hundred Many Many Two Hundred Two Many One Hundred Two Two Many Hundred Two One Two Hundred One\n";

fn ir(src: &str) -> String {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR")
}

fn compiled(src: &str) -> Option<String> {
    let ir = ir(src);
    let dir = std::env::temp_dir().join(format!("karac-mir-concat-chains-{}", std::process::id()));
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

#[test]
fn mir_concat_chains_match_on_both_backends() {
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, WANT);
    if let Some(out) = compiled(SRC) {
        assert_eq!(out, WANT);
    }
}

/// `piece + " " + out` is one add of a moved `piece`: no `string_from`
/// copy of it, and one `push_str` per operand with the room reserved once.
#[test]
fn mir_concat_chain_reserves_once() {
    let module = ir(r#"fn words(n: i64) -> String {
    let mut out = String.from("x");
    let mut piece = n.to_string();
    if out == "" {
        out = piece;
    } else {
        out = piece + " " + out;
    }
    out
}
fn main() {
    println(words(4));
}
"#);
    let body = module
        .split("define ")
        .find(|f| f.lines().next().is_some_and(|l| l.contains("@kara.words(")))
        .expect("words");
    assert!(!body.contains("@karac_mir_string_from("), "{body}");
    assert_eq!(
        body.matches("call void @karac_mir_vec_reserve(").count(),
        1,
        "{body}"
    );
}
