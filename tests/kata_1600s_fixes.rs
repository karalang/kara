//! Fixes found by katas 1632 to 1660, each pinned by the program that
//! found it or the smallest one that shows it.

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-k1600-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

fn karac(args: &[&str], f: &PathBuf) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(f)
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn runs(name: &str, src: &str, want: &str) {
    let f = program(name, src);
    let (ok, out, err) = karac(&["run", "--interp"], &f);
    assert!(ok, "{name}: {err}");
    assert_eq!(out, want, "{name}");
}

/// `karac fix` on `src` leaves a program that checks clean and holds
/// `want`.
fn fixes_to(name: &str, src: &str, want: &str) {
    let f = program(name, src);
    let (_, _, _) = karac(&["fix"], &f);
    let fixed = std::fs::read_to_string(&f).unwrap();
    assert!(fixed.contains(want), "{name}: the fix left\n{fixed}");
    let (ok, out, err) = karac(&["check"], &f);
    assert!(ok, "{name}: after the fix: {out}{err}\n{fixed}");
}

/// CN-7: after `const`, a one-letter name is Const class, at module scope
/// and in a block (kata 1649's Rust template); a lowercase one is not.
#[test]
fn a_one_letter_const_is_a_constant() {
    runs(
        "const1",
        "const N: i64 = 3;\nfn main() {\n    const M: i64 = N + 1;\n    let mut i = 0;\n    while i <= N {\n        i += 1;\n    }\n    println(i + M);\n}\n",
        "8\n",
    );
    let f = program(
        "const2",
        "const n: i64 = 3;\nfn main() {\n    println(n);\n}\n",
    );
    let (ok, _, _) = karac(&["check"], &f);
    assert!(!ok, "a lowercase one-letter const is still refused");
}

/// `v = v` moved the value out after dropping it, which the move check
/// refused as a use of a moved value.
#[test]
fn a_binding_assigned_to_itself_keeps_its_value() {
    runs(
        "selfassign",
        "fn main() {\n    let mut v = [1, 2];\n    v = v;\n    v.push(3);\n    println(v);\n}\n",
        "[1, 2, 3]\n",
    );
}

/// A `Some(node)` inside a pushed tuple, `node` a borrowed shared handle,
/// is copied into an `Option[Node]` slot, as `Some(node)` pushed alone is
/// (kata 1660).
#[test]
fn a_borrowed_handle_in_a_pushed_tuple_is_copied_in() {
    runs(
        "tuplesome",
        "shared struct N {\n    mut v: i32,\n}\nfn main() {\n    let level = [N { v: 1 }, N { v: 2 }];\n    let mut below: Vec[(i32, Option[N])] = Vec.new();\n    for n in level.iter() {\n        below.push((n.v, Some(n)));\n    }\n    level[0].v = 9;\n    println(below[0].1.unwrap().v + below[1].0);\n}\n",
        "11\n",
    );
}

/// The literal a `Vec.filled` fill took reaches the refusal through an
/// `if` whose branches read the fill (kata 1652).
#[test]
fn a_fill_literal_is_fixed_through_an_if() {
    fixes_to(
        "ifcopy",
        "fn sums(code: Vec[i32], k: i64) -> Vec[i32] {\n    let n = code.len();\n    let mut prefix = Vec.filled(n + 1, 0);\n    for i in 0..n {\n        prefix[i + 1] = prefix[i] + code[i];\n    }\n    let mut answer: Vec[i32] = Vec.new();\n    for i in 0..n {\n        let sum = if k > 0 {\n            prefix[n] - prefix[i]\n        } else {\n            0\n        };\n        answer.push(sum);\n    }\n    answer\n}\nfn main() {\n    println(sums([1, 2, 3], 1));\n}\n",
        "Vec.filled(n + 1, 0i32)",
    );
}

