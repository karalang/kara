//! A `let mut` declared `Fn` lent to a `mut ref Fn` parameter that the
//! callee writes through (`*a = || 9`) holds the erased type, so the callee
//! is not specialised to the initializer's closure, and the write lands
//! (B-2026-11-02-21). `karac check` refused it with E0517 ("reached
//! through a shared `ref`"), and a function item passed the same way
//! stopped the interpreter. A parameter the callee only calls keeps the
//! closure's own type, as before. Each program runs under `karac run` and,
//! with the backend built, `karac build`.

mod common;

use std::process::Command;

/// `(name, program, expected stdout)`.
const CASES: &[(&str, &str, &str)] = &[
    (
        "closure_replaced",
        "fn set(a: mut ref Fn() -> i64) {\n    *a = || 9;\n}\nfn main() {\n    let mut f: Fn() -> i64 = || 0;\n    set(mut f);\n    println(f());\n}\n",
        "9\n",
    ),
    (
        "capturing_closure_replaced",
        "fn set(a: mut ref Fn() -> i64) {\n    *a = || 9;\n}\nfn main() {\n    let k = 5;\n    let mut f: Fn() -> i64 = || k;\n    println(f());\n    set(mut f);\n    println(f());\n}\n",
        "5\n9\n",
    ),
    (
        "function_item_replaced",
        "fn set(a: mut ref Fn() -> i64) {\n    *a = || 9;\n}\nfn zero() -> i64 {\n    0\n}\nfn main() {\n    let mut f: Fn() -> i64 = zero;\n    set(mut f);\n    println(f());\n}\n",
        "9\n",
    ),
    (
        "replaced_by_borrowed_param",
        "fn set(a: mut ref Fn() -> i64, b: ref i64) {\n    *a = || *b + 1;\n}\nfn main() {\n    let k = 41;\n    let mut f: Fn() -> i64 = || 0;\n    set(mut f, k);\n    println(f());\n}\n",
        "42\n",
    ),
    (
        "forwarded_then_replaced",
        "fn set(a: mut ref Fn() -> i64) {\n    *a = || 9;\n}\nfn twice(a: mut ref Fn() -> i64) {\n    set(mut a);\n}\nfn main() {\n    let mut h: Fn() -> i64 = || 1;\n    twice(mut h);\n    println(h());\n}\n",
        "9\n",
    ),
    (
        "mut_fn_reset",
        "fn bump(c: mut ref MutFn() -> i64) -> i64 {\n    c() + c()\n}\nfn reset(c: mut ref MutFn() -> i64) {\n    *c = || 100;\n}\nfn main() {\n    let mut n = 0;\n    let mut g: MutFn() -> i64 = || {\n        n += 1;\n        n\n    };\n    println(bump(mut g));\n    reset(mut g);\n    println(g());\n}\n",
        "3\n100\n",
    ),
    (
        "only_called_keeps_closure",
        "fn call(a: mut ref Fn() -> i64) -> i64 {\n    a() + 1\n}\nfn main() {\n    let mut f: Fn() -> i64 = || 4;\n    println(call(mut f));\n}\n",
        "5\n",
    ),
];

fn write(name: &str, src: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "karac-fn-param-written-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let f = d.join("m.kara");
    std::fs::write(&f, src).unwrap();
    f
}

#[test]
fn fn_params_written_through_check_clean() {
    for (name, src, _) in CASES {
        let f = write(&format!("{name}-c"), src);
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg("check")
            .arg(&f)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{name}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn fn_params_written_through_run() {
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
fn fn_params_written_through_build() {
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
