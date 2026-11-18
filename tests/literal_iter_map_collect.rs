//! A list literal read through `iter()` and an adapter keeps its number
//! literals open for the `collect` to fix, as `into_iter()` already did
//! (B-2026-11-11-41): `let b: Vec[i32] = [1, 2].iter().map(|x| *x).collect()`
//! was refused as `C: FromIterator[i64]`, though the same chain over
//! `let a = [1, 2]` checked. A closure that builds a tuple or an `Option`
//! around the item (`|a| (*a, *a)`, `|a| Some(*a)`) also fixes a bound
//! list's literals from the annotation's element (B-2026-11-18-71, kata 1622).
//! `tests/collect_widen_fix.rs` pinned the old refusal for `[1, 2].iter()
//! .map(|x| x + 1)` into `Vec[u8]`; it now builds `[2, 3]`.
//! A bare `[..].iter()` that is not an adapter's receiver (a `for` over it)
//! keeps its old reading.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-literal-iter-map-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

fn runs_to(tag: &str, src: &str, want: &str) {
    let path = temp(tag, src);
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let out = karac(lane, &path);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{lane:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn an_inline_literal_list_through_iter_and_map_collects_into_the_annotated_width() {
    runs_to(
        "inline",
        "fn main() {\n    let b: Vec[i32] = [1, 2].iter().map(|x| *x + 1).collect();\n    \
         println(b);\n    let c: Vec[u8] = [250, 7].iter().cloned().collect();\n    println(c);\n    \
         let small: Vec[u8] = [1, 2].iter().map(|x| x + 1).collect();\n    println(small);\n    \
         let d: Vec[i32] = [4, 5, 6].iter().filter(|x| *x > 4).map(|x| *x).collect();\n    \
         println(d);\n    let mut m: Map[i64, String] = [:];\n    \
         for k in [50, 10].iter() {\n        m.insert(k, f\"v{k}\");\n    }\n    println(m.len());\n}\n",
        "[2, 3]\n[250, 7]\n[2, 3]\n[5, 6]\n2\n",
    );
}

#[test]
fn a_closure_building_a_tuple_or_option_fixes_a_bound_literal_list() {
    runs_to(
        "shaped",
        "fn main() {\n    let q = [3, 4];\n    let z: Vec[(i32, u8)] = q.iter().map(|a| (*a, 200)).collect();\n    \
         println(z);\n    let r = [(1, 2)];\n    let w: Vec[(i32, i32)] = r.iter().map(|p| (p.1, p.0)).collect();\n    \
         println(w);\n    let s = [7, 8];\n    let y: Vec[(i32, i32)] = s.iter().map(|a| (*a, *a * 2)).collect();\n    \
         println(y);\n    let o = [9, 10];\n    let x: Vec[Option[i32]] = o.iter().map(|a| Some(*a)).collect();\n    \
         println(x);\n}\n",
        "[(3, 200), (4, 200)]\n[(2, 1)]\n[(7, 14), (8, 16)]\n[Some(9), Some(10)]\n",
    );
}