/// A tuple's literal reaches a refusal through `while let` popping the
/// queue it was pushed into, and through a tuple array built from a
/// binding that read one (kata 1654).
#[test]
fn a_tuple_literal_is_fixed_through_pops_and_rebuilt_tuples() {
    fixes_to(
        "whilelet",
        "fn jumps(forbidden: own Vec[i32], a: i32, x: i32) -> i32 {\n    let forbidden: Set[i32] = forbidden.into_iter().collect();\n    let mut queue = VecDeque.new();\n    queue.push_back((0, false, 0));\n    while let Some((pos, back, steps)) = queue.pop_front() {\n        if pos == x {\n            return steps;\n        }\n        let next = pos + a;\n        if next <= 20 and not back and not forbidden.contains(next) {\n            queue.push_back((next, false, steps + 1));\n        }\n    }\n    -1\n}\nfn main() {\n    println(jumps([3], 2, 4));\n}\n",
        "push_back((0i32, false, 0))",
    );
    fixes_to(
        "levels",
        "fn jumps(a: i32) -> i32 {\n    let blocked: Set[i32] = Set.new();\n    let mut frontier = [(0, false)];\n    let mut n = 0;\n    while n < 3 {\n        let mut next = Vec.new();\n        for (p, back) in frontier {\n            let moves = [(p + a, back)];\n            for (q, dir) in moves {\n                if not blocked.contains(q) {\n                    next.push((q, dir));\n                }\n            }\n        }\n        frontier = next;\n        n += 1;\n    }\n    n\n}\nfn main() {\n    println(jumps(2));\n}\n",
        "[(0i32, false)]",
    );
}

/// A `match` arm whose call's value is not wanted beside a block arm is
/// fixed to discard it (kata 1632).
#[test]
fn a_mismatched_call_arm_is_fixed_to_discard_its_value() {
    fixes_to(
        "discard",
        "fn main() {\n    let mut m: Map[i64, i64] = Map.new();\n    for r in 0..3 {\n        match m.get(r % 2) {\n            Some(v) => {\n                println(v);\n            }\n            None => m.insert(r % 2, r),\n        }\n    }\n    println(m.len());\n}\n",
        "None => { m.insert(r % 2, r); }",
    );
}

/// `.unwrap()` on a `weak` read warns once where an assignment's value is
/// typed twice (kata 1650).
#[test]
fn a_weak_read_unwrap_in_an_assignment_warns_once() {
    let f = program(
        "weak",
        "shared struct Node {\n    mut val: i32,\n    mut parent: weak Node,\n}\nfn main() {\n    let root = Node { val: 1, parent: None };\n    let child = Node { val: 2, parent: None };\n    child.parent = root;\n    let mut at = child;\n    at = at.parent.unwrap();\n    println(at.val);\n}\n",
    );
    let (_, out, err) = karac(&["check"], &f);
    let all = format!("{out}{err}");
    assert_eq!(all.matches("weak_read_unwrap").count(), 1, "{all}");
}

/// Rust's typed `None`, `None::<T>` (kata 1660's template), is named and
/// fixed to Kāra's `Option[T].None`.
#[test]
fn a_none_turbofish_is_fixed_to_the_typed_none() {
    fixes_to(
        "none_turbofish",
        "fn main() {\n    let mut q = Vec.new();\n    q.push((1i64, None::<i64>));\n    q.push((2i64, Some(3i64)));\n    println(q.len());\n}\n",
        "Option[i64].None",
    );
}

/// A `Slice` slot whose element is still an open literal takes the
/// element of the `Vec` handed to it: `[1, 2, 3, 4].starts_with(piece)`
/// (kata 1640's scan arm, probed on unsuffixed literals).
#[test]
fn a_vec_passed_to_an_open_literal_slice_slot_coerces() {
    runs(
        "starts_open",
        "fn main() {\n    let arr = [1, 2, 3, 4];\n    let piece = [2, 3];\n    let typed: Vec[i64] = [1, 2];\n    println(arr[1..].starts_with(piece));\n    println(arr.starts_with(typed));\n    println(arr.ends_with(piece));\n}\n",
        "true\ntrue\nfalse\n",
    );
}

