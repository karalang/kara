// A faithful Rust MIRROR of karac's `emit_sort_by_mono` algorithm, plus a
// galloping variant of the same, for budget-checking B-2026-08-11-10 BEFORE
// writing any emitter.
//
// Why a mirror and not a probe: the previous budget check for this row
// (§ "Why the budget check misled") measured a partition kernel over the FULL
// 150k array at every level, so every loop had a huge trip count and amortised
// its per-range setup to nothing. It measured the kernel's asymptotic cost, and
// the real sort spends most of its work on small ranges. This mirror runs the
// whole algorithm on the real input, so every pass sees the run-size
// distribution the real sort sees.
//
// The mirror is only trustworthy if it lands near karac's own recorded numbers,
// so `--check` reports its instruction count for the same three patterns the
// harness validates against (karac: few_unique 48.8M, sawtooth 22.5M).
//
//   rustc -O -o mirror mirror.rs
//   ./mirror <pattern> <mode>     mode = plain | gallop | nosort

use std::hint::black_box;
use std::time::Instant;

/// Unchecked slice read/write. karac emits raw GEPs off the data and scratch
/// pointers with no bounds test, so a mirror that pays Rust's bounds checks
/// measures a different loop than the one under study — its `plain` few-unique
/// came out at 68.2M against karac's recorded 48.8M until these were added.
macro_rules! g {
    ($a:expr, $i:expr) => {
        unsafe { *$a.get_unchecked($i) }
    };
}
macro_rules! st {
    ($a:expr, $i:expr, $v:expr) => {
        unsafe { *$a.get_unchecked_mut($i) = $v }
    };
}

const RUN: usize = 32;
/// Timsort's adaptive galloping threshold. Galloping engages only after one
/// side wins `min_gallop` consecutive outputs, and the threshold is lowered on
/// a successful gallop / raised on a failed one, so it pays for itself on the
/// inputs that like it and turns itself off on the ones that do not.
const MIN_GALLOP_INIT: i32 = 7;

type E = (i64, i64);

#[inline(always)]
fn cmp(a: &E, b: &E) -> std::cmp::Ordering {
    a.0.cmp(&b.0)
}

fn build(pattern: &str, n: i64) -> Vec<E> {
    let mut v: Vec<E> = Vec::new();
    let mut seed: i64 = 12345;
    let mut i: i64 = 0;
    while i < n {
        seed = (seed * 1103515245 + 12345) % 2147483648;
        let r = seed;
        let k: i64 = match pattern {
            "random" => r,
            "few_unique" => r % 8,
            "sawtooth" => i % 1000,
            "sorted" => i,
            "reverse" => n - i,
            "nearly_sorted" => {
                if r % 100 == 0 {
                    r
                } else {
                    i
                }
            }
            _ => unreachable!(),
        };
        v.push((k, i));
        i += 1;
    }
    v
}

// ── Phase 1: natural-run detection + RUN padding (karac's shape) ────────────

fn insertion_sort(d: &mut [E], lo: usize, hi: usize) {
    let mut i = lo + 1;
    while i < hi {
        let x = g!(d, i);
        let mut j = i;
        while j > lo && cmp(&g!(d, j - 1), &x) == std::cmp::Ordering::Greater {
            st!(d, j, g!(d, j - 1));
            j -= 1;
        }
        st!(d, j, x);
        i += 1;
    }
}

fn find_runs(d: &mut [E], n: usize) -> Vec<usize> {
    let mut ends: Vec<usize> = Vec::new();
    let mut lo = 0usize;
    while lo < n {
        let mut hi = lo + 1;
        if hi < n {
            if cmp(&d[hi], &d[lo]) == std::cmp::Ordering::Less {
                // strictly descending — extend, then reverse (stable: no ties)
                while hi < n && cmp(&d[hi], &d[hi - 1]) == std::cmp::Ordering::Less {
                    hi += 1;
                }
                d[lo..hi].reverse();
            } else {
                while hi < n && cmp(&d[hi], &d[hi - 1]) != std::cmp::Ordering::Less {
                    hi += 1;
                }
            }
        }
        // pad short runs out to RUN by insertion sort
        if hi - lo < RUN {
            let want = std::cmp::min(lo + RUN, n);
            insertion_sort(d, lo, want);
            hi = want;
        }
        ends.push(hi);
        lo = hi;
    }
    ends
}

// ── Phase 2a: karac's current merge (branchy, take-left-on-tie) ─────────────

