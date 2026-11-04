//! Rust's standard-library imports and collection names go through the Mend
//! loop (B-2026-11-04-5). `use std::collections::HashMap;`, or the `import`
//! `karac fix` makes of the `use`, is deleted, since Kāra's collections,
//! `cmp` and `mem` names are in the prelude; any other `std::` import gets
//! `.` for `::`; `HashMap`, `HashSet`, `BTreeMap` and `BTreeSet` rename to
//! `Map`, `Set`, `SortedMap` and `SortedSet`; and `BinaryHeap.new()` /
//! `BinaryHeap.from(v)` become `PriorityQueue.max_first()` /
//! `max_first_from(v)`, because Kāra's queue pops the smallest first and a
//! rename alone would reverse the program (kata 1046).

use std::path::Path;
use std::process::{Command, Stdio};

/// (name, source, the first check's hint, the run's output)
const CELLS: &[(&str, &str, &str, &str)] = &[
    (
        "hash_map",
        "use std::collections::HashMap;\nfn main() { let mut m: HashMap[i64, i64] = HashMap::new(); m.insert(1, 2); println(m.len()); }",
        "in its prelude, with no import",
        "1",
    ),
    (
        "group",
        "use std::collections::{HashSet, BTreeMap};\nfn main() { let mut s = HashSet::new(); s.insert(3); let mut t: BTreeMap[i64, i64] = BTreeMap::new(); t.insert(2, 1); t.insert(1, 1); println(f\"{s.len()} {t.len()}\"); }",
        "`Set`, `SortedMap` in its prelude",
        "1 2",
    ),
    (
        "heap_from",
        "use std::collections::BinaryHeap;\nfn main() { let mut h = BinaryHeap::from([2, 7, 4, 1, 8, 1]); println(h.pop().unwrap()); }",
        "`PriorityQueue` in its prelude",
        "8",
    ),
    (
        "heap_new",
        "import std::collections::BinaryHeap;\nfn main() { let mut h: BinaryHeap[i64] = BinaryHeap::new(); h.push(3); h.push(9); h.push(5); println(h.pop().unwrap()); }",
        "in its prelude, with no import",
        "9",
    ),
    (
        "cmp_max",
        "use std::cmp::max;\nfn main() { println(max(3, 5)); }",
        "`max` in its prelude",
        "5",
    ),
    (
        "mem_module",
        "use std::mem;\nfn main() { let mut a = 1; let mut b = 2; mem::swap(mut a, mut b); println(f\"{a} {b}\"); }",
        "`std::mem` is Rust's standard library",
        "2 1",
    ),
    (
        "std_process",
        "use std::process::exit;\nfn main() { println(1); exit(0); }",
        "Kāra writes an import path with `.`",
        "1",
    ),
];

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

/// One cell: the first check's diagnostics, then `Ok` with the run's stdout
/// once `karac fix` has carried it to a clean check, or `Err` with why not.
fn mend(dir: &Path, src: &str) -> (String, Result<String, String>) {
    let file = dir.join("cell.kara");
    std::fs::write(&file, format!("{src}\n")).unwrap();
    let (_, _, first) = karac(dir, &["check", "cell.kara"]);
    for _ in 0..4 {
        let before = std::fs::read_to_string(&file).unwrap();
        karac(dir, &["fix", "cell.kara"]);
        if std::fs::read_to_string(&file).unwrap() == before {
            break;
        }
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let (ok, _, err) = karac(dir, &["check", "cell.kara"]);
    if !ok {
        return (
            first,
            Err(format!("still refused:\n{err}\nfixed text:\n{text}")),
        );
    }
    if text.contains("::") || text.starts_with("use ") {
        return (first, Err(format!("a Rust-ism left:\n{text}")));
    }
    let (_, stdout, stderr) = karac(dir, &["__mir-run", "cell.kara"]);
    if stdout.is_empty() {
        return (
            first,
            Err(format!("no output: {stderr}\nfixed text:\n{text}")),
        );
    }
    (first, Ok(stdout))
}

#[test]
fn rust_std_imports_and_collections_mend_to_kara() {
    let root = std::env::temp_dir().join(format!("karac-mend-rust-std-{}", std::process::id()));
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = CELLS
            .iter()
            .map(|&(name, src, hint, want)| {
                let dir = root.join(name);
                s.spawn(move || {
                    std::fs::create_dir_all(&dir).unwrap();
                    let (first, got) = mend(&dir, src);
                    (name, hint, want, first, got)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(results.len(), CELLS.len());
    let mut bad = Vec::new();
    for (name, hint, want, first, got) in results {
        if !first.contains(hint) {
            bad.push(format!("{name}: no {hint:?} on the first check:\n{first}"));
        }
        match got {
            Ok(out) if out.trim_end() == want => {}
            Ok(out) => bad.push(format!("{name}: printed {out:?}, want {want:?}")),
            Err(why) => bad.push(format!("{name}: {why}")),
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
}