/// A list of string literals indexed where a `Str` is wanted reads a
/// `Str` (kata 1662's arm names, `["a", "b"][k]` from a `-> Str`); before,
/// the element was a `String`, and the `.as_str()` fix led to E0509.
#[test]
fn an_indexed_list_of_string_literals_is_a_str() {
    runs(
        "str_list_index",
        "fn name(k: i64) -> Str {\n    [\"a\", \"bc\"][k]\n}\nfn main() {\n    let s: Str = [\"x\", \"y\", \"z\"][2];\n    println(f\"{name(1)} {s}\");\n}\n",
        "bc z\n",
    );
}

/// The `from_iter` fix for a `collect` compared with a typed value, on a
/// parenthesized range receiver: the edits cut `.collect()` from the
/// call's end, not from the range's span, which stops short of `n)`
/// (kata 1666's differential, `seen != (0..n).collect()`).
#[test]
fn a_compared_collect_over_a_parenthesized_range_is_fixed() {
    fixes_to(
        "collect_paren_range",
        "fn main() {\n    let seen: Vec[i64] = [0, 1, 2];\n    let n = 3;\n    if seen != (0..n).collect() {\n        println(\"no\");\n    }\n}\n",
        "Vec[i64].from_iter(0..n)",
    );
}

/// `a.cmp(b) as i32`, Rust's -1 / 0 / 1, is refused (Kāra's `Ordering`
/// holds no numbers) with a fix that writes the match (kata 1686's Rust
/// solution, B-2026-11-20-38).
#[test]
fn an_ordering_cast_to_an_integer_is_fixed_to_a_match() {
    let src = "fn sign(a: i32, b: i32) -> i32 {\n    a.cmp(b) as i32\n}\nfn main() {\n    println(f\"{sign(1, 2)} {sign(2, 2)} {sign(3, 2)}\");\n}\n";
    fixes_to(
        "ordering_cast",
        src,
        "match a.cmp(b) { Ordering.Less => -1i32, Ordering.Equal => 0i32, Ordering.Greater => 1i32 }",
    );
    let f = program("ordering_cast_run", src);
    let _ = karac(&["fix"], &f);
    let (ok, out, err) = karac(&["run", "--interp"], &f);
    assert!(ok, "{err}");
    assert_eq!(out, "-1 0 1\n");
}

/// A `String` extended by a slice of `char`s, Rust's `res.extend(&d[a..b])`
/// (kata 1694's Rust solution): `karac fix` drops the `&`, then reads the
/// slice through `.iter()`, and the program runs (B-2026-11-20-39).
#[test]
fn a_string_extended_by_a_char_slice_is_fixed_to_iter() {
    let src = "fn main() {\n    let d: Vec[char] = \"12345\".chars().collect();\n    let mut res = String.new();\n    res.extend(&d[1..3]);\n    res.push('-');\n    res.extend(&d[3..]);\n    println(res);\n}\n";
    fixes_to("extiter", src, "res.extend(d[1..3].iter());");
    let f = program("extiter2", src);
    let _ = karac(&["fix"], &f);
    let (ok, out, err) = karac(&["run", "--interp"], &f);
    assert!(ok, "{err}");
    assert_eq!(out, "23-45\n");
}

/// An inner `fold` whose `0` seed the outer `max` leaves at the default
/// `i64` while its items are `i32` (kata 1672's fold arm): the refusal
/// names the seed and `karac fix` writes `0i32` (B-2026-11-20-33).
#[test]
fn an_inner_fold_seed_is_fixed_to_its_items_type() {
    let src = "fn wealth(rows: Vec[Vec[i32]]) -> i32 {\n    rows.iter().fold(0, |richest, row| max(richest, row.iter().fold(0, |sum, money| sum + money)))\n}\nfn main() {\n    println(wealth([[1, 2, 3], [3, 2, 2]]));\n}\n";
    fixes_to(
        "inner_fold",
        src,
        "row.iter().fold(0i32, |sum, money| sum + money)",
    );
    let f = program("inner_fold_run", src);
    let _ = karac(&["fix"], &f);
    let (ok, out, err) = karac(&["run", "--interp"], &f);
    assert!(ok, "{err}");
    assert_eq!(out, "7\n");
}

