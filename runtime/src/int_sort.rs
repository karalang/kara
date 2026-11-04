//! A sort for the integer kinds `karac_mir_sort_int` takes that reaches no
//! Rust panic (B-2026-11-04-31).
//!
//! `slice::sort_unstable` on an integer slice keeps one: its small sort's
//! merge checks for a comparator that is not a total order and panics, and
//! that panic keeps std's panic hook and backtrace symbolizer, about 250 KB,
//! in every program that sorts integers. This is the same algorithm,
//! ipnsort by Lukas Bergdoll and Orson Peters as Rust's core library
//! implements it (MIT or Apache-2.0), for `Copy + Ord` values only, so
//! with the drop guards it needs for other types left out and the merge's
//! check, which `Ord` on an integer cannot fail, ending the process. The
//! structure and constants follow `core::slice::sort` at Rust 1.98.

use std::ptr;

/// Up to this length, insertion sort alone.
const ALWAYS_INSERTION: usize = 20;
/// The small sort below the quicksort, and its scratch.
const SMALL: usize = 32;
const PSEUDO_MEDIAN_REC_THRESHOLD: usize = 64;

/// Sorts the `n` values at `v`.
///
/// # Safety
/// `v` is valid for reads and writes of `n` values.
pub(crate) unsafe fn sort<T: Copy + Ord>(v: *mut T, n: usize) {
    if n < 2 {
        return;
    }
    // SAFETY (each call): `v` holds `n` values.
    unsafe {
        if n <= ALWAYS_INSERTION {
            insertion_sort_shift_left(v, n, 1);
            return;
        }
        let (run, reversed) = existing_run(v, n);
        if run == n {
            if reversed {
                reverse(v, n);
            }
            return;
        }
        // `2 * floor(log2(n))`, as core's limit on imbalanced partitions.
        let limit = 2 * (usize::BITS - 1 - (n | 1).leading_zeros());
        quicksort(v, n, None, limit);
    }
}

unsafe fn reverse<T: Copy>(v: *mut T, n: usize) {
    // SAFETY: the caller's `v` and `n`.
    unsafe { std::slice::from_raw_parts_mut(v, n) }.reverse();
}

/// The length of the run `v` starts with, and whether it is strictly
/// descending.
unsafe fn existing_run<T: Copy + Ord>(v: *const T, n: usize) -> (usize, bool) {
    // SAFETY: every index read is below `n`, and `n >= 2`.
    unsafe {
        let mut run = 2;
        let descending = *v.add(1) < *v;
        if descending {
            while run < n && *v.add(run) < *v.add(run - 1) {
                run += 1;
            }
        } else {
            while run < n && !(*v.add(run) < *v.add(run - 1)) {
                run += 1;
            }
        }
        (run, descending)
    }
}

unsafe fn quicksort<T: Copy + Ord>(
    mut v: *mut T,
    mut n: usize,
    mut ancestor: Option<T>,
    mut limit: u32,
) {
    loop {
        // SAFETY (the block): `v` holds `n` values throughout.
        unsafe {
            if n <= SMALL {
                small_sort_network(v, n);
                return;
            }
            if limit == 0 {
                heapsort(v, n);
                return;
            }
            limit -= 1;
            let pivot = choose_pivot(v, n);
            // A pivot no greater than an ancestor's equals it: the values
            // `<=` it are all equal, so only the ones above are left.
            if let Some(a) = ancestor {
                if !(a < *v.add(pivot)) {
                    let le = partition(v, n, pivot, |x, p| !(p < x));
                    v = v.add(le + 1);
                    n -= le + 1;
                    ancestor = None;
                    continue;
                }
            }
            let lt = partition(v, n, pivot, |x, p| x < p);
            let p = *v.add(lt);
            quicksort(v, lt, ancestor, limit);
            v = v.add(lt + 1);
            n -= lt + 1;
            ancestor = Some(p);
        }
    }
}

