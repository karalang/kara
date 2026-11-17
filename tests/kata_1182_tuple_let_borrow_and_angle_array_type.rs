//! Six gaps found by kata 1182's (Shortest Distance to Target Color) Rust
//! arms.
//!
//! B-2026-11-17-103: a tuple `let (i, spots) = (q[0], at[q[1] - 1]);` over a
//! non-`Copy` element was refused with a `.clone()` fix, a copy of the whole
//! element on every query, while `let spots = at[q[1] - 1];` gets the `ref`
//! borrow when the binding is only read. The tuple's part now gets it too.
//!
//! B-2026-11-17-104: `Vec<[Option<usize>; 4]>` stopped the scan of Rust's
//! angle brackets at the `;` of the array type, so the `Vec[..]` rewrite was
//! dropped, and recovery offered a fix writing `[i64; 4;]` that broke the
//! file. A `;` inside `[..]` no longer ends the scan.
//!
//! B-2026-11-17-105: Rust's `|last, (i, c)| { last[c] = Some(i);
//! Some(*last) }` scan closure got a "drop the `*`" error, not the rewrite
//! to Kāra's `scan` shape a state written only through `*last` or only
//! through `last[..]` gets.
//!
//! B-2026-11-17-106: a generic call whose closure returns `(state, item)`,
//! with the state seeded by `None`, `Vec.new()` or `[None; 4]` and the item
//! decided only by the closure, was refused ("expected 'Option[(?T0, _)]',
//! found 'Option[(Option[i64], i64)]'"), and `[None; 4]` kept its enum's
//! own `T`.
//!
//! B-2026-11-17-107: `Vec[Array[Option[usize], 4]]` meeting `i64` indices
//! got no fix to `i64`, which `Vec[Option[usize]]` gets.
//!
//! B-2026-11-17-108: `let k = match p.binary_search(i) { .. };` was
//! rewritten through `partition_point` with its later lines indented to the
//! `match`'s column, not the `let`'s.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> (Output, String) {
    let path =
        std::env::temp_dir().join(format!("karac-kata1182-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let after = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    (out, after)
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

const TUPLE_LET: &str = "fn nearest(at: Vec[Vec[i64]], q: Vec[i64]) -> i64 {\n\
    let (i, spots) = (q[0], at[q[1]]);\n\
    if spots.is_empty() { -1 } else { spots[0] - i }\n\
    }\n\
    fn main() {\n\
    let at: Vec[Vec[i64]] = [[4, 9], [], [7]];\n\
    let mut total = 0;\n\
    for q in [[1, 0], [2, 1], [3, 2]] {\n\
    let (k, row) = (q[1], at[q[1]]);\n\
    total += k + row.len();\n\
    total += nearest(at, q);\n\
    }\n\
    println(f\"{total}\");\n\
    }\n";

#[test]
fn a_tuple_let_part_that_is_only_read_borrows_the_element() {
    let (out, _) = karac("tuple-check", &["check"], TUPLE_LET);
    assert!(!out.status.success(), "{}", text(&out));
    let t = text(&out);
    assert!(
        t.contains("the binding is only read, so the fix borrows the element"),
        "{t}"
    );
    let (out, after) = karac("tuple-fix", &["fix"], TUPLE_LET);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        after.contains("let (i, spots) = (q[0], ref at[q[1]]);"),
        "{after}"
    );
    assert!(
        after.contains("let (k, row) = (q[1], ref at[q[1]]);"),
        "{after}"
    );
    assert!(!after.contains(".clone()"), "{after}");
    let (out, _) = karac("tuple-run", &["__mir-run"], &after);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "12\n");
}

#[test]
fn a_tuple_let_part_whose_collection_changes_keeps_the_copy() {
    let src = "fn main() {\n\
               let mut at: Vec[Vec[i64]] = [[1, 2], [3]];\n\
               let (i, row) = (0, at[0]);\n\
               at.push([9]);\n\
               println(f\"{i} {row.len()} {at.len()}\");\n\
               }\n";
    let (out, after) = karac("tuple-written", &["fix"], src);
    assert!(out.status.success(), "{}", text(&out));
    assert!(after.contains("(0, at[0].clone())"), "{after}");
    let (out, _) = karac("tuple-written-run", &["__mir-run"], &after);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "0 2 3\n");
}

#[test]
fn rust_angle_brackets_around_an_array_type_are_rewritten() {
    let src = "fn main() {\n\
               let a: Vec<[i64; 4]> = Vec.new();\n\
               println(f\"{a.len()}\");\n\
               }\n";
    let (out, _) = karac("angle-array-check", &["check"], src);
    let t = text(&out);
    assert!(
        !t.contains("expected `;` after this expression statement"),
        "{t}"
    );
    let (_, once) = karac("angle-array-fix", &["fix"], src);
    assert!(!once.contains("4;]"), "{once}");
    let (_, twice) = karac("angle-array-fix2", &["fix"], &once);
    assert!(
        twice.contains("let a: Vec[Array[i64, 4]] = Vec.new();"),
        "{twice}"
    );
    let (out, _) = karac("angle-array-run", &["__mir-run"], &twice);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "0\n");
}

