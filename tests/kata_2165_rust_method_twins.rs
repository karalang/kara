//! Katas 2164 and 2165: Rust's `VecDeque::from(v)` and `dq.drain(..)`,
//! `s.remove(i)` and `s.insert(i, c)` on a `String`, and a slice's
//! `rotate_left` / `rotate_right` (since B-2026-11-15-35, methods of their
//! own) were refused with a bare "no method" or
//! "no associated function", which reads as if the operation did not
//! exist. Each refusal now names the Kāra spelling.

use std::path::Path;
use std::process::{Command, Stdio};

fn check(tag: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-kata-2165-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), format!("{src}\n")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "cell.kara"])
        .current_dir(Path::new(&dir))
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout)
}

#[test]
fn kata_2165_vecdeque_from_names_collect() {
    let out = check(
        "from",
        "fn main() {\n    let v: Vec[i64] = vec![3, 1, 2];\n    let dq = VecDeque.from(v);\n    println(dq.len());\n}",
    );
    assert!(
        out.contains("no associated function 'from' on type 'VecDeque': a `VecDeque` is built from a list by `collect`"),
        "{out}"
    );
}

#[test]
fn kata_2165_vecdeque_drain_names_mem_take() {
    let out = check(
        "drain",
        "fn main() {\n    let v: Vec[i64] = vec![3, 1, 2];\n    let mut dq: VecDeque[i64] = v.into_iter().collect();\n    let w: Vec[i64] = dq.drain(..).collect();\n    println(w.len());\n}",
    );
    assert!(
        out.contains("no method 'drain' on type 'VecDeque': a `VecDeque` is emptied into a `Vec` by `mem.take(mut dq)`"),
        "{out}"
    );
}

#[test]
fn kata_2165_string_remove_and_insert_name_vec_of_char() {
    let out = check(
        "string",
        "fn main() {\n    let mut s = \"abc\".to_string();\n    let c = s.remove(1);\n    s.insert(0, c);\n    println(s);\n}",
    );
    for m in ["remove", "insert"] {
        assert!(
            out.contains(&format!("no method '{m}' on type 'String': a `String` is not edited by position; collect its characters into a `Vec[char]`")),
            "{out}"
        );
    }
}

/// B-2026-11-15-35 flipped the two rotation pins: `rotate_left` and
/// `rotate_right` are now `Slice` methods too, so a part of a `Vec` and an
/// array turn in place, as Rust's `[T]` does.
#[test]
fn kata_2165_slice_rotate_is_a_slice_method() {
    let out = check(
        "rotate",
        "fn main() {\n    let mut d: Vec[i64] = [1, 2, 3].to_vec();\n    d[..2].rotate_left(1);\n    d[1..].rotate_right(1);\n    println(d[0]);\n}",
    );
    assert!(out.contains("All checks passed"), "{out}");
    assert!(!out.contains("no method"), "{out}");
}

#[test]
fn kata_2165_array_rotate_is_a_slice_method() {
    let out = check(
        "array",
        "fn main() {\n    let mut a: Array[i64, 3] = [1, 2, 3];\n    a.rotate_left(1);\n    println(a[0]);\n}",
    );
    assert!(out.contains("All checks passed"), "{out}");
    assert!(!out.contains("no method"), "{out}");
}

/// Kata 2168: Rust's `scan` over a lent array, written through its index
/// (`counts[d] += 1`) with no `*`, drew a bare "expected
/// 'Option[(Array[i64, 10], _)]', found 'Option[bool]'" and four errors
/// after it. It is now Rust's shape, as a `*m` write is: the state is
/// copied, written, and handed back with the item.
#[test]
fn kata_2168_scan_over_an_indexed_state_is_rewritten() {
    let dir = std::env::temp_dir().join(format!("karac-kata-2168-scan-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(
        &file,
        "fn main() {\n    let v: Vec[i64] = [1, 2, 1, 3];\n    let firsts: Vec[bool] = v\n        .iter()\n        .scan([0; 4], |seen, x| {\n            seen[x] += 1;\n            let first = seen[x] == 1;\n            Some(first)\n        })\n        .collect();\n    println(firsts);\n}\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(args)
            .current_dir(&dir)
            .stdin(Stdio::null())
            .output()
            .expect("karac");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr),
        )
    };
    let (_, first) = run(&["check", "cell.kara"]);
    assert!(
        first.contains(
            "this closure writes the state `seen` through `seen[..]`, as Rust's `scan` does"
        ),
        "{first}"
    );
    for _ in 0..4 {
        run(&["fix", "cell.kara"]);
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let (clean, _) = run(&["check", "cell.kara"]);
    let (_, out) = run(&["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.contains("let mut seen = seen;") && text.contains("Some((seen, first))"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "[true, true, false, true]", "{text}");
}
