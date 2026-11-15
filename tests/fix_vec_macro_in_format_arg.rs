//! B-2026-11-17-118 (MD1): a `vec![..]` inside a format macro's argument,
//! `println!("{}", g(vec![1, 2]))`, the line every LeetCode harness
//! writes, had no fix: a hole refuses a macro, so the Mend loop ended
//! refused. `karac fix` now writes the argument's `vec![a, b]` as `[a, b]`
//! and its `vec![x; n]` as `Vec.filled(n, x)`, nested ones included.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

#[test]
fn a_vec_macro_in_a_format_argument_is_fixed_into_the_hole() {
    let dir = std::env::temp_dir().join(format!("karac-vecfmt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    let src = "fn g(v: Vec<i32>) -> i32 {\n    v.iter().sum()\n}\n\nfn h(v: Vec<Vec<i32>>) -> usize {\n    v.len()\n}\n\nfn main() {\n    println!(\"{}\", g(vec![1, 2]));\n    println!(\"{} {}\", g(vec![0; 3]), h(vec![vec![1], vec![2, 3]]));\n    println!(\"{}\", g(vec![7; 2]));\n}\n";
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap();
    for _ in 0..4 {
        karac(&["fix", p]);
    }
    let fixed = std::fs::read_to_string(&f).unwrap();
    let check = karac(&["check", p]);
    let out = karac(&["run", "--interp", p]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        check.status.success(),
        "{fixed}\n{}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(fixed.contains("println(f\"{g([1, 2])}\");"), "{fixed}");
    assert!(
        fixed.contains("println(f\"{g(Vec.filled(3, 0))} {h([[1], [2, 3]])}\");"),
        "{fixed}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "3\n0 2\n14\n");
}
