//! A `substring` or `s[a..b]` whose `String` is only lent and then dropped
//! points at the bytes of the text it was cut from, with no capacity, rather
//! than copy them into an allocation (B-2026-11-21-7): kata 2255's
//! `w.push_str("abcde".substring(k, k + 1))` allocated and freed a
//! one-char `String` per call. The text must stay as it is while the cut is
//! live, so a cut of a text that is written, or that is reached through a
//! reference this body holds, still copies. The compiled program prints
//! what the interpreter prints, a panic on a cut inside a char included.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

fn ir(src: &str) -> String {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR")
}

/// The compiled program's exit code, standard output and standard error.
fn compiled(src: &str, tag: &str) -> Option<(i32, String, String)> {
    let ir = ir(src);
    let dir =
        std::env::temp_dir().join(format!("karac-mir-cut-views-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 2).expect("object");
    common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ))?;
    let out = Command::new(&exe).output().expect("run");
    let _ = std::fs::remove_dir_all(&dir);
    Some((
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    ))
}

/// The body of the function `name` in `ir`.
fn body<'a>(ir: &'a str, name: &str) -> &'a str {
    let at = ir
        .match_indices(&format!("@kara.{name}("))
        .map(|(i, _)| i)
        .find(|&i| {
            ir[..i]
                .rsplit('\n')
                .next()
                .is_some_and(|l| l.starts_with("define"))
        })
        .unwrap_or_else(|| panic!("no definition of {name}"));
    let end = ir[at..].find("\n}\n").map_or(ir.len(), |e| at + e);
    &ir[at..end]
}

const SRC: &str = r#"import std.sync.TaskGroup;

fn from_const(n: i64) -> String {
    let mut w = String.new();
    let mut k = 0;
    while k < n {
        w.push_str("abcde".substring(k % 5, k % 5 + 1));
        k = k + 1;
    }
    w
}

fn from_arg(src: String) -> String {
    let mut w = String.new();
    w.push_str(src.substring(1, 3));
    w.push_str(src[0..1]);
    w
}

fn from_local() -> String {
    let s = "héllo".to_string();
    let mut w = String.new();
    w.push_str(s.substring(0, 3));
    w.push_str(s.substring(-4, 99));
    w.push_str(s.substring(5, 3));
    w
}

fn read_mut_arg(s: mut ref String) -> i64 {
    let mut w = String.new();
    w.push_str(s.substring(0, 2));
    w.len()
}

fn mutated() -> String {
    let mut s = "abc".to_string();
    let t = s.substring(0, 2);
    s.push_str("defghijklmnopqrstuvwxyz0123456789");
    let mut w = String.new();
    w.push_str(t);
    w
}

fn itself() -> String {
    let mut w = "xy".to_string();
    w.push_str(w.substring(0, 1));
    w
}

fn written_mut_arg(s: mut ref String) -> String {
    let t = s.substring(0, 1);
    s.push_str("0123456789abcdefghijklmnopqrstuvwxyz");
    let mut w = String.new();
    w.push_str(t);
    w
}

fn keep(s: String) -> String {
    s.substring(0, 2)
}

fn main() {
    println(from_const(7));
    println(from_arg("pqrs"));
    println(from_local());
    let mut m = "mn".to_string();
    println(read_mut_arg(mut m));
    println(mutated());
    println(itself());
    println(written_mut_arg(mut m));
    println(keep("kept"));
    {
        let group = TaskGroup.new();
        group.spawn(|| {
            let mut w = String.new();
            w.push_str("task".substring(1, 3));
            println(f"in a task: {w}");
        });
    }
}
"#;

const WANT: &str = "abcdeab\nqrp\nhéhéllo\n2\nab\nxyx\nm\nke\nin a task: as\n";

#[test]
fn mir_cut_only_lent_views_its_text() {
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, WANT);
    let ir = ir(SRC);
    for name in ["from_const", "from_arg", "from_local", "read_mut_arg"] {
        let f = body(&ir, name);
        assert!(
            f.contains("karac_mir_str_view") && !f.contains("karac_mir_str_slice"),
            "{name}'s cut allocated:\n{f}"
        );
    }
    for name in ["mutated", "itself", "written_mut_arg", "keep"] {
        let f = body(&ir, name);
        assert!(
            f.contains("karac_mir_str_slice"),
            "{name}'s cut must own a copy: its text changes, or it leaves:\n{f}"
        );
    }
    if let Some((code, out, err)) = compiled(SRC, "lent") {
        assert_eq!(code, 0, "{err}");
        assert_eq!(out, WANT);
    }
}

#[test]
fn mir_cut_view_inside_a_char_panics_as_the_copy_does() {
    let src = r#"fn cut(s: String, a: i64, b: i64) -> i64 {
    let mut w = String.new();
    w.push_str(s.substring(a, b));
    w.len()
}

fn main() {
    println(cut("héllo", 0, 99));
    println(cut("héllo", 3, 1));
    println(cut("héllo", 0, 2));
}
"#;
    assert!(body(&ir(src), "cut").contains("karac_mir_str_view"));
    if let Some((code, out, err)) = compiled(src, "panic") {
        assert_eq!((code, out.as_str()), (101, "6\n0\n"), "{err}");
        assert!(err.contains("byte index 2 is not a char boundary"), "{err}");
    }
}
