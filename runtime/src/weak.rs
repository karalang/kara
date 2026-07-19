//! Weak-reference control-block primitives — slice 1 of `weak T` support
//! (design: `docs/spikes/weak-refs.md`, tracks `B-2026-07-19-8`).
//!
//! A weak-capable shared-struct RC box is laid out `{ i64 strong, i64 weak,
//! <fields…> }`: `strong` at byte offset 0, `weak` at byte offset 8. The strong
//! set collectively holds ONE weak count (the Rust `Rc`/`Weak` convention), so
//! the box is freed exactly when BOTH counts reach zero. Codegen drops the
//! payload's heap fields at `strong == 0` (deterministic destruction — the
//! design's promise); the 16-byte header lingers until the last weak ref goes so
//! `upgrade` can always safely read `strong` without a dangling access.
//!
//! Ownership split: **codegen** owns the box allocation (`malloc`, and it must
//! init BOTH counts to 1) and the recursive payload drop; **these primitives**
//! own only the count arithmetic and the box `free`. Non-atomic — the `par`/Arc
//! path gets atomic siblings in a later slice. This slice ships the runtime +
//! unit tests ONLY; no codegen is wired yet, so nothing calls these in a real
//! program until slice 3/4.

extern "C" {
    fn free(ptr: *mut core::ffi::c_void);
}

#[inline]
unsafe fn strong_ptr(b: *mut u8) -> *mut i64 {
    b as *mut i64
}

#[inline]
unsafe fn weak_ptr(b: *mut u8) -> *mut i64 {
    (b as *mut i64).add(1)
}

/// `weak += 1`. Returns the same box — a weak reference IS the same pointer;
/// liveness is read from `strong` at upgrade time, never cached. Codegen emits
/// this at a `weak`-field store (the downgrade), with NO strong retain.
#[no_mangle]
pub extern "C" fn karac_weak_downgrade(b: *mut u8) -> *mut u8 {
    if b.is_null() {
        return b;
    }
    unsafe {
        *weak_ptr(b) += 1;
    }
    b
}

/// `weak -= 1`; free the box iff BOTH counts are now zero (the payload was
/// already dropped when `strong` hit zero). Codegen emits this when a `weak`
/// field or binding leaves scope.
#[no_mangle]
pub extern "C" fn karac_weak_drop(b: *mut u8) {
    if b.is_null() {
        return;
    }
    unsafe {
        let w = weak_ptr(b);
        *w -= 1;
        if *w == 0 && *strong_ptr(b) == 0 {
            free(b as *mut core::ffi::c_void);
        }
    }
}

/// Upgrade a weak ref to a strong one. If the target is still alive
/// (`strong > 0`), `strong += 1` and return the box (the caller now holds a NEW
/// strong reference → `Some`); otherwise return null (`None`). Codegen wraps the
/// result as `Option[T]` at a `weak`-field read.
#[no_mangle]
pub extern "C" fn karac_weak_upgrade(b: *mut u8) -> *mut u8 {
    if b.is_null() {
        return core::ptr::null_mut();
    }
    unsafe {
        let s = strong_ptr(b);
        if *s > 0 {
            *s += 1;
            b
        } else {
            core::ptr::null_mut()
        }
    }
}

/// Strong-release TAIL for a weak-capable box: called by the codegen recursive
/// drop AFTER it has run the payload drop at `strong == 0` (so `strong` is
/// already 0 here). Drops the implicit weak the strong set held and frees the box
/// iff no weak refs remain. This REPLACES the plain `free(box)` that a non-weak
/// box's drop uses.
#[no_mangle]
pub extern "C" fn karac_weak_box_strong_zero_release(b: *mut u8) {
    if b.is_null() {
        return;
    }
    unsafe {
        let w = weak_ptr(b);
        *w -= 1;
        if *w == 0 {
            free(b as *mut core::ffi::c_void);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" {
        fn malloc(size: usize) -> *mut u8;
    }

    /// A fresh weak-capable box with `strong = weak = 1` (the codegen init).
    /// 16-byte header only — the tests never touch a payload.
    fn fresh_box() -> *mut u8 {
        unsafe {
            let b = malloc(16);
            *strong_ptr(b) = 1;
            *weak_ptr(b) = 1;
            b
        }
    }

    fn counts(b: *mut u8) -> (i64, i64) {
        unsafe { (*strong_ptr(b), *weak_ptr(b)) }
    }

    #[test]
    fn downgrade_bumps_weak_only() {
        let b = fresh_box();
        assert_eq!(counts(b), (1, 1));
        let w = karac_weak_downgrade(b);
        assert_eq!(w, b, "a weak ref is the same pointer");
        assert_eq!(counts(b), (1, 2), "downgrade bumps weak, not strong");
        // clean up: drop the extra weak, then release the strong (frees).
        karac_weak_drop(b); // weak 2->1, strong still 1 -> no free
        assert_eq!(counts(b), (1, 1));
        unsafe { *strong_ptr(b) = 0 } // simulate the codegen strong dec to zero
        karac_weak_box_strong_zero_release(b); // weak 1->0 -> free
    }

    #[test]
    fn upgrade_alive_bumps_strong() {
        let b = fresh_box();
        let up = karac_weak_upgrade(b);
        assert_eq!(up, b, "alive upgrade returns the box (Some)");
        assert_eq!(counts(b), (2, 1), "upgrade bumps strong");
        // two strong refs now; dec both, then release.
        unsafe { *strong_ptr(b) = 0 } // both strong refs gone
        karac_weak_box_strong_zero_release(b); // frees (no weak left)
    }

    #[test]
    fn upgrade_dead_returns_null() {
        // Target already dropped: strong == 0, one weak still outstanding.
        let b = fresh_box();
        unsafe { *strong_ptr(b) = 0 } // payload dropped; header alive via weak
        let up = karac_weak_upgrade(b);
        assert!(up.is_null(), "dead upgrade returns null (None), no UAF");
        assert_eq!(counts(b), (0, 1), "dead upgrade leaves counts untouched");
        karac_weak_drop(b); // weak 1->0, strong 0 -> free
    }

    #[test]
    fn box_survives_strong_zero_while_weak_outstanding_then_frees_on_last_weak() {
        // The core lifecycle: a weak ref outlives the strong set. The box must
        // NOT be freed at strong==0 (a weak ref still needs to read `strong`),
        // and must be freed exactly once when the last weak drops.
        let b = fresh_box();
        let _w = karac_weak_downgrade(b); // weak 1->2
        assert_eq!(counts(b), (1, 2));
        unsafe { *strong_ptr(b) = 0 } // codegen strong dec to zero (payload dropped)
        karac_weak_box_strong_zero_release(b); // weak 2->1: box STAYS (weak != 0)
        assert_eq!(counts(b), (0, 1), "box alive for the outstanding weak ref");
        // upgrade now sees the dead target:
        assert!(karac_weak_upgrade(b).is_null());
        karac_weak_drop(b); // weak 1->0, strong 0 -> free (exactly once)
    }

    #[test]
    fn null_is_inert() {
        let n = core::ptr::null_mut();
        assert!(karac_weak_downgrade(n).is_null());
        assert!(karac_weak_upgrade(n).is_null());
        karac_weak_drop(n); // no-op
        karac_weak_box_strong_zero_release(n); // no-op
    }
}
