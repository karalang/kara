//! A `let mut` with a declared `Fn` type that another function value may
//! replace (`mem.replace`, `mem.swap`, an assignment) holds that erased
//! type, not its initializer's closure type (B-2026-11-02-16), and
//! `Option[Fn].replace(|| ..)` erases the closure it keeps; swapping two
//! `Fn` values moves what each holds, not the borrow of the other
//! (B-2026-11-02-19). One that is
//! only called, or lent to a function-typed parameter, keeps the closure's
//! type, so what it captured mutably can be read once it is last used.
//! Each program runs under `karac run` and, with the backend built,
//! `karac build`; both print the program's arithmetic.

mod common;

use std::process::Command;

/// `(name, program, expected stdout)`.
const CASES: &[(&str, &str, &str)] = &[
    (
        "replace_by_closure",
        "fn main() {\n    let mut f: Fn() -> i64 = || 1;\n    let old = mem.replace(mut f, || 2);\n    println(f() + old());\n}\n",
        "3\n",
    ),
    (
        "replace_by_call",
        "fn two() -> Fn() -> i64 {\n    || 2\n}\nfn main() {\n    let mut f: Fn() -> i64 = || 1;\n    let old = mem.replace(mut f, two());\n    println(f() * 10 + old());\n}\n",
        "21\n",
    ),
    (
        "reassign",
        "fn main() {\n    let base = 40;\n    let mut f: Fn() -> i64 = || 1;\n    println(f());\n    f = || base + 2;\n    println(f());\n}\n",
        "1\n42\n",
    ),
    (
        "cross_task",
        "fn main() {\n    let mut f: Fn() -> i64 + CrossTask = || 1;\n    let old = mem.replace(mut f, || 2);\n    println(f() + old());\n}\n",
        "3\n",
    ),
    (
        "function_items",
        "fn inc(x: i64) -> i64 {\n    x + 1\n}\nfn dbl(x: i64) -> i64 {\n    x * 2\n}\nfn main() {\n    let mut f: Fn(i64) -> i64 = inc;\n    println(f(5));\n    f = dbl;\n    println(f(5));\n    let mut g: Fn(i64) -> i64 = |x| x - 1;\n    let old = mem.replace(mut g, inc);\n    println(g(1) * 10 + old(1));\n}\n",
        "6\n10\n20\n",
    ),
    (
        "swap",
        "fn main() {\n    let mut f: Fn() -> i64 = || 1;\n    let mut g: Fn() -> i64 = || 2;\n    mem.swap(mut f, mut g);\n    println(f() * 10 + g());\n}\n",
        "21\n",
    ),
    (
        "swap_with_call",
        "fn mk() -> Fn() -> i64 {\n    || 7\n}\nfn main() {\n    let mut f: Fn() -> i64 = mk();\n    let mut g: Fn() -> i64 = || 0;\n    mem.swap(mut f, mut g);\n    println(f() * 10 + g());\n}\n",
        "7\n",
    ),
    (
        "option_replace",
        "fn main() {\n    let k = 4;\n    let mut o: Option[Fn() -> i64] = None;\n    let _ = o.replace(|| k + 1);\n    let f = o.unwrap();\n    let s: Option[Fn() -> i64] = Some(|| 1);\n    println(f() * 10 + s.unwrap()());\n}\n",
        "51\n",
    ),
    (
        "reassign_in_branch",
        "fn main() {\n    let mut f: Fn(i64) -> i64 = |x: i64| x + 1;\n    if f(1) == 2 {\n        f = |x: i64| x * 3;\n    }\n    println(f(5));\n}\n",
        "15\n",
    ),
    (
        "kept_mut_borrow",
        "fn twice(f: mut ref MutFn()) {\n    f();\n    f();\n}\nfn main() {\n    let mut calls = 0;\n    let mut h: MutFn() = || { calls += 1; };\n    twice(mut h);\n    h();\n    println(calls);\n}\n",
        "3\n",
    ),
    (
        "kept",
        "fn main() {\n    let f: Fn() -> i64 = || 5;\n    let g = || 6;\n    println(f() + g());\n}\n",
        "11\n",
    ),
];

fn write(name: &str, src: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("karac-declared-fn-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let f = d.join("m.kara");
    std::fs::write(&f, src).unwrap();
    f
}

#[test]
fn declared_fn_locals_run() {
    for (name, src, want) in CASES {
        let f = write(name, src);
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg("run")
            .arg(&f)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            *want,
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[cfg(feature = "llvm")]
#[test]
fn declared_fn_locals_build() {
    for (name, src, want) in CASES {
        let f = write(&format!("{name}-b"), src);
        let exe = f.with_extension("bin");
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg("build")
            .arg(&f)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        let why = String::from_utf8_lossy(&out.stderr).to_string();
        if common::link_or_skip(if out.status.success() {
            Ok(())
        } else {
            Err(format!("{name}: {why}"))
        })
        .is_none()
        {
            return;
        }
        let run = Command::new(&exe).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&run.stdout), *want, "{name}");
    }
}