/// Moves the pivot at `pivot` to its place, the values `goes_left` of it
/// before it; their count, which is its index.
#[inline(always)]
unsafe fn partition<T: Copy + Ord>(
    v: *mut T,
    n: usize,
    pivot: usize,
    goes_left: impl Fn(T, T) -> bool,
) -> usize {
    // SAFETY (the block): `pivot < n`, and every pointer stays in `v`.
    unsafe {
        ptr::swap(v, v.add(pivot));
        let p = *v;
        let num_lt = lomuto_branchless_cyclic(v.add(1), n - 1, p, &goes_left);
        ptr::swap(v, v.add(num_lt));
        num_lt
    }
}

/// The cyclic branchless Lomuto partition of core's quicksort: the values
/// at `v` that `goes_left` of `pivot` first; their count.
#[inline(always)]
unsafe fn lomuto_branchless_cyclic<T: Copy>(
    v: *mut T,
    n: usize,
    pivot: T,
    goes_left: &impl Fn(T, T) -> bool,
) -> usize {
    if n == 0 {
        return 0;
    }
    // SAFETY (the block): `right`, `gap` and `v + num_lt` stay below `n`;
    // the gap's value is a copy, so the cycle needs no guard.
    unsafe {
        let gap_value = *v;
        let mut gap = v;
        let mut num_lt = 0usize;
        let mut right = v.add(1);
        let body = |right: *const T, gap: &mut *mut T, num_lt: &mut usize| {
            let r = *right;
            let left = v.add(*num_lt);
            ptr::copy(left, *gap, 1);
            *left = r;
            *gap = right as *mut T;
            *num_lt += goes_left(r, pivot) as usize;
        };
        let unroll_end = v.add(n - 1);
        while right < unroll_end {
            body(right, &mut gap, &mut num_lt);
            body(right.add(1), &mut gap, &mut num_lt);
            right = right.add(2);
        }
        let end = v.add(n);
        while right < end {
            body(right, &mut gap, &mut num_lt);
            right = right.add(1);
        }
        // The value first lifted out closes the cycle.
        let left = v.add(num_lt);
        ptr::copy(left, gap, 1);
        *left = gap_value;
        num_lt + goes_left(gap_value, pivot) as usize
    }
}

unsafe fn choose_pivot<T: Copy + Ord>(v: *const T, n: usize) -> usize {
    let d = n / 8;
    // SAFETY: `n > SMALL`, so every offset is below `n`.
    unsafe {
        let (a, b, c) = (v, v.add(d * 4), v.add(d * 7));
        let m = if n < PSEUDO_MEDIAN_REC_THRESHOLD {
            median3(a, b, c)
        } else {
            median3_rec(a, b, c, d)
        };
        m.offset_from(v) as usize
    }
}

unsafe fn median3_rec<T: Copy + Ord>(
    mut a: *const T,
    mut b: *const T,
    mut c: *const T,
    n: usize,
) -> *const T {
    // SAFETY: each sub-range is within the one it divides.
    unsafe {
        if n * 8 >= PSEUDO_MEDIAN_REC_THRESHOLD {
            let n8 = n / 8;
            a = median3_rec(a, a.add(n8 * 4), a.add(n8 * 7), n8);
            b = median3_rec(b, b.add(n8 * 4), b.add(n8 * 7), n8);
            c = median3_rec(c, c.add(n8 * 4), c.add(n8 * 7), n8);
        }
        median3(a, b, c)
    }
}

#[inline(always)]
unsafe fn median3<T: Copy + Ord>(a: *const T, b: *const T, c: *const T) -> *const T {
    // SAFETY: the caller's pointers are readable.
    unsafe {
        let x = *a < *b;
        let y = *a < *c;
        if x == y {
            let z = *b < *c;
            if z ^ x {
                c
            } else {
                b
            }
        } else {
            a
        }
    }
}

