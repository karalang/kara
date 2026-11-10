//! Kata 2077's ★ arm counted a corridor's triangles as Rust does,
//! `up[a].intersection(&up[b]).count()`. Rust's set operations give an
//! iterator; Kāra's give a set, which has no `count`, and the checker said
//! only "no method 'count' on type 'Set'". It now says why and offers
//! `len()`, which `karac fix` writes; a `SortedSet` is the same.

use std::process::{Command, Stdio};

fn karac(name: &str, cmd: &str, src: &str) -> (String, String) {
    let dir =
        std::env::temp_dir().join(format!("kata_2077_set_count_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([cmd, file.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let text = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        text,
    )
}

const SRC: &str = "fn main() {
    let a: Set[i64] = [1, 2, 3].to_vec().into_iter().collect();
    let b: Set[i64] = [2, 3, 4].to_vec().into_iter().collect();
    let s: SortedSet[i64] = [5, 6].to_vec().into_iter().collect();
    let n = a.intersection(b).count();
    let m = s.count();
    println(f\"{n} {m}\");
}
";

#[test]
fn kata_2077_set_count_names_len() {
    let (out, _) = karac("check", "check", SRC);
    assert!(
        out.contains("no method 'count' on type 'Set': a `Set` holds its size, `len()`; Kāra's `union`, `intersection` and `difference` give a `Set`, not Rust's iterator, so `a.intersection(b).count()` is `a.intersection(b).len()`"),
        "{out}"
    );
    assert!(
        out.contains(
            "no method 'count' on type 'SortedSet': a `SortedSet` holds its size, `len()`"
        ),
        "{out}"
    );
}

#[test]
fn kata_2077_set_count_is_fixed_to_len() {
    let (_, text) = karac("fix", "fix", SRC);
    assert!(text.contains("let n = a.intersection(b).len();"), "{text}");
    assert!(text.contains("let m = s.len();"), "{text}");
}
