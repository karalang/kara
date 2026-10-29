//! A growable buffer of `Copy` values that nothing in can panic, for the
//! runtime paths every compiled program links (printing, `Map` and `Set`
//! tables).
//!
//! Any Rust panic such a path can reach keeps std's panic hook and its DWARF
//! backtrace symbolizer, about 250 KB, in every binary (B-2026-10-28-36,
//! B-2026-10-29-41). `Vec`'s growth reaches its capacity-overflow panic and
//! a slice index its bounds panic, so here growth goes through
//! [`karac_realloc_or_panic`], which aborts when it cannot be had, and an
//! index out of bounds ends the process with a message. Indexing is still
//! checked: a defect that would have panicked aborts instead.

use std::ops::{Index, IndexMut, RangeTo};

use crate::alloc::{karac_free, karac_realloc_or_panic};

pub(crate) struct LeanVec<T: Copy> {
    ptr: *mut T,
    len: usize,
    cap: usize,
}

// SAFETY: a `LeanVec` owns its buffer, as a `Vec` does.
unsafe impl<T: Copy + Send> Send for LeanVec<T> {}
// SAFETY: as above; `&LeanVec` only reads.
unsafe impl<T: Copy + Sync> Sync for LeanVec<T> {}

/// An index out of bounds, which only a runtime defect makes.
#[cold]
#[inline(never)]
fn out_of_bounds() -> ! {
    crate::fatal::write_stderr(b"fatal runtime error: a runtime table index is out of bounds\n");
    std::process::abort()
}

impl<T: Copy> LeanVec<T> {
    pub(crate) const fn new() -> Self {
        LeanVec {
            ptr: std::ptr::null_mut(),
            len: 0,
            cap: 0,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn as_ptr(&self) -> *const T {
        self.ptr
    }

    pub(crate) fn as_slice(&self) -> &[T] {
        if self.len == 0 {
            &[]
        } else {
            // SAFETY: the first `len` slots are written.
            unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
        }
    }

    pub(crate) fn as_mut_slice(&mut self) -> &mut [T] {
        if self.len == 0 {
            &mut []
        } else {
            // SAFETY: the first `len` slots are written, and `self` is
            // borrowed mutably.
            unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
        }
    }

    /// Room for `want` values in all.
    fn reserve_total(&mut self, want: usize) {
        if want > self.cap {
            let cap = want.max(self.cap.saturating_mul(2)).max(8);
            let bytes = cap.saturating_mul(std::mem::size_of::<T>());
            self.ptr = karac_realloc_or_panic(self.ptr.cast(), bytes).cast();
            self.cap = cap;
        }
    }

    pub(crate) fn extend_from_slice(&mut self, s: &[T]) {
        let want = self.len.saturating_add(s.len());
        self.reserve_total(want);
        // SAFETY: the buffer has room for `want` values.
        unsafe { std::ptr::copy_nonoverlapping(s.as_ptr(), self.ptr.add(self.len), s.len()) };
        self.len = want;
    }

    pub(crate) fn push(&mut self, v: T) {
        self.extend_from_slice(std::slice::from_ref(&v));
    }

    pub(crate) fn pop(&mut self) -> Option<T> {
        self.len = self.len.checked_sub(1)?;
        // SAFETY: the slot was written.
        Some(unsafe { self.ptr.add(self.len).read() })
    }

    /// `len` values, the new ones `v`.
    pub(crate) fn resize(&mut self, len: usize, v: T) {
        if len > self.len {
            self.reserve_total(len);
            for i in self.len..len {
                // SAFETY: the buffer has room for `len` values.
                unsafe { self.ptr.add(i).write(v) };
            }
        }
        self.len = len;
    }

    pub(crate) fn truncate(&mut self, len: usize) {
        self.len = self.len.min(len);
    }

    pub(crate) fn clear(&mut self) {
        self.len = 0;
    }
}

impl<T: Copy> Default for LeanVec<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy> Drop for LeanVec<T> {
    fn drop(&mut self) {
        karac_free(self.ptr.cast());
    }
}

impl<T: Copy> Index<usize> for LeanVec<T> {
    type Output = T;

    #[inline]
    fn index(&self, i: usize) -> &T {
        if i >= self.len {
            out_of_bounds()
        }
        // SAFETY: `i < len`.
        unsafe { &*self.ptr.add(i) }
    }
}

impl<T: Copy> IndexMut<usize> for LeanVec<T> {
    #[inline]
    fn index_mut(&mut self, i: usize) -> &mut T {
        if i >= self.len {
            out_of_bounds()
        }
        // SAFETY: `i < len`, and `self` is borrowed mutably.
        unsafe { &mut *self.ptr.add(i) }
    }
}

impl<T: Copy> Index<RangeTo<usize>> for LeanVec<T> {
    type Output = [T];

    fn index(&self, r: RangeTo<usize>) -> &[T] {
        match self.as_slice().get(r) {
            Some(s) => s,
            None => out_of_bounds(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LeanVec;

    #[test]
    fn lean_vec_grows_resizes_and_pops_like_a_vec() {
        let mut v: LeanVec<u64> = LeanVec::new();
        let mut model = Vec::new();
        for i in 0..1000u64 {
            v.push(i * 3);
            model.push(i * 3);
        }
        v.resize(1200, 7);
        model.resize(1200, 7);
        assert_eq!(v.as_slice(), &model[..]);
        v[5] = 99;
        model[5] = 99;
        v.truncate(600);
        model.truncate(600);
        assert_eq!(v.pop(), model.pop());
        assert_eq!(&v[..10], &model[..10]);
        assert_eq!(v.len(), model.len());
        v.clear();
        assert!(v.is_empty() && v.pop().is_none());
    }
}
