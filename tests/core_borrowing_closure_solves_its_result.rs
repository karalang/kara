//! A closure whose parameter borrows may fill a slot that hands the value
//! over (`|s: ref String|` for `OnceFn(own T) -> U`), as an impl may take a
//! parameter in a weaker mode than its trait (core-semantics §4.7). The slot's
//! `T` was not bound through the closure's `ref`, so the call's result type
//! `U` stayed unsolved and the program was refused with "cannot infer type
//! parameter 'U'" (B-2026-10-29-44). Under D5 closures `|s: String|` reads as
//! `|s: ref String|`, so every annotated mapper hit it.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-clsolve-{}-{name}", std::process::id()));
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
fn a_borrowing_closure_in_an_owning_slot_solves_the_call_s_result() {
    for (name, body, want) in [
        (
            "option_map",
            "let up: Option[String] = Some(\"ab\".to_string());
    let n = up.map(|x: ref String| x.len());
    println(n.unwrap_or(0));",
            "2\n",
        ),
        (
            "result_map",
            "let r: Result[String, i64] = Ok(\"abc\".to_string());
    let m = r.map(|x: ref String| x.len());
    println(m.unwrap_or(0));",
            "3\n",
        ),
        (
            "into_iter_map",
            "let v: Vec[String] = vec![\"a\".to_string(), \"bcd\".to_string()];
    let w: Vec[i64] = v.into_iter().map(|x: ref String| x.len()).collect();
    println(w[0] + w[1]);",
            "4\n",
        ),
        (
            "user_generic",
            "let n = ap(\"abcd\".to_string(), |s: ref String| s.len());
    println(n);",
            "4\n",
        ),
    ] {
        let src = format!(
            "fn ap[T, U](x: own T, f: OnceFn(own T) -> U) -> U {{ f(x) }}
fn main() {{
    {body}
}}
"
        );
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
