//! `Ok(Array.from_fn(n, f))` and `Some(..)` where the expected type is
//! `Result[Array[T, N], E]` / `Option[Array[T, N]]` take the length from the
//! payload's slot, as the same call does as a function's tail or at an
//! annotated `let`. It was refused E0200 "Array.from_fn needs the array's
//! length" (B-2026-11-26-83). `Vec.from_fn` takes its element type from
//! the slot the same way (`Some(Vec.from_fn(n, |i| 7))` into an
//! `Option[Vec[i32]]` was refused as `Vec[i64]`). A run-time `n` that is
//! not the length still stops the program.

mod common;

use std::process::Command;

fn karac(args: &[&str], name: &str, src: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-from-fn-payload-{name}-{}-{}",
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
/// the compiled program) prints `want` and exits `code`.
fn checks_and_runs(name: &str, src: &str, want: &str, code: i32) {
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
            && common::link_or_skip(if out.status.code() == Some(code) {
                Ok(())
            } else {
                Err(err.clone())
            })
            .is_none()
        {
            continue;
        }
        assert_eq!(out.status.code(), Some(code), "{name}: {cmd}: {err}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{name}: {cmd}\n{src}"
        );
    }
}

#[test]
fn from_fn_payload_slot_gives_ok_and_some_their_length() {
    checks_and_runs(
        "ok_some",
        "fn f(n: i64) -> Result[Array[String, 3], String] { Ok(Array.from_fn(n, |i| f\"s{i}\")) }\nfn g(n: i64) -> Option[Array[i64, 2]] { Some(Array.from_fn(n, |i| i)) }\nfn r(n: i64) -> Result[Array[i64, 3], String] {\n    if n != 3 { return Err(\"bad\"); }\n    return Ok(Array.from_fn(n, |i| i + 1));\n}\nfn main() {\n    match f(3) { Ok(a) => println(a[2]), Err(e) => println(e) }\n    match g(2) { Some(a) => println(a[1]), None => println(\"none\") }\n    println(r(3).unwrap()[2]);\n    match r(4) { Ok(_) => println(\"?\"), Err(e) => println(e) }\n    let x: Option[Array[i64, 2]] = Some(Array.from_fn(2, |i| i * 10));\n    println(x.unwrap()[1]);\n}\n",
        "s2\n1\n3\nbad\n10\n",
        0,
    );
}

#[test]
fn from_fn_payload_slot_gives_vec_from_fn_its_element() {
    checks_and_runs(
        "vec_elem",
        "fn h(n: i64) -> Option[Vec[i32]] { Some(Vec.from_fn(n, |i| 7)) }\nfn main() { let v = h(2).unwrap(); let w: i32 = v[1]; println(w); println(v.len()); }\n",
        "7\n2\n",
        0,
    );
}

#[test]
fn from_fn_payload_slot_still_checks_the_length_at_run_time() {
    checks_and_runs(
        "wrong_len",
        "fn wrong(n: i64) -> Option[Array[i64, 2]] { Some(Array.from_fn(n, |i| i)) }\nfn main() { println(1); println(wrong(3).unwrap()[0]); }\n",
        "1\n",
        101,
    );
}
