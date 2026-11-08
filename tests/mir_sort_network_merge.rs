//! The merge sort every backend runs for `sort_by`, `sort_by_key`, a
//! `sort` through a user `Ord`, and `String.sorted_by` (B-2026-11-06-34):
//! runs of four sorted by a branch-free network, then each pair of runs
//! merged from both ends at once, and merged again from the front when the
//! two ends took the same element. The interpreter (src/mir/interp.rs
//! `merge_order`), the runtime (runtime/src/mir.rs `merge_widths`) and the
//! emitted sort (src/backend/mir/callable.rs `build_merge_widths`) make the
//! same comparator calls in the same order.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, backend: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-sort-network-{tag}-{backend}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(backend)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn backends() -> &'static [&'static str] {
    if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    }
}

/// `src` prints `want` and exits 0 on every backend.
fn prints(tag: &str, src: &str, want: &str) {
    for b in backends() {
        let out = karac(tag, b, src);
        assert_eq!(
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).as_ref()
            ),
            (Some(0), want),
            "{tag} on {b}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Every size around a run of four and the 1024-element block, few keys
/// and many, ascending, descending, by key and through a user `Ord`: the
/// program counts the neighbours out of order and the ties out of their
/// first order, which must be none.
#[test]
fn sort_network_and_two_ended_merge_sort_stably() {
    const SRC: &str = r#"struct Rev { k: i64, at: i64 }
impl PartialEq for Rev { fn eq(ref self, other: ref Rev) -> bool { self.k == other.k } }
impl Eq for Rev {}
impl PartialOrd for Rev { fn partial_cmp(ref self, other: ref Rev) -> Option[Ordering] { Some(self.cmp(other)) } }
impl Ord for Rev { fn cmp(ref self, other: ref Rev) -> Ordering { other.k.cmp(self.k) } }

fn make(n: i64, seed: i64, keys: i64) -> Vec[(i64, i64)] {
    let mut v: Vec[(i64, i64)] = Vec.new();
    let mut s = seed;
    let mut i = 0;
    while i < n {
        s = (s * 1103515245 + 12345) & 2147483647;
        v.push(((s / 65536) % keys, i));
        i += 1;
    }
    v
}

// Elements out of place: a key out of order, or a tie out of its first order.
fn bad(v: ref Vec[(i64, i64)], up: bool) -> i64 {
    let mut b = 0;
    let mut i = 1;
    while i < v.len() {
        let (k0, a0) = v[i - 1];
        let (k1, a1) = v[i];
        let wrong = if up { k0 > k1 } else { k0 < k1 };
        if wrong or (k0 == k1 and a0 > a1) {
            b += 1;
        }
        i += 1;
    }
    b
}

fn main() {
    let sizes = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 11, 15, 16, 17, 31, 32, 33, 63, 100, 375, 1023, 1024, 1025, 1029, 2048, 2051, 4093];
    let mut wrong = 0;
    for n in sizes {
        for keys in [3, 50, 1000000] {
            let mut a = make(n, n + keys, keys);
            a.sort_by(|x, y| x.0.cmp(y.0));
            wrong += bad(a, true);
            let mut d = make(n, n * 3 + keys, keys);
            d.sort_by(|x, y| y.0.cmp(x.0));
            wrong += bad(d, false);
            let mut kk = make(n, n * 5 + keys, keys);
            kk.sort_by_key(|x| 0 - x.0);
            wrong += bad(kk, false);
            let src = make(n, n * 7 + keys, keys);
            let mut r: Vec[Rev] = src.iter().map(|p| Rev { k: p.0, at: p.1 }).collect();
            r.sort();
            let back: Vec[(i64, i64)] = r.iter().map(|x| (x.k, x.at)).collect();
            wrong += bad(back, false);
        }
    }
    let s = "the quick brown fox jumps over the lazy dog";
    println(s.sorted_by(|a, b| b.cmp(a)));
    println(f"{sizes.len()} sizes, {wrong} out of place");
}
"#;
    prints(
        "stable",
        SRC,
        "zyxwvuuttsrrqpoooonmlkjihhgfeeedcba        
27 sizes, 0 out of place
",
    );
}

/// Comparators that are not an order: one alternating Less and Greater, one
/// drawn at random, one that ignores what it compares. The two ends of a
/// merge then cross (tests/fixtures/sort_schedule/mirror.py counts 1577
/// such merges at 3000 elements), so each result must still hold every
/// element once, and the calls and the order they leave are the mirror's.
/// Up to twenty elements the sort is one insertion sort, so the first four
/// rows changed with it; the schedule is unspecified (docs/library/
/// collections.md, 88642fa4dd), and these rows pin only that the backends
/// and the mirror agree.
#[test]
fn sort_with_a_comparator_that_is_not_an_order_keeps_every_element() {
    const SRC: &str = r#"fn make(n: i64) -> Vec[(i64, i64)] {
    let mut v: Vec[(i64, i64)] = Vec.new();
    let mut i = 0;
    while i < n {
        v.push(((i * 7919) % 101, i));
        i += 1;
    }
    v
}

fn hash(v: ref Vec[(i64, i64)]) -> i64 {
    let mut h = 0;
    for (k, x) in v.iter() {
        h = (h * 31 + k * 1000 + x) % 1000000007;
    }
    h
}

// Each original element exactly once.
fn whole(v: ref Vec[(i64, i64)]) -> bool {
    let mut seen: Vec[bool] = Vec.filled(v.len(), false);
    for (_, x) in v.iter() {
        if seen[x] {
            return false;
        }
        seen[x] = true;
    }
    true
}

fn main() {
    for n in [4, 5, 9, 16, 37, 100, 1025, 3000] {
        let mut a = make(n);
        let mut calls = 0;
        a.sort_by(|x, y| {
            calls += 1;
            if calls % 2 == 0 { Ordering.Less } else { Ordering.Greater }
        });
        let mut b = make(n);
        let mut s = n;
        b.sort_by(|x, y| {
            s = (s * 1103515245 + 12345) & 2147483647;
            if (s / 65536) % 3 == 0 { Ordering.Less } else if (s / 65536) % 3 == 1 { Ordering.Equal } else { Ordering.Greater }
        });
        let mut c = make(n);
        c.sort_by(|x, y| if (x.1 * 7 + y.1) % 5 < 2 { Ordering.Less } else { Ordering.Greater });
        println(f"{n} {calls} {whole(a)} {whole(b)} {whole(c)} {hash(a)} {hash(b)} {hash(c)}");
    }
}
"#;
    prints(
        "odd",
        SRC,
        "4 5 true true true 79527016 22497916 80096956
5 7 true true true 466060576 483068626 484298656
9 15 true true true 29582221 927007762 148559832
16 29 true true true 635204191 960773541 785102681
37 252 true true true 179233396 705144046 458562116
100 966 true true true 526375996 820085617 163361742
1025 17412 true true true 466234892 107918771 928861898
3000 58236 true true true 876830454 317179294 998776518
",
    );
}
