//! Rust's borrowed string arguments go through the Mend loop (kata 1048).
//! `format!("{}{}", &w[..i], &w[i + 1..])` becomes an f-string whose holes
//! name the values, since a hole only displays what it names (a `&` carried
//! into a hole was a parse error with no fix), and `s + &w[i..]`, which
//! `karac fix` first writes `s + ref w[i..]`, becomes
//! `s + w[i..].to_string()`: Kāra's `+` appends an owned `String`.

use std::path::Path;
use std::process::{Command, Stdio};

/// (name, source, the first check's hint, the run's output)
const CELLS: &[(&str, &str, &str, &str)] = &[
    (
        "format_borrowed_views",
        "fn main() { let w = \"abc\".to_string(); let i = 1; let p = format!(\"{}{}\", &w[..i], &w[i + 1..]); println(p); }",
        "write an f-string",
        "ac",
    ),
    (
        "println_borrowed_args",
        "fn main() { let a = 3; let b = \"x\".to_string(); println!(\"{} {}\", &a, &&b); }",
        "write an f-string",
        "3 x",
    ),
    (
        "concat_borrowed_view",
        "fn main() { let w = \"abc\".to_string(); let s = w[..1].to_string() + &w[2..]; println(s); }",
        "`&` is Rust syntax",
        "ac",
    ),
    (
        "concat_ref_view",
        "fn main() { let w = \"abc\".to_string(); let s = \"x\".to_string() + ref w[1..]; println(s); }",
        "Rust's `s + &t` is `s + t.to_string()`",
        "xbc",
    ),
    (
        "concat_borrowed_string",
        "fn main() { let w = \"abc\".to_string(); let s = \"y\".to_string() + &w; println(s); }",
        "`&` is Rust syntax",
        "yabc",
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
    if text.contains('&') || text.contains('!') {
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
fn rust_borrowed_string_arguments_mend_to_kara() {
    let root = std::env::temp_dir().join(format!("karac-mend-rust-borrow-{}", std::process::id()));
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