/// `check` refuses with `want`; `karac fix` then writes `fixed`, after which
/// the program checks and prints `prints`.
fn refused_then_fixed(tag: &str, src: &str, want: &str, fixed: &str, prints: &str) {
    let (out, _) = karac(&format!("{tag}-check"), &["check"], src);
    assert!(!out.status.success(), "{tag}: {}", text(&out));
    assert!(text(&out).contains(want), "{tag}: {}", text(&out));
    let (out, after) = karac(&format!("{tag}-fix"), &["fix"], src);
    assert!(out.status.success(), "{tag}: {}", text(&out));
    assert!(after.contains(fixed), "{tag}: {after}");
    let (out, _) = karac(&format!("{tag}-run"), &["__mir-run"], &after);
    assert!(out.status.success(), "{tag}: {}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), prints, "{tag}");
}

#[test]
fn rusts_scan_writing_an_array_state_and_handing_it_out_with_a_deref_is_rewritten() {
    refused_then_fixed(
        "scan-array-deref",
        "fn main() {\n\
         let colors = [1, 3, 2, 3];\n\
         let left: Vec[Array[i64, 4]] = colors\n\
         .iter()\n\
         .enumerate()\n\
         .scan([-1; 4], |last, (i, c)| {\n\
         last[c] = i;\n\
         Some(*last)\n\
         })\n\
         .collect();\n\
         println(f\"{left[3]}\");\n\
         }\n",
        "this closure writes the state `last` through `last[..]`, as Rust's `scan` does",
        "|last, (i, c)| { let mut last = last; last[c] = i; Some((last, last)) }",
        "[-1, 0, 2, 3]\n",
    );
}

#[test]
fn a_closure_returning_the_state_solves_a_state_seeded_with_none() {
    let src = "fn step[S, O, F: Fn(own S) -> Option[(S, O)]](s: own S, f: own F) -> O {\n\
               match f(s) {\n\
               Some((_, o)) => o,\n\
               None => panic(\"none\"),\n\
               }\n\
               }\n\
               fn main() {\n\
               let a = step(None, |s| Some((Some(1), 5)));\n\
               let b = step(Vec.new(), |s| { let mut s = s; s.push(1); Some((s, 6)) });\n\
               let c: Vec[Option[i64]] = [1, 2].iter().scan(None, |s, x| Some((Some(*x), s))).collect();\n\
               let d: Vec[Array[Option[i64], 2]] = [1, 2]\n\
               .iter()\n\
               .scan([None; 2], |s, x| { let mut s = s; s[1] = Some(*x); Some((s, s)) })\n\
               .collect();\n\
               println(f\"{a} {b} {c} {d[1]}\");\n\
               }\n";
    let (out, _) = karac("none-state-check", &["check"], src);
    let t = text(&out);
    assert!(!t.contains("expected 'Option[("), "{t}");
    assert!(!t.contains("cannot infer type parameter"), "{t}");
    let (_, fixed) = karac("none-state-fix", &["fix"], src);
    let (out, _) = karac("none-state-run", &["__mir-run"], &fixed);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "5 6 [None, Some(1)] [None, Some(2)]\n"
    );
}

#[test]
fn a_usize_inside_an_array_type_that_mixes_with_i64_is_written_i64() {
    refused_then_fixed(
        "usize-in-array",
        "fn main() {\n\
         let left: Vec[Array[Option[usize], 2]] = [1, 0]\n\
         .iter()\n\
         .enumerate()\n\
         .scan([None; 2], |s, (i, c)| { let mut s = s; s[c] = Some(i); Some((s, s)) })\n\
         .collect();\n\
         println(f\"{left[1]}\");\n\
         }\n",
        "Kāra writes sizes, lengths, counts and indices as `i64`",
        "let left: Vec[Array[Option[i64], 2]] =",
        "[Some(1), Some(0)]\n",
    );
}

#[test]
fn a_binary_search_rewrite_lines_up_with_its_statement() {
    let src = "fn find(p: Vec[i64], i: i64) -> i64 {\n\
               \x20   let k = match p.binary_search(i) {\n\
               \x20       Ok(_) => {\n\
               \x20           return 0;\n\
               \x20       }\n\
               \x20       Err(k) => k,\n\
               \x20   };\n\
               \x20   k\n\
               }\n\
               fn main() { println(f\"{find([2, 5, 9], 6)} {find([2, 5, 9], 5)}\"); }\n";
    let (out, after) = karac("bsearch-indent", &["fix"], src);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        after.contains("    let k = {\n        let k = p.partition_point(|y| y < i);\n"),
        "{after}"
    );
    assert!(after.contains("\n    };\n    k\n"), "{after}");
    let (out, _) = karac("bsearch-indent-run", &["__mir-run"], &after);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2 0\n");
}