fn merge_plain(src: &[E], dst: &mut [E], lo: usize, mid: usize, hi: usize) {
    let (mut i, mut j, mut k) = (lo, mid, lo);
    while i < mid && j < hi {
        let (l, r) = (g!(src, i), g!(src, j));
        if cmp(&r, &l) == std::cmp::Ordering::Less {
            st!(dst, k, r);
            j += 1;
        } else {
            st!(dst, k, l);
            i += 1;
        }
        k += 1;
    }
    while i < mid {
        st!(dst, k, g!(src, i));
        i += 1;
        k += 1;
    }
    while j < hi {
        st!(dst, k, g!(src, j));
        j += 1;
        k += 1;
    }
}

// ── Phase 2b: the same merge WITH galloping ────────────────────────────────

/// Leftmost insertion point for `key` in `a[lo..hi]`, by exponential
/// (galloping) search: counts elements STRICTLY LESS than `key`. Used to
/// gallop the RIGHT run — a right element may only be emitted ahead of a left
/// element it is strictly less than, because a tie must go left.
fn gallop_left(key: &E, a: &[E], lo: usize, hi: usize) -> usize {
    let mut ofs = 1usize;
    let mut last = 0usize;
    while lo + ofs < hi && cmp(&g!(a, lo + ofs), key) == std::cmp::Ordering::Less {
        last = ofs;
        ofs = ofs * 2 + 1;
    }
    let (mut l, mut r) = (lo + last, std::cmp::min(lo + ofs, hi));
    while l < r {
        let m = l + (r - l) / 2;
        if cmp(&g!(a, m), key) == std::cmp::Ordering::Less {
            l = m + 1;
        } else {
            r = m;
        }
    }
    l
}

/// Rightmost insertion point: counts elements LESS THAN OR EQUAL to `key`.
/// Used to gallop the LEFT run — a left element ties-to-left against the right
/// run's head, so equal elements belong in the bulk copy.
///
/// Getting these two the wrong way round is the whole stability question, and
/// it is silent: the first draft here swapped them, produced correctly SORTED
/// output on every pattern, and only the equal-key tiebreak revealed it.
fn gallop_right(key: &E, a: &[E], lo: usize, hi: usize) -> usize {
    let mut ofs = 1usize;
    let mut last = 0usize;
    while lo + ofs < hi && cmp(&g!(a, lo + ofs), key) != std::cmp::Ordering::Greater {
        last = ofs;
        ofs = ofs * 2 + 1;
    }
    let (mut l, mut r) = (lo + last, std::cmp::min(lo + ofs, hi));
    while l < r {
        let m = l + (r - l) / 2;
        if cmp(&g!(a, m), key) == std::cmp::Ordering::Greater {
            r = m;
        } else {
            l = m + 1;
        }
    }
    l
}

fn merge_gallop(src: &[E], dst: &mut [E], lo: usize, mid: usize, hi: usize, min_gallop: &mut i32) {
    let (mut i, mut j, mut k) = (lo, mid, lo);
    let mut wins_l = 0i32;
    let mut wins_r = 0i32;
    'outer: while i < mid && j < hi {
        // Ordinary element-at-a-time merge until one side wins repeatedly.
        while i < mid && j < hi {
            let (l, r) = (g!(src, i), g!(src, j));
            if cmp(&r, &l) == std::cmp::Ordering::Less {
                st!(dst, k, r);
                j += 1;
                k += 1;
                wins_r += 1;
                wins_l = 0;
                if wins_r >= *min_gallop {
                    break;
                }
            } else {
                st!(dst, k, l);
                i += 1;
                k += 1;
                wins_l += 1;
                wins_r = 0;
                if wins_l >= *min_gallop {
                    break;
                }
            }
        }
        if i >= mid || j >= hi {
            break;
        }
        // Gallop mode: keep bulk-copying while each gallop pays off.
        loop {
            // How much of the LEFT run precedes src[j]?
            let n_l = gallop_right(&g!(src, j), src, i, mid) - i;
            if n_l > 0 {
                dst[k..k + n_l].copy_from_slice(&src[i..i + n_l]);
                k += n_l;
                i += n_l;
                if i >= mid {
                    break 'outer;
                }
            }
            st!(dst, k, g!(src, j));
            k += 1;
            j += 1;
            if j >= hi {
                break 'outer;
            }

            // How much of the RIGHT run precedes-or-ties src[i]?
            let n_r = gallop_left(&g!(src, i), src, j, hi) - j;
            if n_r > 0 {
                dst[k..k + n_r].copy_from_slice(&src[j..j + n_r]);
                k += n_r;
                j += n_r;
                if j >= hi {
                    break 'outer;
                }
            }
            st!(dst, k, g!(src, i));
            k += 1;
            i += 1;
            if i >= mid {
                break 'outer;
            }

            if (n_l as i32) < MIN_GALLOP_INIT && (n_r as i32) < MIN_GALLOP_INIT {
                // Neither gallop paid: raise the bar and go back to plain.
                *min_gallop += 1;
                wins_l = 0;
                wins_r = 0;
                continue 'outer;
            }
            // A gallop paid: lower the bar so we re-enter sooner next time.
            if *min_gallop > 1 {
                *min_gallop -= 1;
            }
        }
    }
    if i < mid {
        let n = mid - i;
        dst[k..k + n].copy_from_slice(&src[i..mid]);
    } else if j < hi {
        let n = hi - j;
        dst[k..k + n].copy_from_slice(&src[j..hi]);
    }
}

