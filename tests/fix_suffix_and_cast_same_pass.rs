//! B-2026-11-17-128 (kata 1594): `redundant_suffix` and `lossless_cast` applied in
//! one `karac fix` pass narrowed an `i64` table. `vec![vec![0i64; n]; m]`
//! lost its suffix (the stores' casts type it) and every `as i64` store
//! lost its cast (the suffix types it), so nothing typed the table any
//! more: it became `i32`, checked clean, and overflowed at run time. The
//! cast fixes now wait for a pass with no suffix fix in it.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

#[test]
fn a_suffix_and_the_casts_it_makes_lossless_are_not_dropped_together() {
    let dir = std::env::temp_dir().join(format!("karac-suffixcast-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    let src = "fn f(grid: Vec[Vec[i32]]) -> i32 {\n    let m = grid.len();\n    let n = grid[0].len();\n    let mut mx = vec![vec![0i64; n]; m];\n    mx[0][0] = grid[0][0] as i64;\n    for i in 1..m {\n        mx[i][0] = mx[i - 1][0] * grid[i][0] as i64;\n    }\n    let res = mx[m - 1][0];\n    (res % 1_000_000_007) as i32\n}\n\nfn main() {\n    println(f\"{f([[100000], [100000], [100000]])}\");\n}\n";
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap();
    for _ in 0..3 {
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
    assert!(fixed.contains("Vec.filled(m, Vec.filled(n, 0))"), "{fixed}");
    assert!(fixed.contains("mx[0][0] = grid[0][0] as i64;"), "{fixed}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "993000007\n",
        "{fixed}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
