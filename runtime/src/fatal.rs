//! Lean fatal-error reporting for the compute hot path.
//!
//! Runtime symbols reachable from any Vec/String program — `karac_alloc_or_panic`
//! (every `Vec.push`/`with_capacity`/`filled`), `karac_string_slice` (every
//! `String` slice), … — must NOT touch `std::io::stderr()` / `eprintln!` on
//! their fatal paths. The std-IO stderr handle anchors ~250 KB of std-IO `__TEXT`
//! that `-dead_strip` would otherwise strip from a lean compute binary, so a
//! single reachable use bloats *every* such binary (B-2026-06-11-8: a trivial
//! `Vec`/`String` program ballooned 33 KB → 285 KB). These helpers print via a
//! raw `write(2)` syscall and format into a stack buffer instead — no std-IO, no
//! heap, nothing that survives onto the hot path.

use core::fmt::{self, Write};

extern "C" {
    // POSIX `write(2)`; on wasm32-wasip1 this resolves to wasi-libc's `write`.
    fn write(fd: i32, buf: *const u8, count: usize) -> isize;
}

/// Best-effort raw write of `msg` to stderr (fd 2). Short writes are ignored —
/// the caller is already on a fatal path with nothing to fall back to.
pub fn write_stderr(msg: &[u8]) {
    // SAFETY: `msg` is a valid readable slice of `msg.len()` bytes; fd 2 is the
    // process's stderr. We discard the return value deliberately.
    unsafe {
        let _ = write(2, msg.as_ptr(), msg.len());
    }
}

/// A fixed-capacity stack buffer implementing [`core::fmt::Write`], so a fatal
/// path can `write!(buf, "...")` a formatted diagnostic with no heap and no
/// std-IO. Overflow truncates silently — this is a best-effort diagnostic, not
/// a contract. 256 bytes comfortably holds every current fatal message.
pub struct StackMsg {
    buf: [u8; 256],
    len: usize,
}

impl StackMsg {
    pub fn new() -> Self {
        Self {
            buf: [0; 256],
            len: 0,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl Default for StackMsg {
    fn default() -> Self {
        Self::new()
    }
}

impl Write for StackMsg {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let b = s.as_bytes();
        let n = b.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&b[..n]);
        self.len += n;
        Ok(())
    }
}

/// Format `args` into a stack buffer and write the result to stderr — the lean
/// `eprintln!` replacement for fatal paths. Appends no trailing newline; include
/// it in the format string.
pub fn eprint_fmt(args: fmt::Arguments) {
    let mut m = StackMsg::new();
    let _ = m.write_fmt(args);
    write_stderr(m.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stackmsg_formats_and_truncates() {
        let mut m = StackMsg::new();
        let _ = write!(m, "n={} end={}", 7, 42);
        assert_eq!(m.as_bytes(), b"n=7 end=42");

        // Overflow truncates rather than panicking.
        let mut big = StackMsg::new();
        for _ in 0..100 {
            let _ = write!(big, "0123456789");
        }
        assert_eq!(big.as_bytes().len(), 256);
    }
}
