//! The `karac check` gaps kata 1053 (Previous Permutation With One Swap)
//! found writing its arms the way Rust writes them:
//!
//! - a slice bound by `let` stays borrowed to its last use, not its scope's
//!   end (core-semantics.md §5), so `let tail = v[1..]; let k = tail.len();
//!   v.swap(0, k);` is accepted, while a use that could hand the view on
//!   keeps it to the end of the scope;
//! - a comparison ends the view an operand takes (`v.as_slice() < s`), and
//!   a slice binding's own method (`s.len()` on a `mut Slice`) takes no
//!   second borrow of the source;
//! - a `let ... else` whose value runs a closure reading a binding may move
//!   that binding in its `else` block;
//! - `&v[a..b]` is fixed by dropping the `&`, since a range index is already
//!   a view, and a `Vec` ordered against a `Slice` is fixed with `.as_slice()`.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("karac-k1053-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

/// `check` succeeds and `__mir-run` prints `want`.
fn accepted(tag: &str, src: &str, want: &str) {
    let path = write(tag, src);
    let file = path.to_str().unwrap();
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
        want,
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// `check` fails with `want` in its output.
fn refused(tag: &str, src: &str, want: &str) {
    let path = write(tag, src);
    let check = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    let err = String::from_utf8_lossy(&check.stderr);
    assert!(!check.status.success(), "accepted:\n{src}");
    assert!(err.contains(want), "missing {want:?} in:\n{err}");
}

#[test]
fn a_let_bound_slice_is_borrowed_to_its_last_use() {
    accepted(
        "nll",
        "fn main() {
    let mut out: Vec[i64] = [5, 1, 2, 3];
    let tail = out[1..];
    let k = tail.len();
    let last = tail[k - 1];
    out.swap(0, k);
    println(f\"{out} {last}\");
}
",
        "[3, 1, 2, 5] 3\n",
    );
}

#[test]
fn a_slice_used_after_the_write_or_handed_on_stays_borrowed() {
    let conflict = "cannot be borrowed as `mut ref T` because it is also borrowed as `Slice[T]`";
    refused(
        "used-after",
        "fn main() {
    let mut out: Vec[i64] = [5, 1, 2, 3];
    let tail = out[1..];
    out.push(9);
    println(f\"{tail.len()}\");
}
",
        conflict,
    );
    refused(
        "tuple",
        "fn main() {
    let mut out: Vec[i64] = [5, 1, 2, 3];
    let tail = out[1..];
    let w = (tail, 1);
    out.push(9);
    println(f\"{w.0}\");
}
",
        conflict,
    );
    refused(
        "pushed",
        "fn main() {
    let mut out: Vec[i64] = [5, 1, 2, 3];
    let tail = out[1..];
    let mut keep: Vec[Option[ref i64]] = Vec.new();
    keep.push(tail.first());
    out.push(9);
    println(f\"{keep[0]}\");
}
",
        conflict,
    );
}

#[test]
fn a_mut_slice_s_own_method_takes_no_second_borrow_of_its_source() {
    accepted(
        "mut-view-method",
        "fn main() {
    let mut v: Vec[i64] = [1, 2, 3];
    let s = v.as_slice_mut();
    s[0] = 9;
    let n = s.len();
    s.reverse();
    println(f\"{n} {v}\");
}
",
        "3 [3, 2, 9]\n",
    );
}

#[test]
fn a_comparison_ends_the_view_its_operand_takes() {
    accepted(
        "compare-view",
        "fn main() {
    let arr: Vec[i64] = [3, 1, 2];
    let mut best: Vec[i64] = [0];
    let mut tried: Vec[i64] = [2, 1, 3];
    if tried.as_slice() < arr.as_slice() {
        best = tried;
    }
    println(f\"{best}\");
}
",
        "[2, 1, 3]\n",
    );
}

#[test]
fn a_let_else_may_move_what_its_value_s_closure_reads() {
    accepted(
        "let-else",
        "fn first_big(arr: Slice[i64]) -> Vec[i64] {
    let out = arr.to_vec();
    let Some(i) = (0..out.len()).find(|k| out[k] > 2) else {
        return out;
    };
    println(f\"{i}\");
    out
}

fn main() {
    println(f\"{first_big([1, 3])} {first_big([1, 2])}\");
}
",
        "1\n[1, 3] [1, 2]\n",
    );
    refused(
        "let-else-loop",
        "fn main() {
    let out: Vec[i64] = [1, 2];
    for _ in 0..2 {
        let Some(i) = (0..out.len()).find(|k| out[k] > 5) else {
            let gone = out;
            println(f\"{gone}\");
            continue;
        };
        println(f\"{i}\");
    }
}
",
        "moved inside a loop",
    );
}

#[test]
fn karac_fix_drops_the_borrow_of_a_range_and_views_a_compared_vec() {
    let path = write(
        "fixes",
        "fn main() {
    let out: Vec[i64] = [5, 1, 2, 3];
    let tail = &out[1..];
    let copy: Vec[i64] = [1, 2, 3];
    println(f\"{tail.len()} {copy == tail} {copy < out[1..]}\");
}
",
    );
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    assert!(
        err.contains("`v[a..b]` is already a view of `v`, a `Slice[T]`, so it takes no borrow"),
        "{err}"
    );
    for _ in 0..3 {
        karac(&["fix", file]);
    }
    let text = std::fs::read_to_string(&path).unwrap();
    let clean = karac(&["check", file]);
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(
        text.contains("let tail = out[1..];")
            // `==` between a `Vec` and a `Slice` needs no view
            // (design.md § Slices, Comparison; B-2026-11-13-17).
            && text.contains("{copy == tail}")
            && text.contains("copy.as_slice() < out[1..]"),
        "{text}"
    );
    assert!(
        clean.status.success(),
        "{}\n{text}",
        String::from_utf8_lossy(&clean.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "3 true false\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
