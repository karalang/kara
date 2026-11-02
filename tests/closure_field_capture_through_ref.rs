//! A closure that uses a borrowed binding only through its fields
//! captures those fields through the borrow (core semantics §9.1, §2.1):
//! with `p: mut ref P2`, `|| { p.a += 1; }` holds `(*p).a`, so writing
//! `p.b` while it is live is no conflict (B-2026-11-01-34). The same holds
//! for a method's `mut ref self`.

use std::path::PathBuf;
use std::process::{Command, Output};

fn program(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-closure-field-ref-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .current_dir(path.parent().unwrap())
        .env("KARAC_AUTO_PAR", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn all(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// `karac check` passes and the MIR interpreter prints `expected`; under
/// `llvm`, `karac run` prints it too.
fn checks_and_prints(tag: &str, src: &str, expected: &str) {
    let path = program(tag, src);
    let out = karac(&["check"], &path);
    assert!(out.status.success(), "{}", all(&out));
    let mut cmds = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        cmds.push("run");
    }
    for cmd in cmds {
        let out = karac(&[cmd], &path);
        assert!(out.status.success(), "{cmd}: {}", all(&out));
        assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "{cmd}");
    }
}

/// `karac check` refuses with E0516.
fn refused(tag: &str, src: &str) {
    let path = program(tag, src);
    let out = karac(&["check"], &path);
    assert!(!out.status.success(), "{}", all(&out));
    assert!(all(&out).contains("E0516"), "{}", all(&out));
}

const P2: &str = "struct P2 {\n    a: i64,\n    b: i64,\n    v: Vec[i64],\n}\n\n";

/// The expected lines are the program's arithmetic: `g` bumps `a` twice
/// (1 -> 3, pushing 2 and 3) and sets `b` to 5; `rd` is 3 * 10 + 7;
/// `sh` is 3 + 1 + 7; `h` adds 100 to `b` twice and 1000 to `a`.
#[test]
fn a_closure_captures_fields_through_a_mut_ref_param() {
    let src = format!(
        "{P2}fn g(p: mut ref P2) {{\n    let mut f = || {{\n        p.a += 1;\n        p.v.push(p.a);\n    }};\n    \
         p.b = 5;\n    f();\n    f();\n}}\n\n\
         fn rd(p: mut ref P2) -> i64 {{\n    let f = || p.a * 10;\n    p.b = 7;\n    f() + p.b\n}}\n\n\
         fn sh(p: ref P2) -> i64 {{\n    let f = || p.a + 1;\n    let x = p.b;\n    f() + x\n}}\n\n\
         fn h(p: mut ref P2) {{\n    let mut f = || {{\n        p.b += 100;\n    }};\n    f();\n    \
         p.a = p.a + 1000;\n    f();\n}}\n\n\
         fn main() {{\n    let mut p = P2 {{ a: 1, b: 2, v: Vec.new() }};\n    g(mut p);\n    \
         println(f\"{{p.a}} {{p.b}} {{p.v.len()}} {{p.v[1]}}\");\n    println(rd(mut p));\n    \
         println(sh(p));\n    h(mut p);\n    println(f\"{{p.a}} {{p.b}}\");\n}}\n"
    );
    checks_and_prints("param", &src, "3 5 2 3\n37\n11\n1003 207\n");
}

/// The expected line is the program's arithmetic: two bumps of `a` from 1,
/// and `b` set to 5.
#[test]
fn a_closure_captures_fields_through_mut_ref_self() {
    let src = "struct Q {\n    a: i64,\n    b: i64,\n}\n\n\
               impl Q {\n    fn g(mut ref self) {\n        let mut f = || {\n            self.a += 1;\n        };\n        \
               self.b = 5;\n        f();\n        f();\n    }\n}\n\n\
               fn main() {\n    let mut q = Q { a: 1, b: 2 };\n    q.g();\n    println(f\"{q.a} {q.b}\");\n}\n";
    checks_and_prints("self", src, "3 5\n");
}

/// The field the closure holds stays held: writing it is refused, whether
/// the closure writes or only reads it.
#[test]
fn a_field_a_closure_holds_through_a_mut_ref_stays_held() {
    refused(
        "same-write",
        "struct P {\n    a: i64,\n    b: i64,\n}\n\n\
         fn g(p: mut ref P) {\n    let mut f = || {\n        p.a += 1;\n    };\n    p.a = 5;\n    f();\n}\n\n\
         fn main() {\n    let mut p = P { a: 1, b: 2 };\n    g(mut p);\n    println(p.a);\n}\n",
    );
    refused(
        "same-read",
        "struct P {\n    a: i64,\n    b: i64,\n}\n\n\
         fn g(p: mut ref P) {\n    let f = || p.a;\n    p.a = 5;\n    println(f());\n}\n\n\
         fn main() {\n    let mut p = P { a: 1, b: 2 };\n    g(mut p);\n    println(p.a);\n}\n",
    );
}
