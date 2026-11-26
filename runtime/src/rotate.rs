//! `rotate_left` / `rotate_right` on a `Vec` or slice in compiled code
//! (B-2026-11-26-7): the elements are moved as bytes, the short side
//! through a stack buffer and the rest with one `memmove`, as Rust's
//! `ptr_rotate` does, where the library body's three reversals move each
//! element about four times through bounds-checked swaps. A rotation is a
//! permutation, so moving the bytes keeps every value whole: nothing is
//! cloned or dropped.

use std::mem::MaybeUninit;
use std::ptr;

/// The short side that goes through the stack buffer, in bytes.
const BUF: usize = 256;

/// Turns the `left + right` bytes at `p` so that the `right` bytes after
/// the first `left` come first.
///
/// # Safety
/// `p` is valid for reads and writes of `left + right` bytes.
unsafe fn rotate_bytes(p: *mut u8, mut left: usize, mut right: usize) {
    // SAFETY (throughout): every pointer stays inside the caller's
    // `left + right` bytes, `mid - left .. mid + right`.
    unsafe {
        let mut mid = p.add(left);
        loop {
            if left == 0 || right == 0 {
                return;
            }
            if left.min(right) <= BUF {
                let mut buf = [MaybeUninit::<u8>::uninit(); BUF];
                let b = buf.as_mut_ptr().cast::<u8>();
                let start = mid.sub(left);
                if left <= right {
                    ptr::copy_nonoverlapping(start, b, left);
                    ptr::copy(mid, start, right);
                    ptr::copy_nonoverlapping(b, start.add(right), left);
                } else {
                    ptr::copy_nonoverlapping(mid, b, right);
                    ptr::copy(start, start.add(right), left);
                    ptr::copy_nonoverlapping(b, start, right);
                }
                return;
            }
            // Both sides are long: swap the shorter side's worth of bytes
            // across the middle until one side fits the buffer, as core's
            // third `ptr_rotate` algorithm does.
            if left >= right {
                while left >= right {
                    ptr::swap_nonoverlapping(mid.sub(right), mid, right);
                    mid = mid.sub(right);
                    left -= right;
                }
            } else {
                while right >= left {
                    ptr::swap_nonoverlapping(mid.sub(left), mid, left);
                    mid = mid.add(left);
                    right -= left;
                }
            }
        }
    }
}

/// Turns the `n` elements of `size` bytes at `p` `mid` places toward the
/// front: the element at `mid` comes first. The library checks
/// `0 <= mid <= n` before the call; anything else does nothing.
///
/// # Safety
/// `p` holds `n` elements of `size` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_rotate_left(p: *mut u8, n: i64, mid: i64, size: i64) {
    if size <= 0 || mid <= 0 || mid >= n {
        return;
    }
    let (size, n, mid) = (size as usize, n as usize, mid as usize);
    // SAFETY: the caller's contract; `mid < n`.
    unsafe { rotate_bytes(p, mid * size, (n - mid) * size) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turned(n: usize, mid: usize, size: usize) -> Vec<u8> {
        let mut v: Vec<u8> = (0..n * size).map(|i| (i % 251) as u8).collect();
        // SAFETY: `v` holds `n` elements of `size` bytes.
        unsafe { karac_mir_rotate_left(v.as_mut_ptr(), n as i64, mid as i64, size as i64) };
        v
    }

    #[test]
    fn rotate_left_matches_core_for_every_shape() {
        for size in [1usize, 3, 8, 24] {
            for n in [0usize, 1, 2, 5, 31, 40, 300, 1000] {
                for mid in [0, 1, n / 3, n / 2, n.saturating_sub(1), n]
                    .into_iter()
                    .filter(|&m| m <= n)
                {
                    let mut want: Vec<u8> = (0..n * size).map(|i| (i % 251) as u8).collect();
                    want.rotate_left(mid * size);
                    assert_eq!(turned(n, mid, size), want, "n={n} mid={mid} size={size}");
                }
            }
        }
    }
}
