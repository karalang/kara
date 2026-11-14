//! B-2026-11-14-11: `clear()` keeps a hashed table's index, emptied, rather
//! than freeing it, and a word-sized entry is stored with one move. These
//! pin that a table cleared at many sizes, large then small and back,
//! with deleted cells before each clear, still finds exactly what it
//! holds, on the interpreter, compiled and under ASan.

use std::process::{Command, Stdio};

fn run(sub: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-map-clear-refill-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([sub, "cell.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const SRC: &str = r#"fn main() {
  let sizes = [1000, 10, 600, 3, 0, 40];
  let mut s: Set[i64] = Set.new();
  let mut round = 0;
  for n in sizes.iter() {
    for i in 0..n { s.insert(i * 7 + round); }
    for i in 0..n { if i % 3 == 0 { s.remove(i * 7 + round); } }
    let mut hits = 0;
    for i in 0..n { if s.contains(i * 7 + round) { hits += 1; } }
    println(f"{n}: len {s.len()} hits {hits} has0 {s.contains(round)}");
    s.clear();
    s.insert(-1);
    println(f"cleared: len {s.len()} has-1 {s.contains(-1)} has7 {s.contains(7 + round)}");
    s.clear();
    round += 1;
  }
  let mut m: Map[String, i64] = Map.new();
  for n in [400, 5, 120].iter() {
    for i in 0..n { m[f"k{i}"] = i * n; }
    println(f"map {n}: len {m.len()} k4 {m.get("k4") ?? -1}");
    m.clear();
    m["z"] = n;
    println(f"cleared: len {m.len()} z {m.get("z") ?? -1} k1 {m.get("k1") ?? -1}");
  }
}
"#;

// From a Python mirror of the program.
const WANT: &str = "1000: len 666 hits 666 has0 false
cleared: len 1 has-1 true has7 false
10: len 6 hits 6 has0 false
cleared: len 1 has-1 true has7 false
600: len 400 hits 400 has0 false
cleared: len 1 has-1 true has7 false
3: len 2 hits 2 has0 false
cleared: len 1 has-1 true has7 false
0: len 0 hits 0 has0 false
cleared: len 1 has-1 true has7 false
40: len 26 hits 26 has0 false
cleared: len 1 has-1 true has7 false
map 400: len 400 k4 1600
cleared: len 1 z 400 k1 -1
map 5: len 6 k4 20
cleared: len 1 z 5 k1 -1
map 120: len 121 k4 480
cleared: len 1 z 120 k1 -1
";

#[test]
fn map_set_cleared_and_refilled_at_many_sizes() {
    let (ok, out, err) = run("__mir-run", SRC);
    assert!(ok, "interpreted: {err}");
    assert_eq!(out, WANT, "interpreted: {err}");
    if !cfg!(feature = "llvm") {
        return;
    }
    for sub in ["__mir-llvm-run", "__mir-llvm-asan"] {
        let (ok, out, err) = run(sub, SRC);
        if err.contains("Linker failed")
            && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none()
        {
            eprintln!("skipping {sub}: no runtime archive");
            continue;
        }
        assert!(ok, "{sub}: {err}");
        assert_eq!(out, WANT, "{sub}: {err}");
    }
}
