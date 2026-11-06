//! A closure handed to a method chain that ends in `?`
//! (`let h = half(n).map_err(|_| bad.clone())?;`) runs before the `?`
//! returns or passes the value on, so moving a binding it reads after the
//! statement is fine. The statement-closure rule of B-2026-10-29-43 walked
//! the chain only through method calls, so a `?` hid the closure and the
//! ownership pass paired its read with the later move as "moved inside a
//! loop", with no loop in the program (B-2026-11-06-36). The pair inside a
//! loop is still refused.

use std::process::Command;

fn check(name: &str, body: &str) -> (bool, String, String) {
    let src = format!(
        "fn half(n: i64) -> Result[i64, ()] {{
    if n % 2 == 0 {{ Ok(n / 2) }} else {{ Err(()) }}
}}

fn take(s: own String) -> i64 {{
    s.len()
}}

{body}

fn main() {{
    println(f\"{{parse(4)}} {{parse(3)}} {{parse(40)}}\");
}}
"
    );
    let dir = std::env::temp_dir().join(format!("karac-qread-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, &src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        src,
    )
}

#[test]
fn a_binding_moved_after_a_question_chain_that_reads_it_is_accepted() {
    for (name, body, want) in [
        (
            "return",
            "fn parse(n: i64) -> Result[i64, String] {
    let bad = \"bad\".to_string();
    let h = half(n).map_err(|_| bad.clone())?;
    if h > 10 {
        return Err(bad);
    }
    Ok(h)
}",
            "Ok(2) Err(bad) Err(bad)\n",
        ),
        (
            "tail",
            "fn parse(n: i64) -> Result[i64, String] {
    let bad = \"bad\".to_string();
    let h = half(n).map_err(|_| bad.clone())?;
    Ok(h + take(bad))
}",
            "Ok(5) Err(bad) Ok(23)\n",
        ),
        (
            "mid_chain",
            "fn parse(n: i64) -> Result[i64, String] {
    let bad = \"bad\".to_string();
    let h = half(n).map_err(|_| bad.clone())?.max(1);
    Ok(h + take(bad))
}",
            "Ok(5) Err(bad) Ok(23)\n",
        ),
    ] {
        let (ok, err, src) = check(name, body);
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
fn a_question_chain_closure_that_runs_after_the_move_is_still_refused() {
    let (ok, err, _) = check(
        "loop",
        "fn parse(n: i64) -> Result[i64, String] {
    let bad = \"bad\".to_string();
    let mut t = 0;
    for i in 0..n {
        t += half(i * 2).map_err(|_| bad.clone())?;
        if t > 100 {
            t += take(bad);
        }
    }
    Ok(t)
}",
    );
    assert!(
        !ok,
        "a move in a loop the closure runs in again must be refused"
    );
    assert!(err.contains("moved inside a loop"), "{err}");
}
