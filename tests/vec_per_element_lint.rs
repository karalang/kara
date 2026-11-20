//! B-2026-11-19-102: `Vec.filled(n, [false, false])` puts a heap `Vec` in
//! every slot, since a list literal is a `Vec` unless an `Array` is expected
//! (design.md § List literals); kata 1654 ran 13x Rust until it wrote
//! `[false; 2]`. The `vec_per_element` lint names the cost where the rows are
//! only indexed, and `karac fix` writes `[v; N]` when the literal repeats one
//! value.

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-vec-per-element-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

const SRC: &str = "fn main() {
    let n = 5;
    let mut seen = Vec.filled(n + 1, [false, false]);
    seen[2][1] = true;
    let mut cnt = Vec.filled(3, [0, 1]);
    cnt[0][0] += 4;
    let mut grow = Vec.filled(2, [1, 1]);
    grow[0].push(3);
    let k = Vec.filled(2, [7, 7]);
    for row in k.iter() { println(row.len()); }
    println(f\"{seen[2][1]} {seen[0][0]} {seen.len()} {seen[1].len()} {cnt[0][0]} {grow[0].len()}\");
}
";

#[test]
fn vec_per_element_fires_only_where_the_rows_are_indexed() {
    let path = scratch("check.kara", SRC);
    let out = text(&karac(&["check"], &path));
    assert_eq!(out.matches("warning[vec_per_element]").count(), 2, "{out}");
    assert!(out.contains("slot of `seen`"), "{out}");
    assert!(
        out.contains("write `[v; 2]` for an inline `Array`"),
        "{out}"
    );
    assert!(
        out.contains("annotate `cnt` as a `Vec` of `Array[T, 2]`"),
        "{out}"
    );
    // A row that is pushed onto, and one that is iterated, stay `Vec`s.
    assert!(!out.contains("slot of `grow`"), "{out}");
    assert!(!out.contains("slot of `k`"), "{out}");
}

#[test]
fn karac_fix_writes_a_repeat_array_and_the_program_prints_the_same() {
    let path = scratch("fix.kara", SRC);
    let before = karac(&["run"], &path);
    let fix = karac(&["fix"], &path);
    assert!(fix.status.success(), "{}", text(&fix));
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        fixed.contains("let mut seen = Vec.filled(n + 1, [false; 2]);"),
        "{fixed}"
    );
    // Only the repeated literal has a machine fix.
    assert!(fixed.contains("Vec.filled(3, [0, 1])"), "{fixed}");
    let after = karac(&["run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&after.stdout),
        String::from_utf8_lossy(&before.stdout),
        "{}",
        text(&after)
    );
    assert_eq!(
        String::from_utf8_lossy(&after.stdout),
        "2\n2\ntrue false 6 2 4 3\n"
    );
}

/// The lint has its own code in the typecheck lint band (design.md §2).
#[test]
fn vec_per_element_has_its_own_code() {
    let path = scratch("json.kara", SRC);
    let out = text(&karac(&["check", "--output=json"], &path));
    assert!(out.contains("\"code\":\"W0321\""), "{out}");
}
