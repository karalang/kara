//! An iterator method called straight on a sequence (`v.find(..)`, no
//! `.iter()`) goes through the Mend loop: `karac check` names the `.iter()`
//! hop, `karac fix` inserts it, and the program runs. `find`, `flatten`,
//! `nth`, `position` and `rev` had drifted out of the typechecker's
//! iterator-surface list, so they drew an edit-distance guess ("did you mean
//! 'fill'?") with no fix; kata 1044's `windows.windows(2).find(..)` found it
//! (B-2026-11-04-51). A `flatten` over collection items, the step after the
//! hop for `g.flatten()` on a `Vec[Vec[T]]`, is fixed to `flat_map`
//! (B-2026-11-04-52).

use std::path::Path;
use std::process::{Command, Stdio};

const CELLS: &[(&str, &str, &str)] = &[
    (
        "vec_find",
        "fn main() { let v = [3, 1, 4]; match v.find(|x| x > 3) { Some(x) => println(x), None => println(0) } }",
        "4",
    ),
    (
        "vec_position",
        "fn main() { let v = [3, 1, 4]; println(v.position(|x| x == 4).unwrap()); }",
        "2",
    ),
    (
        "vec_nth",
        "fn main() { let v = [3, 1, 4]; println(v.nth(1).unwrap()); }",
        "1",
    ),
    (
        "vec_rev",
        "fn main() { let v = [3, 1, 4]; let w: Vec[i64] = v.rev().collect(); println(w[0]); }",
        "4",
    ),
    (
        "vec_flatten",
        "fn main() { let g = [[1, 2], [3]]; let f: Vec[i64] = g.flatten().collect(); println(f.len()); }",
        // The hop, then `.iter().flatten()` over `Vec` items becomes
        // `.flat_map(|v| v.iter())` (B-2026-11-04-52).
        "3",
    ),
    (
        "into_iter_flatten",
        "fn main() { let g = [[1, 2], [3]]; for x in g.into_iter().flatten() { print(x); } println(\"\"); }",
        "123",
    ),
    (
        "slice_position",
        "fn main() { let v = [3, 1, 4]; let s = v[0..3]; println(s.position(|x| x == 1).unwrap()); }",
        "1",
    ),
    (
        "array_find",
        "fn main() { let a: Array[i64, 3] = [3, 1, 4]; println(a.find(|x| x < 3).unwrap()); }",
        "1",
    ),
    (
        "windows_find",
        "fn main() { let mut w = [\"ab\", \"ba\", \"ab\"]; w.sort(); match w.windows(2).find(|p| p[0] == p[1]) { Some(p) => println(p[0]), None => println(\"none\") } }",
        "ab",
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
    if !text.contains(".iter().") && !text.contains(".into_iter().") && !text.contains(".flat_map(")
    {
        return (first, Err(format!("no fix applied:\n{text}")));
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
fn an_iterator_method_on_a_sequence_mends_to_an_iter_hop() {
    let root = std::env::temp_dir().join(format!("karac-mend-iter-{}", std::process::id()));
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = CELLS
            .iter()
            .map(|&(name, src, want)| {
                let dir = root.join(name);
                s.spawn(move || {
                    std::fs::create_dir_all(&dir).unwrap();
                    let (first, got) = mend(&dir, src);
                    (name, want, first, got)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(results.len(), CELLS.len());
    let mut bad = Vec::new();
    for (name, want, first, got) in results {
        // A call's result is walked with `into_iter` (B-2026-11-11-46).
        let hint = match name {
            "into_iter_flatten" => "in place of `.flatten()`",
            "windows_find" => "require an explicit `.into_iter()`",
            _ => "require an explicit `.iter()`",
        };
        if !first.contains(hint) {
            bad.push(format!(
                "{name}: no `.iter()` hint on the first check:\n{first}"
            ));
        }
        match got {
            Ok(out) if out.trim_end() == want => {}
            Ok(out) => bad.push(format!("{name}: printed {out:?}, want {want:?}")),
            Err(why) => bad.push(format!("{name}: {why}")),
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
}
