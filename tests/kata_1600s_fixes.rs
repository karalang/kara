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
