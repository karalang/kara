//! A slice range whose bounds are `ref` integers (`nums[a..=b]` with `a`
//! and `b` bound from `l.iter().zip(r.iter())`, or `nums[a..]` with `a` a
//! closure's `ref i32` item) checked clean and then failed to compile:
//! "invalid MIR: IntToInt cast from ref i32 to u64". A plain index read
//! the bound through; the range now does too (kata 1630).

use std::process::Command;

fn run(tag: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-rangeref-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["run", "--interp", f.to_str().unwrap()])
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_slice_range_takes_ref_bounds_from_a_zip_pattern() {
    let src = "fn main() {\n    let nums: Vec[i32] = [4, 6, 5, 9];\n    let l: Vec[i32] = [0, 1];\n    let r: Vec[i32] = [2, 3];\n    for (a, b) in l.iter().zip(r.iter()) {\n        println(f\"{nums[a..=b].to_vec()} {nums[a..b].len()}\");\n    }\n}\n";
    assert_eq!(run("zip", src), "[4, 6, 5] 2\n[6, 5, 9] 2\n");
}

#[test]
fn a_slice_range_takes_a_closure_ref_item_as_its_start() {
    let src = "fn main() {\n    let nums: Vec[i32] = [4, 6, 5, 9];\n    let l: Vec[i32] = [0, 1];\n    let got: Vec[i64] = l.iter().map(|a| nums[a..].len()).collect();\n    println(f\"{got}\");\n}\n";
    assert_eq!(run("closure", src), "[4, 3]\n");
}