// ── Driver: karac's phase-2 ping-pong ──────────────────────────────────────

fn sort_mirror(v: &mut Vec<E>, gallop: bool) {
    let n = v.len();
    if n < 2 {
        return;
    }
    let mut ends = find_runs(v.as_mut_slice(), n);
    let mut scratch: Vec<E> = v.clone();
    let mut src_is_v = true;
    let mut min_gallop = MIN_GALLOP_INIT;

    while ends.len() > 1 {
        let mut next: Vec<usize> = Vec::with_capacity(ends.len() / 2 + 1);
        let mut lo = 0usize;
        let mut idx = 0usize;
        while idx < ends.len() {
            if idx + 1 < ends.len() {
                let mid = ends[idx];
                let hi = ends[idx + 1];
                if src_is_v {
                    if gallop {
                        merge_gallop(v, &mut scratch, lo, mid, hi, &mut min_gallop);
                    } else {
                        merge_plain(v, &mut scratch, lo, mid, hi);
                    }
                } else if gallop {
                    merge_gallop(&scratch, v, lo, mid, hi, &mut min_gallop);
                } else {
                    merge_plain(&scratch, v, lo, mid, hi);
                }
                next.push(hi);
                lo = hi;
                idx += 2;
            } else {
                let hi = ends[idx];
                if src_is_v {
                    scratch[lo..hi].copy_from_slice(&v[lo..hi]);
                } else {
                    v[lo..hi].copy_from_slice(&scratch[lo..hi]);
                }
                next.push(hi);
                lo = hi;
                idx += 1;
            }
        }
        ends = next;
        src_is_v = !src_is_v;
    }
    if !src_is_v {
        v.copy_from_slice(&scratch);
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let pattern = a[1].as_str();
    let mode = a[2].as_str();
    if mode == "time" {
        // Same slope method as measure.py: (t[R] - t[1]) / (R-1) removes
        // process startup and the one-time input build; the per-round clone is
        // inside both, so it cancels.
        let base = build(pattern, 150_000);
        for which in ["plain", "gallop"] {
            let gal = which == "gallop";
            let mut best = f64::MAX;
            for _ in 0..7 {
                let t1 = {
                    let s = Instant::now();
                    let mut w = base.clone();
                    sort_mirror(&mut w, gal);
                    black_box(&w);
                    s.elapsed().as_secs_f64()
                };
                let tr = {
                    let s = Instant::now();
                    for _ in 0..25 {
                        let mut w = base.clone();
                        sort_mirror(&mut w, gal);
                        black_box(&w);
                    }
                    s.elapsed().as_secs_f64()
                };
                let slope = (tr - t1) / 24.0;
                if slope < best {
                    best = slope;
                }
            }
            println!("{pattern} {which} {:.3} ms", best * 1000.0);
        }
        return;
    }
    let mut w = black_box(build(pattern, 150_000));
    match mode {
        "plain" => sort_mirror(&mut w, false),
        "gallop" => sort_mirror(&mut w, true),
        "nosort" => {}
        _ => unreachable!(),
    }
    // Correctness guard: any mode that sorted must be sorted and stable.
    if mode != "nosort" {
        for t in 1..w.len() {
            let o = cmp(&w[t - 1], &w[t]);
            assert!(o != std::cmp::Ordering::Greater, "not sorted at {t}", t = t);
            if o == std::cmp::Ordering::Equal {
                assert!(w[t - 1].1 < w[t].1, "not stable at {t}", t = t);
            }
        }
    }
    black_box(&w);
    println!("{}", w[0].0);
}
