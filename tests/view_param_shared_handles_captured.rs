//! B-2026-11-18-75: since B-2026-11-17-92 a closure capturing a `Vec` of
//! `shared` handles, built from a parameter that holds views
//! (`xs: own Vec[(Str, P)]` mapped to `Vec[(String, P)]`), and stored
//! through an `escaping` parameter was refused with E0509 ("a view the
//! parameter `xs` holds is stored in an escaping closure"), when `P` has a
//! function-typed field. The call rule counted the function field inside
//! each `shared` handle as a part that could carry the view, but no view
//! may be stored in a `shared` value (core semantics §5.7), so a handle
//! carries none. Found by apps/cli/mini_parsy (its `Parser` handles).

use std::process::Command;

const SRC: &str = r#"shared struct P { run: Fn(i64) -> i64 }
fn new_p(run: escaping Fn(i64) -> i64) -> P { P { run: run } }
fn seq(xs: own Vec[(Str, P)]) -> P {
    let named: Vec[(String, P)] = xs.into_iter().map(|(n, p)| (n.to_string(), p)).collect();
    new_p(|i| {
        let mut t = i;
        for (n, p) in named.iter() { t += n.len() + (p.run)(i); }
        t
    })
}
fn main() {
    let a: Str = "ab";
    let mut v: Vec[(Str, P)] = Vec.new();
    v.push((a, new_p(|i| i * 2)));
    let q = seq(v);
    println(f"{(q.run)(3)}");
}"#;

fn run(backend: &str) -> String {
    let path =
        std::env::temp_dir().join(format!("karac-b75-{backend}-{}.kara", std::process::id()));
    std::fs::write(&path, SRC).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(backend)
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "{backend}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn handles_built_from_a_view_param_are_captured_by_an_escaping_closure() {
    assert_eq!(run("__mir-run"), "11\n");
    #[cfg(feature = "llvm")]
    assert_eq!(run("__mir-llvm-run"), "11\n");
}