#[inline(never)]
unsafe fn heapsort<T: Copy + Ord>(v: *mut T, n: usize) {
    let mut i = n + n / 2;
    while i > 0 {
        i -= 1;
        // SAFETY: `i < n` where it swaps; the heap is `v[..min(i, n)]`.
        unsafe {
            let node = if i >= n {
                i - n
            } else {
                ptr::swap(v, v.add(i));
                0
            };
            sift_down(v, i.min(n), node);
        }
    }
}

#[inline(always)]
unsafe fn sift_down<T: Copy + Ord>(v: *mut T, n: usize, mut node: usize) {
    loop {
        let mut child = 2 * node + 1;
        if child >= n {
            break;
        }
        // SAFETY: `node` and `child` (plus one, when checked) are below `n`.
        unsafe {
            if child + 1 < n {
                child += (*v.add(child) < *v.add(child + 1)) as usize;
            }
            if !(*v.add(node) < *v.add(child)) {
                break;
            }
            ptr::swap_nonoverlapping(v.add(node), v.add(child), 1);
        }
        node = child;
    }
}

unsafe fn small_sort_network<T: Copy + Ord>(v: *mut T, n: usize) {
    if n < 2 {
        return;
    }
    let half = n / 2;
    let no_merge = n < 18;
    let mut region = v;
    let mut len = if no_merge { n } else { half };
    // SAFETY (the block): each region is within `v[..n]`; the scratch holds
    // `SMALL >= n` values.
    unsafe {
        loop {
            let presorted = if len >= 13 {
                sort13_optimal(region);
                13
            } else if len >= 9 {
                sort9_optimal(region);
                9
            } else {
                1
            };
            insertion_sort_shift_left(region, len, presorted);
            if no_merge {
                return;
            }
            if region != v {
                break;
            }
            region = v.add(half);
            len = n - half;
        }
        let mut scratch = std::mem::MaybeUninit::<[T; SMALL]>::uninit();
        let s = scratch.as_mut_ptr() as *mut T;
        bidirectional_merge(v, n, s);
        ptr::copy_nonoverlapping(s, v, n);
    }
}

#[inline(always)]
unsafe fn swap_if_less<T: Copy + Ord>(v: *mut T, a: usize, b: usize) {
    // SAFETY: the networks' indices are below their length.
    unsafe {
        let (pa, pb) = (v.add(a), v.add(b));
        let swap = *pb < *pa;
        let lo = std::hint::select_unpredictable(swap, pb, pa);
        let hi = std::hint::select_unpredictable(swap, pa, pb);
        let keep = *hi;
        ptr::copy(lo, pa, 1);
        *pb = keep;
    }
}

/// The comparators of a sorting network, written out as core writes them.
macro_rules! network {
    ($v:expr; $(($a:literal, $b:literal)),* $(,)?) => {
        $(swap_if_less($v, $a, $b);)*
    };
}

unsafe fn sort9_optimal<T: Copy + Ord>(v: *mut T) {
    // SAFETY: `v` holds 9 values.
    unsafe {
        network!(v; (0, 3), (1, 7), (2, 5), (4, 8), (0, 7), (2, 4), (3, 8), (5, 6), (0, 2), (1, 3), (4, 5),
            (7, 8), (1, 4), (3, 6), (5, 7), (0, 1), (2, 4), (3, 5), (6, 8), (2, 3), (4, 5), (6, 7), (1, 2),
            (3, 4), (5, 6));
    }
}

unsafe fn sort13_optimal<T: Copy + Ord>(v: *mut T) {
    // SAFETY: `v` holds 13 values.
    unsafe {
        network!(v; (0, 12), (1, 10), (2, 9), (3, 7), (5, 11), (6, 8), (1, 6), (2, 3), (4, 11), (7, 9),
            (8, 10), (0, 4), (1, 2), (3, 6), (7, 8), (9, 10), (11, 12), (4, 6), (5, 9), (8, 11), (10, 12),
            (0, 5), (3, 8), (4, 7), (6, 11), (9, 10), (0, 1), (2, 5), (6, 9), (7, 8), (10, 11), (1, 3),
            (2, 4), (5, 6), (9, 10), (1, 2), (3, 4), (5, 7), (6, 8), (2, 3), (4, 5), (6, 7), (8, 9), (3, 4),
            (5, 6));
    }
}

