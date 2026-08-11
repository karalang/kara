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
//   ./mirror <pattern> <mode>     mode = plain | gallop | part | nosort
//
// `part` is the full-array stable partition of § "Direction 7" — the one fix
// shape the pass-count arithmetic in § "Direction 6" leaves standing. `SPAN=n`
// in the environment sets the size at which it hands off to the merge.

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
            // kN = N distinct keys, shuffled: sweeps the cardinality axis so the
            // gate threshold is validated against a measured crossover rather
            // than against the one benchmark pattern that motivated it.
            p if p.starts_with('k') => r % p[1..].parse::<i64>().unwrap(),
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

fn find_runs(d: &mut [E], lo0: usize, n: usize) -> Vec<usize> {
    let mut ends: Vec<usize> = Vec::new();
    let mut lo = lo0;
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

// ── Direction 7: full-array stable partition, merging BELOW it ─────────────
//
// The inversion that makes this different from the run-builder already tried
// and reverted for B-2026-08-10-20. That one bounded the partition to a `span`
// and merged ABOVE it, so it removed log2(span/RUN) merge passes off the
// BOTTOM and left the top ones intact; it also paid the fixed per-range cost
// of a partition on ranges near its base size. This one partitions from the
// whole array DOWN to `span` and merges below, so it removes the top passes and
// every partition it performs is on a large range — the regime where the
// original budget check's 14.38 instructions/element/level is the honest number
// rather than a flattering one.
//
// It is the only shape § "Direction 6" leaves standing, because it is the only
// one that can stop early: when every element of a range compares equal to the
// pivot, that range is sorted AND stable and needs no further work. That is how
// 8 distinct keys can be resolved in ~3 levels instead of 13 merge passes.

/// Stable 2-way count-then-scatter partition of `src[lo..hi)` into `dst`.
///
/// One counting pass tallies BOTH `< pivot` and `<= pivot`, which is what lets
/// the split predicate be chosen without a second pass and lets an all-equal
/// block be recognised for free:
///
///   nlt > 0                 split on `<`  at nlt; right is all-equal iff
///                           nle == len (pivot was the range maximum)
///   nlt == 0, nle < len     split on `<=` at nle; LEFT is the all-equal block
///                           (this is the `t=1` retry of the reverted design,
///                           minus the retry — folding it into the same pass)
///   nlt == 0, nle == len    whole range is equal: return None, no scatter
///
/// THE GATE COMES FOR FREE, which is what makes this direction shippable at
/// all. `neq = nle - nlt` is the number of elements EQUAL to a randomly chosen
/// pivot, so `len / neq` is an unbiased estimate of the range's distinct-key
/// count — and the counting pass has already computed it BEFORE a single
/// element is written. A range that is not low-cardinality abandons here having
/// moved no data, and the caller merges it exactly as karac does today.
///
/// It is re-evaluated per range rather than once for the array, so a mixed
/// input (a few huge equal blocks, the rest distinct) partitions the part that
/// pays and merges the part that does not.
enum Part {
    /// Whole range compares equal: already sorted and stable.
    AllEqual,
    /// Not low-cardinality; nothing was scattered.
    Abandon,
    Split(usize, bool, bool),
}

fn part_once(src: &[E], dst: &mut [E], lo: usize, hi: usize, rng: &mut u64, gate: usize) -> Part {
    let len = hi - lo;
    let p = pivot(src, lo, hi, rng);
    let (mut nlt, mut nle) = (0usize, 0usize);
    for i in lo..hi {
        let c = cmp(&g!(src, i), &p);
        nlt += (c == std::cmp::Ordering::Less) as usize;
        nle += (c != std::cmp::Ordering::Greater) as usize;
    }
    if gate > 0 && (nle - nlt) * gate < len {
        return Part::Abandon;
    }
    let (split, use_le, left_eq, right_eq) = if nlt > 0 {
        (nlt, false, false, nle == len)
    } else if nle < len {
        (nle, true, true, false)
    } else {
        return Part::AllEqual;
    };
    // Scatter: two cursors, one linear scan, relative order preserved on both
    // sides — that is the whole of the stability argument. Branchless: the
    // index is a select, both cursors advance by a bool.
    let (mut l, mut r) = (lo, lo + split);
    for i in lo..hi {
        let x = g!(src, i);
        let c = cmp(&x, &p);
        let left = if use_le {
            c != std::cmp::Ordering::Greater
        } else {
            c == std::cmp::Ordering::Less
        };
        let idx = if left { l } else { r };
        st!(dst, idx, x);
        l += left as usize;
        r += !left as usize;
    }
    Part::Split(split, left_eq, right_eq)
}

#[inline(always)]
fn xorshift(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

/// Median of three RANDOM samples — the choice the reverted emitter already
/// made ("pivot xorshift plus a 64-bit urem"), and it is not interchangeable
/// with the textbook fixed-position median-of-3. Sampling `lo`, `lo+len/2` and
/// `hi-1` is degenerate on periodic input: on the sawtooth (`i % 1000`, so a
/// 1000-long ramp repeated 150 times) those three positions hold 0, 0 and 999,
/// the median is 0, and 0 is the range MINIMUM — so every level peels off only
/// the 150 copies of the minimum and the recursion is ~1000 levels deep.
/// Measured: 2328.7M instructions on sawtooth against the merge's 23.8M.
fn pivot(a: &[E], lo: usize, hi: usize, rng: &mut u64) -> E {
    let len = (hi - lo) as u64;
    let mut pick = || g!(a, lo + (xorshift(rng) % len) as usize);
    let (x, y, z) = (pick(), pick(), pick());
    if cmp(&x, &y) == std::cmp::Ordering::Less {
        if cmp(&y, &z) == std::cmp::Ordering::Less {
            y
        } else if cmp(&x, &z) == std::cmp::Ordering::Less {
            z
        } else {
            x
        }
    } else if cmp(&x, &z) == std::cmp::Ordering::Less {
        x
    } else if cmp(&y, &z) == std::cmp::Ordering::Less {
        z
    } else {
        y
    }
}

/// karac's existing phase 1 + phase 2, restricted to one range and leaving the
/// result in `a`. This is what the partition recursion bottoms out into.
fn merge_range(a: &mut [E], b: &mut [E], lo0: usize, hi0: usize) {
    let ends = find_runs(a, lo0, hi0);
    phase2_range(a, b, lo0, hi0, ends);
}

/// Phase 2 alone, over a caller-supplied run table — so the top level can hand
/// back the `ends` phase 1 already produced instead of rescanning for it.
fn phase2_range(a: &mut [E], b: &mut [E], lo0: usize, hi0: usize, ends0: Vec<usize>) {
    let mut ends = ends0;
    let mut src_is_a = true;
    while ends.len() > 1 {
        let mut next: Vec<usize> = Vec::with_capacity(ends.len() / 2 + 1);
        let mut lo = lo0;
        let mut idx = 0usize;
        while idx < ends.len() {
            if idx + 1 < ends.len() {
                let (mid, hi) = (ends[idx], ends[idx + 1]);
                if src_is_a {
                    merge_plain(a, b, lo, mid, hi);
                } else {
                    merge_plain(b, a, lo, mid, hi);
                }
                next.push(hi);
                lo = hi;
                idx += 2;
            } else {
                let hi = ends[idx];
                if src_is_a {
                    b[lo..hi].copy_from_slice(&a[lo..hi]);
                } else {
                    a[lo..hi].copy_from_slice(&b[lo..hi]);
                }
                next.push(hi);
                lo = hi;
                idx += 1;
            }
        }
        ends = next;
        src_is_a = !src_is_a;
    }
    if !src_is_a {
        a[lo0..hi0].copy_from_slice(&b[lo0..hi0]);
    }
}

/// Bring `[lo,hi)` home to `a` when the ping-pong left it in `b`.
fn fix(a: &mut [E], b: &[E], lo: usize, hi: usize, in_a: bool) {
    if !in_a {
        a[lo..hi].copy_from_slice(&b[lo..hi]);
    }
}

/// Sort `[lo,hi)`, whose live data is in `a` iff `in_a`. Postcondition: sorted,
/// stable, live in `a`.
#[derive(Default)]
struct Stats {
    parts: usize,
    parted: usize,
    abandoned: usize,
    abandoned_el: usize,
    all_eq: usize,
    est_distinct: usize,
    ordered_pct: usize,
    merged_el: usize,
    copied_el: usize,
}

#[allow(clippy::too_many_arguments)]
fn psort(
    a: &mut [E],
    b: &mut [E],
    lo: usize,
    hi: usize,
    in_a: bool,
    span: usize,
    gate: usize,
    rng: &mut u64,
    st: &mut Stats,
) {
    if hi - lo <= span {
        if !in_a {
            st.copied_el += hi - lo;
        }
        fix(a, b, lo, hi, in_a);
        st.merged_el += hi - lo;
        merge_range(a, b, lo, hi);
        return;
    }
    st.parts += 1;
    let res = if in_a {
        part_once(a, b, lo, hi, rng, gate)
    } else {
        part_once(b, a, lo, hi, rng, gate)
    };
    let (split, left_eq, right_eq) = match res {
        Part::AllEqual => {
            // Already sorted and stable — the early exit that low cardinality
            // buys, and the whole reason this shape can beat a merge.
            st.all_eq += 1;
            if !in_a {
                st.copied_el += hi - lo;
            }
            fix(a, b, lo, hi, in_a);
            return;
        }
        Part::Abandon => {
            // The gate said no, and it said so having written nothing. Fall
            // back to exactly what karac does today.
            st.abandoned += 1;
            st.abandoned_el += hi - lo;
            if !in_a {
                st.copied_el += hi - lo;
            }
            fix(a, b, lo, hi, in_a);
            st.merged_el += hi - lo;
            merge_range(a, b, lo, hi);
            return;
        }
        Part::Split(s, l, r) => (s, l, r),
    };
    st.parted += hi - lo;
    let nin = !in_a; // the scatter moved it to the other buffer
    let mid = lo + split;
    for (l, h, eq) in [(lo, mid, left_eq), (mid, hi, right_eq)] {
        if eq {
            st.all_eq += 1;
            if !nin {
                st.copied_el += h - l;
            }
            fix(a, b, l, h, nin);
        } else {
            psort(a, b, l, h, nin, span, gate, rng, st);
        }
    }
}

/// THE ENTRY PROBE IS O(1) IN n, and it has to be, because both of the obvious
/// placements for a full-pass probe lose:
///
///   before phase 1  `sorted` 3.1M -> 4.4M and `reverse` 3.7M -> 5.0M. One
///                   counting pass is ~1.3M, which is 42% of an input phase 1
///                   resolves in a single run — and those are the patterns
///                   karac beats driftsort 7x on.
///   after phase 1   free for sorted/reverse, but few-unique goes 19.4M ->
///                   33.9M, because phase 1's RUN=32 insertion padding costs
///                   ~14.5M on an input whose natural runs are ~2 long, and the
///                   partition then throws that work away. 33.9M is barely
///                   better than the 34.25M run-builder this row already calls
///                   not worth having.
///
/// So the decision has to be made BEFORE phase 1 and cost nothing. 256 random
/// samples settle both halves of it: the number of DISTINCT keys among them
/// estimates the cardinality (8 true keys -> 8 observed; 1000 -> ~226; all
/// distinct -> 256), and the fraction of sampled adjacent pairs already in
/// order estimates how much phase 1's run detection would find. Both are needed
/// — cardinality alone would partition an input that is ALREADY SORTED over few
/// keys, which phase 1 resolves in one run.
fn probe(a: &[E], n: usize, rng: &mut u64) -> (usize, usize) {
    const M: usize = 256;
    let mut keys = [0i64; M];
    let mut ordered = 0usize;
    for slot in keys.iter_mut() {
        let i = (xorshift(rng) % (n as u64 - 1)) as usize;
        *slot = g!(a, i).0;
        ordered += (*slot <= g!(a, i + 1).0) as usize;
    }
    keys.sort_unstable();
    let mut d = 1usize;
    for i in 1..M {
        d += (keys[i] != keys[i - 1]) as usize;
    }
    (d, ordered * 100 / M)
}

/// Unused placement, kept only because § Direction 7 quotes its numbers. Run it
/// unconditionally and `sorted` goes 3.1M -> 4.4M and `reverse` 3.7M -> 5.0M:
/// one counting pass is ~1.3M instructions, which is 42% of an input phase 1
/// already resolves in a single run. Those are the patterns karac beats
/// driftsort 7x on, and this row's own notes reject buying the worst pattern by
/// regressing the best.
///
/// Phase 1 has already answered the question for free. `ends.len()` is the
/// number of natural runs, so phase 2 will do ceil(log2(ends.len())) passes; if
/// that is small there is nothing for a partition to win and the probe is pure
/// loss. Deciding AFTER phase 1 rather than before it costs sorted and reverse
/// exactly nothing, and it is also the smaller emitter change — a decision
/// inserted between two phases karac already has, not a restructure.
fn sort_part(v: &mut [E], span: usize, gate: usize, rungate: usize, st: &mut Stats) {
    let n = v.len();
    if n < 2 {
        return;
    }
    let mut rng: u64 = std::env::var("SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    // Two gates, at the two places each is free. The O(1) sample decides
    // whether to TRY at all; inside the recursion, `part_once`'s own counting
    // pass has already computed the exact tie count, so each range re-decides
    // for nothing. A mixed input therefore partitions the part that pays and
    // merges the part that does not.
    let (d, ordered_pct) = if gate > 0 && n > 4096 {
        probe(v, n, &mut rng)
    } else {
        (0, 0)
    };
    st.est_distinct = d;
    st.ordered_pct = ordered_pct;
    // Same scratch allocation the merge path already makes, initialised the
    // same way, so the two modes are compared on equal terms.
    let mut scratch: Vec<E> = v.to_vec();
    if d <= gate && ordered_pct < rungate {
        psort(v, &mut scratch, 0, n, true, span, gate, &mut rng, st);
        return;
    }
    st.merged_el += n;
    let ends = find_runs(v, 0, n); // phase 1, exactly as today
    phase2_range(v, &mut scratch, 0, n, ends);
}

// ── Driver: karac's phase-2 ping-pong ──────────────────────────────────────

fn sort_mirror(v: &mut Vec<E>, gallop: bool) {
    let n = v.len();
    if n < 2 {
        return;
    }
    let mut ends = find_runs(v.as_mut_slice(), 0, n);
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
    let envn = |k: &str, d: usize| -> usize {
        std::env::var(k)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(d)
    };
    let span = envn("SPAN", 4096);
    // ORDERED=p merges instead of partitioning when >= p% of sampled adjacent
    // pairs are already in order, i.e. when phase 1 will find long natural runs.
    let rungate = envn("ORDERED", 95);
    // GATE=k partitions a range only when >= len/k elements tie with the pivot,
    // i.e. estimated distinct keys <= k. GATE=0 disables the gate entirely
    // (partition unconditionally), which is what the ungated measurement used.
    let gate = envn("GATE", 64);
    if mode == "time" {
        // Same slope method as measure.py: (t[R] - t[1]) / (R-1) removes
        // process startup and the one-time input build; the per-round clone is
        // inside both, so it cancels.
        let base = build(pattern, 150_000);
        for which in ["plain", "gallop", "part"] {
            let run = |w: &mut Vec<E>| match which {
                "plain" => sort_mirror(w, false),
                "gallop" => sort_mirror(w, true),
                _ => sort_part(w.as_mut_slice(), span, gate, rungate, &mut Stats::default()),
            };
            let mut best = f64::MAX;
            for _ in 0..7 {
                let t1 = {
                    let s = Instant::now();
                    let mut w = base.clone();
                    run(&mut w);
                    black_box(&w);
                    s.elapsed().as_secs_f64()
                };
                let tr = {
                    let s = Instant::now();
                    for _ in 0..25 {
                        let mut w = base.clone();
                        run(&mut w);
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
    let mut st = Stats::default();
    match mode {
        "plain" => sort_mirror(&mut w, false),
        "gallop" => sort_mirror(&mut w, true),
        "part" | "stats" => sort_part(w.as_mut_slice(), span, gate, rungate, &mut st),
        "nosort" => {}
        _ => unreachable!(),
    }
    if mode == "stats" {
        // Verify the mechanism FIRED, rather than that it compiled — the
        // discipline that made § Direction 6's null result trustworthy.
        println!(
            "{pattern} span={span} gate={gate} ordered>={rungate}% | est_distinct={} ordered={}% \
             | partitions={} elems_partitioned={} \
             all_equal_exits={} abandoned={} ({} elems) merged={} elems  parity_copies={} elems",
            st.est_distinct,
            st.ordered_pct,
            st.parts,
            st.parted,
            st.all_eq,
            st.abandoned,
            st.abandoned_el,
            st.merged_el,
            st.copied_el
        );
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
