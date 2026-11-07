//! Spellings kata 1062's arms met, each now checking clean and running the
//! same on every backend.
//!
//! - `s[other..other + len]` with `other` an iterator's `ref i64`: the range's
//!   start was the borrow and its end the `i64` the `+` read through it, and
//!   the two bounds were refused as different types. A bound that borrows an
//!   integer now reads it, as an operator's operand does.
//! - `b[x..].cmp(b[y..])` in a suffix sort: collections.md gives `Slice` the
//!   lexicographic `Ord` that `<` on two slices already used, but `Slice`
//!   had no `cmp` method ("no method 'cmp' on type 'Slice'").

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// What `karac check` says, and what each backend prints.
fn check_and_run(name: &str, src: &str) -> (bool, String, Vec<(String, String)>) {
    let dir = std::env::temp_dir().join(format!("karac-kata-1062-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), format!("{src}\n")).unwrap();
    let (clean, _, said) = karac(&dir, &["check", "cell.kara"]);
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    let outs = backends
        .iter()
        .map(|b| {
            let (_, out, err) = karac(&dir, &[b, "cell.kara"]);
            (b.to_string(), out + &err)
        })
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    (clean, said, outs)
}

#[test]
fn kata_1062_range_bound_borrowing_an_integer_reads_it() {
    let src = "fn main() {\n    let starts: Vec[i64] = [1, 3];\n    let s: Vec[i64] = [10, 20, 30, 40, 50];\n    let len = 2;\n    for other in starts.iter() {\n        let window = s[other..other + len];\n        let total: i64 = window.iter().sum();\n        let mut steps = 0;\n        for j in other..other + len {\n            steps += j;\n        }\n        println(f\"{window.len()} {total} {steps}\");\n    }\n}";
    let (clean, said, outs) = check_and_run("range", src);
    assert!(clean && !said.contains("warning"), "{said}");
    for (b, out) in outs {
        assert_eq!(out, "2 50 3\n2 90 7\n", "{b}");
    }
}

#[test]
fn kata_1062_slice_cmp_is_lexicographic() {
    let src = "fn order(o: Ordering) -> String {\n    match o {\n        Ordering.Less => \"<\".to_string(),\n        Ordering.Equal => \"=\".to_string(),\n        Ordering.Greater => \">\".to_string(),\n    }\n}\n\nfn main() {\n    let b = \"banana\".bytes();\n    let mut starts: Vec[i64] = (0..b.len()).collect();\n    starts.sort_by(|x, y| b[x..].cmp(b[y..]));\n    println(starts);\n    let v: Vec[u8] = [3, 1, 2];\n    let w: Vec[u8] = [3, 1];\n    println(f\"{order(v.as_slice().cmp(w.as_slice()))} {order(w[..].cmp(v[..]))} {order(v[1..2].cmp(w[1..]))}\");\n}";
    let (clean, said, outs) = check_and_run("cmp", src);
    assert!(clean && !said.contains("warning"), "{said}");
    for (b, out) in outs {
        assert_eq!(out, "[5, 3, 1, 0, 4, 2]\n> < =\n", "{b}");
    }
}
