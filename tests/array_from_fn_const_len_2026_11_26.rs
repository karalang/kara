//! `Array.from_fn(n, f)` builds an array whose length is a `const`
//! parameter (`let out: Array[T, N] = Array.from_fn(N, f)` inside `impl[T,
//! const N: i64]`), which the instance fixes. It was refused "Array.from_fn
//! needs the array's length" there, which kept a generic array impl from
//! building its own type; the library's `Deserialize for Array[T, N]`
//! needs it (B-2026-11-26-69).

mod common;

use std::process::Command;

fn karac(args: &[&str], name: &str, src: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-from-fn-const-{name}-{}-{}",
        args[0].trim_start_matches('_'),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// `karac check` accepts `src`, and the interpreter (and, with the backend,
/// the compiled program) prints `want`.
fn accepted_and_prints(name: &str, src: &str, want: &str) {
    let check = karac(&["check"], name, src);
    assert!(
        check.status.success(),
        "{name}: check: {}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for cmd in backends {
        let out = karac(&[cmd], name, src);
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        if *cmd != "__mir-run"
            && common::link_or_skip(if out.status.success() {
                Ok(())
            } else {
                Err(err.clone())
            })
            .is_none()
        {
            continue;
        }
        assert!(out.status.success(), "{name}: {cmd}: {err}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{name}: {cmd}\n{src}"
        );
    }
}

#[test]
fn array_from_fn_at_a_const_parameter_length() {
    accepted_and_prints(
        "const_len",
        "trait Fill { fn fill() -> Self; }\nimpl[const N: i64] Fill for Array[String, N] {\n    fn fill() -> Array[String, N] {\n        let out: Array[String, N] = Array.from_fn(N, |i| f\"s{i}\");\n        out\n    }\n}\nfn make[T: Fill]() -> T { T.fill() }\nfn main() { let a: Array[String, 3] = make(); println(f\"{a[0]} {a[2]}\"); }\n",
        "s0 s2\n",
    );
}

#[test]
fn array_from_fn_const_length_moves_vec_elements_in() {
    accepted_and_prints(
        "moved",
        "fn take[const N: i64](v: own Vec[String]) -> Array[String, N] {\n    let mut it = v.into_iter();\n    let out: Array[String, N] = Array.from_fn(N, |_| it.next().unwrap());\n    out\n}\nfn main() { let a: Array[String, 2] = take([\"p\".to_string(), \"q\".to_string()]); println(f\"{a[0]}{a[1]}\"); }\n",
        "pq\n",
    );
}