/// Inserts `v[offset..n]` one at a time into the sorted `v[..offset]`.
unsafe fn insertion_sort_shift_left<T: Copy + Ord>(v: *mut T, n: usize, offset: usize) {
    let mut tail = offset;
    while tail < n {
        // SAFETY: every index is below `n`.
        unsafe {
            let x = *v.add(tail);
            let mut j = tail;
            while j > 0 && x < *v.add(j - 1) {
                *v.add(j) = *v.add(j - 1);
                j -= 1;
            }
            *v.add(j) = x;
        }
        tail += 1;
    }
}

/// Merges the sorted halves `src[..n/2]` and `src[n/2..n]` into `dst` from
/// both ends at once.
unsafe fn bidirectional_merge<T: Copy + Ord>(src: *const T, n: usize, dst: *mut T) {
    let half = n / 2;
    // SAFETY (the block): the four cursors stay within their halves while
    // the order is total, which the closing check confirms.
    unsafe {
        let (mut left, mut right, mut out) = (src, src.add(half), dst);
        let (mut left_rev, mut right_rev, mut out_rev) =
            (src.add(half - 1), src.add(n - 1), dst.add(n - 1));
        for _ in 0..half {
            let take_left = !(*right < *left);
            *out = if take_left { *left } else { *right };
            right = right.add(!take_left as usize);
            left = left.add(take_left as usize);
            out = out.add(1);

            let take_left = !(*right_rev < *left_rev);
            *out_rev = if take_left { *right_rev } else { *left_rev };
            right_rev = right_rev.wrapping_sub(take_left as usize);
            left_rev = left_rev.wrapping_sub(!take_left as usize);
            out_rev = out_rev.sub(1);
        }
        let left_end = left_rev.wrapping_add(1);
        let right_end = right_rev.wrapping_add(1);
        if !n.is_multiple_of(2) {
            let left_nonempty = left < left_end;
            *out = if left_nonempty { *left } else { *right };
            left = left.add(left_nonempty as usize);
            right = right.add(!left_nonempty as usize);
        }
        if left != left_end || right != right_end {
            not_a_total_order();
        }
    }
}

#[cold]
#[inline(never)]
fn not_a_total_order() -> ! {
    crate::fatal::write_stderr(
        b"fatal runtime error: an integer sort found its order inconsistent\n",
    );
    std::process::abort()
}

#[cfg(test)]
mod tests {
    /// The port sorts as `sort_unstable` does, for every integer kind, at
    /// every length through the small sorts and past them, on random,
    /// few-valued, ascending, descending and sawtooth input.
    #[test]
    fn int_sort_matches_sort_unstable() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        macro_rules! kind {
            ($t:ty) => {
                let lens = (0..80usize).chain([100, 127, 128, 129, 500, 1000, 4096, 20000]);
                for n in lens {
                    for shape in 0..5 {
                        let v: Vec<$t> = (0..n)
                            .map(|i| match shape {
                                0 => rnd() as $t,
                                1 => (rnd() % 3) as $t,
                                2 => i as $t,
                                3 => (n - i) as $t,
                                _ => (i % 17) as $t,
                            })
                            .collect();
                        let mut want = v.clone();
                        want.sort_unstable();
                        let mut got = v;
                        // SAFETY: `got` holds its length.
                        unsafe { super::sort(got.as_mut_ptr(), got.len()) };
                        assert_eq!(got, want, "{} n={n} shape={shape}", stringify!($t));
                    }
                }
            };
        }
        kind!(u8);
        kind!(i8);
        kind!(u16);
        kind!(i16);
        kind!(u32);
        kind!(i32);
        kind!(u64);
        kind!(i64);
    }
}
