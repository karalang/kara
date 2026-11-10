//! A closure with no declared return type whose `return` carries an integer
//! of a non-default width and whose tail (or another `return`) is an
//! unsuffixed literal: the literal takes the closure's integer type rather
//! than defaulting to `i64` and conflicting (B-2026-11-10-23). `karac fix`
//! produces exactly this shape when it deletes Rust's `|..| -> i32`, so the
//! fix used to leave a program the checker refused (kata 1093).

fn fixed(src: &str) -> String {
    let result = karac::parse(src);
    let mut edits: Vec<_> = result.fix_edits.values().cloned().collect();
    edits.extend(result.fix_diffs.values().flatten().cloned());
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut out = src.to_string();
    for e in edits {
        out.replace_range(e.offset..e.offset + e.length, &e.replacement);
    }
    out
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .unwrap_or_else(|e| panic!("build: {e}\n{src}"))
        .output
}

fn refused(src: &str) -> String {
    match karac::mir::lower::run_source(src) {
        Ok(r) => panic!("accepted, printed {:?}:\n{src}", r.output),
        Err(e) => e.to_string(),
    }
}

#[test]
fn a_literal_tail_takes_the_typed_returns_width() {
    let src = "fn main() {\n\
               \x20   let f = |k: i64| {\n\
               \x20       if k > 3 {\n\
               \x20           return k as i32;\n\
               \x20       }\n\
               \x20       0\n\
               \x20   };\n\
               \x20   let x: i32 = f(7);\n\
               \x20   let y: i32 = f(1);\n\
               \x20   println(f\"{x} {y}\");\n}\n";
    assert_eq!(run(src), "7 0\n");
}

#[test]
fn a_literal_return_takes_the_typed_tails_width() {
    let src = "fn main() {\n\
               \x20   let b = |k: i64| {\n\
               \x20       if k > 5 {\n\
               \x20           return k as u16;\n\
               \x20       }\n\
               \x20       if k > 2 {\n\
               \x20           return 7 + 1;\n\
               \x20       }\n\
               \x20       k as u16 * 2\n\
               \x20   };\n\
               \x20   let d = |k: i64| {\n\
               \x20       if k > 0 {\n\
               \x20           return 1;\n\
               \x20       }\n\
               \x20       k as i8\n\
               \x20   };\n\
               \x20   let x: u16 = b(9) + b(3) + b(1);\n\
               \x20   let y: i8 = d(5) + d(-2);\n\
               \x20   println(f\"{x} {y}\");\n}\n";
    assert_eq!(run(src), "19 -1\n");
}

#[test]
fn an_all_literal_closure_still_defaults_to_i64() {
    let src = "fn main() {\n\
               \x20   let a = |k: i64| {\n\
               \x20       if k > 0 {\n\
               \x20           return 2 * 3;\n\
               \x20       }\n\
               \x20       -1\n\
               \x20   };\n\
               \x20   let z: i64 = a(1) + a(0);\n\
               \x20   println(f\"{z}\");\n}\n";
    assert_eq!(run(src), "5\n");
}

#[test]
fn a_literal_out_of_the_closures_range_is_refused() {
    let src = "fn main() {\n\
               \x20   let f = |k: i64| {\n\
               \x20       if k > 0 {\n\
               \x20           return k as u8;\n\
               \x20       }\n\
               \x20       300\n\
               \x20   };\n\
               \x20   println(f\"{f(1)}\");\n}\n";
    let e = refused(src);
    assert!(e.contains("out of range for 'u8'"), "{e}");
}

#[test]
fn a_literal_against_a_non_integer_return_still_conflicts() {
    let src = "fn main() {\n\
               \x20   let f = |k: i64| {\n\
               \x20       if k > 0 {\n\
               \x20           return \"x\".to_string();\n\
               \x20       }\n\
               \x20       0\n\
               \x20   };\n\
               \x20   println(f(1));\n}\n";
    let e = refused(src);
    assert!(e.contains("closure returns conflicting types"), "{e}");
}

#[test]
fn deleting_rusts_return_type_leaves_a_program_that_runs() {
    let src = "fn main() {\n\
               \x20   let v: Vec[i64] = [4, 9, 2].to_vec();\n\
               \x20   let find = |t: i64| -> i32 {\n\
               \x20       for i in 0..v.len() {\n\
               \x20           if v[i] == t {\n\
               \x20               return i as i32;\n\
               \x20           }\n\
               \x20       }\n\
               \x20       -1\n\
               \x20   };\n\
               \x20   let a: i32 = find(9);\n\
               \x20   let b: i32 = find(5);\n\
               \x20   println(f\"{a} {b}\");\n}\n";
    let out = fixed(src);
    assert!(out.contains("let find = |t: i64| {"), "{out}");
    assert_eq!(run(&out), "1 -1\n");
}
