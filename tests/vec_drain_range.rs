//! `Vec.drain(start..end)` (docs/library/collections.md, B-2026-11-04-77).
//! The behaviour is pinned by `corpus/fixtures/mir_collections/vec_drain_range`;
//! this file pins the fix-it for Rust's range spellings, which `drain` refuses:
//! each is rewritten to the `start..end` form, and the fixed program runs.

use std::process::Command;

fn fixed(tag: &str, range: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-drain-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    let src = format!(
        "fn main() {{\n    let mut v: Vec[i64] = [1, 2, 3, 4, 5].to_vec();\n    let i = 1;\n    \
         let d: Vec[i64] = v.drain({range}).collect();\n    println(f\"{{d}} {{v}} {{i}}\");\n}}\n"
    );
    std::fs::write(&f, src).unwrap();
    let karac = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(args)
            .arg(&f)
            .output()
            .unwrap()
    };
    let check = karac(&["check"]);
    assert!(
        !check.status.success(),
        "{tag}: `drain({range})` checked clean"
    );
    let diag = String::from_utf8_lossy(&check.stderr).into_owned();
    karac(&["fix"]);
    let text = std::fs::read_to_string(&f).unwrap();
    let run = karac(&["run", "--interp"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        run.status.success(),
        "{tag}: {}\n{text}",
        String::from_utf8_lossy(&run.stderr)
    );
    let call = text
        .lines()
        .find_map(|l| l.split("v.drain(").nth(1))
        .and_then(|rest| rest.strip_suffix(").collect();"))
        .unwrap_or_else(|| panic!("{tag}: no drain call in\n{text}"))
        .to_string();
    (
        diag,
        format!("{call} => {}", String::from_utf8_lossy(&run.stdout)),
    )
}

#[test]
fn drain_rust_range_spellings_are_fixed_to_start_end() {
    let cases = [
        ("from", "1..", "1..v.len() => [2, 3, 4, 5] [1] 1\n"),
        ("to", "..2", "0..2 => [1, 2] [3, 4, 5] 1\n"),
        ("full", "..", "0..v.len() => [1, 2, 3, 4, 5] [] 1\n"),
        ("incl_lit", "1..=2", "1..3 => [2, 3] [1, 4, 5] 1\n"),
        ("incl_sum", "i..=i + 1", "i..i + 2 => [2, 3] [1, 4, 5] 1\n"),
        (
            "incl_mul",
            "i..=i * 2",
            "i..i * 2 + 1 => [2, 3] [1, 4, 5] 1\n",
        ),
    ];
    for (tag, range, want) in cases {
        let (diag, got) = fixed(tag, range);
        assert!(
            diag.contains("`Vec.drain` takes a `start..end` range"),
            "{tag}: {diag}"
        );
        assert_eq!(got, want, "{tag}");
    }
}
