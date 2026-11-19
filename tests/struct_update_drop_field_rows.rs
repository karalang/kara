//! B-2026-11-19-7: a struct update `P { b: 3, ..x }` whose base supplies a
//! field of a type with its own `impl Drop` was refused as
//! `partial_move_of_drop_struct` "cannot move a non-`Copy` field out of
//! `D`", though `P` has no `Drop` and the field moves whole. The desugar
//! gives the base and the field read one span, so the base's recorded type
//! there was the field's.

use std::process::{Command, Output, Stdio};

fn karac(cmd: &str, tag: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-struct-update-drop-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

const D: &str = "struct D { n: i64 }\n\
                 impl Drop for D { fn drop(mut ref self) { println(f\"drop {self.n}\"); } }\n";

#[test]
fn a_struct_update_moves_a_drop_typed_field_out_of_its_base() {
    let src = format!(
        "{D}struct P {{ a: D, b: i64 }}\n\
         fn main() {{ let x = P {{ a: D {{ n: 1 }}, b: 2 }}; let y = P {{ b: 3, ..x }}; \
         println(f\"mid {{y.a.n}}\"); }}\n"
    );
    let check = text(&karac("check", "one", &src));
    assert!(check.contains("All checks passed"), "{check}");
    let run = karac("__mir-run", "one", &src);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "mid 1\ndrop 1\n",
        "{}",
        text(&run)
    );
}

#[test]
fn a_struct_update_moves_two_drop_typed_fields_and_a_string() {
    let src = format!(
        "{D}struct P {{ a: D, c: D, s: String, b: i64 }}\n\
         fn main() {{\n\
         \x20   let x = P {{ a: D {{ n: 1 }}, c: D {{ n: 2 }}, s: \"hi\".to_string(), b: 2 }};\n\
         \x20   let y = P {{ b: 3, ..x }};\n\
         \x20   println(f\"mid {{y.a.n}} {{y.c.n}} {{y.s}} {{y.b}}\");\n\
         }}\n"
    );
    let check = text(&karac("check", "two", &src));
    assert!(check.contains("All checks passed"), "{check}");
    let run = karac("__mir-run", "two", &src);
    // Struct fields drop in reverse declaration order (core-semantics §8).
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "mid 1 2 hi 3\ndrop 2\ndrop 1\n",
        "{}",
        text(&run)
    );
}

#[test]
fn a_struct_update_out_of_a_struct_with_its_own_drop_is_still_refused() {
    let src = "struct D { n: i64 }\n\
               struct P { a: D, b: i64 }\n\
               impl Drop for P { fn drop(mut ref self) { println(\"drop P\"); } }\n\
               fn main() { let x = P { a: D { n: 1 }, b: 2 }; let y = P { b: 3, ..x }; \
               println(f\"mid {y.a.n}\"); }\n";
    let check = text(&karac("check", "own-drop", src));
    assert!(
        check.contains("cannot move a non-`Copy` field out of `P`"),
        "{check}"
    );
}
