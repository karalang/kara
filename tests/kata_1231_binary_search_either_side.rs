//! Kata 1231's jumps arm: `match xs.binary_search(v) { Ok(i) => i, Err(i)
//! => i }`, the index either way, is fixed to the search alone, not to an
//! `if` whose two branches are the same (B-2026-11-19-28).

use std::process::Command;

fn karac() -> &'static str {
    env!("CARGO_BIN_EXE_karac")
}

fn check_fix_run(tag: &str, src: &str) -> (String, String, String, String) {
    let dir = std::env::temp_dir().join(format!("kata1231_bs_{}_{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("m.kara");
    std::fs::write(&path, src).unwrap();
    let run = |args: &[&str]| {
        let o = Command::new(karac())
            .args(args)
            .arg(&path)
            .output()
            .unwrap();
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    };
    let err = run(&["check"]);
    run(&["fix"]);
    let fixed = std::fs::read_to_string(&path).unwrap();
    let after = run(&["check"]);
    let out = Command::new(karac())
        .args(["__mir-run"])
        .arg(&path)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&out.stdout).to_string();
    let _ = std::fs::remove_dir_all(&dir);
    (err, fixed, after, out)
}

fn wrap(body: &str) -> String {
    format!(
        "fn at(v: Vec[i64], x: i64) -> i64 {{\n    {body}\n}}\n\nfn main() {{\n    let v = [1, 3, 5, 7];\n    println(f\"{{at(v, 0)}} {{at(v, 3)}} {{at(v, 4)}} {{at(v, 9)}}\");\n}}\n"
    )
}

#[test]
fn kata_1231_binary_search_index_either_way_is_the_search_alone() {
    for (tag, body) in [
        (
            "ok-err",
            "match v.binary_search(x) { Ok(i) => i, Err(i) => i }",
        ),
        (
            "err-ok",
            "match v.binary_search(x) { Err(j) => j, Ok(j) => j }",
        ),
    ] {
        let (err, fixed, after, out) = check_fix_run(tag, &wrap(body));
        assert!(err.contains("binary_search"), "{tag}: {err}");
        assert!(
            fixed.contains(".partition_point(")
                && !fixed.contains("binary_search")
                && !fixed.contains(" else "),
            "{tag}: {fixed}"
        );
        assert!(
            after.contains("All checks passed"),
            "{tag}: {after}\n{fixed}"
        );
        assert_eq!(out, "0 1 2 4\n", "{tag}: {fixed}");
    }
}

#[test]
fn kata_1231_binary_search_sides_that_differ_keep_the_test() {
    let (_, fixed, after, out) = check_fix_run(
        "differ",
        &wrap("match v.binary_search(x) { Ok(i) => i, Err(i) => 0 - i - 1 }"),
    );
    assert!(fixed.contains(" else "), "{fixed}");
    assert!(after.contains("All checks passed"), "{after}\n{fixed}");
    assert_eq!(out, "-1 1 -3 -5\n", "{fixed}");
}
