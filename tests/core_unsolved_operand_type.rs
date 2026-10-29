//! design.md § Type inference: an unsolved variable is an error at the end
//! of the body. `==` takes its own `Rhs` (`PartialEq[Rhs]`), so in
//! `s.chars().collect() == t` nothing fixes what `collect` builds. Check
//! passed it and the MIR builder refused it ("an expression whose type does
//! not lower (UnsolvedParam("C"))"), the shape `karac fix` leaves after
//! deleting a method turbofish (B-2026-10-29-36). An operand whose type
//! the other side does fix still checks.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-unsolvedop-{}-{name}", std::process::id()));
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
fn a_collect_compared_with_a_value_is_refused_at_check() {
    for (name, src) in [
        (
            "string",
            "fn main() {
    let s = \"aBcD\";
    let t: String = \"BD\".to_string();
    let ok = s.chars().filter(|c| c.is_ascii_uppercase()).collect() == t;
    println(ok);
}
",
        ),
        (
            "vec",
            "fn main() {
    let v: Vec[i64] = [1, 2].to_vec();
    let w: Vec[i64] = [2, 4].to_vec();
    println(w == v.iter().map(|x| *x * 2).collect());
}
",
        ),
    ] {
        let (ok, err) = check(name, src);
        assert!(!ok, "{name}: check must refuse");
        assert!(
            err.contains("cannot infer type parameter 'C', this call's result"),
            "{name}: {err}"
        );
    }
}

#[test]
fn an_operand_the_other_side_fixes_still_checks() {
    let src = "fn main() {
    let v: Vec[i64] = [1, 2, 3].to_vec();
    let x: i64 = 4;
    let total = x + v.iter().map(|y| *y * 2).sum();
    let m = v.len() + v.iter().filter(|y| *y > 1).count();
    let best = v.iter().max().unwrap() == 3;
    let parsed = i64.parse(\"3\").ok() == Some(3);
    let caps: String = \"aBcD\".chars().filter(|c| c.is_ascii_uppercase()).collect();
    println(f\"{total} {m} {best} {caps == \"BD\"} {parsed}\");
}
";
    let (ok, err) = check("fixed", src);
    assert!(ok, "{err}");
    assert_eq!(
        karac::mir::lower::run_source(src)
            .expect("MIR interpreter")
            .output,
        "16 5 true true true\n"
    );
}
