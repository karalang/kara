//! `SortedSet.from([..])` and `SortedMap.from([..])` build the sorted
//! collections from a list, as `Set.from` and `Map.from` do: a repeated
//! element is kept once, a repeated key keeps its last value (kata 1049,
//! whose fold starts from `SortedSet.from([0])`).

use std::process::{Command, Stdio};

#[test]
fn sorted_set_and_sorted_map_build_from_a_list() {
    let path = std::env::temp_dir().join(format!("karac-sorted-from-{}.kara", std::process::id()));
    std::fs::write(
        &path,
        "fn main() {
    let s = SortedSet.from([3, 1, 2, 3]);
    let m = SortedMap.from([(\"b\", 2), (\"a\", 1), (\"b\", 5)]);
    let sums: SortedSet[i64] = [4, 1].iter().fold(SortedSet.from([0]), |made, x| {
        made.iter().flat_map(|t| [*t, *t + *x].into_iter()).collect()
    });
    println(f\"{s} {s.len()} {m} {sums}\");
}
",
    )
    .unwrap();
    let file = path.to_str().unwrap();
    let karac = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("karac")
    };
    let check = karac(&["check", file]);
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "{1, 2, 3} 3 {a: 1, b: 5} {0, 1, 4, 5}\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
