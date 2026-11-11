//! A char's `to_string()` whose `String` is only lent and then dropped
//! writes the char's UTF-8 bytes into a slot of the frame instead of
//! allocating (B-2026-11-10-25): kata 2062's `"aeiou".contains(c.to_string())`,
//! the char-pattern fix-it's form, allocated and freed a `String` per call.
//! A `String` that is moved (returned, pushed, kept) still owns a heap
//! buffer, and so does one in a task's body, whose locals can outlive a
//! poll. The compiled program prints what the interpreter prints for
//! chars of one to four bytes.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

fn ir(src: &str) -> String {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR")
}

/// The compiled program's exit code, standard output and standard error.
fn compiled(src: &str) -> Option<(i32, String, String)> {
    let ir = ir(src);
    let dir = std::env::temp_dir().join(format!("karac-mir-char-str-{}", std::process::id()));
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

fn is_vowel(c: char) -> bool {
    "aeiou".contains(c.to_string())
}

fn width(c: char) -> i64 {
    let s = c.to_string();
    s.len()
}

fn bracket(c: char) -> String {
    let s = c.to_string();
    let mut t = String.from("<");
    t.push_str(s.as_str());
    t.push_str(">");
    t
}

fn keep(c: char) -> String {
    c.to_string()
}

fn main() {
    let text = "héllo wörld €uro 𝄞 aeiou";
    let mut vowels = 0;
    let mut bytes = 0;
    let mut all = String.new();
    let mut kept: Vec[String] = [];
    for c in text.chars() {
        if is_vowel(c) {
            vowels += 1;
        }
        bytes += width(c);
        all.push_str(bracket(c).as_str());
        if "é€𝄞ö".contains(c.to_string()) {
            kept.push(keep(c));
            kept.push(c.to_string());
        }
    }
    println(f"{vowels} vowels, {bytes} of {text.len()} bytes");
    println(all);
    println(kept.join(","));
    {
        let group = TaskGroup.new();
        group.spawn(|| {
            let mut n = 0;
            for c in "a€𝄞e".chars() {
                if "ae𝄞".contains(c.to_string()) {
                    n += 1;
                }
            }
            println(f"in a task: {n}");
        });
    }
}
"#;

const WANT: &str = "8 vowels, 31 of 31 bytes\n<h><é><l><l><o>< ><w><ö><r><l><d>< ><€><u><r><o>< ><𝄞>< ><a><e><i><o><u>\né,é,ö,ö,€,€,𝄞,𝄞\nin a task: 3\n";

#[test]
fn mir_char_to_string_only_lent_stays_in_the_frame() {
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, WANT);
    let ir = ir(SRC);
    for name in ["is_vowel", "width", "bracket"] {
        let f = body(&ir, name);
        assert!(
            f.contains("char.utf8") && !f.contains("karac_mir_fmt_end_string"),
            "{name}'s `c.to_string()` allocated:\n{f}"
        );
    }
    let keep = body(&ir, "keep");
    assert!(
        !keep.contains("char.utf8") && keep.contains("karac_mir_fmt_end_string"),
        "a returned `c.to_string()` must own a heap buffer:\n{keep}"
    );
    if let Some((code, out, err)) = compiled(SRC) {
        assert_eq!(code, 0, "{err}");
        assert_eq!(out, WANT);
    }
}
