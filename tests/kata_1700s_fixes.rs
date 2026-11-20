//! Fixes found by katas 1708 to 1738, each pinned by the smallest
//! program that shows it.

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-k1700-{name}-{}", std::process::id()));
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

/// `isqrt` on the integers up to 64 bits, Rust's floor of the square
/// root (kata 1714's ★ and memo arms; B-2026-11-20-44).
#[test]
fn isqrt_is_the_floor_of_the_square_root() {
    runs("isqrt", "fn main() {\n    let a: i64 = 99;\n    let b: u64 = 18446744073709551615;\n    let c: i32 = 2147483647;\n    let d: u8 = 255;\n    let e: i64 = 9223372036854775807;\n    println(f\"{a.isqrt()} {b.isqrt()} {c.isqrt()} {d.isqrt()} {e.isqrt()}\");\n}\n", "9 4294967295 46340 15 3037000499\n");
}

/// A negative signed `isqrt` panics with Rust's message (B-2026-11-20-44).
#[test]
fn a_negative_isqrt_panics() {
    let f = program("isqrt_neg", "fn root(x: i64) -> i64 {\n    x.isqrt()\n}\nfn main() {\n    println(root(16));\n    println(root(-4));\n}\n");
    let (ok, out, err) = karac(&["run", "--interp"], &f);
    assert!(!ok);
    assert_eq!(out, "4\n");
    assert!(
        err.contains("argument of integer square root cannot be negative"),
        "{err}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn isqrt_is_the_floor_of_the_square_root_compiled() {
    let f = program("isqrt_llvm", "fn main() {\n    let a: i64 = 99;\n    let b: u64 = 18446744073709551615;\n    let c: i32 = 2147483647;\n    let d: u8 = 255;\n    let e: i64 = 9223372036854775807;\n    println(f\"{a.isqrt()} {b.isqrt()} {c.isqrt()} {d.isqrt()} {e.isqrt()}\");\n}\n");
    let (ok, out, err) = karac(&["__mir-llvm-run"], &f);
    assert!(ok, "{err}");
    assert_eq!(out, "9 4294967295 46340 15 3037000499\n");
    let f = program("isqrt_llvm_neg", "fn root(x: i64) -> i64 {\n    x.isqrt()\n}\nfn main() {\n    println(root(16));\n    println(root(-4));\n}\n");
    let (ok, out, err) = karac(&["__mir-llvm-run"], &f);
    assert!(!ok);
    assert_eq!(out, "4\n");
    assert!(
        err.contains("argument of integer square root cannot be negative"),
        "{err}"
    );
}

/// `max()` straight on a collection is one error, `.iter()` named and
/// fixed, without a second one saying the element must be numeric
/// (kata 1708's windows arm; B-2026-11-20-41).
#[test]
fn max_on_a_collection_is_one_error_and_fixed() {
    let f = program("win_check", "fn largest(nums: Vec[i32], k: i64) -> Vec[i32] {\n    nums.windows(k).max().unwrap().to_vec()\n}\nfn main() {\n    println(f\"{largest([1, 4, 5, 2, 3], 3)}\");\n}\n");
    let (ok, out, err) = karac(&["check"], &f);
    assert!(!ok);
    let all = format!("{out}{err}");
    assert!(all.contains("1 error(s) found"), "{all}");
    assert!(!all.contains("requires a numeric"), "{all}");
    fixes_to("win_fix", "fn largest(nums: Vec[i32], k: i64) -> Vec[i32] {\n    nums.windows(k).max().unwrap().to_vec()\n}\nfn main() {\n    println(f\"{largest([1, 4, 5, 2, 3], 3)}\");\n}\n", "nums.windows(k).iter().max()");
    let f = program("win_run", "fn largest(nums: Vec[i32], k: i64) -> Vec[i32] {\n    nums.windows(k).max().unwrap().to_vec()\n}\nfn main() {\n    println(f\"{largest([1, 4, 5, 2, 3], 3)}\");\n}\n");
    let _ = karac(&["fix"], &f);
    let (ok, out, err) = karac(&["run", "--interp"], &f);
    assert!(ok, "{err}");
    assert_eq!(out, "[5, 2, 3]\n");
}

/// A cast that widens by itself, `ans += (hi - lo) as i64`, is dropped
/// with its parentheses (kata 1712's Rust solution; B-2026-11-20-42).
#[test]
fn a_dropped_whole_value_cast_takes_its_parentheses() {
    fixes_to("paren", "fn count(v: Vec[i32]) -> i64 {\n    let mut ans: i64 = 0;\n    for i in 0..v.len() {\n        let hi = v[i];\n        let lo = 1i32;\n        ans += (hi - lo) as i64;\n    }\n    ans\n}\nfn main() {\n    println(count([3, 5]));\n}\n", "ans += hi - lo;");
}

/// A `binary_search` match whose `Ok` arm is empty becomes one `if not`,
/// not an empty branch and an `else` (kata 1713's Rust solution;
/// B-2026-11-20-43).
#[test]
fn an_empty_binary_search_arm_is_left_out() {
    fixes_to("bs_empty", "fn longest(places: Vec[i64]) -> i64 {\n    let mut tails: Vec[i64] = Vec.new();\n    for p in places.iter() {\n        let p = *p;\n        match tails.binary_search(&p) {\n            Ok(_) => {}\n            Err(i) => {\n                if i == tails.len() {\n                    tails.push(p);\n                } else {\n                    tails[i] = p;\n                }\n            }\n        }\n    }\n    tails.len()\n}\nfn main() {\n    println(longest([3, 1, 2, 2, 5, 4]));\n}\n", "if not (");
    let f = program("bs_empty_run", "fn longest(places: Vec[i64]) -> i64 {\n    let mut tails: Vec[i64] = Vec.new();\n    for p in places.iter() {\n        let p = *p;\n        match tails.binary_search(&p) {\n            Ok(_) => {}\n            Err(i) => {\n                if i == tails.len() {\n                    tails.push(p);\n                } else {\n                    tails[i] = p;\n                }\n            }\n        }\n    }\n    tails.len()\n}\nfn main() {\n    println(longest([3, 1, 2, 2, 5, 4]));\n}\n");
    let _ = karac(&["fix"], &f);
    let fixed = std::fs::read_to_string(&f).unwrap();
    assert!(!fixed.contains("{} else"), "{fixed}");
    let (ok, out, err) = karac(&["run", "--interp"], &f);
    assert!(ok, "{err}");
    assert_eq!(out, "3\n");
}

/// Counters that meet the `i32` total only through a free `min`,
/// `total += min(firsts, seconds) * less`, are declared `i32` by the fix
/// (kata 1717's counts arm; B-2026-11-20-45).
#[test]
fn literals_read_through_a_free_min_are_declared() {
    fixes_to("minlit", "fn score(s: Str, x: i32, y: i32) -> i32 {\n    let mut total: i32 = 0;\n    let mut firsts = 0;\n    let mut seconds = 0;\n    for c in s.chars() {\n        if c == 'a' {\n            firsts += 1;\n        } else {\n            seconds += 1;\n        }\n    }\n    total += min(firsts, seconds) * x.max(y);\n    total\n}\nfn main() {\n    println(score(\"abab\", 4, 5));\n}\n", "let mut firsts: i32 = 0;");
    let f = program("minlit_run", "fn score(s: Str, x: i32, y: i32) -> i32 {\n    let mut total: i32 = 0;\n    let mut firsts = 0;\n    let mut seconds = 0;\n    for c in s.chars() {\n        if c == 'a' {\n            firsts += 1;\n        } else {\n            seconds += 1;\n        }\n    }\n    total += min(firsts, seconds) * x.max(y);\n    total\n}\nfn main() {\n    println(score(\"abab\", 4, 5));\n}\n");
    let _ = karac(&["fix"], &f);
    let (ok, out, err) = karac(&["run", "--interp"], &f);
    assert!(ok, "{err}");
    assert_eq!(out, "10\n");
}

/// A `scan` whose items are its state, reduced to one and bound by a
/// `let`, is the seed's type: a narrowing at its use suffixes the seed
/// (kata 1732's scan arm; B-2026-11-20-47).
#[test]
fn a_scan_seed_takes_the_suffix_of_its_reduction() {
    fixes_to(
        "scan_seed",
        "fn highest(gain: Vec[i32]) -> i32 {\n    let top = gain.iter().scan(0, |a, g| { let a = a + g; Some((a, a)) }).max().unwrap();\n    max(0, top)\n}\nfn main() {\n    println(highest([-5, 1, 5, 0, -7]));\n}\n",
        ".scan(0i32, ",
    );
}

/// The item of a `let` bound to a search over a literal range carries
/// the range to its uses: a narrowing push suffixes the range (kata
/// 1720's differential; B-2026-11-20-46).
#[test]
fn a_range_item_let_takes_the_ranges_suffix() {
    fixes_to(
        "range_let",
        "fn main() {\n    let mut arr: Vec[i32] = [3];\n    let next = (0..16).find(|v| arr[0] ^ v == 5).unwrap();\n    arr.push(next);\n    println(arr);\n}\n",
        "(0i32..16)",
    );
}

const TUPLE_BRANCHES: &str = "fn count(xs: Vec[i32]) -> i32 {\n    let (_, n) = xs.iter().map(|x| x + 0).fold((0, 0i32), |(best, n), side| if side > best { (side, n + 1) } else { (best, n) });\n    n\n}\nfn main() {\n    println(count([1, 2, 3]));\n    let side: i32 = 5;\n    let mut acc = (0, 0i32);\n    acc = if side > acc.0 { (side, acc.1 + 1) } else { (acc.0, acc.1) };\n    println(acc);\n    let mut c: (i64, i32) = (0, 0);\n    c = if side > 2 { (side, 1) } else { (7, 2) };\n    println(c);\n}\n";

/// An `if` of tuple literals is built at the widths of the slot it
/// fills, a `fold` accumulator or an assigned tuple, as a `let`'s is
/// (kata 1725's fold arm; B-2026-11-20-48).
#[test]
fn tuple_literal_branches_are_built_at_the_slots_widths() {
    runs("tuple_branches", TUPLE_BRANCHES, "3\n(5, 1)\n(5, 1)\n");
}

/// The same compiled, where a closure typed by its branches returned an
/// `(i32, i32)` that `fold` read as an `(i64, i32)` and counted 0
/// (B-2026-11-20-48).
#[cfg(feature = "llvm")]
#[test]
fn tuple_literal_branches_are_built_at_the_slots_widths_compiled() {
    let f = program("tuple_branches_llvm", TUPLE_BRANCHES);
    let (ok, out, err) = karac(&["__mir-llvm-run"], &f);
    assert!(ok, "{err}");
    assert_eq!(out, "3\n(5, 1)\n(5, 1)\n");
}

/// A literal that meets a `usize` parameter in the round that makes the
/// parameter an `i64` is not declared a `usize`, which the next round
/// would only turn to a redundant `: i64` (kata 1728's Rust arm;
/// B-2026-11-20-49).
#[test]
fn a_literal_meeting_a_retyped_usize_is_left_undeclared() {
    let f = program(
        "usize_literal",
        "fn start(cells: Vec[i64], jump: i32) -> i64 {\n    let mut mouse = 0;\n    let mut floors = 0;\n    for i in 0..cells.len() {\n        if cells[i] == 1 {\n            mouse = i;\n        }\n        floors += 1;\n    }\n    walk(mouse, floors * 2, jump as usize)\n}\nfn walk(at: usize, limit: usize, jump: usize) -> i64 {\n    ((at + jump) % limit) as i64\n}\nfn main() {\n    println(start([0, 0, 1, 0], 3));\n}\n",
    );
    let (_, _, _) = karac(&["fix"], &f);
    let fixed = std::fs::read_to_string(&f).unwrap();
    assert!(fixed.contains("let mut mouse = 0;"), "{fixed}");
    assert!(!fixed.contains("usize"), "{fixed}");
    let (ok, out, err) = karac(&["check"], &f);
    assert!(ok, "after the fix: {out}{err}\n{fixed}");
}

const SET_COMPARISONS: &str = "fn main() {\n    let a: Set[i32] = Set.from([1, 2, 3]);\n    let b: Set[i32] = Set.from([4, 5]);\n    let c: Set[i32] = Set.from([2, 3]);\n    let e: Set[i32] = Set.new();\n    println(f\"{a.is_disjoint(b)} {a.is_disjoint(c)} {c.is_subset(a)} {a.is_subset(c)} {a.is_superset(c)} {c.is_superset(a)} {e.is_subset(b)} {e.is_disjoint(e)}\");\n    let s: SortedSet[String] = SortedSet.from([\"x\".to_string(), \"y\".to_string()]);\n    let t: SortedSet[String] = SortedSet.from([\"y\".to_string()]);\n    println(f\"{s.is_disjoint(t)} {t.is_subset(s)} {s.is_superset(t)} {t.is_superset(s)}\");\n    let v: Vec[Set[i32]] = [a.clone(), b.clone()];\n    println(v[0].is_disjoint(v[1]) and a.is_superset(a));\n}\n";

/// `Set` and `SortedSet` compare as Rust's and Python's sets do, reading
/// the other set without taking it (kata 1733's ★ and Rust arms;
/// B-2026-11-20-50).
#[test]
fn sets_say_whether_they_are_disjoint_subsets_or_supersets() {
    runs(
        "set_comparisons",
        SET_COMPARISONS,
        "true false true false true false true true\nfalse true true false\ntrue\n",
    );
}

/// The same compiled (B-2026-11-20-50).
#[cfg(feature = "llvm")]
#[test]
fn sets_say_whether_they_are_disjoint_subsets_or_supersets_compiled() {
    let f = program("set_comparisons_llvm", SET_COMPARISONS);
    let (ok, out, err) = karac(&["__mir-llvm-run"], &f);
    assert!(ok, "{err}");
    assert_eq!(
        out,
        "true false true false true false true true\nfalse true true false\ntrue\n"
    );
}

/// Items made from a value a `let` binds to folds seeded with literals,
/// `once(first).chain(..scan(first, ..))` collected into a `Vec[i32]`:
/// the fix gives the folds' seeds the element's suffix, as a narrowing
/// return of `first` does (kata 1734's fold arm; B-2026-11-20-64).
#[test]
fn a_collect_from_a_fold_seeded_let_fixes_the_seeds() {
    fixes_to(
        "once_fold_seeds",
        "fn decode(encoded: Vec[i32]) -> Vec[i32] {\n    let n = encoded.len() as i32 + 1;\n    let all = (1..n + 1).fold(0, |x, v| x ^ v);\n    let rest = encoded.iter().skip(1).step_by(2).fold(0, |x, c| x ^ c);\n    let first = all ^ rest;\n    once(first).chain(encoded.iter().scan(first, |last, c| Some((last ^ c, last ^ c)))).collect()\n}\nfn main() {\n    println(decode([6, 5, 4, 6]));\n}\n",
        "fold(0i32, |x, c|",
    );
}

/// An element read back out of a vector that fill elements were pushed
/// into, `v[k - 1]` after `v.push(pre[i][j])` with `pre` a fill of fills
/// of `0`, is fixed on the fill's literal (kata 1738's Rust arm;
/// B-2026-11-20-69).
#[test]
fn an_element_pushed_from_a_fill_fixes_the_fills_literal() {
    fixes_to(
        "pushed_fill",
        "fn kth(matrix: Vec[Vec[i32]], k: i64) -> i32 {\n    let (m, n) = (matrix.len(), matrix[0].len());\n    let mut pre = Vec.filled(m + 1, Vec.filled(n + 1, 0));\n    let mut v = Vec.with_capacity(m * n);\n    for i in 1..m + 1 {\n        for j in 1..n + 1 {\n            pre[i][j] = pre[i - 1][j] ^ pre[i][j - 1] ^ pre[i - 1][j - 1] ^ matrix[i - 1][j - 1];\n            v.push(pre[i][j]);\n        }\n    }\n    v.sort();\n    v[v.len() - k]\n}\nfn main() {\n    println(kth([[5, 2], [1, 6]], 1));\n}\n",
        "Vec.filled(n + 1, 0i32)",
    );
}
