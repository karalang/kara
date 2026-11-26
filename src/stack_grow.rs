//! Stack growth for the front end's and the interpreter's deep recursion:
//! `stacker::maybe_grow` natively, a plain call on wasm32, where psm has no
//! stack switching and the browser playground runs on the stack it is given
//! (B-2026-11-25-57).

/// Runs `f`, first moving onto a fresh `stack_size` segment when less than
/// `red_zone` bytes of the current stack remain (natively).
#[inline]
pub fn maybe_grow<R, F: FnOnce() -> R>(red_zone: usize, stack_size: usize, f: F) -> R {
    #[cfg(not(target_arch = "wasm32"))]
    {
        stacker::maybe_grow(red_zone, stack_size, f)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (red_zone, stack_size);
        f()
    }
}