/// `PriorityQueue.min()`, a direction guessed into the constructor's name
/// (kata 1673): the refusal names `PriorityQueue.new()`, the queue that
/// pops the smallest first, and `karac fix` writes it (B-2026-11-20-34).
#[test]
fn a_guessed_priority_queue_constructor_is_fixed() {
    let src = "fn main() {\n    let mut heap: PriorityQueue[i64] = PriorityQueue.min();\n    heap.push(3);\n    heap.push(1);\n    println(heap.pop().unwrap());\n}\n";
    fixes_to("pq_min", src, "PriorityQueue.new()");
    let f = program("pq_min_run", src);
    let _ = karac(&["fix"], &f);
    let (ok, out, err) = karac(&["run", "--interp"], &f);
    assert!(ok, "{err}");
    assert_eq!(out, "1\n");
}

/// A `for` element copied out of `nums.iter()` and pushed into a queue of
/// `ref i32` lends the reference into `nums`, not a borrow of the loop's
/// copy, so the loop variable is not "written while borrowed" (kata 1675's
/// mutant M1, B-2026-11-20-35). Check only: the queue's sift of a reference
/// element does not lower yet (PREDICTS B-2026-11-20-40).
#[test]
fn a_copied_for_element_pushed_as_a_reference_checks() {
    let src = "fn largest(nums: Vec[i32]) -> i32 {\n    let mut queue = PriorityQueue.max_first();\n    for x in nums.iter() {\n        let start = x;\n        queue.push(start);\n    }\n    *queue.pop().unwrap()\n}\nfn main() {\n    println(largest([3, 8, 5]));\n}\n";
    let f = program("for_ref_elem", src);
    let (ok, out, err) = karac(&["check"], &f);
    assert!(ok, "{out}{err}");
}

/// A row of a fill of fills walked by `iter().max()`, `*dp[0][n -
/// 1].iter().max().unwrap()` returned as `i32` (kata 1682's Rust
/// solution): the fill's literal is read one level below the row, and
/// `karac fix` writes `0i32` (B-2026-11-20-36).
#[test]
fn a_walked_row_of_a_fill_of_fills_fixes_its_literal() {
    let src = "fn best(n: i64) -> i32 {\n    let mut dp = Vec.filled(n, Vec.filled(n, Vec.filled(3, 0)));\n    for i in 0..n {\n        let mut inner = 0;\n        inner = inner.max(dp[i][0][1]);\n        dp[i][n - 1][2] = inner + 2;\n    }\n    *dp[0][n - 1].iter().max().unwrap()\n}\nfn main() {\n    println(best(2));\n}\n";
    fixes_to("fill_walk", src, "Vec.filled(3, 0i32)");
    let f = program("fill_walk_run", src);
    let _ = karac(&["fix"], &f);
    let (ok, out, err) = karac(&["run", "--interp"], &f);
    assert!(ok, "{err}");
    assert_eq!(out, "2\n");
}

/// A fill of fills of `None`, `Vec.filled(2, Vec.filled(2, None))`, whose
/// element a later write decides (kata 1690's memo arm): the inner fill
/// takes its type from the outer one, as a single fill does, where it was
/// an internal E0516 (B-2026-11-20-37).
#[test]
fn a_fill_of_fills_of_none_takes_its_type_from_a_use() {
    runs(
        "fill_none",
        "fn put(memo: mut ref Vec[Vec[Option[i32]]]) {\n    memo[0][1] = Some(3);\n}\nfn main() {\n    let mut memo = Vec.filled(2, Vec.filled(2, None));\n    put(mut memo);\n    memo[1][0] = Some(4i32);\n    println(f\"{memo[0][1].unwrap_or(0)} {memo[1][0].unwrap_or(0)} {memo[1][1].unwrap_or(0)}\");\n}\n",
        "3 4 0\n",
    );
}
