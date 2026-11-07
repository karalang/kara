//! A spawned closure whose `Result` error type only a later use fixes
//! (`group.spawn(|| { let h = half(x)?; Ok(h + 1) })`, then
//! `let r: Result[i64, String] = h.join();`) opens that parameter as a
//! variable the later use solves, as a `let` does (B-2026-11-06-11). It was
//! left the bare parameter `E` in the handle's type, so the program checked
//! (bare `let h`) or not (handles in a `Vec`) and every lane refused it at
//! lowering (B-2026-11-06-38). Nothing fixing it is still an error.

use std::process::Command;

const HALF: &str = "fn half(n: i64) -> Result[i64, String] {
    if n % 2 == 0 { Ok(n / 2) } else { Err(\"odd\".to_string()) }
}
";

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-spawn-e-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_spawned_closures_error_type_is_fixed_by_the_join_annotation() {
    for (name, body, want) in [
        (
            "let_handle",
            "fn main() {
    let group = TaskGroup.new();
    let h = group.spawn(|| {
        let v = half(4)?;
        Ok(v + 1)
    });
    let r: Result[i64, String] = h.join();
    match r {
        Ok(v) => println(f\"ok {v}\"),
        Err(e) => println(f\"err {e}\"),
    }
}
",
            "ok 3\n",
        ),
        (
            "handles_in_vec",
            "fn total(xs: Slice[i64]) -> Result[i64, String] {
    let group = TaskGroup.new();
    let mut handles = Vec.new();
    for x in xs.iter() {
        let x = *x;
        handles.push(group.spawn(|| {
            let mut got: Vec[i64] = Vec.new();
            for i in 0..x {
                got.push(half(i * 2)?);
            }
            Ok(got)
        }));
    }
    let mut sum = 0;
    for h in handles.into_iter() {
        let r: Result[Vec[i64], String] = h.join();
        for g in r?.into_iter() {
            sum += g;
        }
    }
    Ok(sum)
}

fn main() {
    match total([2, 3][..]) {
        Ok(v) => println(f\"ok {v}\"),
        Err(e) => println(f\"err {e}\"),
    }
}
",
            "ok 4\n",
        ),
    ] {
        let src = format!("{HALF}\n{body}");
        let (ok, err) = check(name, &src);
        assert!(ok, "{name}: {err}");
        assert_eq!(
            karac::mir::lower::run_source(&src)
                .expect("MIR interpreter")
                .output,
            want,
            "{name}"
        );
    }
}

#[test]
fn a_spawned_closures_error_type_nothing_fixes_is_still_refused() {
    let (ok, err) = check(
        "unfixed",
        "fn main() {
    let group = TaskGroup.new();
    let mut hs = Vec.new();
    hs.push(group.spawn(|| { Ok(1) }));
    for h in hs.into_iter() {
        let r = h.join();
        if r.is_ok() {
            println(\"ok\");
        }
    }
}
",
    );
    assert!(!ok);
    assert!(err.contains("cannot infer type parameter 'E'"), "{err}");
}
