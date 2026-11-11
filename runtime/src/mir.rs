//! Natives for the MIR backend (`src/codegen/mir/`): printing, process
//! exit, and the string and vector operations its compiled bodies call.
//!
//! The values are laid out as `src/mir/layout.rs` says: a `String` or
//! `Vec[T]` is `{ptr, len, cap}` over a `malloc` buffer (`cap == 0` owns no
//! buffer, which is how a literal's bytes are borrowed), a `str` is
//! `{ptr, len}`. The interpreter's natives (`src/mir/interp.rs`) are the
//! reference semantics for each entry point here.

use std::cell::RefCell;

use crate::alloc::karac_realloc_or_panic;
use crate::fmt::format_spec::float_text;
use crate::lean_vec::LeanVec;

/// A `String` or `Vec[T]` as compiled code holds it.
#[repr(C)]
pub struct RawVec {
    pub ptr: *mut u8,
    #[cfg(target_pointer_width = "32")]
    _ptr_hi: u32,
    pub len: usize,
    #[cfg(target_pointer_width = "32")]
    _len_hi: u32,
    pub cap: usize,
    #[cfg(target_pointer_width = "32")]
    _cap_hi: u32,
}

impl RawVec {
    /// A vector of `len` elements in `cap` slots at `ptr`. Compiled code's
    /// layout is three 64-bit words whatever the target, so on a 32-bit one
    /// (wasm32) each field is padded to 8 bytes, the high half zero
    /// (B-2026-10-22-23).
    pub const fn new(ptr: *mut u8, len: usize, cap: usize) -> Self {
        RawVec {
            ptr,
            #[cfg(target_pointer_width = "32")]
            _ptr_hi: 0,
            len,
            #[cfg(target_pointer_width = "32")]
            _len_hi: 0,
            cap,
            #[cfg(target_pointer_width = "32")]
            _cap_hi: 0,
        }
    }
}

thread_local! {
    /// The text being formatted: a `println` or a `format` pushes where
    /// its text starts, appends its parts, and pops it off the end, so
    /// nested formats share one buffer and a format allocates nothing but
    /// its result.
    static FMT: RefCell<ThreadOwned<FmtBuf>> = const {
        RefCell::new(thread_owned(FmtBuf {
            buf: LeanVec::new(),
            starts: LeanVec::new(),
        }))
    };
    /// Standard output off the pool ([`OUT_SHARED`] on it).
    static OUT_LOCAL: RefCell<ThreadOwned<LeanVec<u8>>> = const {
        RefCell::new(thread_owned(LeanVec::new()))
    };
    /// The message `panic(msg)` or a native set for the abort that follows
    /// it, held in a [`LeanVec`] rather than a `String` so that setting it
    /// reaches no panic (B-2026-11-02-24).
    static PANIC: RefCell<ThreadOwned<PanicMessage>> = const {
        RefCell::new(thread_owned(PanicMessage { set: false, text: LeanVec::new() }))
    };
}

/// Standard output under the pool: one buffer for the program, so the
/// pool's workers write in order. Standard output is flushed at exit, at an
/// abort, and before anything is written to standard error so the two stay
/// in order on a terminal.
static OUT_SHARED: crate::mir_task::PoolLock<LeanVec<u8>> =
    crate::mir_task::PoolLock::new(LeanVec::new());

/// `f` of the program's standard output buffer, as `program_state!`
/// shares one, but panic-free ([`LeanVec`]).
fn with_out<R>(f: impl FnOnce(&mut LeanVec<u8>) -> R) -> R {
    if crate::mir_task::pooled() {
        return OUT_SHARED.with(f);
    }
    match OUT_LOCAL.try_with(|o| o.try_borrow_mut().map(|mut o| f(&mut o))) {
        Ok(Ok(r)) => {
            thread_bufs::release_at_exit();
            r
        }
        _ => buffer_in_use(),
    }
}

/// The panic message a [`PANIC`] holds, and whether one was set (an
/// empty `panic("")` is still one).
struct PanicMessage {
    set: bool,
    text: LeanVec<u8>,
}

/// `f` of this thread's panic message.
fn with_panic<R>(f: impl FnOnce(&mut PanicMessage) -> R) -> R {
    match PANIC.try_with(|p| p.try_borrow_mut().map(|mut p| f(&mut p))) {
        Ok(Ok(r)) => {
            thread_bufs::release_at_exit();
            r
        }
        _ => buffer_in_use(),
    }
}

/// Sets the message for the abort that follows, without a panic of its own.
pub(crate) fn set_panic_bytes(msg: &[u8]) {
    with_panic(|p| {
        p.text.clear();
        p.text.extend_from_slice(msg);
        p.set = true;
    });
}

/// Takes the message set for this abort, if one was.
fn take_panic_message() -> Option<String> {
    with_panic(|p| {
        if !std::mem::replace(&mut p.set, false) {
            return None;
        }
        let msg = String::from_utf8_lossy(p.text.as_slice()).into_owned();
        p.text.clear();
        Some(msg)
    })
}

/// A print buffer asked for while it is in use, which only a runtime
/// defect could do: the process ends, as a panic here would not let it
/// stay panic-free ([`LeanVec`]).
#[cold]
fn buffer_in_use() -> ! {
    crate::fatal::write_stderr(b"fatal runtime error: a print buffer is in use\n");
    std::process::abort()
}

/// A thread-local print buffer: one std's thread-local storage does not
/// drop, since registering its destructor reaches a panic; the thread's
/// buffers are freed by [`thread_bufs`] instead.
#[cfg(all(unix, feature = "net"))]
type ThreadOwned<T> = std::mem::ManuallyDrop<T>;
#[cfg(not(all(unix, feature = "net")))]
type ThreadOwned<T> = T;

#[cfg(all(unix, feature = "net"))]
const fn thread_owned<T>(v: T) -> ThreadOwned<T> {
    std::mem::ManuallyDrop::new(v)
}
#[cfg(not(all(unix, feature = "net")))]
const fn thread_owned<T>(v: T) -> ThreadOwned<T> {
    v
}

/// Frees a thread's print buffers when it exits, through a pthread key's
/// destructor in place of std's thread-local one (B-2026-10-28-36).
#[cfg(all(unix, feature = "net"))]
mod thread_bufs {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{FMT, OUT_LOCAL, PANIC};

    /// The key, plus one; 0 before it is made.
    static KEY: AtomicUsize = AtomicUsize::new(0);

    thread_local! {
        static SET: Cell<bool> = const { Cell::new(false) };
    }

    /// Has this thread's buffers freed when it exits.
    #[inline]
    pub(super) fn release_at_exit() {
        if SET.try_with(Cell::get).unwrap_or(true) {
            return;
        }
        let Some(key) = key() else { return };
        SET.with(|s| s.set(true));
        // SAFETY: a key made by `key`; the value only marks the thread.
        unsafe { libc::pthread_setspecific(key, std::ptr::dangling::<u8>().cast()) };
    }

    fn key() -> Option<libc::pthread_key_t> {
        match KEY.load(Ordering::Acquire) {
            0 => {}
            k => return Some((k - 1) as libc::pthread_key_t),
        }
        // SAFETY: `release` is a destructor of the type pthreads expects.
        let mut key: libc::pthread_key_t = 0;
        if unsafe { libc::pthread_key_create(&mut key, Some(release)) } != 0 {
            return None;
        }
        match KEY.compare_exchange(0, key as usize + 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => Some(key),
            Err(k) => {
                // Another thread made one first.
                // SAFETY: the key just made, which nothing has used.
                unsafe { libc::pthread_key_delete(key) };
                Some((k - 1) as libc::pthread_key_t)
            }
        }
    }

    /// Frees the exiting thread's buffers, leaving empty ones that a
    /// later use (another destructor's print) would grow again.
    extern "C" fn release(_: *mut libc::c_void) {
        let _ = SET.try_with(|s| s.set(false));
        let _ = FMT.try_with(|f| {
            if let Ok(mut f) = f.try_borrow_mut() {
                drop(std::mem::take(&mut f.buf));
                drop(std::mem::take(&mut f.starts));
            }
        });
        let _ = OUT_LOCAL.try_with(|o| {
            if let Ok(mut o) = o.try_borrow_mut() {
                drop(std::mem::take(&mut **o));
            }
        });
        let _ = PANIC.try_with(|p| {
            if let Ok(mut p) = p.try_borrow_mut() {
                p.set = false;
                drop(std::mem::take(&mut p.text));
            }
        });
    }
}

#[cfg(not(all(unix, feature = "net")))]
mod thread_bufs {
    /// Std's thread-local destructors free the buffers here.
    #[inline]
    pub(super) fn release_at_exit() {}
}

const OUT_LIMIT: usize = 1 << 16;

struct FmtBuf {
    buf: LeanVec<u8>,
    starts: LeanVec<usize>,
}

/// `f` of this thread's format buffer.
fn with_fmt<R>(f: impl FnOnce(&mut FmtBuf) -> R) -> R {
    match FMT.try_with(|b| b.try_borrow_mut().map(|mut b| f(&mut b))) {
        Ok(Ok(r)) => {
            thread_bufs::release_at_exit();
            r
        }
        _ => buffer_in_use(),
    }
}

/// Pops the innermost format's text, handing it to `f`.
fn fmt_pop<T>(f: impl FnOnce(&[u8]) -> T) -> T {
    with_fmt(|b| {
        let start = b.starts.pop().unwrap_or(b.buf.len());
        let out = f(b.buf.as_slice().get(start..).unwrap_or_default());
        b.buf.truncate(start);
        out
    })
}

pub(crate) fn out_flush() {
    with_out(|o| {
        if !o.is_empty() {
            crate::fatal::write_all(1, o.as_slice());
            o.clear();
        }
    });
}

fn write(stderr: bool, bytes: &[u8]) {
    if stderr {
        out_flush();
        crate::fatal::write_all(2, bytes);
    } else {
        let full = with_out(|o| {
            o.extend_from_slice(bytes);
            // A terminal sees each line as it ends (docs/library/io.md §
            // Buffering); a pipe or file gets the block.
            o.len() >= OUT_LIMIT || (bytes.contains(&b'\n') && stdout_is_terminal())
        });
        if full {
            out_flush();
        }
    }
}

/// Whether standard output is a terminal, asked once.
fn stdout_is_terminal() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static TERMINAL: AtomicU8 = AtomicU8::new(0);
    match TERMINAL.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let t = is_terminal(1);
            TERMINAL.store(1 + u8::from(t), Ordering::Relaxed);
            t
        }
    }
}

/// Whether the file descriptor `fd` is a terminal.
#[cfg(unix)]
fn is_terminal(fd: i32) -> bool {
    extern "C" {
        fn isatty(fd: i32) -> i32;
    }
    // SAFETY: a query of a descriptor number.
    unsafe { isatty(fd) == 1 }
}

#[cfg(not(unix))]
fn is_terminal(fd: i32) -> bool {
    use std::io::IsTerminal;
    if fd == 2 {
        std::io::stderr().is_terminal()
    } else {
        std::io::stdout().is_terminal()
    }
}

/// # Safety
/// `ptr` is readable for `len` bytes (or `len` is 0).
unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 {
        &[]
    } else {
        // SAFETY: the caller's contract.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

fn append(s: &str) {
    append_bytes(s.as_bytes());
}

fn append_bytes(s: &[u8]) {
    let direct = with_fmt(|f| {
        if f.starts.is_empty() {
            return true;
        }
        f.buf.extend_from_slice(s);
        false
    });
    if direct {
        write(false, s);
    }
}

/// The decimal digits of `v`, at the end of `out`.
fn digits(mut v: u64, out: &mut [u8; 20]) -> usize {
    let mut i = out.len();
    loop {
        i -= 1;
        out[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            return i;
        }
    }
}

/// Writes what is buffered for standard output.
#[no_mangle]
pub extern "C" fn karac_mir_flush() {
    out_flush();
}

/// Starts formatting a `println` / `print` / `format`.
#[no_mangle]
pub extern "C" fn karac_mir_fmt_begin() {
    with_fmt(|f| {
        let at = f.buf.len();
        f.starts.push(at);
    });
}

/// # Safety
/// `ptr` is readable for `len` bytes of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_fmt_str(ptr: *const u8, len: u64) {
    let len = len as usize;
    // SAFETY: the caller's contract; a `str` holds UTF-8.
    append_bytes(unsafe { bytes(ptr, len) });
}

#[no_mangle]
pub extern "C" fn karac_mir_fmt_i64(v: i64) {
    let mut d = [0u8; 20];
    let i = digits(v.unsigned_abs(), &mut d);
    if v < 0 {
        d[i - 1] = b'-';
        append_bytes(&d[i - 1..]);
    } else {
        append_bytes(&d[i..]);
    }
}

#[no_mangle]
pub extern "C" fn karac_mir_fmt_u64(v: u64) {
    let mut d = [0u8; 20];
    let i = digits(v, &mut d);
    append_bytes(&d[i..]);
}

#[no_mangle]
pub extern "C" fn karac_mir_fmt_i128(lo: u64, hi: u64, signed: bool) {
    let bits = (u128::from(hi) << 64) | u128::from(lo);
    let neg = signed && (bits as i128) < 0;
    let mut v = if neg {
        (bits as i128).unsigned_abs()
    } else {
        bits
    };
    // Written out rather than `to_string`: `u128`'s `Display` keeps a
    // bounds panic, and with it std's panic hook, in every binary that
    // prints one (B-2026-11-04-31). 39 digits and a sign at most.
    let mut d = [0u8; 40];
    let mut i = d.len();
    loop {
        i -= 1;
        if let Some(slot) = d.get_mut(i) {
            *slot = b'0' + (v % 10) as u8;
        }
        v /= 10;
        if v == 0 {
            break;
        }
    }
    if neg {
        i -= 1;
        if let Some(slot) = d.get_mut(i) {
            *slot = b'-';
        }
    }
    append_bytes(d.get(i..).unwrap_or_default());
}

// A float's `Display` is at most 24 bytes (`-1.2345678901234567e-308`).
// It is written by `float_text`, which nothing in can panic: core's float
// formatting kept std's panic hook in every binary that printed a float
// (B-2026-11-06-2).
#[no_mangle]
pub extern "C" fn karac_mir_fmt_f64(v: f64) {
    let mut text = float_text::Stack::<32>::new();
    float_text::write_display_f64(&mut text, v);
    append_bytes(text.as_slice());
}

#[no_mangle]
pub extern "C" fn karac_mir_fmt_f32(v: f32) {
    let mut text = float_text::Stack::<32>::new();
    float_text::write_display_f32(&mut text, v);
    append_bytes(text.as_slice());
}

#[no_mangle]
pub extern "C" fn karac_mir_fmt_bool(v: u8) {
    append(if v != 0 { "true" } else { "false" });
}

#[no_mangle]
pub extern "C" fn karac_mir_fmt_char(c: u32) {
    let mut b = [0u8; 4];
    append(char::from_u32(c).unwrap_or('\u{fffd}').encode_utf8(&mut b));
}

/// Ends a `println` (bit 0 of `flags`: add a newline; bit 1: to standard
/// error).
#[no_mangle]
pub extern "C" fn karac_mir_print_end(flags: u32) {
    if flags & 1 != 0 {
        with_fmt(|f| {
            if f.starts.is_empty() {
                let at = f.buf.len();
                f.starts.push(at);
            }
            f.buf.push(b'\n');
        });
    }
    fmt_pop(|text| write(flags & 2 != 0, text));
}

/// Ends a `format` into the `String` at `out`.
///
/// # Safety
/// `out` is writable for a `String`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_fmt_end_string(out: *mut RawVec) {
    // SAFETY: the caller's contract.
    fmt_pop(|text| unsafe { string_of(out, text) });
}

/// One f-string hole with a spec (`{x:.3}`, `{n:04}`, `{s:>10}`): the
/// value, rendered by `kind` (0 a signed integer `lo`, 1 an unsigned one,
/// 2 a 128-bit signed `hi:lo`, 3 an unsigned one, 4 the float `f`, 5 the
/// text the innermost format buffer holds, which is popped) through the
/// interpreter's own `FormatSpec`, as a `String` at `out`. The spec is
/// parsed on each call: `parse_bytes` reads a few bytes and allocates
/// nothing. Nothing on this path can panic (B-2026-11-06-2).
///
/// # Safety
/// `spec` is readable for `spec_len` bytes; `out` is writable for a
/// `String`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_fmt_spec(
    out: *mut RawVec,
    spec: *const u8,
    spec_len: u64,
    kind: i32,
    lo: u64,
    hi: u64,
    f: f64,
) {
    use crate::fmt::format_spec::FormatSpec;
    // SAFETY: the caller's contract.
    let spec_bytes = unsafe { bytes(spec, spec_len as usize) };
    let Ok(fs) = FormatSpec::parse_bytes(spec_bytes) else {
        // The parser refuses a bad spec, so a compiled program never has
        // one.
        if kind == 5 {
            fmt_pop(|_| ());
        }
        // SAFETY: the caller's contract.
        unsafe { string_of(out, b"<bad format spec>") };
        return;
    };
    let wide = (u128::from(hi) << 64) | u128::from(lo);
    if kind == 5 {
        let mut t = LeanVec::new();
        fmt_pop(|text| fs.write_str(text, &mut t));
        // SAFETY: the caller's contract.
        unsafe { string_of(out, t.as_slice()) };
        return;
    }
    let render = |t: &mut dyn float_text::Out| spec_number(&fs, kind, wide, f, t);
    // A number's text nearly always fits on the stack; a wider one (a
    // big width or precision) is written again into the heap. Not a
    // thread-local buffer: one with a destructor brings in std's
    // destructor list, whose growth can panic.
    let mut text = float_text::Stack::<128>::new();
    render(&mut text);
    if !text.overflowed() {
        // SAFETY: the caller's contract.
        unsafe { string_of(out, text.as_slice()) };
        return;
    }
    let mut t = LeanVec::new();
    render(&mut t);
    // SAFETY: the caller's contract.
    unsafe { string_of(out, t.as_slice()) };
}

/// The number `karac_mir_fmt_spec` was handed (`kind` 0 to 4, or 6 for an
/// `f32` widened to `f`; a 64-bit one in the low half of `wide`), written to
/// `t`.
fn spec_number(
    fs: &crate::fmt::format_spec::FormatSpec,
    kind: i32,
    wide: u128,
    f: f64,
    t: &mut dyn float_text::Out,
) {
    let mut t = DynOut(t);
    match kind {
        0 | 1 => fs.write_int64(wide as u64, kind == 0, &mut t),
        2 | 3 => fs.write_int128(wide, kind == 2, &mut t),
        6 => fs.write_float32(f as f32, &mut t),
        _ => fs.write_float(f, &mut t),
    }
}

/// An `Out` behind a `dyn`, so one rendering serves both buffers.
struct DynOut<'a>(&'a mut dyn float_text::Out);

impl float_text::Out for DynOut<'_> {
    fn put(&mut self, b: &[u8]) {
        self.0.put(b);
    }
    fn repeat(&mut self, b: &[u8], n: usize) {
        self.0.repeat(b, n);
    }
}

impl float_text::Out for LeanVec<u8> {
    fn put(&mut self, b: &[u8]) {
        self.extend_from_slice(b);
    }
}

/// Writes a fresh `String` holding `b` at `out`.
///
/// # Safety
/// `out` is writable for a `String`.
pub(crate) unsafe fn string_of(out: *mut RawVec, b: &[u8]) {
    let ptr = if b.is_empty() {
        std::ptr::null_mut()
    } else {
        let p = karac_realloc_or_panic(std::ptr::null_mut(), b.len());
        // SAFETY: `p` has room for `b.len()` bytes.
        unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), p, b.len()) };
        p
    };
    // SAFETY: the caller's contract.
    unsafe { out.write(RawVec::new(ptr, b.len(), b.len())) };
}

/// `Env.args()`: the program's arguments as a `Vec[String]` at `out`. Under
/// `karac run` the hosting process is the JIT runner, so the program's argv
/// comes from `KARAC_PROGRAM_ARGS` (unit-separator joined, as
/// `karac_runtime_env_args_into` reads it); a built binary's is its own.
///
/// # Safety
/// `out` is writable for a `Vec[String]`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_env_args(out: *mut RawVec) {
    let args: Vec<String> = match std::env::var("KARAC_PROGRAM_ARGS") {
        Ok(packed) => packed.split('\u{1F}').map(str::to_string).collect(),
        Err(_) => std::env::args_os()
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
    };
    let size = std::mem::size_of::<RawVec>();
    let ptr = if args.is_empty() {
        std::ptr::null_mut()
    } else {
        karac_realloc_or_panic(std::ptr::null_mut(), args.len() * size)
    };
    for (i, a) in args.iter().enumerate() {
        // SAFETY: `ptr` has room for `args.len()` strings.
        unsafe { string_of(ptr.add(i * size).cast(), a.as_bytes()) };
    }
    // SAFETY: the caller's contract.
    unsafe { out.write(RawVec::new(ptr, args.len(), args.len())) };
}

/// `String.from(s)`: a copy of the `str` `ptr[..len]`.
///
/// # Safety
/// `out` is writable for a `String`; `ptr` readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_string_from(out: *mut RawVec, ptr: *const u8, len: u64) {
    let len = len as usize;
    // SAFETY: the caller's contract.
    unsafe { string_of(out, bytes(ptr, len)) };
}

/// The fewest elements of `size` bytes a vector's first block holds, as
/// Rust's `Vec` starts: 8 for bytes, 4 for elements up to 1 KiB, 1 beyond.
/// A start of 8 for every element up to 256 bytes (B-2026-10-14-69, for
/// kata 638's six-element memo keys, which presizing now covers) doubled
/// the memory of many short `Vec[i64]`s: kata 3629's million factor lists
/// took 64 bytes each where Rust takes 32, 89 MiB of RSS against 60 on the
/// M5 (B-2026-11-04-38). The MIR interpreter's `try_*` twins use the same
/// rule.
pub(crate) fn min_first_cap(size: usize) -> usize {
    match size {
        0..=1 => 8,
        2..=1024 => 4,
        _ => 1,
    }
}

/// Makes room for `more` elements of `size` bytes in the vector at `v`.
///
/// # Safety
/// `v` is a live `String` or `Vec` of `size`-byte elements.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_vec_reserve(v: *mut RawVec, more: u64, size: u64) {
    let more = more as usize;
    let size = size as usize;
    // SAFETY: the caller's contract.
    let v = unsafe { &mut *v };
    let Some(want) = v.len.checked_add(more) else {
        // Not `expect`: a panic here keeps std's panic hook and backtrace
        // symbolizer in every binary that grows a vector (B-2026-10-28-36).
        crate::fatal::write_stderr(b"panic: capacity overflow\n");
        std::process::abort()
    };
    if want <= v.cap || size == 0 {
        if size == 0 {
            v.cap = usize::MAX;
        }
        return;
    }
    let cap = want.max(v.cap.saturating_mul(2)).max(min_first_cap(size));
    let old = if v.cap == 0 && v.len > 0 {
        // A borrowed literal: copy it out before growing.
        let p = karac_realloc_or_panic(std::ptr::null_mut(), cap * size);
        // SAFETY: the literal holds `len` elements; `p` has room.
        unsafe { std::ptr::copy_nonoverlapping(v.ptr, p, v.len * size) };
        v.ptr = p;
        v.cap = cap;
        return;
    } else if v.cap == 0 {
        std::ptr::null_mut()
    } else {
        v.ptr
    };
    v.ptr = karac_realloc_or_panic(old, cap * size);
    v.cap = cap;
}

/// The bytes `n` elements of `size` bytes take, or `None` when that is
/// more than any buffer may hold ([`crate::alloc::KARAC_MAX_ALLOC_BYTES`]):
/// the capacity cannot be represented. The MIR interpreter uses the same
/// bound for its `CapacityOverflow`.
fn capacity_bytes(n: usize, size: usize) -> Option<usize> {
    n.checked_mul(size)
        .filter(|&b| b as u64 <= crate::alloc::KARAC_MAX_ALLOC_BYTES)
}

/// `reserve` for the fallible twins (`try_reserve`, `try_reserve_exact`):
/// makes room for `more` elements of `size` bytes in the vector at `v` and
/// returns -1, or leaves it unchanged and returns -2 when the capacity
/// cannot be represented, or the byte count the allocator refused.
///
/// # Safety
/// `v` is a live `String` or `Vec` of `size`-byte elements.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_vec_try_reserve(v: *mut RawVec, more: u64, size: u64) -> i64 {
    let more = more as usize;
    let size = size as usize;
    // SAFETY: the caller's contract.
    let v = unsafe { &mut *v };
    let Some(want) = v.len.checked_add(more) else {
        return -2;
    };
    if capacity_bytes(want, size).is_none() {
        return -2;
    }
    if want <= v.cap || size == 0 {
        if size == 0 {
            v.cap = usize::MAX;
        }
        return -1;
    }
    // The growth `karac_mir_vec_reserve` picks, unless only `want` fits.
    let grown = want.max(v.cap.saturating_mul(2)).max(min_first_cap(size));
    let cap = if capacity_bytes(grown, size).is_some() {
        grown
    } else {
        want
    };
    let bytes = cap * size;
    let p = crate::alloc::karac_alloc_fallible(bytes);
    if p.is_null() {
        return bytes as i64;
    }
    if v.len > 0 {
        // SAFETY: the vector holds `len` elements; `p` has room for them.
        unsafe { std::ptr::copy_nonoverlapping(v.ptr, p, v.len * size) };
    }
    if v.cap != 0 {
        // SAFETY: an owned buffer (a borrowed literal has `cap == 0`).
        unsafe { free(v.ptr.cast()) };
    }
    v.ptr = p;
    v.cap = cap;
    -1
}

/// `with_capacity` for the fallible twin: writes an empty `Vec` with room
/// for exactly `n` elements of `size` bytes at `out` and returns -1, or
/// writes nothing and returns -2 or the refused byte count, as
/// [`karac_mir_vec_try_reserve`] does. Without an allocation (`n` or `size`
/// zero) the vector is empty with no buffer.
///
/// # Safety
/// `out` is writable for a `Vec`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_vec_try_alloc(out: *mut RawVec, n: u64, size: u64) -> i64 {
    let n = n as usize;
    let size = size as usize;
    let Some(bytes) = capacity_bytes(n, size) else {
        return -2;
    };
    let (ptr, cap) = if bytes == 0 {
        (std::ptr::null_mut(), 0)
    } else {
        let p = crate::alloc::karac_alloc_fallible(bytes);
        if p.is_null() {
            return bytes as i64;
        }
        (p, n)
    };
    // SAFETY: the caller's contract.
    unsafe { out.write(RawVec::new(ptr, 0, cap)) };
    -1
}

/// -1 when `n` elements of `size` bytes can be represented, -2 when not:
/// the check of `VecDeque.try_with_capacity`, which allocates nothing.
#[no_mangle]
pub extern "C" fn karac_mir_capacity_fits(n: u64, size: u64) -> i64 {
    let n = n as usize;
    let size = size as usize;
    if capacity_bytes(n, size).is_some() {
        -1
    } else {
        -2
    }
}

/// Writes a `Vec` of `n` uninitialized `size`-byte elements at `out`
/// (`len == cap == n`), for the caller to fill.
///
/// # Safety
/// `out` is writable for a `Vec`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_vec_alloc(out: *mut RawVec, n: u64, size: u64) {
    let n = n as usize;
    let size = size as usize;
    let Some(bytes) = n.checked_mul(size) else {
        // Not `expect`, as in `karac_mir_vec_reserve` (B-2026-11-02-24).
        crate::fatal::write_stderr(b"panic: capacity overflow\n");
        std::process::abort()
    };
    let ptr = if bytes == 0 {
        std::ptr::null_mut()
    } else {
        karac_realloc_or_panic(std::ptr::null_mut(), bytes)
    };
    let cap = if bytes == 0 { 0 } else { n };
    // SAFETY: the caller's contract.
    unsafe { out.write(RawVec::new(ptr, n, cap)) };
}

/// Whether `a[..an]` and `b[..bn]` hold the same bytes.
///
/// # Safety
/// Each pointer is readable for its length.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_bytes_eq(a: *const u8, an: u64, b: *const u8, bn: u64) -> u8 {
    let an = an as usize;
    let bn = bn as usize;
    // SAFETY: the caller's contract.
    u8::from(unsafe { bytes(a, an) == bytes(b, bn) })
}

/// The order of `a[..an]` and `b[..bn]` as byte strings: -1, 0 or 1.
///
/// # Safety
/// Each pointer is readable for its length.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_bytes_cmp(a: *const u8, an: u64, b: *const u8, bn: u64) -> i32 {
    let an = an as usize;
    let bn = bn as usize;
    // SAFETY: the caller's contract.
    match unsafe { bytes(a, an).cmp(bytes(b, bn)) } {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// Appends the `size` bytes at `elem` to the vector at `v`.
///
/// # Safety
/// As [`karac_mir_vec_reserve`]; `elem` readable for `size` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_vec_push(v: *mut RawVec, elem: *const u8, size: u64) {
    let size = size as usize;
    // SAFETY: the caller's contract.
    unsafe {
        karac_mir_vec_reserve(v, 1, size as u64);
        let v = &mut *v;
        if size != 0 {
            std::ptr::copy_nonoverlapping(elem, v.ptr.add(v.len * size), size);
        }
        v.len += 1;
    }
}

/// Appends the bytes `ptr[..len]` to the `String` at `s`.
///
/// # Safety
/// `s` is a live `String`; `ptr` readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_string_push_str(s: *mut RawVec, ptr: *const u8, len: u64) {
    let len = len as usize;
    // SAFETY: the caller's contract.
    unsafe {
        karac_mir_vec_reserve(s, len as u64, 1);
        let v = &mut *s;
        if len != 0 {
            std::ptr::copy_nonoverlapping(ptr, v.ptr.add(v.len), len);
        }
        v.len += len;
    }
}

/// Appends the UTF-8 of the char `c` to the `String` at `s`.
///
/// # Safety
/// `s` is a live `String`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_string_push_char(s: *mut RawVec, c: u32) {
    let mut buf = [0u8; 4];
    let t = char::from_u32(c)
        .unwrap_or('\u{fffd}')
        .encode_utf8(&mut buf);
    // SAFETY: the caller's contract.
    unsafe { karac_mir_string_push_str(s, t.as_ptr(), t.len() as u64) };
}

/// # Safety
/// `ptr` is readable for `len` bytes of UTF-8.
unsafe fn text<'a>(ptr: *const u8, len: usize) -> &'a str {
    // SAFETY: the caller's contract; a `str` or `String` holds UTF-8.
    unsafe { std::str::from_utf8_unchecked(bytes(ptr, len)) }
}

/// A test of `a` against `b`: 0 `contains`, 1 `starts_with`, 2
/// `ends_with`, 3 `a < b`.
///
/// # Safety
/// Each pointer is readable for its length, of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_test(
    op: i32,
    a: *const u8,
    an: u64,
    b: *const u8,
    bn: u64,
) -> u8 {
    let an = an as usize;
    let bn = bn as usize;
    // SAFETY: the caller's contract.
    let (a, b) = unsafe { (text(a, an), text(b, bn)) };
    u8::from(match op {
        0 => contains(a, b),
        1 => a.starts_with(b),
        2 => a.ends_with(b),
        _ => a < b,
    })
}

extern "C" {
    fn memchr(s: *const std::ffi::c_void, c: std::ffi::c_int, n: usize) -> *mut std::ffi::c_void;
}

/// Whether `needle` is in `hay`. Each place `memchr` finds the needle's
/// first byte is compared byte by byte, which on ordinary text ends after a
/// byte or two; once those comparisons have cost twice the haystack, the
/// rest goes to Rust's two-way search, so the worst case stays linear. A
/// two-way search alone sets up a factorization of the needle on every
/// call, which made a 100-byte `contains` cost some 1.2 us (kata 796,
/// B-2026-10-19-26).
fn contains(hay: &str, needle: &str) -> bool {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    let Some(&first) = n.first() else {
        return true;
    };
    let m = n.len();
    if m > h.len() {
        return false;
    }
    // A needle of up to 256 bytes whose first byte keeps turning up goes to
    // the shift-table search, which calls nothing per window
    // (B-2026-10-29-32); `memchr` stays faster where that byte is rare.
    let short = m <= 256;
    let (hp, np) = (h.as_ptr(), n.as_ptr());
    // The places the needle can start.
    let starts = h.len() - m + 1;
    let budget = 2 * h.len() + 64;
    let (mut i, mut spent, mut hits) = (0, 0, 0usize);
    while i < starts {
        // SAFETY: `hp + i` is readable for `starts - i` bytes.
        let p = unsafe { memchr(hp.add(i).cast(), first.into(), starts - i) };
        if p.is_null() {
            return false;
        }
        let at = p as usize - hp as usize;
        // SAFETY: `at < starts`, so `at + k < h.len()` for every `k < m`.
        let k = (1..m)
            .find(|&k| unsafe { *hp.add(at + k) != *np.add(k) })
            .unwrap_or(m);
        if k == m {
            return true;
        }
        spent += k;
        hits += 1;
        if short && hits * 16 > at + 64 {
            return find_short(h.get(at + 1..).unwrap_or_default(), n);
        }
        if spent > budget {
            return hay.contains(needle);
        }
        i = at + 1;
    }
    false
}

/// The hash of the byte pair that ends at `p`, which [`find_short`]'s shift
/// table is indexed by.
///
/// # Safety
/// `p - 1` and `p` are readable.
#[inline(always)]
unsafe fn pair_hash(p: *const u8) -> usize {
    // SAFETY: the caller's contract.
    let (a, b) = unsafe { (usize::from(*p.sub(1)), usize::from(*p)) };
    b.wrapping_sub(a << 3) & 255
}

/// Whether `n`, of 2 to 256 bytes and no longer than `h`, is in `h`: a
/// Horspool search keyed on the byte pair at the window's end, the
/// short-needle path of glibc's `memmem`. A window whose end pair is
/// nowhere in the needle moves on by the whole needle, one whose pair is
/// moves to line the two up, and only a window whose end pair hashes like
/// the needle's own is compared. Nothing is called per window, where a
/// `memchr` for the first byte made two calls per two bytes of a binary
/// string (B-2026-10-29-32). The worst case is the needle's length times
/// the haystack's, at most 256 times.
fn find_short(h: &[u8], n: &[u8]) -> bool {
    let m = n.len();
    if !(2..=256).contains(&m) || m > h.len() {
        return false;
    }
    let m1 = m - 1;
    let (hp, np) = (h.as_ptr(), n.as_ptr());
    // `shift[x]` is the last place below `m1` that a pair of hash `x` ends
    // at in the needle, 0 for none, and `m1` for the needle's end pair.
    let mut shift = [0u8; 256];
    for j in 1..m1 {
        // SAFETY: `1 <= j < m`.
        shift[unsafe { pair_hash(np.add(j)) }] = j as u8;
    }
    // SAFETY: `1 <= m1 < m`.
    let end = unsafe { pair_hash(np.add(m1)) };
    // How far a window moves once its end pair matched and the rest did not.
    let shift1 = m1 - usize::from(shift[end]);
    shift[end] = m1 as u8;
    // `p` is where in `h` the window's last byte is.
    let last = h.len() - 1;
    let mut p = m1;
    while p <= last {
        // SAFETY: `1 <= m1 <= p <= last`.
        let at = usize::from(shift[unsafe { pair_hash(hp.add(p)) }]);
        if at == 0 {
            p += m1;
        } else if at < m1 {
            p += m1 - at;
        } else {
            // SAFETY: the window `p - m1 ..= p` is in `h`, and `n` is `m`
            // bytes.
            if (0..m).all(|k| unsafe { *hp.add(p - m1 + k) == *np.add(k) }) {
                return true;
            }
            p += shift1;
        }
    }
    false
}

/// A new `String` at `out` from `s`: 0 `trim`, 1 `to_uppercase`, 2
/// `to_lowercase`, 3 `trim_start`, 4 `trim_end`, 5 its chars sorted.
///
/// # Safety
/// `out` is writable for a `String`; `s` readable for `n` bytes of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_map(out: *mut RawVec, op: i32, s: *const u8, n: u64) {
    let n = n as usize;
    // SAFETY: the caller's contract.
    let s = unsafe { text(s, n) };
    // The trims and an ASCII sort build the result in place, with no
    // string in between.
    let view = match op {
        0 => Some(s.trim()),
        3 => Some(s.trim_start()),
        4 => Some(s.trim_end()),
        5 if s.is_ascii() => Some(s),
        _ => None,
    };
    if let Some(v) = view {
        // SAFETY: as above.
        unsafe {
            string_of(out, v.as_bytes());
            if op == 5 && !(*out).ptr.is_null() {
                std::slice::from_raw_parts_mut((*out).ptr, (*out).len).sort_unstable();
            }
        }
        return;
    }
    let t = match op {
        1 => s.to_uppercase(),
        2 => s.to_lowercase(),
        _ => {
            let mut cs: Vec<char> = s.chars().collect();
            cs.sort_unstable();
            cs.into_iter().collect()
        }
    };
    // SAFETY: as above.
    unsafe { string_of(out, t.as_bytes()) };
}

/// The bytes `lo..hi` of `s` that mode `mode` picks, or `None` for a
/// panic. Mode 0 is `substring`, which saturates (an end past the text is
/// its end, an empty or inverted range is empty); mode 1 is `s[lo..hi]`,
/// which panics when out of range or inverted; both panic off a char
/// boundary. Mode 2 is `String.slice`: a start out of range is empty, the
/// end clamps, and a cut inside a char is lossy, so its text may be new.
/// The message of a string cut [`str_range`] refuses, as the interpreter
/// words it, for the abort that follows.
fn str_range_panic(s: &str, lo: i64, hi: i64, mode: i32) {
    let len = s.len() as i64;
    // `substring` refuses only a cut inside a code point, and each bound
    // saturates to `0..=len` (B-2026-11-03-16).
    let (lo, hi) = if mode == 0 {
        // `max`/`min`, not `clamp`: `clamp`'s `min <= max` assert would
        // link std's panic hook into every binary that slices.
        (lo.max(0).min(len), hi.max(0).min(len))
    } else {
        (lo, hi)
    };
    // Written out rather than `format!`ed: core's integer and `str`
    // formatting keep a bounds panic, and with it std's panic hook, in
    // every binary that slices a string (B-2026-11-02-24).
    let mut msg = LeanVec::<u8>::new();
    let range = |msg: &mut LeanVec<u8>| {
        push_i64(msg, lo);
        msg.extend_from_slice(b"..");
        push_i64(msg, hi);
    };
    // Either failure is a slice out of bounds (`index_out_of_bounds`), as
    // the MIR interpreter reports it.
    PANIC_KIND.store(1, std::sync::atomic::Ordering::Relaxed);
    if mode != 0 && !(0 <= lo && lo <= hi && hi <= len) {
        msg.extend_from_slice(b"string slice bounds ");
        range(&mut msg);
        msg.extend_from_slice(b" out of range (len ");
        push_i64(&mut msg, len);
        msg.push(b')');
    } else {
        msg.extend_from_slice(b"E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY: byte range ");
        range(&mut msg);
        msg.extend_from_slice(b" does not fall on UTF-8 char boundaries of ");
        push_str_debug(&mut msg, s);
    }
    set_panic_bytes(msg.as_slice());
}

/// `v` in decimal, as `karac_mir_fmt_i64` writes it.
fn push_i64(out: &mut LeanVec<u8>, v: i64) {
    let mut d = [0u8; 20];
    let i = digits(v.unsigned_abs(), &mut d);
    if v < 0 {
        out.push(b'-');
    }
    out.extend_from_slice(d.get(i..).unwrap_or_default());
}

/// `s` as `{:?}` writes a `str`: quoted, with what `str::escape_debug`
/// escapes escaped, except a single quote, which `Debug` leaves alone.
fn push_str_debug(out: &mut LeanVec<u8>, s: &str) {
    let mut put = |c: char| {
        let mut b = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
    };
    put('"');
    let mut esc = s.escape_debug();
    while let Some(c) = esc.next() {
        if c != '\\' {
            put(c);
            continue;
        }
        // Every backslash `escape_debug` writes opens an escape.
        match esc.next() {
            Some('\'') => put('\''),
            Some(n) => {
                put('\\');
                put(n);
            }
            None => put('\\'),
        }
    }
    put('"');
}

/// The text [`str_range`] picks: a view into the string, or, for a byte
/// range cut inside a code point (`String.slice`), those bytes made valid
/// as `String::from_utf8_lossy` makes them, in a [`LeanVec`] so that the
/// natives that slice stay panic-free (B-2026-11-02-24).
enum Picked<'a> {
    Text(&'a str),
    Lossy(LeanVec<u8>),
}

impl Picked<'_> {
    fn as_bytes(&self) -> &[u8] {
        match self {
            Picked::Text(t) => t.as_bytes(),
            Picked::Lossy(b) => b.as_slice(),
        }
    }
}

/// `b` as text, each invalid sequence replaced with U+FFFD.
fn lossy(b: &[u8]) -> Picked<'_> {
    if let Ok(t) = std::str::from_utf8(b) {
        return Picked::Text(t);
    }
    let mut out = LeanVec::new();
    for chunk in b.utf8_chunks() {
        out.extend_from_slice(chunk.valid().as_bytes());
        if !chunk.invalid().is_empty() {
            out.extend_from_slice("\u{FFFD}".as_bytes());
        }
    }
    Picked::Lossy(out)
}

fn str_range(s: &str, lo: i64, hi: i64, mode: i32) -> Option<Picked<'_>> {
    let len = s.len() as i64;
    if mode == 2 {
        if lo < 0 || lo > len {
            return Some(Picked::Text(""));
        }
        let end = hi.clamp(lo, len);
        // `get`, not an index, whose panic the bounds above rule out but
        // the binary would keep (B-2026-11-02-24).
        let cut = s.as_bytes().get(lo as usize..end as usize);
        return Some(lossy(cut.unwrap_or_default()));
    }
    let (lo, hi) = if mode == 0 {
        // Each bound saturates to `0..=len` (strings.md, B-2026-11-03-16).
        // `max`/`min`, not `clamp`: `clamp`'s assert links std's panic hook.
        let (lo, hi) = (lo.max(0).min(len), hi.max(0).min(len));
        // An end inside a code point panics even when the range is empty
        // there (docs/library/strings.md, B-2026-10-22-38).
        let cut = |x: i64| !s.is_char_boundary(x as usize);
        if cut(lo) || cut(hi) {
            return None;
        }
        if lo >= hi {
            return Some(Picked::Text(""));
        }
        (lo, hi)
    } else {
        if !(0 <= lo && lo <= hi && hi <= len) {
            return None;
        }
        (lo, hi)
    };
    s.get(lo as usize..hi as usize).map(Picked::Text)
}

/// The text [`str_range`] picks as a new `String` at `out`. Returns 1 for
/// a panic, with nothing written.
///
/// # Safety
/// `out` is writable for a `String`; `s` readable for `n` bytes of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_slice(
    out: *mut RawVec,
    s: *const u8,
    n: u64,
    lo: i64,
    hi: i64,
    mode: i32,
) -> u8 {
    let n = n as usize;
    // SAFETY: the caller's contract.
    let s = unsafe { text(s, n) };
    match str_range(s, lo, hi, mode) {
        // SAFETY: as above.
        Some(t) => unsafe { string_of(out, t.as_bytes()) },
        None => {
            str_range_panic(s, lo, hi, mode);
            return 1;
        }
    }
    0
}

/// The text [`str_range`] picks as a `str` view `{ptr, len}` at `out`:
/// a view into `s` itself, so nothing to drop. Text that is new (a
/// `String.slice` cut inside a char, made lossy) is leaked, as a view
/// cannot own it. Returns 1 for a panic, with nothing written.
///
/// # Safety
/// `out` is writable for two words; `s` readable for `n` bytes of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_view(
    out: *mut u64,
    s: *const u8,
    n: u64,
    lo: i64,
    hi: i64,
    mode: i32,
) -> u8 {
    let n = n as usize;
    // SAFETY: the caller's contract.
    let s = unsafe { text(s, n) };
    let (p, len) = match str_range(s, lo, hi, mode) {
        Some(Picked::Text(t)) => (t.as_ptr(), t.len()),
        Some(Picked::Lossy(t)) => {
            let t = std::mem::ManuallyDrop::new(t);
            (t.as_ptr(), t.len())
        }
        None => {
            str_range_panic(s, lo, hi, mode);
            return 1;
        }
    };
    // SAFETY: as above.
    unsafe {
        out.write(p as usize as u64);
        out.add(1).write(len as u64);
    }
    0
}

/// The char starting at byte `i` of `s`, or -1 (a panic) when `i` is out
/// of range or not on a char boundary.
///
/// # Safety
/// `s` is readable for `n` bytes of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_char_at_byte(s: *const u8, n: u64, i: i64) -> i64 {
    let n = n as usize;
    // SAFETY: the caller's contract.
    let s = unsafe { text(s, n) };
    if i < 0 || i as usize >= s.len() {
        return -1;
    }
    match s.get(i as usize..).and_then(|t| t.chars().next()) {
        Some(c) => i64::from(u32::from(c)),
        None => -1,
    }
}

/// The char ending at byte `i` of `s` (the one whose last byte is `i - 1`),
/// or -1 (a panic) when `i` is out of range or not on a char boundary. The
/// back step of `chars()` (`StrChars.next_back`).
///
/// # Safety
/// `s` is readable for `n` bytes of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_char_before_byte(s: *const u8, n: u64, i: i64) -> i64 {
    let n = n as usize;
    // SAFETY: the caller's contract.
    let s = unsafe { text(s, n) };
    if i <= 0 || i as usize > s.len() {
        return -1;
    }
    match s.get(..i as usize).and_then(|t| t.chars().next_back()) {
        Some(c) => i64::from(u32::from(c)),
        None => -1,
    }
}

/// `String.from_utf8`: the bytes as a new `String` at `out` and 0, or 1
/// when the input ends inside a sequence and 2 for an invalid byte, with
/// nothing written.
///
/// # Safety
/// `out` is writable for a `String`; `p` readable for `n` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_from_utf8(out: *mut RawVec, p: *const u8, n: u64) -> i32 {
    let n = n as usize;
    // SAFETY: the caller's contract.
    let b = unsafe { bytes(p, n) };
    match std::str::from_utf8(b) {
        Ok(_) => {
            // SAFETY: as above.
            unsafe { string_of(out, b) };
            0
        }
        Err(e) if e.error_len().is_none() => 1,
        Err(_) => 2,
    }
}

/// `s.replace(a, b)` as a new `String` at `out`.
///
/// # Safety
/// `out` is writable for a `String`; each pointer readable for its length.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_replace(
    out: *mut RawVec,
    s: *const u8,
    sn: u64,
    a: *const u8,
    an: u64,
    b: *const u8,
    bn: u64,
) {
    let sn = sn as usize;
    let an = an as usize;
    let bn = bn as usize;
    // SAFETY: the caller's contract.
    let t = unsafe { text(s, sn).replace(text(a, an), text(b, bn)) };
    // SAFETY: as above.
    unsafe { string_of(out, t.as_bytes()) };
}

/// The byte offset of the first `needle` in `s`, or -1. A non-negative
/// `c` is a char needle and `needle` is unread.
///
/// # Safety
/// Each pointer is readable for its length, of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_find(
    s: *const u8,
    sn: u64,
    needle: *const u8,
    nn: u64,
    c: i64,
) -> i64 {
    let sn = sn as usize;
    let nn = nn as usize;
    // SAFETY: the caller's contract.
    let s = unsafe { text(s, sn) };
    let at = if c >= 0 {
        char::from_u32(c as u32).and_then(|c| s.find(c))
    } else if nn == 1 {
        // A one-byte needle is an ASCII char: `find` by char is a memchr,
        // where a `&str` needle builds a two-way searcher on every call
        // (kata 591's `find(">")`, a third of its instructions).
        // SAFETY: as above, and `nn == 1`.
        s.find(char::from(unsafe { *needle }))
    } else {
        // SAFETY: as above.
        s.find(unsafe { text(needle, nn) })
    };
    at.map_or(-1, |b| b as i64)
}

/// The `i`-th char of `s` (op 0), or -1 past the end or below zero; or
/// the count of its chars (op 1).
///
/// # Safety
/// `s` is readable for `n` bytes of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_chars(op: i32, s: *const u8, n: u64, i: i64) -> i64 {
    let n = n as usize;
    // SAFETY: the caller's contract.
    let s = unsafe { text(s, n) };
    if op == 1 {
        return s.chars().count() as i64;
    }
    usize::try_from(i)
        .ok()
        .and_then(|i| s.chars().nth(i))
        .map_or(-1, |c| i64::from(u32::from(c)))
}

/// `s.strip_prefix(p)` (op 0) or `strip_suffix` (op 1): 1 with the rest
/// as a new `String` at `out`, or 0 with nothing written.
///
/// # Safety
/// `out` is writable for a `String`; each pointer readable for its length.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_strip(
    out: *mut RawVec,
    op: i32,
    s: *const u8,
    sn: u64,
    p: *const u8,
    pn: u64,
) -> u8 {
    let sn = sn as usize;
    let pn = pn as usize;
    // SAFETY: the caller's contract.
    let (s, p) = unsafe { (text(s, sn), text(p, pn)) };
    let rest = if op == 0 {
        s.strip_prefix(p)
    } else {
        s.strip_suffix(p)
    };
    match rest {
        Some(t) => {
            // SAFETY: as above.
            unsafe { string_of(out, t.as_bytes()) };
            1
        }
        None => 0,
    }
}

/// `s.replacen(a, b, n)` (a negative count is 0) as a new `String` at
/// `out`.
///
/// # Safety
/// `out` is writable for a `String`; each pointer readable for its length.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn karac_mir_str_replacen(
    out: *mut RawVec,
    s: *const u8,
    sn: u64,
    a: *const u8,
    an: u64,
    b: *const u8,
    bn: u64,
    n: i64,
) {
    let sn = sn as usize;
    let an = an as usize;
    let bn = bn as usize;
    // SAFETY: the caller's contract.
    let t = unsafe { text(s, sn).replacen(text(a, an), text(b, bn), n.max(0) as usize) };
    // SAFETY: as above.
    unsafe { string_of(out, t.as_bytes()) };
}

/// Runs a program's `main` body on a 1 GiB stack (reserved, committed as
/// it is touched) and returns its exit code, so recursion reaches the depth
/// the interpreter's does.
///
/// Where it can, it switches the main thread onto that stack rather than
/// starting a thread: once a process has a second thread, glibc's `malloc`
/// and `free` lock their arena on every call, which made an allocating
/// program (a linked-list merge) 1.7x slower than on the process's own
/// thread. The stack-overflow guard covers the new stack.
///
/// It first ignores `SIGPIPE`, as the interpreter's Rust start up does: a
/// write to a peer that has gone (a client that drops its connection
/// without `close_notify`, a closed pipe) then fails that write with an
/// `IoError` rather than killing the program (docs/library/io.md,
/// B-2026-11-07-25).
#[no_mangle]
pub extern "C" fn karac_mir_run_main(body: extern "C" fn() -> i32) -> i32 {
    crate::fatal::ignore_sigpipe();
    // The small-chunk lists from the start (B-2026-11-11-42).
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    crate::alloc::small::start();
    #[cfg(all(
        unix,
        feature = "net",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    if !big_stack::under_sanitizer() {
        if let Some(code) = big_stack::run(body) {
            return code;
        }
    }
    #[cfg(all(unix, feature = "net"))]
    if let Some(code) = main_thread::run(body) {
        return code;
    }
    #[cfg(not(all(unix, feature = "net")))]
    if let Ok(h) = std::thread::Builder::new()
        .name("main".into())
        .stack_size(1 << 30)
        .spawn(move || body())
    {
        return h.join().unwrap_or(101);
    }
    // No thread to be had: run it here, on the process's own stack.
    body()
}

/// The thread `main` runs on when it cannot run on the process's own
/// thread. Started through pthreads rather than `std::thread`, whose spawn
/// and join reach panics that keep std's panic hook and backtrace
/// symbolizer in every binary (B-2026-10-28-36).
#[cfg(all(unix, feature = "net"))]
mod main_thread {
    type Slot = (extern "C" fn() -> i32, i32);

    extern "C" fn start(arg: *mut libc::c_void) -> *mut libc::c_void {
        // SAFETY: `run` passes its slot, which outlives the thread.
        let slot = unsafe { &mut *arg.cast::<Slot>() };
        // SAFETY: names the calling thread with a NUL-terminated name.
        #[cfg(target_os = "linux")]
        unsafe {
            libc::pthread_setname_np(libc::pthread_self(), c"main".as_ptr())
        };
        #[cfg(target_os = "macos")]
        unsafe {
            libc::pthread_setname_np(c"main".as_ptr())
        };
        slot.1 = (slot.0)();
        std::ptr::null_mut()
    }

    /// Runs `body` on a thread with a 1 GiB stack and answers its exit
    /// code; `None` when no thread could be started.
    pub(super) fn run(body: extern "C" fn() -> i32) -> Option<i32> {
        let mut slot: Slot = (body, 101);
        // SAFETY: the attributes are initialized before use and destroyed
        // after; the thread is joined before `slot` goes out of scope.
        unsafe {
            let mut attr: libc::pthread_attr_t = std::mem::zeroed();
            if libc::pthread_attr_init(&mut attr) != 0 {
                return None;
            }
            libc::pthread_attr_setstacksize(&mut attr, 1 << 30);
            let mut thread: libc::pthread_t = std::mem::zeroed();
            let started =
                libc::pthread_create(&mut thread, &attr, start, (&raw mut slot).cast()) == 0;
            libc::pthread_attr_destroy(&mut attr);
            if !started {
                return None;
            }
            libc::pthread_join(thread, std::ptr::null_mut());
        }
        Some(slot.1)
    }
}

#[cfg(all(
    unix,
    feature = "net",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod big_stack {
    const SIZE: usize = 1 << 30;
    /// The inaccessible low end, which an overflow faults on.
    const GUARD: usize = 1 << 16;

    /// Whether AddressSanitizer is linked in, which `__mir-llvm-asan` does.
    /// LeakSanitizer reports nothing allocated while the stack pointer is on
    /// a stack it was not told about, so a lane run on this stack could not
    /// see a leak (B-2026-10-18-31); there the thread path runs `main`
    /// instead, whose stack the sanitizer knows.
    pub(super) fn under_sanitizer() -> bool {
        // SAFETY: a lookup of a NUL-terminated name in the loaded objects.
        !unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"__asan_init".as_ptr()) }.is_null()
    }

    /// Runs `body` on a fresh stack; `None` when none could be mapped.
    pub(super) fn run(body: extern "C" fn() -> i32) -> Option<i32> {
        // SAFETY: an anonymous private mapping, reserved, not committed.
        let lo = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if lo == libc::MAP_FAILED {
            return None;
        }
        // SAFETY: the low pages of the mapping just made.
        unsafe { libc::mprotect(lo, GUARD, libc::PROT_NONE) };
        let lo = lo as usize;
        let top = lo + SIZE;
        crate::stack_guard::imp::install_for(Some((lo + GUARD, top)));
        // SAFETY: `top` is the 16-byte-aligned end of a mapping that
        // nothing else uses; `body` runs on it with the C ABI and returns,
        // and the callee-saved register holding the old stack pointer
        // brings it back. The mapping is left in place: the process exits
        // after `main` returns.
        let code: i32;
        #[cfg(target_arch = "x86_64")]
        unsafe {
            std::arch::asm!(
                "mov r12, rsp",
                "mov rsp, {top}",
                "call {f}",
                "mov rsp, r12",
                top = in(reg) top,
                f = in(reg) body,
                out("r12") _,
                lateout("eax") code,
                clobber_abi("C"),
            );
        }
        #[cfg(target_arch = "aarch64")]
        unsafe {
            std::arch::asm!(
                "mov x20, sp",
                "mov sp, {top}",
                "blr {f}",
                "mov sp, x20",
                top = in(reg) top,
                f = in(reg) body,
                out("x20") _,
                lateout("w0") code,
                clobber_abi("C"),
            );
        }
        Some(code)
    }
}

/// `binary_search`: the index std's search over the `n` elements of
/// `size` bytes at `buf` finds for the value at `key` under `cmp`, or -1.
///
/// # Safety
/// `buf` holds `n` elements; `cmp` is sound on an element and the key.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_bsearch(
    buf: *const u8,
    n: u64,
    size: u64,
    cmp: unsafe extern "C" fn(*const u8, *const u8) -> i32,
    key: *const u8,
) -> i64 {
    let n = n as usize;
    let size = size as usize;
    let at = |i: usize| {
        // SAFETY: the caller's contract.
        let o = unsafe { cmp(buf.add(i * size), key) };
        o.cmp(&0)
    };
    if n == 0 {
        // An empty Vec's buffer may be null.
        return -1;
    }
    if size == 0 {
        // Every element is the same zero-sized value.
        let idx = vec![(); n];
        return idx.binary_search_by(|_| at(0)).map_or(-1, |i| i as i64);
    }
    // std's search, driven over the buffer's first `n` bytes so each probe
    // names its index by its address, with nothing allocated per call
    // (B-2026-10-18-9): `n * size >= n` bytes are readable.
    // SAFETY: as above; the bytes are only addressed, never read.
    let bytes = unsafe { std::slice::from_raw_parts(buf, n) };
    bytes
        .binary_search_by(|b| at((b as *const u8 as usize) - (buf as usize)))
        .map_or(-1, |i| i as i64)
}

/// A float to an integer of `bits` bits, as `numeric_conv` converts it:
/// family 0 saturating, 1 wrapping, 2 checked, 3 trunc. Writes the value
/// (as its i64 bits) at `out` and returns 0, or 1 when the value is out of
/// range for a checked or trunc conversion, with nothing written.
///
/// # Safety
/// `out` is writable for an `i64`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_f2i(
    f: f64,
    family: i32,
    bits: i32,
    signed: u8,
    out: *mut i64,
) -> u8 {
    let signed = signed != 0;
    let saturating = |f: f64| -> i64 {
        match (bits, signed) {
            (8, true) => i64::from(f as i8),
            (16, true) => i64::from(f as i16),
            (32, true) => i64::from(f as i32),
            (8, false) => i64::from(f as u8),
            (16, false) => i64::from(f as u16),
            (32, false) => i64::from(f as u32),
            (64, false) => f as u64 as i64,
            _ => f as i64,
        }
    };
    let v = match family {
        0 => saturating(f),
        1 => {
            let t = f.trunc();
            if !t.is_finite() {
                0
            } else {
                let raw = t as i128;
                let modulus = 1i128 << bits;
                let mut m = raw & (modulus - 1);
                if signed && (m & (1i128 << (bits - 1))) != 0 {
                    m -= modulus;
                }
                m as i64
            }
        }
        _ => {
            if !f.is_finite() {
                return 1;
            }
            let t = f.trunc();
            let (lo, hi) = if signed {
                let half = 2f64.powi(bits - 1);
                (-half, half - 1.0)
            } else {
                (0.0, 2f64.powi(bits) - 1.0)
            };
            if !(t >= lo && t <= hi) {
                return 1;
            }
            saturating(f)
        }
    };
    // SAFETY: the caller's contract.
    unsafe { out.write(v) };
    0
}

thread_local! {
    /// The ids of the spans `with_span` made active, innermost last.
    static SPANS: RefCell<Vec<i64>> = const { RefCell::new(Vec::new()) };
}

/// A copy of the thread's active span stack, for a task being spawned.
pub(crate) fn current_spans() -> Vec<i64> {
    SPANS.with(|s| s.borrow().clone())
}

/// Installs `spans` as the thread's active span stack, returning the old one.
pub(crate) fn swap_spans(spans: Vec<i64>) -> Vec<i64> {
    SPANS.with(|s| std::mem::replace(&mut *s.borrow_mut(), spans))
}

/// Makes the span `id` active.
#[no_mangle]
pub extern "C" fn karac_mir_span_push(id: i64) {
    SPANS.with(|s| s.borrow_mut().push(id));
}

/// Ends the innermost active span.
#[no_mangle]
pub extern "C" fn karac_mir_span_pop() {
    SPANS.with(|s| s.borrow_mut().pop());
}

/// The innermost active span's id, or 0.
#[no_mangle]
pub extern "C" fn karac_mir_span_top() -> i64 {
    SPANS.with(|s| s.borrow().last().copied().unwrap_or(0))
}

thread_local! {
    static RAND: std::cell::Cell<u64> = std::cell::Cell::new(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64)
            | 1,
    );
}

/// `RandomSource.next_u64`: xorshift64 from a clock seed, as the
/// interpreter's.
#[no_mangle]
pub extern "C" fn karac_mir_rand_next() -> u64 {
    RAND.with(|r| {
        let mut x = r.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        r.set(x);
        x
    })
}

/// `Env.set(name, value)`.
///
/// # Safety
/// `p` and `v` are readable for `n` and `vn` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_env_set(p: *const u8, n: u64, v: *const u8, vn: u64) {
    let n = n as usize;
    let vn = vn as usize;
    // SAFETY: the caller's contract.
    let (name, val) = unsafe { (bytes(p, n), bytes(v, vn)) };
    if let (Ok(name), Ok(val)) = (std::str::from_utf8(name), std::str::from_utf8(val)) {
        // SAFETY: the program runs its environment edits on one thread,
        // as the interpreter does.
        unsafe { std::env::set_var(name, val) };
    }
}

/// `f64.total_cmp` (`f32` when `narrow`): -1, 0 or 1. Every NaN orders
/// as the positive quiet one, as the interpreter canonicalizes it.
#[no_mangle]
pub extern "C" fn karac_mir_total_cmp(a: f64, b: f64, narrow: u8) -> i32 {
    let canon = |f: f64| if f.is_nan() { f64::NAN } else { f };
    let o = if narrow != 0 {
        (canon(a) as f32).total_cmp(&(canon(b) as f32))
    } else {
        canon(a).total_cmp(&canon(b))
    };
    o as i32
}

/// The length of the NUL-terminated bytes at `p` (0 for null): a `CStr`
/// or `CString` is a pointer to them.
///
/// # Safety
/// `p` is null or points at NUL-terminated bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_cstr_len(p: *const u8) -> i64 {
    if p.is_null() {
        return 0;
    }
    // SAFETY: the caller's contract.
    unsafe { std::ffi::CStr::from_ptr(p.cast()) }
        .to_bytes()
        .len() as i64
}

/// `String.to_cstring`: writes a malloc'd NUL-terminated copy of the `n`
/// bytes at `p` to `*out` and returns 1, or returns 0 (writing nothing)
/// when they hold a NUL, which C would read as the end.
///
/// # Safety
/// `p` is readable for `n` bytes; `out` is writable for a pointer.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_to_cstring(out: *mut *mut u8, p: *const u8, n: u64) -> u8 {
    let n = n as usize;
    // SAFETY: the caller's contract.
    let b = unsafe { bytes(p, n) };
    if b.contains(&0) {
        return 0;
    }
    let buf = crate::alloc::karac_realloc_or_panic(std::ptr::null_mut(), n + 1);
    // SAFETY: `buf` holds `n + 1` bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(b.as_ptr(), buf, n);
        *buf.add(n) = 0;
        *out = buf;
    }
    1
}

/// Frees a `CString`'s buffer.
///
/// # Safety
/// `p` came from [`karac_mir_to_cstring`] and is not used after.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_cstring_free(p: *mut u8) {
    // SAFETY: the caller's contract; the buffer came from the C allocator.
    unsafe { free(p.cast()) };
}

/// `CStr.to_string` (`view` 0) or `to_string_slice` (1): validates the
/// NUL-terminated bytes at `p` as UTF-8; on success writes a `String`
/// copy (or, for a view, the bytes' `{ptr, len}`) at `out` and returns
/// 0, else returns 1 for a cut-off final sequence or 2 for any other bad
/// byte.
///
/// # Safety
/// `p` points at NUL-terminated bytes; `out` is writable for a `String`
/// (three words) or a view (two).
#[no_mangle]
pub unsafe extern "C" fn karac_mir_cstr_to_string(out: *mut RawVec, p: *const u8, view: u8) -> i32 {
    // SAFETY: the caller's contract.
    let b = unsafe { std::ffi::CStr::from_ptr(p.cast()) }.to_bytes();
    match std::str::from_utf8(b) {
        Ok(_) if view != 0 => {
            let w = out.cast::<usize>();
            // SAFETY: as above.
            unsafe {
                *w = p as usize;
                *w.add(1) = b.len();
            }
            0
        }
        Ok(_) => {
            // SAFETY: as above.
            unsafe { string_of(out, b) };
            0
        }
        Err(e) if e.error_len().is_none() => 1,
        Err(_) => 2,
    }
}

crate::mir_task::program_state! {
    /// Each `Semaphore`'s free permits, by id - 1.
    static SEMAPHORES: Vec<i64> = Vec::new();
}
crate::mir_task::program_state! {
    /// Each `RateLimiter`'s rate, capacity and per-key `(tokens, last)`.
    static LIMITERS: Vec<Limiter> = Vec::new();
}

struct Limiter {
    rate: f64,
    capacity: f64,
    buckets: std::collections::HashMap<Vec<u8>, (f64, std::time::Instant)>,
}

thread_local! {
    /// A permit the running task's `__ready` took for the `acquire` that
    /// follows it (B-2026-11-11-34).
    static SEM_TAKEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `Semaphore.new(n)`: its id.
#[no_mangle]
pub extern "C" fn karac_mir_sem_new(n: i64) -> i64 {
    SEMAPHORES.with(|s| {
        s.push(n.max(0));
        s.len() as i64
    })
}

/// A free permit of the semaphore `id` taken: `Some(true)` when one was,
/// `Some(false)` when none is free, `None` when `id` names no semaphore.
fn sem_take(id: i64) -> Option<bool> {
    SEMAPHORES.with(|s| {
        let free = usize::try_from(id - 1).ok().and_then(|i| s.get_mut(i))?;
        Some(
            *free > 0 && {
                *free -= 1;
                true
            },
        )
    })
}

/// `__ready` of `Semaphore.acquire(timeout_ms)` (B-2026-11-11-34): ready
/// when a permit is free, which it takes for the `acquire` after it (on a
/// multi-worker executor another task could take it in between), or once
/// the timeout has passed; until then the task waits for another task's
/// `release`. A handle that names no semaphore is ready, and fails.
#[no_mangle]
pub extern "C" fn karac_mir_ready_sem(id: i64, ms: i64) -> u8 {
    if crate::mir_task::deadline_set().is_none() {
        return 1;
    }
    let free = match sem_take(id) {
        Some(true) => {
            SEM_TAKEN.with(|t| t.set(true));
            true
        }
        Some(false) => false,
        None => true,
    };
    crate::mir_task::ready_by(free, ms.max(0).saturating_mul(1_000_000)).0
}

/// `Semaphore.acquire(timeout_ms)`: 1 when a permit was taken, 0 (a
/// timeout) when none is free. In a task its `__ready` waited for one;
/// outside the executor no task can release one, so an exhausted
/// semaphore waits the whole timeout. A handle that names no semaphore
/// fails at once.
#[no_mangle]
pub extern "C" fn karac_mir_sem_acquire(id: i64, ms: i64) -> u8 {
    if SEM_TAKEN.with(|t| t.replace(false)) {
        return 1;
    }
    match sem_take(id) {
        Some(true) => 1,
        Some(false) => {
            if crate::mir_task::deadline_set().is_none() {
                #[cfg(not(target_arch = "wasm32"))]
                std::thread::sleep(std::time::Duration::from_millis(ms.max(0) as u64));
                #[cfg(target_arch = "wasm32")]
                let _ = ms;
            }
            0
        }
        None => 0,
    }
}

/// `Semaphore.release`: a permit back, which a waiting `acquire` may take.
/// Uncapped, as a counting semaphore's signal (`Semaphore.new(0)` and a
/// release) is (B-2026-11-11-34).
#[no_mangle]
pub extern "C" fn karac_mir_sem_release(id: i64) {
    SEMAPHORES.with(|s| {
        if let Some(free) = usize::try_from(id - 1).ok().and_then(|i| s.get_mut(i)) {
            *free = free.saturating_add(1);
        }
    });
    crate::mir_task::peer_progressed();
}

/// `RateLimiter.new_token_bucket(rate, capacity)`: its id.
#[no_mangle]
pub extern "C" fn karac_mir_limiter_new(rate: i64, cap: i64) -> i64 {
    LIMITERS.with(|l| {
        l.push(Limiter {
            rate: rate.max(0) as f64,
            capacity: cap.max(0) as f64,
            buckets: Default::default(),
        });
        l.len() as i64
    })
}

/// `RateLimiter.try_acquire(key)`: a fresh key's bucket starts full and
/// refills against the monotonic clock; 1 when a token was taken. An id
/// that names no limiter fails closed.
///
/// # Safety
/// `p` is readable for `n` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_limiter_try(id: i64, p: *const u8, n: u64) -> u8 {
    let n = n as usize;
    // SAFETY: the caller's contract.
    let key = unsafe { bytes(p, n) }.to_vec();
    LIMITERS.with(|l| {
        let Some(l) = usize::try_from(id - 1).ok().and_then(|i| l.get_mut(i)) else {
            return 0;
        };
        let now = std::time::Instant::now();
        let (rate, cap) = (l.rate, l.capacity);
        let (tokens, last) = l.buckets.entry(key).or_insert((cap, now));
        let elapsed = now.saturating_duration_since(*last).as_secs_f64();
        *tokens = (*tokens + elapsed * rate).min(cap);
        *last = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            1
        } else {
            0
        }
    })
}

/// `std.time::sleep_ms`: a real pause; a count of zero or less is a
/// no-op.
#[no_mangle]
pub extern "C" fn karac_mir_sleep_ms(ms: i64) {
    // Under the task executor, the readiness loop before it already waited
    // the pause out.
    if crate::mir_task::take_slept() {
        return;
    }
    if ms > 0 {
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::sleep(std::time::Duration::from_millis(ms as u64));
    }
}

/// `s.repeat(n)` (a negative count is 0) as a new `String` at `out`.
///
/// # Safety
/// `out` is writable for a `String`; `s` readable for `sn` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_repeat(out: *mut RawVec, s: *const u8, sn: u64, n: i64) {
    let sn = sn as usize;
    // Copied out by doubling rather than through `str::repeat`, whose
    // `expect` on the length keeps std's panic hook in every binary that
    // repeats a string (B-2026-11-04-31).
    let total = match usize::try_from(n.max(0)) {
        _ if sn == 0 => Some(0),
        Ok(n) => sn.checked_mul(n),
        Err(_) => None,
    };
    let Some(total) = total else {
        crate::fatal::write_stderr(b"panic: capacity overflow\n");
        std::process::abort()
    };
    let ptr = if total == 0 {
        std::ptr::null_mut()
    } else {
        let p = karac_realloc_or_panic(std::ptr::null_mut(), total);
        // SAFETY: `s` is readable for `sn` bytes and `p` has room for
        // `total`, a whole number of copies; each copy reads the `filled`
        // bytes already written before them.
        unsafe {
            std::ptr::copy_nonoverlapping(s, p, sn);
            let mut filled = sn;
            while filled < total {
                let c = filled.min(total - filled);
                std::ptr::copy_nonoverlapping(p, p.add(filled), c);
                filled += c;
            }
        }
        p
    };
    // SAFETY: the caller's contract.
    unsafe { out.write(RawVec::new(ptr, total, total)) };
}

/// A new `Vec[String]` at `out` of the parts of `s`: 0 `split(sep)`, 1
/// `split_whitespace`, 2 `lines`.
///
/// # Safety
/// `out` is writable for a `Vec`; each pointer readable for its length.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_split(
    out: *mut RawVec,
    op: i32,
    s: *const u8,
    sn: u64,
    sep: *const u8,
    sepn: u64,
) {
    let sn = sn as usize;
    let sepn = sepn as usize;
    // SAFETY: the caller's contract.
    let (s, sep) = unsafe { (text(s, sn), text(sep, sepn)) };
    let parts: Vec<&str> = match op {
        0 => s.split(sep).collect(),
        1 => s.split_whitespace().collect(),
        _ => s.lines().collect(),
    };
    let size = std::mem::size_of::<RawVec>();
    // SAFETY: as above; the new buffer holds `parts.len()` Strings.
    unsafe {
        karac_mir_vec_alloc(out, parts.len() as u64, size as u64);
        let buf = (*out).ptr as *mut RawVec;
        for (i, p) in parts.iter().enumerate() {
            string_of(buf.add(i), p.as_bytes());
        }
    }
}

/// `v.join(sep)` over `n` texts laid out `stride` bytes apart from `elems`
/// (each starting with its `{ptr, len}`), as a new `String` at `out`.
///
/// # Safety
/// `out` is writable for a `String`; `elems` holds `n` texts; `sep` is
/// readable for `sepn` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_join(
    out: *mut RawVec,
    elems: *const u8,
    n: u64,
    stride: u64,
    sep: *const u8,
    sepn: u64,
) {
    let n = n as usize;
    let stride = stride as usize;
    let sepn = sepn as usize;
    // Sized first and copied into one allocation of the runtime's own: a
    // `String` grown by `push_str` reaches std's capacity-overflow panic,
    // and with it the panic and backtrace machinery, some 270 KB of every
    // binary that joins (kata 331, B-2026-11-04-31). The sum saturates: a
    // length no allocation can hold fails as out of memory.
    // SAFETY: the caller's contract, `n` live strings `stride` bytes apart
    // at `elems`.
    let elem = |i: usize| unsafe { &*(elems.add(i * stride) as *const RawVec) };
    let mut total = sepn.saturating_mul(n.saturating_sub(1));
    for i in 0..n {
        total = total.saturating_add(elem(i).len);
    }
    if total == 0 {
        // SAFETY: the caller's contract; `out` is writable.
        unsafe { out.write(RawVec::new(std::ptr::null_mut(), 0, 0)) };
        return;
    }
    let p = karac_realloc_or_panic(std::ptr::null_mut(), total);
    let put = |from: *const u8, len: usize, at: &mut usize| {
        // An empty string's pointer may be null: nothing is read from it.
        if len > 0 {
            // SAFETY: `from` holds `len` bytes, and `p` has room for
            // `total`, the sum of every `len` put.
            unsafe { std::ptr::copy_nonoverlapping(from, p.add(*at), len) };
            *at += len;
        }
    };
    let mut at = 0;
    for i in 0..n {
        if i > 0 {
            put(sep, sepn, &mut at);
        }
        let e = elem(i);
        put(e.ptr, e.len, &mut at);
    }
    // SAFETY: the caller's contract; `out` is writable.
    unsafe { out.write(RawVec::new(p, total, total)) };
}

extern "C" {
    fn free(ptr: *mut core::ffi::c_void);
}

/// The `fs` functions, by `op`: `write(path, text)` (0),
/// `read_to_string(path)` (1), `read_lines(path)` (2), `exists(path)` (3),
/// `metadata(path)` (4), `read_dir(path)` (5), `create_dir` (6),
/// `create_dir_all` (7), `remove_file` (8), `remove_dir` (9),
/// `remove_dir_all` (10), `rename(path, text)` (11), `copy(path, text)`
/// (12), `read_bytes(path)` (13), `write_bytes(path, text)` (14),
/// `symlink(path, text)` (15, the target and the link), `read_link(path)`
/// (16), `symlink_metadata(path)` (17), `set_permissions(path, value[0])`
/// (18) and `set_modified(path, value[0])` (19), on the real file system
/// (docs/library/io.md § Files). Returns -1
/// on success, with the text read as a `String` (or the lines or entry
/// names as a `Vec[String]`) at `out`, `copy`'s byte count at `value[0]`,
/// or `metadata`'s size, is-file, is-dir, modification time (ms since
/// the Unix epoch), is-symlink and mode at `value[0..6]`; `exists` returns
/// 1 or 0. Otherwise the
/// code of the failure's `IoError` variant (`NotFound`, `PermissionDenied`,
/// `AlreadyExists`, `UnexpectedEof`, `InvalidUtf8`, `Interrupted`, `Other`,
/// `InvalidInput`, `Unsupported`: codegen's `IO_ERRORS`), with `Other`'s
/// message as a `String` at `out`. The interpreter's twin is src/mir/fs.rs.
///
/// # Safety
/// Each pointer is readable for its length; `out` is writable for a
/// `String` or `Vec`, and `value` for six `i64`s.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_fs(
    op: i32,
    path: *const u8,
    path_len: u64,
    text: *const u8,
    text_len: u64,
    out: *mut RawVec,
    value: *mut i64,
) -> i32 {
    let path_len = path_len as usize;
    let text_len = text_len as usize;
    use std::io::ErrorKind as K;
    // SAFETY: the caller's contract.
    let path = String::from_utf8_lossy(unsafe { bytes(path, path_len) }).into_owned();
    // SAFETY: as above.
    let text = unsafe { bytes(text, text_len) };
    let to = || String::from_utf8_lossy(text).into_owned();
    let lines_out = |lines: &[&str]| {
        // SAFETY: `out` is writable for a `Vec` of `String`s.
        unsafe {
            karac_mir_vec_alloc(
                out,
                lines.len() as u64,
                std::mem::size_of::<RawVec>() as u64,
            );
            let buf = (*out).ptr.cast::<RawVec>();
            for (i, l) in lines.iter().enumerate() {
                string_of(buf.add(i), l.as_bytes());
            }
        }
    };
    let res: std::io::Result<()> = match op {
        0 => std::fs::write(&path, text),
        1 | 2 => std::fs::read_to_string(&path).map(|t| {
            if op == 2 {
                lines_out(&t.lines().collect::<Vec<_>>());
            } else {
                // SAFETY: `out` is writable for a `String`.
                unsafe { string_of(out, t.as_bytes()) };
            }
        }),
        3 => return std::fs::metadata(&path).is_ok() as i32,
        4 | 17 => fs_metadata(&path, op == 4).map(|m| {
            for (i, w) in m.iter().enumerate() {
                // SAFETY: `value` is writable for six `i64`s.
                unsafe { *value.add(i) = *w };
            }
        }),
        5 => fs_read_dir(&path).map(|names| {
            lines_out(&names.iter().map(String::as_str).collect::<Vec<_>>());
        }),
        6 => std::fs::create_dir(&path),
        7 => std::fs::create_dir_all(&path),
        8 => std::fs::remove_file(&path),
        9 => std::fs::remove_dir(&path),
        10 => std::fs::remove_dir_all(&path),
        11 => std::fs::rename(&path, to()),
        // `read_bytes` / `write_bytes` (B-2026-10-15-28): the bytes as a
        // `Vec[u8]`, which has a `String`'s layout.
        // SAFETY: `out` is writable for a `Vec[u8]`.
        13 => std::fs::read(&path).map(|b| unsafe { string_of(out, &b) }),
        14 => std::fs::write(&path, text),
        // Links, modes and times (B-2026-10-15-59).
        15 => fs_symlink(&path, &to()),
        // SAFETY: `out` is writable for a `String`.
        16 => fs_read_link(&path).map(|t| unsafe { string_of(out, t.as_bytes()) }),
        // SAFETY: `value[0]` holds the mode or the time.
        18 => fs_set_permissions(&path, unsafe { *value }),
        // SAFETY: as above.
        19 => fs_set_modified(&path, unsafe { *value }),
        // SAFETY: `value` is writable for an `i64`.
        _ => std::fs::copy(&path, to()).map(|n| unsafe { *value = n as i64 }),
    };
    let e = match res {
        Ok(()) => return -1,
        Err(e) => e,
    };
    match e.kind() {
        K::NotFound => 0,
        K::PermissionDenied => 1,
        K::AlreadyExists => 2,
        K::UnexpectedEof => 3,
        K::InvalidData => 4,
        K::Interrupted => 5,
        K::InvalidInput => 7,
        K::Unsupported => 8,
        _ => {
            // SAFETY: `out` is writable for a `String`.
            unsafe { string_of(out, e.to_string().as_bytes()) };
            6
        }
    }
}

/// `fs.metadata` (`follow`) or `fs.symlink_metadata`: size, is-file,
/// is-dir, modification time in ms since the Unix epoch, is-symlink and
/// the mode's permission bits. The interpreter's twin is src/mir/fs.rs.
fn fs_metadata(path: &str, follow: bool) -> std::io::Result<[i64; 6]> {
    let m = if follow {
        std::fs::metadata(path)?
    } else {
        std::fs::symlink_metadata(path)?
    };
    let ms = match m.modified()?.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    };
    Ok([
        m.len() as i64,
        m.is_file() as i64,
        m.is_dir() as i64,
        ms,
        m.file_type().is_symlink() as i64,
        fs_mode(&m),
    ])
}

/// The permission bits (`0o7777`): the Unix mode, or one made from the
/// read-only flag elsewhere (`0o666` / `0o444`, `0o777` / `0o555` for a
/// directory).
fn fs_mode(m: &std::fs::Metadata) -> i64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        i64::from(m.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let all = if m.is_dir() { 0o777 } else { 0o666 };
        if m.permissions().readonly() {
            all & !0o222
        } else {
            all
        }
    }
}

/// `fs.set_permissions`: a mode outside `0..=0o7777` is `InvalidInput`;
/// without Unix modes only the owner's write bit counts.
fn fs_set_permissions(path: &str, mode: i64) -> std::io::Result<()> {
    if !(0..=0o7777).contains(&mode) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a mode is 0 to 0o7777",
        ));
    }
    #[cfg(unix)]
    let p = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::Permissions::from_mode(mode as u32)
    };
    #[cfg(not(unix))]
    let p = {
        let mut p = std::fs::metadata(path)?.permissions();
        p.set_readonly(mode & 0o200 == 0);
        p
    };
    std::fs::set_permissions(path, p)
}

/// `fs.set_modified`: the modification time to `ms` since the Unix epoch,
/// leaving the access time; opens the file to set it.
fn fs_set_modified(path: &str, ms: i64) -> std::io::Result<()> {
    let d = std::time::Duration::from_millis(ms.unsigned_abs());
    let t = if ms >= 0 {
        std::time::UNIX_EPOCH.checked_add(d)
    } else {
        std::time::UNIX_EPOCH.checked_sub(d)
    }
    .ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "the time is out of range")
    })?;
    #[cfg(windows)]
    let f = {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_WRITE_ATTRIBUTES, and FILE_FLAG_BACKUP_SEMANTICS so a
        // directory opens too.
        std::fs::OpenOptions::new()
            .access_mode(0x100)
            .custom_flags(0x0200_0000)
            .open(path)?
    };
    #[cfg(not(windows))]
    let f = std::fs::File::open(path)?;
    f.set_modified(t)
}

/// `fs.symlink(target, link)`; a directory link on Windows when the target
/// is a directory, `Unsupported` where there are no links.
fn fs_symlink(target: &str, link: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        let at = std::path::Path::new(link)
            .parent()
            .unwrap_or(std::path::Path::new(""))
            .join(target);
        if at.is_dir() {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "symbolic links are not supported here",
        ))
    }
}

/// `fs.read_link`: the stored target; one that is not UTF-8 is an
/// `InvalidData` error.
fn fs_read_link(path: &str) -> std::io::Result<String> {
    std::fs::read_link(path)?
        .into_os_string()
        .into_string()
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "a link target is not UTF-8",
            )
        })
}

/// `fs.read_dir`: the entry names sorted byte-wise; a name that is not
/// UTF-8 is an `InvalidData` error.
fn fs_read_dir(path: &str) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let name = entry?.file_name().into_string().map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "a file name is not UTF-8")
        })?;
        names.push(name);
    }
    names.sort();
    Ok(names)
}

/// A `VecDeque`: a ring of `cap` slots from which `len` elements, starting
/// at slot `head`, are live (zero-sized elements keep `cap` and `head` 0).
#[repr(C)]
pub struct RawDeque {
    pub ptr: *mut u8,
    #[cfg(target_pointer_width = "32")]
    _ptr_hi: u32,
    pub head: usize,
    #[cfg(target_pointer_width = "32")]
    _head_hi: u32,
    pub len: usize,
    #[cfg(target_pointer_width = "32")]
    _len_hi: u32,
    pub cap: usize,
    #[cfg(target_pointer_width = "32")]
    _cap_hi: u32,
}

impl RawDeque {
    /// As [`RawVec::new`], with the ring's `head` (B-2026-10-22-23).
    pub const fn new(ptr: *mut u8, head: usize, len: usize, cap: usize) -> Self {
        RawDeque {
            ptr,
            #[cfg(target_pointer_width = "32")]
            _ptr_hi: 0,
            head,
            #[cfg(target_pointer_width = "32")]
            _head_hi: 0,
            len,
            #[cfg(target_pointer_width = "32")]
            _len_hi: 0,
            cap,
            #[cfg(target_pointer_width = "32")]
            _cap_hi: 0,
        }
    }
}

impl RawDeque {
    fn slot(&self, i: usize, size: usize) -> *mut u8 {
        if size == 0 {
            return self.ptr;
        }
        // SAFETY: a slot index below `cap`, inside the buffer.
        unsafe { self.ptr.add((self.head + i) % self.cap * size) }
    }

    /// Makes room for one more element, unwrapping into a new buffer.
    fn grow(&mut self, size: usize) {
        if size == 0 || self.len < self.cap {
            return;
        }
        let cap = (self.cap * 2).max(4);
        let buf = karac_realloc_or_panic(std::ptr::null_mut(), cap * size);
        // The ring is full, so its elements are the run from `head` to the
        // end of the buffer and then the run from its start up to `head`:
        // two copies, not one per element.
        if self.cap != 0 {
            let first = self.cap - self.head;
            // SAFETY: slots `head..cap` and `0..head` are live and inside
            // the old buffer; the new buffer has room for `len` elements.
            unsafe {
                std::ptr::copy_nonoverlapping(self.ptr.add(self.head * size), buf, first * size);
                std::ptr::copy_nonoverlapping(self.ptr, buf.add(first * size), self.head * size);
            }
            // SAFETY: the old buffer came from `karac_realloc_or_panic`.
            unsafe { free(self.ptr.cast()) };
        }
        self.ptr = buf;
        self.head = 0;
        self.cap = cap;
    }
}

/// Writes a deque of `n` uninitialized `size`-byte elements at `out`
/// (`head == 0`, `len == cap == n`), for the caller to fill.
///
/// # Safety
/// `out` is writable for a deque.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_deque_alloc(out: *mut RawDeque, n: u64, size: u64) {
    let n = n as usize;
    let size = size as usize;
    let mut v = RawVec::new(std::ptr::null_mut(), 0, 0);
    // SAFETY: `v` is a writable vector.
    unsafe { karac_mir_vec_alloc(&mut v, n as u64, size as u64) };
    // SAFETY: the caller's contract.
    unsafe { out.write(RawDeque::new(v.ptr, 0, n, v.cap)) };
}

/// The element `i` of the deque at `q` (in bounds).
///
/// # Safety
/// `q` is a live deque of `size`-byte elements with more than `i` of them.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_deque_slot(q: *const RawDeque, i: u64, size: u64) -> *mut u8 {
    let i = i as usize;
    let size = size as usize;
    // SAFETY: the caller's contract.
    unsafe { (*q).slot(i, size) }
}

/// Adds the `size` bytes at `elem` at the back, or the front when `front`.
///
/// # Safety
/// `q` is a live deque of `size`-byte elements; `elem` is readable.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_deque_push(
    q: *mut RawDeque,
    elem: *const u8,
    size: u64,
    front: u8,
) {
    let size = size as usize;
    // SAFETY: the caller's contract.
    let q = unsafe { &mut *q };
    q.grow(size);
    if front != 0 && size != 0 {
        q.head = (q.head + q.cap - 1) % q.cap;
    }
    let at = if front != 0 { 0 } else { q.len };
    q.len += 1;
    // SAFETY: `grow` made room; slot `at` is free.
    unsafe { std::ptr::copy_nonoverlapping(elem, q.slot(at, size), size) };
}

/// Inserts the `size` bytes at `elem` as element `i`, moving the ones
/// after it back one, and returns 1; returns 0, inserting nothing, when
/// `i` is past the end.
///
/// # Safety
/// `q` is a live deque of `size`-byte elements; `elem` is readable.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_deque_insert(
    q: *mut RawDeque,
    i: u64,
    elem: *const u8,
    size: u64,
) -> u8 {
    let i = i as usize;
    let size = size as usize;
    // SAFETY: the caller's contract.
    let q = unsafe { &mut *q };
    if i > q.len {
        return 0;
    }
    q.grow(size);
    for j in (i..q.len).rev() {
        // SAFETY: element `j` is live and slot `j + 1` is free or already
        // moved from; `grow` left room for one more.
        unsafe { std::ptr::copy_nonoverlapping(q.slot(j, size), q.slot(j + 1, size), size) };
    }
    q.len += 1;
    // SAFETY: slot `i` was vacated above.
    unsafe { std::ptr::copy_nonoverlapping(elem, q.slot(i, size), size) };
    1
}

/// Moves the back element, or the front one when `front`, to `out` and
/// returns 1, or returns 0 when the deque is empty.
///
/// # Safety
/// `q` is a live deque of `size`-byte elements; `out` is writable.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_deque_pop(
    q: *mut RawDeque,
    out: *mut u8,
    size: u64,
    front: u8,
) -> u8 {
    let size = size as usize;
    // SAFETY: the caller's contract.
    let q = unsafe { &mut *q };
    if q.len == 0 {
        return 0;
    }
    let at = if front != 0 { 0 } else { q.len - 1 };
    // SAFETY: element `at` is live.
    unsafe { std::ptr::copy_nonoverlapping(q.slot(at, size), out, size) };
    if front != 0 && size != 0 {
        q.head = (q.head + 1) % q.cap;
    }
    q.len -= 1;
    1
}

/// A stable sort of `n` elements of `size` bytes at `buf` by `cmp`, which
/// returns -1, 0 or 1 (the interpreter's bottom-up merge, so equal
/// elements keep their order).
///
/// # Safety
/// `buf` holds `n` elements; `cmp` is sound on any two of them.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_sort(
    buf: *mut u8,
    n: u64,
    size: u64,
    cmp: unsafe extern "C" fn(*const u8, *const u8) -> i32,
) {
    let n = n as usize;
    let size = size as usize;
    // SAFETY: the caller's contract.
    unsafe { merge_sort(buf, n, size, &mut |a, b| cmp(a, b)) };
}

/// Sorts `n` integers at `buf` in their numeric order: `kind` is the
/// width in bits (8, 16, 32 or 64), plus 256 when signed. Equal integers
/// cannot be told apart, so this agrees with the stable sort.
///
/// # Safety
/// `buf` holds `n` integers of that width, aligned to it.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_sort_int(buf: *mut u8, n: u64, kind: i64) {
    let n = n as usize;
    if n < 2 {
        return;
    }
    // Core's own sort, without its panic (`int_sort`, B-2026-11-04-31).
    // SAFETY (each arm): the caller's contract.
    unsafe {
        use crate::int_sort::sort;
        match kind {
            8 => sort(buf, n),
            16 => sort(buf as *mut u16, n),
            32 => sort(buf as *mut u32, n),
            64 => sort(buf as *mut u64, n),
            264 => sort(buf as *mut i8, n),
            272 => sort(buf as *mut i16, n),
            288 => sort(buf as *mut i32, n),
            320 => sort(buf as *mut i64, n),
            // A fatal write, not `panic!` (B-2026-11-04-31): codegen emits
            // only the kinds above.
            _ => {
                crate::fatal::write_stderr(
                    b"fatal runtime error: karac_mir_sort_int: no such integer kind\n",
                );
                std::process::abort()
            }
        }
    }
}

/// [`karac_mir_sort`] with a comparator that also takes `ctx` (a
/// `sort_by` closure's environment).
///
/// # Safety
/// As [`karac_mir_sort`]; `cmp` is sound with `ctx`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_sort_ctx(
    buf: *mut u8,
    n: u64,
    size: u64,
    cmp: unsafe extern "C" fn(*const u8, *const u8, *const u8) -> i32,
    ctx: *const u8,
) {
    let n = n as usize;
    let size = size as usize;
    // SAFETY: the caller's contract.
    unsafe { merge_sort(buf, n, size, &mut |a, b| cmp(ctx, a, b)) };
}

/// # Safety
/// `buf` holds `n` elements of `size` bytes; `cmp` is sound on any two.
unsafe fn merge_sort(
    buf: *mut u8,
    n: usize,
    size: usize,
    cmp: &mut dyn FnMut(*const u8, *const u8) -> i32,
) {
    if n < 2 || size == 0 {
        return;
    }
    // A fixed element size lets each move be a plain load and store.
    // SAFETY (each arm): the caller's contract.
    unsafe {
        match size {
            1 => merge_passes::<1>(buf, n, size, cmp),
            2 => merge_passes::<2>(buf, n, size, cmp),
            4 => merge_passes::<4>(buf, n, size, cmp),
            8 => merge_passes::<8>(buf, n, size, cmp),
            16 => merge_passes::<16>(buf, n, size, cmp),
            24 => merge_passes::<24>(buf, n, size, cmp),
            32 => merge_passes::<32>(buf, n, size, cmp),
            _ => merge_passes::<0>(buf, n, size, cmp),
        }
    }
}

/// The block a merge sort finishes its narrow widths in before the wide
/// ones. The MIR interpreter's `merge_order` (src/mir/interp.rs
/// `MERGE_BLOCK`) and the sort codegen emits use the same value and
/// schedule, so a comparator sees the same calls in the same order on
/// every backend (B-2026-10-13-18).
const MERGE_BLOCK: usize = 1024;

/// The runs [`merge_widths`] sorts when it starts at width 1, as the
/// interpreter's `SMALL_RUN` and the emitted sort do.
const SMALL_RUN: usize = 4;

/// At most this many elements are one insertion sort, with no scratch
/// buffer, as the interpreter's `SMALL_SORT` and the emitted sort do.
const SMALL_SORT: usize = 20;

/// The bottom-up merge of [`merge_sort`], between `buf` and one scratch
/// buffer: widths 1, 2, 4, ..., the right element first only when
/// strictly less. Past [`MERGE_BLOCK`] elements, every width below it runs
/// one block at a time (in cache) before the wider passes over the whole
/// buffer; the merges are the ones full passes would make. `N` is the
/// element size, or 0 for "`size`, copied".
///
/// # Safety
/// As [`merge_sort`].
unsafe fn merge_passes<const N: usize>(
    buf: *mut u8,
    n: usize,
    size: usize,
    cmp: &mut dyn FnMut(*const u8, *const u8) -> i32,
) {
    if n <= SMALL_SORT {
        // SAFETY: the caller's contract.
        unsafe { insertion(buf, 0, n, size, cmp) };
        return;
    }
    // A `LeanVec`, not `vec!`, whose capacity check keeps a Rust panic in
    // every binary that sorts (B-2026-11-04-31).
    let mut scratch = LeanVec::<u8>::with_capacity(n.saturating_mul(size));
    let tmp = scratch.as_mut_ptr();
    if n <= MERGE_BLOCK {
        // SAFETY: the caller's contract; `tmp` holds `n` elements.
        unsafe { merge_widths::<N>(buf, tmp, n, size, 1, n, cmp) };
        return;
    }
    for b in (0..n).step_by(MERGE_BLOCK) {
        let len = MERGE_BLOCK.min(n - b);
        // SAFETY: block `b` of both buffers, `len` elements in bounds.
        unsafe {
            merge_widths::<N>(
                buf.add(b * size),
                tmp.add(b * size),
                len,
                size,
                1,
                MERGE_BLOCK,
                cmp,
            )
        };
    }
    // SAFETY: as above.
    unsafe { merge_widths::<N>(buf, tmp, n, size, MERGE_BLOCK, n, cmp) };
}

/// Insertion-sorts elements `from..n` of `size` bytes at `buf`: each in
/// turn moves left past each it is strictly less than.
///
/// # Safety
/// `buf` holds `n` elements of `size` bytes.
unsafe fn insertion(
    buf: *mut u8,
    from: usize,
    n: usize,
    size: usize,
    cmp: &mut dyn FnMut(*const u8, *const u8) -> i32,
) {
    if from + 1 >= n {
        return;
    }
    // SAFETY: every index passed is below `n`, in a buffer of `n`.
    let at = |i: usize| unsafe { buf.add(i * size) };
    let mut room = LeanVec::<u8>::with_capacity(size);
    let x = room.as_mut_ptr();
    for k in from + 1..n {
        // SAFETY: indices below `n`; `x` holds one element.
        unsafe {
            std::ptr::copy_nonoverlapping(at(k), x, size);
            let mut j = k;
            while j > from && cmp(x, at(j - 1)) < 0 {
                std::ptr::copy_nonoverlapping(at(j - 1), at(j), size);
                j -= 1;
            }
            std::ptr::copy_nonoverlapping(x, at(j), size);
        }
    }
}

/// The passes of widths `width`, `2 * width`, ... below `limit` over the
/// `n` elements at `buf`, using `tmp` (room for `n`), ending in `buf`. A
/// start at width 1 first sorts each whole run of [`SMALL_RUN`] with the
/// interpreter's `sort4` network (src/mir/interp.rs) and insertion-sorts
/// a short last run; each merge runs from both ends at once, as
/// `merge_pass` there does, with the same comparisons in the same order.
///
/// # Safety
/// `buf` and `tmp` hold `n` elements of `size` bytes and do not overlap.
unsafe fn merge_widths<const N: usize>(
    buf: *mut u8,
    tmp: *mut u8,
    n: usize,
    size: usize,
    mut width: usize,
    limit: usize,
    cmp: &mut dyn FnMut(*const u8, *const u8) -> i32,
) {
    let (mut src, mut dst) = (buf, tmp);
    // SAFETY: every index passed is below `n`, in a buffer of `n`.
    let at = |b: *mut u8, i: usize| unsafe { b.add(i * size) };
    let put = |d: *mut u8, s: *const u8| {
        // SAFETY: both point at whole elements of distinct buffers.
        unsafe {
            if N == 0 {
                std::ptr::copy_nonoverlapping(s, d, size);
            } else {
                (d as *mut [u8; N]).write_unaligned((s as *const [u8; N]).read_unaligned());
            }
        }
    };
    if width == 1 {
        let mut lo = 0;
        while lo + SMALL_RUN <= n {
            let v = |k: usize| at(buf, lo + k) as *const u8;
            let c1 = usize::from(cmp(v(1), v(0)) < 0);
            let c2 = usize::from(cmp(v(3), v(2)) < 0);
            let (a, b, c, d) = (c1, 1 - c1, 2 + c2, 3 - c2);
            let c3 = cmp(v(c), v(a)) < 0;
            let c4 = cmp(v(d), v(b)) < 0;
            let min = if c3 { c } else { a };
            let max = if c4 { b } else { d };
            let left = if c3 {
                a
            } else if c4 {
                c
            } else {
                b
            };
            let right = if c4 {
                d
            } else if c3 {
                b
            } else {
                c
            };
            let c5 = cmp(v(right), v(left)) < 0;
            let (l, h) = if c5 { (right, left) } else { (left, right) };
            // Through `tmp`: the four go out in their order, then back.
            for (k, from) in [min, l, h, max].into_iter().enumerate() {
                put(at(tmp, lo + k), v(from));
            }
            // SAFETY: four whole elements of each buffer, in bounds.
            unsafe { std::ptr::copy_nonoverlapping(at(tmp, lo), at(buf, lo), SMALL_RUN * size) };
            lo += SMALL_RUN;
        }
        // SAFETY: `lo..n` is in `buf`.
        unsafe { insertion(buf, lo, n, size, cmp) };
        width = SMALL_RUN;
    }
    while width < limit {
        // A plain loop: `step_by` asserts a nonzero step, a Rust panic in
        // every binary that sorts (B-2026-11-04-31).
        let mut lo = 0;
        while lo < n {
            let mid = (lo + width).min(n);
            let hi = (lo + 2 * width).min(n);
            // Cursors at the front, and one past the back.
            let (mut i, mut j, mut k) = (lo, mid, lo);
            let (mut ie, mut je, mut ke) = (mid, hi, hi);
            for _ in 0..(mid - lo).min(hi - mid) {
                let right = cmp(at(src, j), at(src, i)) < 0;
                put(at(dst, k), at(src, if right { j } else { i }));
                j += usize::from(right);
                i += usize::from(!right);
                k += 1;
                ke -= 1;
                let left = cmp(at(src, je - 1), at(src, ie - 1)) < 0;
                put(at(dst, ke), at(src, if left { ie - 1 } else { je - 1 }));
                ie -= usize::from(left);
                je -= usize::from(!left);
            }
            // Both ends took one element: merge the pair again from the
            // front, so the result is a permutation.
            if i > ie || j > je {
                (i, j, k, ie, je) = (lo, mid, lo, mid, hi);
            }
            while i < ie && j < je {
                let right = cmp(at(src, j), at(src, i)) < 0;
                put(at(dst, k), at(src, if right { j } else { i }));
                j += usize::from(right);
                i += usize::from(!right);
                k += 1;
            }
            // SAFETY: the rest of each run, whole elements in bounds.
            unsafe {
                std::ptr::copy_nonoverlapping(at(src, i), at(dst, k), (ie - i) * size);
                k += ie - i;
                std::ptr::copy_nonoverlapping(at(src, j), at(dst, k), (je - j) * size);
            }
            lo = hi;
        }
        std::mem::swap(&mut src, &mut dst);
        width *= 2;
    }
    if src != buf {
        // SAFETY: the sorted elements are in the scratch buffer.
        unsafe { std::ptr::copy_nonoverlapping(src, buf, n * size) };
    }
}

/// The bytes of the values `forget` took, kept so that what they own stays
/// reachable for a leak checker: `forget` leaks by design, and LeakSanitizer
/// under `__mir-llvm-asan` would otherwise report it (B-2026-10-22-38).
static FORGOTTEN: std::sync::Mutex<Vec<Vec<u8>>> = std::sync::Mutex::new(Vec::new());

/// `forget(x)`: keeps a copy of `x`'s `n` bytes at `p` and runs no drop.
///
/// # Safety
/// `p` is readable for `n` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_forget(p: *const u8, n: u64) {
    let n = n as usize;
    // SAFETY: the caller's contract.
    let bytes = unsafe { std::slice::from_raw_parts(p, n) }.to_vec();
    FORGOTTEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(bytes);
}

/// The panic record's kind of the next abort (design.md § Panic record,
/// B-2026-11-05-18), as a code into [`PANIC_KINDS`]; `u8::MAX` when no
/// `panic_kind` call named one, and the abort's reason decides.
static PANIC_KIND: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);

/// Set by `karac_jit_runner` when `karac run` / `karac test` asked for the
/// panic record; a built binary run directly never writes it.
static PANIC_RECORD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The panic record's kinds, at their codes: `karac::mir::syntax::PANIC_KINDS`.
const PANIC_KINDS: [&[u8]; 10] = [
    b"explicit",
    b"index_out_of_bounds",
    b"overflow",
    b"div_by_zero",
    b"unwrap",
    b"borrow_conflict",
    b"assertion",
    b"alloc_failure",
    b"unreachable",
    b"deadlock",
];

/// `panic_kind(code)`: the kind of the abort that follows.
#[no_mangle]
pub extern "C" fn karac_mir_panic_kind(kind: i64) {
    PANIC_KIND.store(
        u8::try_from(kind).unwrap_or(0),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// The runner asks for the panic record.
#[no_mangle]
pub extern "C" fn karac_mir_panic_record_enable() {
    PANIC_RECORD.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// After an abort's human report, the record's kind on its own line, when
/// the runner asked for it: `{"type":"panic_record","kind":K}`. The kind is
/// the one a `panic_kind` call named; else `explicit` when the program set
/// the message (`panic(msg)`); else the one the abort's reason text names.
fn panic_record(message_set: bool, reason_text: &[u8]) {
    let kind = PANIC_KIND.swap(u8::MAX, std::sync::atomic::Ordering::Relaxed);
    if !PANIC_RECORD.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let code = if kind != u8::MAX {
        kind
    } else if message_set {
        0
    } else {
        match reason_text {
            b"index out of bounds" => 1,
            b"integer overflow" | b"shift amount out of range" => 2,
            b"division by zero" => 3,
            b"entered unreachable code" => 8,
            _ => 0,
        }
    };
    let name = PANIC_KINDS
        .get(usize::from(code))
        .copied()
        .unwrap_or(b"explicit");
    let mut line = LeanVec::<u8>::new();
    line.extend_from_slice(b"{\"type\":\"panic_record\",\"kind\":\"");
    line.extend_from_slice(name);
    line.extend_from_slice(b"\"}\n");
    write(true, line.as_slice());
}

/// Ends a `panic_message` format: its text is the next abort's message.
#[no_mangle]
pub extern "C" fn karac_mir_panic_set() {
    // No `rich_abort_used`: a message alone is a part the plain report in
    // `karac_mir_abort_at` writes, and this path stays panic-free, so a
    // program whose `unwrap` or `panic(msg)` may fire keeps std's panic
    // hook out of its binary (B-2026-11-02-24).
    fmt_pop(set_panic_bytes);
}

/// Ends the process with `code`, after writing what is buffered.
#[no_mangle]
pub extern "C" fn karac_mir_exit(code: i32) -> ! {
    out_flush();
    #[cfg(all(unix, feature = "net"))]
    if code == 101 && linked_with_asan() {
        // A panic runs no Drop (core semantics §10.2), so what it leaves
        // allocated is not a leak; under `__mir-llvm-asan` leave without
        // LeakSanitizer's exit check, which would report it (B-2026-10-20-38).
        // SAFETY: ends the process; nothing runs after it.
        unsafe { libc::_exit(code) }
    }
    // libc's `exit`, not `std::process::exit`, whose flush of std's stdout
    // (which every writer in the runtime flushes as it writes) reaches a
    // panic, and with it std's panic hook and backtrace symbolizer
    // (B-2026-10-28-36).
    extern "C" {
        fn exit(code: i32) -> !;
    }
    // SAFETY: ends the process.
    unsafe { exit(code) }
}

/// Whether AddressSanitizer is linked in, which `__mir-llvm-asan` does.
#[cfg(all(unix, feature = "net"))]
fn linked_with_asan() -> bool {
    // SAFETY: a lookup of a NUL-terminated name in the loaded objects.
    !unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"__asan_init".as_ptr()) }.is_null()
}

/// The program's `#[panic_handler]` thunk (`__karac_panic_thunk`), which
/// `main` registers before it runs: a unit function of no arguments, so it
/// takes only the slot for its result.
static PANIC_HANDLER: std::sync::atomic::AtomicPtr<u8> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Set once a panic has begun running the handler; a panic after that,
/// in the handler or in another task, ends the process at once.
static PANICKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The message and location of the panic the handler runs for.
static PANIC_INFO: std::sync::Mutex<(String, String)> =
    std::sync::Mutex::new((String::new(), String::new()));

/// Registers the program's panic-handler thunk (core semantics §10.2).
#[no_mangle]
pub extern "C" fn karac_mir_set_panic_handler(thunk: *mut u8) {
    rich_abort_used();
    PANIC_HANDLER.store(thunk, std::sync::atomic::Ordering::SeqCst);
}

/// Whether a panic may report itself: false once one has begun, whose
/// handler is running or has run. A panic for which this is false exits
/// with code 101 without a second report.
pub(crate) fn panic_begins() -> bool {
    !PANICKING.swap(true, std::sync::atomic::Ordering::SeqCst)
}

/// Runs the program's panic handler, if it has one, for the panic whose
/// record was just written: once, then the caller exits with code 101.
pub(crate) fn run_panic_handler(message: &str, location: &str) {
    let thunk = PANIC_HANDLER.load(std::sync::atomic::Ordering::SeqCst);
    if thunk.is_null() {
        return;
    }
    if let Ok(mut info) = PANIC_INFO.lock() {
        *info = (message.to_string(), location.to_string());
    }
    // SAFETY: `karac_mir_set_panic_handler` was given the thunk, a unit
    // function whose only parameter is the slot for its result.
    let f: extern "C" fn(*mut u8) = unsafe { std::mem::transmute(thunk) };
    let mut slot = [0u64; 2];
    f(slot.as_mut_ptr().cast());
}

/// `__karac_panic_message` (`which` 0) or `__karac_panic_location` (1):
/// that part of the panic being handled, as a `String` at `out`.
///
/// # Safety
/// `out` is writable for a `String`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_panic_info(out: *mut RawVec, which: i32) {
    let text = PANIC_INFO
        .lock()
        .map(|i| if which == 0 { i.0.clone() } else { i.1.clone() })
        .unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe { string_of(out, text.as_bytes()) };
}

/// A deadlock, which is a panic (core semantics §10.1): `error: deadlock:
/// {what}` on standard error, the panic handler, then exit 101.
pub(crate) fn deadlock(what: &str) -> ! {
    if panic_begins() {
        crate::mir_task::drain_logs();
        karac_mir_flush();
        eprintln!("error: deadlock: {what}");
        run_panic_handler(&format!("deadlock: {what}"), "");
    }
    karac_mir_exit(101)
}

/// An abort's message, marked when it was raised while a contract
/// predicate was being evaluated, in it or in a function it calls
/// (design.md § Contracts rule 2, B-2026-10-25-23).
fn abort_message(msg: String) -> String {
    if crate::in_contract_predicate() {
        format!("{CONTRACT_PREDICATE_PANICKED}{msg}")
    } else {
        msg
    }
}

const CONTRACT_PREDICATE_PANICKED: &str = "contract predicate panicked: ";

/// How many frames the error return trace keeps, the most recent ones
/// (design.md § Error return traces).
const ERROR_TRACE_DEPTH: usize = 64;

/// What the trace reads when its `?` chain ran across tasks.
const ERROR_TRACE_SPLIT: &str = "unavailable (the ? chain was split across parallel tasks)";

/// A trace frame: its place's address and length, its task, and its
/// `.context()` message.
type TraceFrame = (usize, usize, Option<u64>, Option<Box<str>>);

/// The thread's error return trace (the program's under the pool, whose
/// tasks move between workers): the place (`file:line:col`, a string
/// constant of the program, as its address and length) of each `?` that
/// returned an error since the last one that succeeded, oldest first, with
/// the task that ran it (`None` outside a task) and the message of a
/// `.context(msg)` site; and whether it dropped its oldest frames to stay
/// [`ERROR_TRACE_DEPTH`] deep.
struct ErrorTrace {
    frames: Vec<TraceFrame>,
    truncated: bool,
}

crate::mir_task::program_state! {
    static ERROR_TRACE: ErrorTrace = ErrorTrace {
        frames: Vec::new(),
        truncated: false,
    };
}

/// A `?` at `at` returned its error (B-2026-10-25-3). `live` is the
/// program's count of threads whose trace has frames, which a `?` that
/// succeeds reads to skip [`karac_mir_error_trace_clear`] when it is 0.
///
/// # Safety
/// `at` is a string constant of the program, readable for `at_len` bytes
/// for as long as the program runs, and `live` is the program's count, an
/// aligned `u64` that lives as long.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_error_trace_push(
    at: *const u8,
    at_len: u64,
    live: *const std::sync::atomic::AtomicU64,
) {
    // SAFETY: the caller's contract.
    unsafe { error_trace_push(at, at_len, live, None) }
}

/// [`karac_mir_error_trace_push`] for a `.context(msg)` site: the frame
/// keeps a copy of `msg` (design.md § Errors, B-2026-10-25-44).
///
/// # Safety
/// As for [`karac_mir_error_trace_push`], and `msg` is readable for
/// `msg_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_error_trace_push_context(
    at: *const u8,
    at_len: u64,
    live: *const std::sync::atomic::AtomicU64,
    msg: *const u8,
    msg_len: u64,
) {
    // SAFETY: the caller's contract.
    let msg = String::from_utf8_lossy(unsafe { bytes(msg, msg_len as usize) });
    // SAFETY: the caller's contract.
    unsafe { error_trace_push(at, at_len, live, Some(msg.into())) }
}

/// # Safety
/// As for [`karac_mir_error_trace_push`].
unsafe fn error_trace_push(
    at: *const u8,
    at_len: u64,
    live: *const std::sync::atomic::AtomicU64,
    context: Option<Box<str>>,
) {
    rich_abort_used();
    let task = crate::mir_task::current_task();
    ERROR_TRACE.with(|t| {
        if t.frames.is_empty() {
            // SAFETY: the caller's contract.
            unsafe { &*live }.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if t.frames.len() >= ERROR_TRACE_DEPTH {
            t.frames.remove(0);
            t.truncated = true;
        }
        t.frames.push((at as usize, at_len as usize, task, context));
    });
}

/// A `?` succeeded: the error, if any, was handled. Only the running
/// task's frames go; another task's chain may still be on its way
/// (B-2026-10-25-30).
///
/// # Safety
/// `live` is as for [`karac_mir_error_trace_push`].
#[no_mangle]
pub unsafe extern "C" fn karac_mir_error_trace_clear(live: *const std::sync::atomic::AtomicU64) {
    let task = crate::mir_task::current_task();
    ERROR_TRACE.with(|t| {
        if !t.frames.is_empty() {
            t.frames.retain(|f| f.2 != task);
            if !t.frames.is_empty() {
                return;
            }
            t.truncated = false;
            // SAFETY: the caller's contract.
            unsafe { &*live }.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
    });
}

/// The error return trace as the diagnostic it goes with prints it, ending
/// at a panic's own place `last` when there is one, or nothing when it has
/// no frame (design.md § Error return traces, § Panic record).
fn error_trace_text(last: Option<&str>) -> String {
    ERROR_TRACE.with(|t| {
        // Frames of more than one task: no one order of them is the chain
        // (design.md § Error return traces, B-2026-10-25-30).
        if t.frames.iter().any(|f| f.2 != t.frames[0].2) {
            return format!("Error return trace: {ERROR_TRACE_SPLIT}\n");
        }
        let mut frames: Vec<std::borrow::Cow<'_, str>> = t
            .frames
            .iter()
            .map(|(p, n, _, context)| {
                // SAFETY: `karac_mir_error_trace_push`'s contract.
                let at = String::from_utf8_lossy(unsafe { bytes(*p as *const u8, *n) });
                match context {
                    // `run`'s JSON reads the message back from this text.
                    Some(c) => format!(
                        "{at}  context: {}",
                        c.replace('\\', "\\\\").replace('\n', "\\n")
                    )
                    .into(),
                    None => at,
                }
            })
            .collect();
        let mut truncated = t.truncated;
        if let Some(at) = last {
            if frames.len() >= ERROR_TRACE_DEPTH {
                frames.remove(0);
                truncated = true;
            }
            frames.push(at.into());
        }
        if frames.is_empty() {
            return String::new();
        }
        let mut text = String::from("Error return trace:\n");
        for f in frames {
            text.push_str(&format!("  {f}\n"));
        }
        if truncated {
            text.push_str(&format!(
                "  ... (trace truncated, max {ERROR_TRACE_DEPTH} frames)\n"
            ));
        }
        text
    })
}

/// After `main` returned an error and `Error: e` was printed: the error
/// return trace, on standard error.
#[no_mangle]
pub extern "C" fn karac_mir_error_trace_print() {
    let text = error_trace_text(None);
    if !text.is_empty() {
        write(true, text.as_bytes());
    }
}

/// A runtime error: the message on standard error, then exit 101.
///
/// # Safety
/// `ptr` is readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_abort(ptr: *const u8, len: u64) -> ! {
    let len = len as usize;
    if !panic_begins() {
        karac_mir_exit(101)
    }
    crate::mir_task::drain_logs();
    let set = take_panic_message();
    let message_set = set.is_some();
    // SAFETY: the caller's contract.
    let reason = unsafe { bytes(ptr, len) };
    let msg = abort_message(set.unwrap_or_else(|| String::from_utf8_lossy(reason).into_owned()));
    write(true, format!("runtime error: {msg}\n").as_bytes());
    panic_record(message_set, reason);
    run_panic_handler(&msg, "");
    karac_mir_exit(101)
}

/// A runtime error at a place in the source: the message, then `at`
/// (`file:line:col`), as the MIR interpreter reports one; then exit 101.
/// An empty `at` reports the message alone.
///
/// # Safety
/// `ptr` is readable for `len` bytes and `at` for `at_len`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_abort_at(
    ptr: *const u8,
    len: u64,
    at: *const u8,
    at_len: u64,
) -> ! {
    if !panic_begins() {
        karac_mir_exit(101)
    }
    let rich = RICH_ABORT.load(std::sync::atomic::Ordering::Acquire);
    if !rich.is_null() {
        // SAFETY: `rich_abort_used` stores `rich_abort`, an `AbortAt`.
        let rich: AbortAt = unsafe { std::mem::transmute(rich) };
        // SAFETY: the caller's contract.
        unsafe { rich(ptr, len, at, at_len) }
    }
    // None of the parts `rich_abort` adds was set: the message and the
    // place, and the error return trace that is the place alone.
    // SAFETY: the caller's contract.
    let (msg, at) = unsafe { (bytes(ptr, len as usize), bytes(at, at_len as usize)) };
    let mut report = LeanVec::<u8>::new();
    report.extend_from_slice(b"runtime error: ");
    if crate::in_contract_predicate() {
        report.extend_from_slice(CONTRACT_PREDICATE_PANICKED.as_bytes());
    }
    let mut message_set = false;
    with_panic(|p| {
        message_set = p.set;
        report.extend_from_slice(if p.set { p.text.as_slice() } else { msg });
        p.set = false;
        p.text.clear();
    });
    report.push(b'\n');
    if !at.is_empty() {
        for part in [&b"  at "[..], at, b"\nError return trace:\n  ", at, b"\n"] {
            report.extend_from_slice(part);
        }
    }
    write(true, report.as_slice());
    panic_record(message_set, msg);
    karac_mir_exit(101)
}

/// The abort report's parts that only some programs need: the error
/// return trace, held log records, and the program's
/// panic handler. [`karac_mir_abort_at`] reaches them through this
/// pointer, which the natives that set any of them store
/// ([`rich_abort_used`]), so a program that sets none links none of them,
/// nor the panics their `String`s and maps reach, which keep std's panic
/// hook and backtrace symbolizer in a binary (B-2026-10-28-36).
static RICH_ABORT: std::sync::atomic::AtomicPtr<()> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

type AbortAt = unsafe fn(*const u8, u64, *const u8, u64) -> !;

/// The program set a part of the abort report [`rich_abort`] writes.
#[inline]
pub(crate) fn rich_abort_used() {
    // `black_box`: LTO sees every store to the pointer, and would
    // otherwise call `rich_abort` directly from `karac_mir_abort_at`.
    RICH_ABORT.store(
        std::hint::black_box(rich_abort as AbortAt as *mut ()),
        std::sync::atomic::Ordering::Release,
    );
}

/// The message a `panic(msg)` or a native set for the abort that follows.
fn set_panic_message(msg: String) {
    set_panic_bytes(msg.as_bytes());
}

/// [`karac_mir_abort_at`]'s report once a part of it was set.
///
/// # Safety
/// As [`karac_mir_abort_at`].
unsafe fn rich_abort(ptr: *const u8, len: u64, at: *const u8, at_len: u64) -> ! {
    let len = len as usize;
    let at_len = at_len as usize;
    crate::mir_task::drain_logs();
    let set = take_panic_message();
    let message_set = set.is_some();
    // SAFETY: the caller's contract.
    let reason = unsafe { bytes(ptr, len) };
    let msg = abort_message(set.unwrap_or_else(|| String::from_utf8_lossy(reason).into_owned()));
    // SAFETY: the caller's contract.
    let at = String::from_utf8_lossy(unsafe { bytes(at, at_len) });
    let report = if at.is_empty() {
        format!("runtime error: {msg}\n")
    } else {
        let trace = error_trace_text(Some(at.as_ref()));
        format!("runtime error: {msg}\n  at {at}\n{trace}")
    };
    write(true, report.as_bytes());
    panic_record(message_set, reason);
    run_panic_handler(&msg, &at);
    karac_mir_exit(101)
}

/// A panic in a program with an isolating group (core-semantics §11.12):
/// a task of one takes it, and the call returns, for the caller to leave
/// by its cancel exits; anywhere else it is `karac_mir_abort_at`'s.
///
/// # Safety
/// As [`karac_mir_abort_at`].
#[no_mangle]
pub unsafe extern "C" fn karac_mir_panic_at(ptr: *const u8, len: u64, at: *const u8, at_len: u64) {
    // SAFETY: the caller's contract.
    let place = String::from_utf8_lossy(unsafe { bytes(at, at_len as usize) }).into_owned();
    let message = || {
        abort_message(take_panic_message().unwrap_or_else(|| {
            // SAFETY: the caller's contract.
            String::from_utf8_lossy(unsafe { bytes(ptr, len as usize) }).into_owned()
        }))
    };
    if crate::mir_task::contain_panic(message, &place) {
        // The task's frames, and any contract predicate among them, are
        // gone; a later panic's own message is its own.
        take_panic_message();
        crate::leave_predicates();
        return;
    }
    // SAFETY: the caller's contract.
    unsafe { karac_mir_abort_at(ptr, len, at, at_len) }
}

/// `karac_mir_task_failure(message, location)`, in a task's poll once its
/// frame left by its cancel exits: [`crate::mir_task::task_failure`]'s
/// answer, with a panic's message and location written as `String`s at
/// `message` and `location` (left alone otherwise).
///
/// # Safety
/// `message` and `location` are writable for a `String` each.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_task_failure(message: *mut RawVec, location: *mut RawVec) -> u8 {
    let (why, panic) = crate::mir_task::task_failure();
    if let Some((m, l)) = panic {
        // SAFETY: the caller's contract.
        unsafe {
            string_of(message, m.as_bytes());
            string_of(location, l.as_bytes());
        }
    }
    why
}

/// A string in Rust's `Debug` form (`"a\n"`), for `dbg`.
///
/// # Safety
/// `ptr` is readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_fmt_str_debug(ptr: *const u8, len: u64) {
    let len = len as usize;
    // SAFETY: the caller's contract.
    let b = unsafe { bytes(ptr, len) };
    append(&format!("{:?}", String::from_utf8_lossy(b)));
}

/// A char in Rust's `Debug` form (`'a'`), for `dbg`.
#[no_mangle]
pub extern "C" fn karac_mir_fmt_char_debug(c: u32) {
    append(&format!("{:?}", char::from_u32(c).unwrap_or('\u{fffd}')));
}

/// The char method `op` (the index into the list below) of `c`, with
/// `radix` for `is_digit` and `to_digit`: a predicate is 0 or 1, a case
/// mapping is the mapped char (itself when the mapping expands), and
/// `to_digit` is the digit or -1. Bit 8 of `op` reads `c` as a byte (an
/// integer receiver), which is in no class when it is above 255.
#[no_mangle]
pub extern "C" fn karac_mir_char(op: u32, c: i64, radix: i64) -> i64 {
    let name = CHAR_OPS.get((op & 0xff) as usize).copied().unwrap_or("");
    // A radix outside 2..=36 panics, worded as the MIR interpreter words it;
    // `i64::MIN` tells the caller to abort (B-2026-10-22-38).
    if matches!(name, "is_digit" | "to_digit") && !(2..=36).contains(&radix) {
        let msg = format!("char.{name}: radix {radix} is not in 2..=36");
        set_panic_message(msg);
        return i64::MIN;
    }
    let ch = if op & 0x100 != 0 {
        u8::try_from(c).ok().map(char::from)
    } else {
        u32::try_from(c).ok().and_then(char::from_u32)
    };
    let Some(ch) = ch else {
        return 0;
    };
    let b = i64::from;
    let one = |it: &mut dyn Iterator<Item = char>| {
        let first = it.next();
        match (first, it.next()) {
            (Some(m), None) => m,
            _ => ch,
        }
    };
    let r = u32::try_from(radix).unwrap_or(0);
    match name {
        "is_alphabetic" => b(ch.is_alphabetic()),
        "is_numeric" => b(ch.is_numeric()),
        "is_alphanumeric" => b(ch.is_alphanumeric()),
        "is_uppercase" => b(ch.is_uppercase()),
        "is_lowercase" => b(ch.is_lowercase()),
        "is_whitespace" => b(ch.is_whitespace()),
        "is_ascii" => b(ch.is_ascii()),
        "is_ascii_digit" => b(ch.is_ascii_digit()),
        "is_ascii_alphabetic" => b(ch.is_ascii_alphabetic()),
        "is_ascii_alphanumeric" => b(ch.is_ascii_alphanumeric()),
        "is_ascii_uppercase" => b(ch.is_ascii_uppercase()),
        "is_ascii_lowercase" => b(ch.is_ascii_lowercase()),
        "is_ascii_punctuation" => b(ch.is_ascii_punctuation()),
        "is_ascii_whitespace" => b(ch.is_ascii_whitespace()),
        "is_ascii_hexdigit" => b(ch.is_ascii_hexdigit()),
        "to_lowercase" => i64::from(u32::from(one(&mut ch.to_lowercase()))),
        "to_uppercase" => i64::from(u32::from(one(&mut ch.to_uppercase()))),
        "to_ascii_lowercase" => i64::from(u32::from(ch.to_ascii_lowercase())),
        "to_ascii_uppercase" => i64::from(u32::from(ch.to_ascii_uppercase())),
        "is_digit" => b(ch.is_digit(r)),
        "to_digit" => ch.to_digit(r).map_or(-1, i64::from),
        _ => 0,
    }
}

/// The char methods [`karac_mir_char`] answers, by index; codegen keeps
/// the same list (`src/codegen/mir/num.rs`).
const CHAR_OPS: &[&str] = &[
    "is_alphabetic",
    "is_numeric",
    "is_alphanumeric",
    "is_uppercase",
    "is_lowercase",
    "is_whitespace",
    "is_ascii",
    "is_ascii_digit",
    "is_ascii_alphabetic",
    "is_ascii_alphanumeric",
    "is_ascii_uppercase",
    "is_ascii_lowercase",
    "is_ascii_punctuation",
    "is_ascii_whitespace",
    "is_ascii_hexdigit",
    "to_lowercase",
    "to_uppercase",
    "to_ascii_lowercase",
    "to_ascii_uppercase",
    "is_digit",
    "to_digit",
];

/// `i128.parse(s)` / `u128.parse(s)` and their `from_str_radix`: 1 with
/// the number's 16 bytes at `out` when the trimmed text is one that fits,
/// else 0 (B-2026-10-18-11).
///
/// # Safety
/// `ptr` is readable for `len` bytes; `out` is writable for 16 bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_parse_int128(
    ptr: *const u8,
    len: u64,
    radix: i64,
    signed: u8,
    out: *mut u8,
) -> u8 {
    let len = len as usize;
    // SAFETY: the caller's contract.
    let Ok(text) = std::str::from_utf8(unsafe { bytes(ptr, len) }) else {
        return 0;
    };
    let text = text.trim();
    if !(2..=36).contains(&radix) {
        return 0;
    }
    let got = match radix_digits(text.as_bytes(), radix as u32, signed != 0) {
        Some((true, m)) => signed_of(true, m).map(i128::to_ne_bytes),
        Some((false, m)) if signed != 0 => signed_of(false, m).map(i128::to_ne_bytes),
        Some((false, m)) => Some(m.to_ne_bytes()),
        None => None,
    };
    let Some(b) = got else {
        return 0;
    };
    // SAFETY: the caller's contract.
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), out, 16) };
    1
}

/// `text` as an integer type's `from_str_radix` reads it: a `+`, or a `-`
/// when `signed`, then one or more digits of `radix` (2 to 36, either
/// case), as the sign and the magnitude; `None` for any other text or a
/// magnitude past `u128`. Written out because core's `from_str_radix`
/// asserts its radix, a Rust panic in every binary that parses an integer
/// (B-2026-11-04-31).
fn radix_digits(text: &[u8], radix: u32, signed: bool) -> Option<(bool, u128)> {
    let (neg, digits) = match text {
        [b'-', rest @ ..] if signed => (true, rest),
        [b'+', rest @ ..] => (false, rest),
        _ => (false, text),
    };
    if digits.is_empty() {
        return None;
    }
    let mut m: u128 = 0;
    for &c in digits {
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'z' => c - b'a' + 10,
            b'A'..=b'Z' => c - b'A' + 10,
            _ => return None,
        } as u32;
        if d >= radix {
            return None;
        }
        m = m.checked_mul(radix as u128)?.checked_add(d as u128)?;
    }
    Some((neg, m))
}

/// The `i128` of a sign and a magnitude, when it is one.
fn signed_of(neg: bool, m: u128) -> Option<i128> {
    if neg {
        (m <= 1u128 << 127).then(|| (m as i128).wrapping_neg())
    } else {
        i128::try_from(m).ok()
    }
}

/// A radix-10 integer of at most 18 digits, with an optional sign and
/// ASCII whitespace around it, read in place: it cannot overflow an `i64`,
/// and the text is all ASCII, so `str::trim` would have cut the same
/// bytes. `None` sends every other text to the `from_str_radix` path,
/// which gives the verdict on it (B-2026-10-13-34).
fn short_decimal(raw: &[u8], radix: i64) -> Option<(bool, i128)> {
    if radix != 10 {
        return None;
    }
    let mut b = raw;
    while let [first, rest @ ..] = b {
        if !first.is_ascii_whitespace() {
            break;
        }
        b = rest;
    }
    while let [rest @ .., last] = b {
        if !last.is_ascii_whitespace() {
            break;
        }
        b = rest;
    }
    let (neg, digits) = match b.first() {
        Some(b'-') => (true, &b[1..]),
        Some(b'+') => (false, &b[1..]),
        _ => (false, b),
    };
    if digits.is_empty() || digits.len() > 18 {
        return None;
    }
    let mut n: i64 = 0;
    for &d in digits {
        if !d.is_ascii_digit() {
            return None;
        }
        n = n * 10 + (d - b'0') as i64;
    }
    Some((neg, if neg { -(n as i128) } else { n as i128 }))
}

/// `T.parse(s)` / `T.from_str_radix(s, radix)` for an integer of `bits`
/// bits (at most 64): 1 with the number at `out` when the trimmed text is
/// one that fits, else 0.
///
/// # Safety
/// `ptr` is readable for `len` bytes; `out` is writable.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_parse_int(
    ptr: *const u8,
    len: u64,
    radix: i64,
    bits: u32,
    signed: u8,
    out: *mut i64,
) -> u8 {
    let len = len as usize;
    // SAFETY: the caller's contract.
    let raw = unsafe { bytes(ptr, len) };
    if !(2..=36).contains(&radix) {
        return 0;
    }
    let n = match short_decimal(raw, radix) {
        Some((true, _)) if signed == 0 => return 0,
        Some((_, n)) => n,
        None => {
            // Invalid UTF-8 cannot parse, so it fails here rather than
            // through a lossy, allocated copy (B-2026-10-13-34).
            let Ok(text) = std::str::from_utf8(raw) else {
                return 0;
            };
            let text = text.trim();
            if signed == 0 && text.starts_with('-') {
                return 0;
            }
            match radix_digits(text.as_bytes(), radix as u32, true)
                .and_then(|(neg, m)| signed_of(neg, m))
            {
                Some(n) => n,
                None => return 0,
            }
        }
    };
    let fits = match (bits, signed != 0) {
        // The common widths, without the variable 128-bit shifts.
        (64, true) => i64::try_from(n).is_ok(),
        (64, false) => u64::try_from(n).is_ok(),
        (_, true) => {
            let half = 1i128 << (bits - 1);
            (-half..half).contains(&n)
        }
        (_, false) => n >= 0 && n < (1i128 << bits),
    };
    if !fits {
        return 0;
    }
    // SAFETY: the caller's contract.
    unsafe { *out = n as i64 };
    1
}

/// `f64.parse(s)`: 1 with the number at `out`, else 0.
///
/// # Safety
/// `ptr` is readable for `len` bytes; `out` is writable.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_parse_f64(ptr: *const u8, len: u64, out: *mut f64) -> u8 {
    let len = len as usize;
    // SAFETY: the caller's contract.
    let Ok(text) = std::str::from_utf8(unsafe { bytes(ptr, len) }) else {
        return 0;
    };
    match text.trim().parse::<f64>() {
        Ok(v) => {
            // SAFETY: the caller's contract.
            unsafe { *out = v };
            1
        }
        Err(_) => 0,
    }
}

/// `f32.parse(s)`: 1 with the number, parsed at `f32` and widened, at
/// `out`, else 0. Parsing at `f64` and narrowing would round twice
/// (B-2026-10-21-65).
///
/// # Safety
/// `ptr` is readable for `len` bytes; `out` is writable.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_parse_f32(ptr: *const u8, len: u64, out: *mut f64) -> u8 {
    let len = len as usize;
    // SAFETY: the caller's contract.
    let Ok(text) = std::str::from_utf8(unsafe { bytes(ptr, len) }) else {
        return 0;
    };
    match text.trim().parse::<f32>() {
        Ok(v) => {
            // SAFETY: the caller's contract.
            unsafe { *out = f64::from(v) };
            1
        }
        Err(_) => 0,
    }
}

/// `a.pow(e)` for an integer of `bits` bits (at most 64): 0 with the
/// power at `out`, 1 when it overflows the type, 2 for an exponent out of
/// `u32`. An unsigned `a` arrives as its bits.
///
/// # Safety
/// `out` is writable.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_ipow(
    a: i64,
    e: i64,
    bits: u32,
    signed: u8,
    out: *mut i64,
) -> u8 {
    let Ok(e) = u32::try_from(e) else {
        return 2;
    };
    let a = if signed != 0 {
        i128::from(a)
    } else {
        i128::from(a as u64)
    };
    let (lo, hi) = if signed != 0 {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << bits) - 1)
    };
    match a.checked_pow(e) {
        Some(v) if (lo..=hi).contains(&v) => {
            // SAFETY: the caller's contract.
            unsafe { *out = v as i64 };
            0
        }
        _ => 1,
    }
}

/// An `Arena[T]`'s items: a vector of them and the arena's id, which its
/// `ArenaRef` handles carry.
#[repr(C)]
pub struct RawArena {
    pub items: RawVec,
    pub id: i64,
}

static ARENA_IDS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// `Arena.new()`: an empty arena with the next id (from 1, as the
/// interpreter numbers them).
#[no_mangle]
pub extern "C" fn karac_mir_arena_new() -> *mut RawArena {
    let id = ARENA_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    Box::into_raw(Box::new(RawArena {
        items: RawVec::new(std::ptr::null_mut(), 0, 0),
        id,
    }))
}

/// Frees an arena whose items compiled code has dropped.
///
/// # Safety
/// `a` came from [`karac_mir_arena_new`] and is not used again.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_arena_free(a: *mut RawArena) {
    // SAFETY: the caller's contract.
    let a = unsafe { Box::from_raw(a) };
    if a.items.cap != 0 && a.items.cap != usize::MAX {
        // SAFETY: the buffer came from `karac_realloc_or_panic`.
        unsafe { free(a.items.ptr.cast()) };
    }
}

/// An `Interner`'s strings, each a `String` that stays where it is so
/// `resolve` can lend it, and their ids. Its drop frees them all.
pub struct MirInterner {
    // Boxed so a lent `String` stays put when the vector grows.
    #[allow(clippy::vec_box)]
    strings: Vec<Box<RawVec>>,
    ids: std::collections::HashMap<Vec<u8>, usize>,
}

/// `Interner.new()`.
#[no_mangle]
pub extern "C" fn karac_mir_interner_new() -> *mut MirInterner {
    Box::into_raw(Box::new(MirInterner {
        strings: Vec::new(),
        ids: std::collections::HashMap::new(),
    }))
}

/// `Interner.intern(s)`: the id of `ptr[..len]`, new or not.
///
/// # Safety
/// `h` came from [`karac_mir_interner_new`]; `ptr` is readable for `len`
/// bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_interner_intern(
    h: *mut MirInterner,
    ptr: *const u8,
    len: u64,
) -> i64 {
    let len = len as usize;
    // SAFETY: the caller's contract.
    let (t, b) = unsafe { (&mut *h, bytes(ptr, len)) };
    if let Some(&id) = t.ids.get(b) {
        return id as i64;
    }
    let mut s = Box::new(RawVec::new(std::ptr::null_mut(), 0, 0));
    // SAFETY: `s` is a writable `String`.
    unsafe { string_of(&mut *s, b) };
    t.strings.push(s);
    t.ids.insert(b.to_vec(), t.strings.len() - 1);
    t.strings.len() as i64 - 1
}

/// The empty `String` a foreign symbol resolves to.
struct EmptyString(RawVec);
// SAFETY: it is never written; its null buffer is never read.
unsafe impl Sync for EmptyString {}
static EMPTY_STRING: EmptyString = EmptyString(RawVec::new(std::ptr::null_mut(), 0, 0));

/// `Interner.resolve(sym)`: the interned `String`, or an empty one for a
/// symbol out of range.
///
/// # Safety
/// `h` came from [`karac_mir_interner_new`].
#[no_mangle]
pub unsafe extern "C" fn karac_mir_interner_resolve(
    h: *const MirInterner,
    id: i64,
) -> *const RawVec {
    // SAFETY: the caller's contract.
    let t = unsafe { &*h };
    match usize::try_from(id).ok().and_then(|i| t.strings.get(i)) {
        Some(s) => &**s,
        None => &EMPTY_STRING.0,
    }
}

/// Frees an interner and its strings; nothing it lent is still read.
///
/// # Safety
/// `h` came from [`karac_mir_interner_new`] and is not used again.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_interner_free(h: *mut MirInterner) {
    // SAFETY: the caller's contract; the box came from `Box::into_raw`.
    let t = unsafe { Box::from_raw(h) };
    for s in &t.strings {
        if s.cap != 0 {
            // SAFETY: the buffer came from `string_of`'s allocator.
            unsafe { free(s.ptr.cast()) };
        }
    }
}

/// `Interner.len()`.
///
/// # Safety
/// `h` came from [`karac_mir_interner_new`].
#[no_mangle]
pub unsafe extern "C" fn karac_mir_interner_len(h: *const MirInterner) -> i64 {
    // SAFETY: the caller's contract.
    unsafe { (*h).strings.len() as i64 }
}

/// `f16` and `bf16` are held as their bits and computed through `f32`;
/// these round an `f32` to the narrow type, as the interpreter's
/// `FloatTy::round` (`src/mir/ty.rs`), whose `f16` code this copies.
#[no_mangle]
pub extern "C" fn karac_mir_f16_from(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xFF) as i32;
    let frac = b & 0x007F_FFFF;
    if exp == 0xFF {
        return if frac != 0 {
            sign | 0x7E00 | ((frac >> 13) as u16)
        } else {
            sign | 0x7C00
        };
    }
    if exp == 0 && frac == 0 {
        return sign;
    }
    let e = exp - 127;
    if e >= 16 {
        return sign | 0x7C00;
    }
    if e >= -14 {
        let mant = 0x0080_0000 | frac;
        let base = mant >> 13;
        let rem = mant & 0x1FFF;
        let mut r = base;
        if rem > 0x1000 || (rem == 0x1000 && (base & 1) == 1) {
            r += 1;
        }
        let mut ee = (e + 15) as u32;
        let mut mm = r & 0x3FF;
        if r == 0x800 {
            ee += 1;
            mm = 0;
        }
        if ee >= 31 {
            return sign | 0x7C00;
        }
        return sign | ((ee as u16) << 10) | (mm as u16);
    }
    if e < -25 {
        return sign;
    }
    let mant = 0x0080_0000u32 | frac;
    let shift = (13 + (-14 - e)) as u32;
    let base = mant >> shift;
    let rem = mant & ((1u32 << shift) - 1);
    let half = 1u32 << (shift - 1);
    let mut r = base as u16;
    if rem > half || (rem == half && (r & 1) == 1) {
        r += 1;
    }
    sign | r
}

/// The `f32` an `f16`'s bits stand for (exact).
#[no_mangle]
pub extern "C" fn karac_mir_f16_to(h: u16) -> f32 {
    let neg = h & 0x8000 != 0;
    let exp = ((h >> 10) & 0x1F) as i32;
    let mant = f64::from(h & 0x3FF);
    let v = if exp == 31 {
        if mant != 0.0 {
            f64::NAN
        } else {
            f64::INFINITY
        }
    } else if exp == 0 {
        mant * (2.0f64).powi(-24)
    } else {
        (1.0 + mant / 1024.0) * (2.0f64).powi(exp - 15)
    };
    (if neg { -v } else { v }) as f32
}

/// An `f32` rounded to `bf16` bits: to nearest even, a NaN kept quiet.
#[no_mangle]
pub extern "C" fn karac_mir_bf16_from(f: f32) -> u16 {
    let bits = f.to_bits();
    if f.is_nan() {
        ((bits | 0x0040_0000) >> 16) as u16
    } else {
        let lsb = (bits >> 16) & 1;
        (bits.wrapping_add(0x7FFF + lsb) >> 16) as u16
    }
}

/// A channel of the MIR backend, as the interpreter's `Chan`: a queue of
/// `size`-byte values, with a capacity (0 is unbounded for a plain
/// channel, holds nothing for a `BoundedChannel`) and the count of live
/// ends of each kind. A `recv` never blocks: it finds a value or none,
/// as the interpreter's does. Queued values drop through `drop` once both
/// kinds of end are gone.
pub struct MirChan {
    queue: std::sync::Mutex<MirChanState>,
    size: usize,
    cap: usize,
    bounded: bool,
    drop: Option<unsafe extern "C" fn(*mut u8)>,
}

struct MirChanState {
    queue: std::collections::VecDeque<Box<[u8]>>,
    senders: usize,
    receivers: usize,
}

/// A new channel with one end of each kind.
#[no_mangle]
pub extern "C" fn karac_mir_chan_new(
    size: u64,
    cap: i64,
    bounded: u8,
    drop: Option<unsafe extern "C" fn(*mut u8)>,
) -> *mut MirChan {
    let size = size as usize;
    Box::into_raw(Box::new(MirChan {
        queue: std::sync::Mutex::new(MirChanState {
            queue: std::collections::VecDeque::new(),
            senders: 1,
            receivers: 1,
        }),
        size,
        cap: cap.max(0) as usize,
        bounded: bounded != 0,
        drop,
    }))
}

thread_local! {
    /// Set by a ready answer of `karac_mir_chan_ready` in a task and taken
    /// by the channel call it guards, which may then answer "not ready" (3
    /// from a send, 2 from a recv) having done nothing: another worker of
    /// the pool filled or drained the channel in between, and the
    /// readiness loop goes back to its `__ready` (`mir_task.rs`, "Armed
    /// calls"; B-2026-10-31-34).
    static CHAN_ARMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Queues the `size` bytes at `v`: 0 when queued, 1 when the channel is
/// full, 2 when no receiver is left and 3 when an armed send found it full
/// (`v` is then left to the caller). A null `h` is a `BoundedChannel {
/// handle_id: 0 }` written by hand, not made by `new`: it fails closed,
/// refusing as full (bounded_channel.kara; B-2026-11-09-78).
///
/// # Safety
/// `h` is null or a live channel; `v` is readable for its `size` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_chan_send(h: *mut MirChan, v: *const u8) -> i32 {
    if h.is_null() {
        CHAN_ARMED.with(|a| a.set(false));
        return 1;
    }
    // SAFETY: the caller's contract.
    let c = unsafe { &*h };
    let armed = CHAN_ARMED.with(|a| a.replace(false));
    let mut s = c.queue.lock().unwrap_or_else(|p| p.into_inner());
    if !c.bounded && s.receivers == 0 {
        return 2;
    }
    if (c.bounded || c.cap > 0) && s.queue.len() >= c.cap {
        if armed && !c.bounded {
            crate::mir_task::set_again();
            return 3;
        }
        return 1;
    }
    // SAFETY: as above.
    s.queue.push_back(unsafe { bytes(v, c.size) }.into());
    crate::mir_task::chan_changed();
    0
}

/// Whether a `recv` (`send` 0) or a `send` on `h` can complete now: a
/// value is queued or no sender is left, or there is room or no receiver
/// is left (a `BoundedChannel` refuses at once when full, so its send is
/// always ready). A null `h` (a hand-rolled `BoundedChannel`) is ready,
/// its call answering at once.
///
/// # Safety
/// `h` is null or a live channel.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_chan_ready(h: *mut MirChan, send: u8) -> u8 {
    if h.is_null() {
        return 1;
    }
    // SAFETY: the caller's contract.
    let c = unsafe { &*h };
    let s = c.queue.lock().unwrap_or_else(|p| p.into_inner());
    let ready = if send == 0 {
        !s.queue.is_empty() || s.senders == 0
    } else {
        c.bounded || c.cap == 0 || s.queue.len() < c.cap || s.receivers == 0
    };
    drop(s);
    if ready && crate::mir_task::in_task() {
        CHAN_ARMED.with(|a| a.set(true));
    }
    u8::from(ready)
}

/// Moves the oldest value to `out`: 1 when there was one, else 0, or 2
/// when an armed recv found the channel empty with a sender left (`out`
/// is then untouched). A null `h` (a hand-rolled `BoundedChannel`) holds
/// nothing.
///
/// # Safety
/// `h` is null or a live channel; `out` is writable for its `size` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_chan_recv(h: *mut MirChan, out: *mut u8) -> u8 {
    if h.is_null() {
        CHAN_ARMED.with(|a| a.set(false));
        return 0;
    }
    // SAFETY: the caller's contract.
    let c = unsafe { &*h };
    let armed = CHAN_ARMED.with(|a| a.replace(false));
    let mut s = c.queue.lock().unwrap_or_else(|p| p.into_inner());
    match s.queue.pop_front() {
        Some(b) => {
            // SAFETY: as above.
            unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), out, c.size) };
            crate::mir_task::chan_changed();
            1
        }
        None if armed && s.senders > 0 => {
            crate::mir_task::set_again();
            2
        }
        None => 0,
    }
}

/// Whether every sender of `h` is gone, for a `select`'s receive arm; a
/// null `h` has none.
///
/// # Safety
/// `h` is null or a live channel.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_chan_closed(h: *mut MirChan) -> u8 {
    if h.is_null() {
        return 1;
    }
    // SAFETY: the caller's contract.
    let c = unsafe { &*h };
    let s = c.queue.lock().unwrap_or_else(|p| p.into_inner());
    u8::from(s.senders == 0)
}

/// Another end of the kind `sender` names; a null `h` stays null.
///
/// # Safety
/// `h` is null or a live channel.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_chan_clone(h: *mut MirChan, sender: u8) -> *mut MirChan {
    if h.is_null() {
        return h;
    }
    // SAFETY: the caller's contract.
    let c = unsafe { &*h };
    let mut s = c.queue.lock().unwrap_or_else(|p| p.into_inner());
    if sender != 0 {
        s.senders += 1;
    } else {
        s.receivers += 1;
    }
    h
}

/// One end of the kind `sender` names is gone; with every end gone, the
/// queued values drop and the channel is freed.
///
/// # Safety
/// `h` is null or a live channel, and the end is not used again.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_chan_end(h: *mut MirChan, sender: u8) {
    if h.is_null() {
        return;
    }
    crate::mir_task::chan_changed();
    let last = {
        // SAFETY: the caller's contract.
        let c = unsafe { &*h };
        let mut s = c.queue.lock().unwrap_or_else(|p| p.into_inner());
        let n = if sender != 0 {
            &mut s.senders
        } else {
            &mut s.receivers
        };
        *n = n.saturating_sub(1);
        s.senders == 0 && s.receivers == 0
    };
    if !last {
        return;
    }
    // SAFETY: the last end; the channel came from `Box::into_raw`.
    let c = unsafe { Box::from_raw(h) };
    let s = c.queue.into_inner().unwrap_or_else(|p| p.into_inner());
    if let Some(d) = c.drop {
        for mut b in s.queue {
            // SAFETY: the bytes are a value the glue drops.
            unsafe { d(b.as_mut_ptr()) };
        }
    }
}

/// A dropped `Sender`.
///
/// # Safety
/// As [`karac_mir_chan_end`].
#[no_mangle]
pub unsafe extern "C" fn karac_mir_chan_drop_sender(h: *mut MirChan) {
    // SAFETY: the caller's contract.
    unsafe { karac_mir_chan_end(h, 1) }
}

/// A dropped `Receiver`.
///
/// # Safety
/// As [`karac_mir_chan_end`].
#[no_mangle]
pub unsafe extern "C" fn karac_mir_chan_drop_receiver(h: *mut MirChan) {
    // SAFETY: the caller's contract.
    unsafe { karac_mir_chan_end(h, 0) }
}

/// Writes the chars of `s[..n]` to `out` (room for `n` chars) and returns
/// how many there are.
///
/// # Safety
/// `s` is readable for `n` bytes of UTF-8; `out` is writable for `n`
/// `u32`s.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_decode(out: *mut u32, s: *const u8, n: u64) -> u64 {
    // SAFETY: the caller's contract.
    let text = unsafe { std::str::from_utf8_unchecked(bytes(s, n as usize)) };
    let mut k = 0;
    for c in text.chars() {
        // SAFETY: a string of `n` bytes has at most `n` chars.
        unsafe { out.add(k).write(u32::from(c)) };
        k += 1;
    }
    k as u64
}

/// The chars `c[..k]` as a new `String` at `out`.
///
/// # Safety
/// `c` holds `k` valid chars; `out` is writable for a `String`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_string_from_chars(out: *mut RawVec, c: *const u32, k: u64) {
    let k = k as usize;
    let mut text = String::with_capacity(k);
    for i in 0..k {
        // SAFETY: the caller's contract.
        let v = unsafe { c.add(i).read() };
        text.push(char::from_u32(v).unwrap_or(char::REPLACEMENT_CHARACTER));
    }
    // SAFETY: the caller's contract.
    unsafe { string_of(out, text.as_bytes()) };
}

/// An erased function value of the MIR backend: `{code, env, drop}`.
type MirFnBytes = [usize; 3];

/// Drops an erased function value: its captures through their glue, then
/// their block (a null environment owns nothing).
///
/// # Safety
/// `f` is a live erased function value, not used again.
unsafe fn drop_mir_fn(f: &MirFnBytes) {
    let env = f[1] as *mut u8;
    if env.is_null() {
        return;
    }
    if f[2] != 0 {
        // SAFETY: `drop` is the glue of the captures at `env`.
        let d: unsafe extern "C" fn(*mut u8) = unsafe { std::mem::transmute(f[2]) };
        unsafe { d(env) };
    }
    // SAFETY: the captures' block came from `malloc`.
    unsafe { free(env.cast()) };
}

/// A `Pool[T]` of the MIR backend, as the interpreter's `PoolEntry`: the
/// factory and health check (erased function values), the idle values
/// (`size` bytes each), the count minted and not evicted, its bound and the
/// checked-out connection ids. Every `Pool` value and checked-out
/// connection holds a reference; the last one gone drops the idle values
/// and the functions and frees it.
pub struct MirPool {
    refs: usize,
    size: usize,
    drop: Option<unsafe extern "C" fn(*mut u8)>,
    create: MirFnBytes,
    check: Option<MirFnBytes>,
    slots: Vec<Box<[u8]>>,
    active: i64,
    max: i64,
    out: std::collections::HashSet<i64>,
    next_conn: i64,
    /// `close` was called: nothing is lent or kept any more.
    closed: bool,
    /// How many `acquire` calls may wait at once, and how many do
    /// (B-2026-10-21-44).
    max_waiters: i64,
    waiting: i64,
}

/// Every pool's state changes under this lock: a `Pool` crosses tasks, and
/// under the pool's workers two tasks can reach one at once. Drop glue and
/// the executor run outside it.
static POOL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// `f` of the pool at `h`, under [`POOL_LOCK`].
///
/// # Safety
/// `h` is a live pool.
unsafe fn with_pool<T>(h: *mut MirPool, f: impl FnOnce(&mut MirPool) -> T) -> T {
    let _held = POOL_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: the caller's contract; the lock keeps every other worker's
    // reference out.
    f(unsafe { &mut *h })
}

impl MirPool {
    /// Whether `acquire` can complete now: an idle value, room for a new
    /// one, or a closed pool.
    fn free(&self) -> bool {
        self.closed || !self.slots.is_empty() || self.active < self.max
    }
}

thread_local! {
    /// What the waiting `acquire` that `karac_mir_ready_pool` let through
    /// comes to without a value: 1 `Busy`, 0 `Timeout`.
    static POOL_VERDICT: std::cell::Cell<Option<u8>> = const { std::cell::Cell::new(None) };
}

/// `__ready` of a `Pool.acquire` with a timeout of `ns` nanoseconds: ready
/// when the pool has a value to lend. Otherwise the task joins the pool's
/// waiters and waits by the deadline, or is turned away at once when
/// `max_waiters` already wait; [`karac_mir_pool_full`] then reads which.
///
/// # Safety
/// `h` is a live pool or null.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_ready_pool(h: *mut MirPool, ns: i64) -> u8 {
    let Some(waiting) = crate::mir_task::deadline_set() else {
        return 1;
    };
    if h.is_null() {
        return 1;
    }
    // SAFETY: the caller's contract.
    let (free, turned) = unsafe {
        with_pool(h, |p| {
            let free = p.free();
            if !free && !waiting {
                if p.waiting >= p.max_waiters {
                    return (free, true);
                }
                p.waiting += 1;
            }
            (free, false)
        })
    };
    if turned {
        POOL_VERDICT.with(|v| v.set(Some(1)));
        return crate::mir_task::ready_by(true, ns).0;
    }
    let (ready, timed_out) = crate::mir_task::ready_by(free, ns);
    if ready != 0 && (waiting || !free) {
        // SAFETY: as above.
        unsafe { with_pool(h, |p| p.waiting -= 1) };
    }
    if timed_out {
        POOL_VERDICT.with(|v| v.set(Some(0)));
    }
    ready
}

/// `acquire` on a full pool with a timeout of `ns` nanoseconds: 1 for
/// `Busy`, 0 for `Timeout`. After a wait, what it came to; outside one,
/// nothing can give a value back, so the call waits its whole timeout.
///
/// # Safety
/// `h` is a live pool.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_full(h: *mut MirPool, ns: i64) -> u8 {
    if let Some(v) = POOL_VERDICT.with(|v| v.take()) {
        return v;
    }
    // In a task with no verdict, `__ready` saw a value or room that
    // another worker took before this call: the readiness loop asks again
    // rather than this worker sleep the timeout out (B-2026-11-11-47).
    if crate::mir_task::in_task() {
        crate::mir_task::set_again();
        return 0;
    }
    // SAFETY: the caller's contract.
    if unsafe { with_pool(h, |p| p.waiting >= p.max_waiters) } {
        return 1;
    }
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::sleep(std::time::Duration::from_nanos(ns.max(0) as u64));
    #[cfg(target_arch = "wasm32")]
    let _ = ns;
    0
}

/// A new pool of `size`-byte values minted by the erased function at
/// `create` (moved in), at most `max` at once, with at most `max_waiters`
/// callers waiting in `acquire`.
///
/// # Safety
/// `create` is a live erased function value, which the pool now owns.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_new(
    size: u64,
    max: i64,
    max_waiters: i64,
    create: *const MirFnBytes,
    drop: Option<unsafe extern "C" fn(*mut u8)>,
) -> *mut MirPool {
    let size = size as usize;
    Box::into_raw(Box::new(MirPool {
        refs: 1,
        size,
        drop,
        // SAFETY: the caller's contract.
        create: unsafe { create.read_unaligned() },
        check: None,
        slots: Vec::new(),
        active: 0,
        max,
        out: Default::default(),
        next_conn: 1,
        closed: false,
        max_waiters,
        waiting: 0,
    }))
}

/// `pool.close()` (B-2026-10-21-45): the idle values drop now, and from here
/// on [`karac_mir_pool_closed`] is 1 and [`karac_mir_pool_return`] keeps
/// nothing.
///
/// # Safety
/// `h` is a live pool.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_close(h: *mut MirPool) {
    // SAFETY: the caller's contract.
    let (slots, drop) = unsafe {
        with_pool(h, |p| {
            p.closed = true;
            (std::mem::take(&mut p.slots), p.drop)
        })
    };
    if let Some(d) = drop {
        for mut b in slots {
            // SAFETY: the bytes are a value the glue drops.
            unsafe { d(b.as_mut_ptr()) };
        }
    }
    // A waiting `acquire` now fails `PoolClosed`.
    crate::mir_task::peer_progressed();
}

/// 1 when `close` was called on pool `h`, else 0.
///
/// # Safety
/// `h` is a live pool.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_closed(h: *mut MirPool) -> u8 {
    // SAFETY: the caller's contract.
    u8::from(unsafe { with_pool(h, |p| p.closed) })
}

/// Another `Pool` value for `h`; `check` (when not null, moved in)
/// replaces its health check.
///
/// # Safety
/// `h` is a live pool; `check` is null or a live erased function value.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_share(
    h: *mut MirPool,
    check: *const MirFnBytes,
) -> *mut MirPool {
    // SAFETY: the caller's contract.
    let old = unsafe {
        with_pool(h, |p| {
            p.refs += 1;
            // SAFETY: as above.
            (!check.is_null()).then(|| p.check.replace(check.read_unaligned()))
        })
    };
    if let Some(Some(old)) = old {
        // SAFETY: the pool owned the replaced check.
        unsafe { drop_mir_fn(&old) };
    }
    h
}

/// Where the pool's factory (`which` 0) or health check (1, null when
/// none) is held.
///
/// # Safety
/// `h` is a live pool.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_fn(h: *mut MirPool, which: u8) -> *const MirFnBytes {
    // SAFETY: the caller's contract.
    unsafe {
        with_pool(h, |p| match (which, &p.check) {
            (0, _) => &p.create as *const MirFnBytes,
            (_, Some(c)) => c,
            _ => std::ptr::null(),
        })
    }
}

/// Moves the newest idle value to `out`: 1 when there was one, else 0.
///
/// # Safety
/// `h` is a live pool; `out` is writable for its value size.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_pop(h: *mut MirPool, out: *mut u8) -> u8 {
    // SAFETY: the caller's contract.
    match unsafe { with_pool(h, |p| p.slots.pop().map(|b| (b, p.size))) } {
        Some((b, size)) => {
            // SAFETY: as above.
            unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), out, size) };
            1
        }
        None => 0,
    }
}

/// Counts the pool's live values: `delta` -1 for an evicted one or one
/// whose factory failed; 0 answers whether another may be minted (1) or
/// not (0), and when it may, counts it at once, so two workers minting
/// together cannot pass `max` (B-2026-11-11-47).
///
/// # Safety
/// `h` is a live pool.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_count(h: *mut MirPool, delta: i64) -> u8 {
    // SAFETY: the caller's contract.
    let r = unsafe {
        with_pool(h, |p| {
            if delta == 0 {
                let room = p.active < p.max;
                p.active += i64::from(room);
                return u8::from(room);
            }
            p.active = (p.active + delta).max(0);
            1
        })
    };
    if delta < 0 {
        // Room for a waiting `acquire`.
        crate::mir_task::peer_progressed();
    }
    r
}

/// A new connection id, recorded as checked out; the connection holds a
/// reference on the pool.
///
/// # Safety
/// `h` is a live pool.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_check_out(h: *mut MirPool) -> i64 {
    // SAFETY: the caller's contract.
    unsafe {
        with_pool(h, |p| {
            let c = p.next_conn;
            p.next_conn += 1;
            p.out.insert(c);
            p.refs += 1;
            c
        })
    }
}

/// Hands back the value at `val` of connection `conn` from pool `h`, to
/// the pool `into` (null for `h`): 1 when the pool took it (it was
/// checked out there), else 0 and the caller drops it. The connection's
/// reference on `h` is gone either way.
///
/// # Safety
/// `h` is a live pool the connection holds a reference on; `val` is
/// readable for its value size.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_return(
    h: *mut MirPool,
    into: *mut MirPool,
    conn: i64,
    val: *const u8,
) -> u8 {
    // SAFETY: the caller's contract.
    let took = unsafe {
        with_pool(h, |p| {
            if p.out.remove(&conn) && (into.is_null() || into == h) && !p.closed {
                // SAFETY: as above.
                p.slots.push(bytes(val, p.size).into());
                1
            } else {
                0
            }
        })
    };
    // SAFETY: the connection's reference.
    unsafe { karac_mir_pool_drop(h) };
    // A value, or room, for a waiting `acquire`: the executor polls it
    // again rather than nap to its deadline (B-2026-11-11-47).
    crate::mir_task::peer_progressed();
    took
}

/// A `Pool` value (or a connection's reference) is gone; with the last
/// one, the idle values and the functions drop and the pool is freed.
///
/// # Safety
/// `h` is null or a live pool, and this reference is not used again.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_drop(h: *mut MirPool) {
    if h.is_null() {
        return;
    }
    // SAFETY: the caller's contract.
    let left = unsafe {
        with_pool(h, |p| {
            p.refs -= 1;
            p.refs
        })
    };
    if left > 0 {
        return;
    }
    // SAFETY: the last reference; the pool came from `Box::into_raw`.
    let p = unsafe { Box::from_raw(h) };
    if let Some(d) = p.drop {
        for mut b in p.slots {
            // SAFETY: the bytes are a value the glue drops.
            unsafe { d(b.as_mut_ptr()) };
        }
    }
    // SAFETY: the pool owned both functions.
    unsafe { drop_mir_fn(&p.create) };
    if let Some(c) = &p.check {
        unsafe { drop_mir_fn(c) };
    }
}

#[cfg(test)]
mod tests {
    /// A null channel, a `BoundedChannel { handle_id: 0 }` written by
    /// hand, fails closed: a send is refused as full, a recv finds nothing,
    /// and its end, clone and readiness touch nothing (B-2026-11-09-78).
    #[test]
    fn null_channel_fails_closed() {
        let h: *mut super::MirChan = std::ptr::null_mut();
        let v = 7i64;
        let mut out = 0i64;
        // SAFETY: each call accepts a null channel.
        unsafe {
            assert_eq!(super::karac_mir_chan_send(h, (&v as *const i64).cast()), 1);
            assert_eq!(
                super::karac_mir_chan_recv(h, (&mut out as *mut i64).cast()),
                0
            );
            assert_eq!(super::karac_mir_chan_ready(h, 0), 1);
            assert_eq!(super::karac_mir_chan_closed(h), 1);
            assert!(super::karac_mir_chan_clone(h, 1).is_null());
            super::karac_mir_chan_end(h, 1);
            super::karac_mir_chan_end(h, 0);
        }
        assert_eq!(out, 0);
    }

    /// Random pushes and pops at both ends agree with `std`'s `VecDeque`,
    /// with 1-, 8- and 24-byte elements, so the ring wraps and grows from
    /// every head position (the growth copies the two runs of a wrapped
    /// ring in bulk).
    #[test]
    fn deque_agrees_with_std_vecdeque() {
        use std::collections::VecDeque;
        let mut seed = 958u64;
        let mut next = |k: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % k
        };
        for size in [1usize, 8, 24] {
            for _ in 0..200 {
                let mut q = super::RawDeque::new(std::ptr::null_mut(), 0, 0, 0);
                let mut want: VecDeque<u8> = VecDeque::new();
                let mut tag = 0u8;
                for _ in 0..next(300) {
                    let front = next(2) as u8;
                    if next(3) == 0 {
                        let mut out = vec![0u8; size];
                        // SAFETY: `q` is a live deque of `size`-byte elements.
                        let got = unsafe {
                            super::karac_mir_deque_pop(&mut q, out.as_mut_ptr(), size as u64, front)
                        };
                        let expect = if front != 0 {
                            want.pop_front()
                        } else {
                            want.pop_back()
                        };
                        assert_eq!(got != 0, expect.is_some());
                        if let Some(t) = expect {
                            assert!(out.iter().all(|&b| b == t), "size {size}");
                        }
                    } else {
                        tag = tag.wrapping_add(1);
                        let elem = vec![tag; size];
                        // SAFETY: as above; `elem` holds `size` bytes.
                        unsafe {
                            super::karac_mir_deque_push(&mut q, elem.as_ptr(), size as u64, front)
                        };
                        if front != 0 {
                            want.push_front(tag)
                        } else {
                            want.push_back(tag)
                        }
                    }
                    assert_eq!(q.len, want.len());
                    for (i, &t) in want.iter().enumerate() {
                        // SAFETY: element `i` is live.
                        let at = unsafe { super::karac_mir_deque_slot(&q, i as u64, size as u64) };
                        // SAFETY: the slot holds `size` bytes.
                        let bytes = unsafe { std::slice::from_raw_parts(at, size) };
                        assert!(bytes.iter().all(|&b| b == t), "size {size}, element {i}");
                    }
                }
                if q.cap != 0 {
                    // SAFETY: the buffer came from `karac_realloc_or_panic`.
                    unsafe { super::free(q.ptr.cast()) };
                }
            }
        }
    }

    /// `find_short` (B-2026-10-29-32) agrees with a naive search, on
    /// small alphabets, on all 256 bytes (where the pair hashes collide)
    /// and across the 256-byte end of its range.
    #[test]
    fn find_short_agrees_with_a_naive_search() {
        let mut seed = 1016u64;
        let mut next = |k: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % k
        };
        for round in 0..20000 {
            // Small alphabets make every pair occur; all 256 bytes make
            // hashes collide.
            let letters = [1, 2, 3, 4, 256][round % 5];
            let nn = 2 + next(if round % 7 == 0 { 300 } else { 18 }) as usize;
            let hn = next(nn as u64 * 4 + 40) as usize;
            let mut word =
                |len: usize| -> Vec<u8> { (0..len).map(|_| next(letters) as u8).collect() };
            let h = word(hn);
            let mut n = word(nn);
            if hn >= nn && next(2) == 0 {
                let at = next((hn - nn + 1) as u64) as usize;
                n = h[at..at + nn].to_vec();
            }
            let naive = hn >= nn && h.windows(nn).any(|w| w == &n[..]);
            let want = naive && nn <= 256;
            assert_eq!(super::find_short(&h, &n), want, "{h:?} {n:?}");
        }
    }

    /// The memchr scan agrees with `str::contains`, on random text over
    /// small alphabets and on the needles that exhaust its budget and hand
    /// over to the two-way search (`aaa..ab` in `aaa..a`).
    #[test]
    fn contains_agrees_with_str_contains() {
        let mut seed = 7u64;
        let mut next = |k: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % k
        };
        for _ in 0..4000 {
            let letters = 1 + next(3);
            let hn = next(120) as usize;
            let nn = next(12) as usize;
            let word = |len: usize, next: &mut dyn FnMut(u64) -> u64| -> String {
                (0..len)
                    .map(|_| (b'a' + next(letters) as u8) as char)
                    .collect()
            };
            let h = word(hn, &mut next);
            let n = word(nn, &mut next);
            assert_eq!(super::contains(&h, &n), h.contains(&n), "{h:?} {n:?}");
            if hn > nn && nn > 0 {
                let at = next((hn - nn) as u64) as usize;
                let n = &h[at..at + nn];
                assert!(super::contains(&h, n), "{h:?} {n:?}");
            }
        }
        let hay = "a".repeat(5000);
        for tail in ["b", "ab", "ba"] {
            let needle = format!("{}{tail}", "a".repeat(300));
            assert!(!super::contains(&hay, &needle));
            assert!(
                super::contains(&format!("{hay}{tail}"), &needle)
                    == format!("{hay}{tail}").contains(&needle)
            );
        }
        assert!(super::contains("héllo wörld", "ö"));
        assert!(super::contains("x", ""));
        assert!(!super::contains("", "x"));
    }

    use super::{
        karac_mir_fmt_begin, karac_mir_fmt_char, karac_mir_fmt_end_string, karac_mir_fmt_i64,
        karac_mir_fmt_str, karac_mir_fmt_u64, karac_mir_parse_int, karac_mir_str_repeat,
        merge_sort, radix_digits, signed_of, RawVec,
    };

    /// `karac_mir_parse_int` on `text`, as an i64 of `bits` bits.
    fn parse(text: &[u8], radix: i64, bits: u32, signed: bool) -> Option<i64> {
        let mut out = 0i64;
        // SAFETY: `text` is readable and `out` writable.
        let ok = unsafe {
            karac_mir_parse_int(
                text.as_ptr(),
                text.len() as u64,
                radix,
                bits,
                signed as u8,
                &mut out,
            )
        };
        (ok != 0).then_some(out)
    }

    // The in-place radix-10 path gives the verdict the i128 path gave
    // (B-2026-10-13-34): signs, trimming, junk, 18 and 19 digits, bounds.
    #[test]
    fn parse_int_short_decimal_agrees_with_from_str_radix() {
        let cases: &[&[u8]] = &[
            b"0",
            b"12",
            b"-12",
            b"+7",
            b" 12",
            b"12\n",
            b"",
            b"-",
            b"+",
            b"1_0",
            b"12a",
            b"--1",
            b"+-1",
            b"999999999999999999",
            b"-999999999999999999",
            b"9223372036854775807",
            b"-9223372036854775808",
            b"9223372036854775808",
            b"127",
            b"128",
            b"-128",
            b"-129",
            b"255",
            b"256",
            b"-0",
            b"\xff1",
            b"\xe0\x80",
            b" -0 ",
            b"\x0b12",
            b"12\x0c",
            b"\t+5\r\n",
            b"\xc2\xa012",
            b"1 2",
        ];
        for &t in cases {
            for (bits, signed) in [(64, true), (8, true), (8, false), (64, false)] {
                let want = std::str::from_utf8(t).ok().and_then(|s| {
                    let s = s.trim();
                    if !signed && s.starts_with('-') {
                        return None;
                    }
                    let n = s.parse::<i128>().ok()?;
                    let fits = if signed {
                        let half = 1i128 << (bits - 1);
                        (-half..half).contains(&n)
                    } else {
                        n >= 0 && n < (1i128 << bits)
                    };
                    fits.then_some(n as i64)
                });
                assert_eq!(parse(t, 10, bits, signed), want, "{:?} {bits} {signed}", t);
            }
        }
        assert_eq!(parse(b"ff", 16, 64, true), Some(255));
        assert_eq!(parse(b"12", 1, 64, true), None);
    }

    fn end_string() -> String {
        let mut out = RawVec::new(std::ptr::null_mut(), 0, 0);
        // SAFETY: `out` is a writable `String`.
        unsafe { karac_mir_fmt_end_string(&mut out) };
        if out.ptr.is_null() {
            return String::new();
        }
        // SAFETY: the runtime wrote `len` bytes at `ptr`, from `malloc`.
        let s = unsafe { std::slice::from_raw_parts(out.ptr, out.len) }.to_vec();
        // SAFETY: as above.
        unsafe { free(out.ptr.cast()) };
        String::from_utf8(s).unwrap()
    }

    extern "C" {
        fn free(ptr: *mut core::ffi::c_void);
    }

    /// Formats nest on the one buffer: an inner format ends with its own
    /// text and leaves the outer one's in place; integers print at their
    /// extremes.
    #[test]
    fn formats_nest_and_print_integers() {
        let s = |t: &str| unsafe { karac_mir_fmt_str(t.as_ptr(), t.len() as u64) };
        karac_mir_fmt_begin();
        s("a");
        karac_mir_fmt_i64(i64::MIN);
        karac_mir_fmt_begin();
        s("in");
        karac_mir_fmt_u64(u64::MAX);
        assert_eq!(end_string(), "in18446744073709551615");
        karac_mir_fmt_i64(0);
        karac_mir_fmt_i64(-7);
        karac_mir_fmt_char('é' as u32);
        assert_eq!(end_string(), "a-92233720368547758080-7é");
        karac_mir_fmt_begin();
        assert_eq!(end_string(), "");
    }

    /// The written-out `from_str_radix` agrees with core's on signs, both
    /// cases of digit, digits past the radix, empty and sign-only text,
    /// and the edges of `i128` and `u128` (B-2026-11-04-31).
    #[test]
    fn radix_digits_agree_with_from_str_radix() {
        let texts = [
            "0",
            "7",
            "-7",
            "+7",
            "",
            "-",
            "+",
            "+-1",
            "-+1",
            "--1",
            "ff",
            "FF",
            "-fF",
            "z",
            "Z",
            "10",
            "12",
            "1_0",
            " 1",
            "1 ",
            "é",
            "00000000000000000000000000000000000000000001",
            "170141183460469231731687303715884105727",
            "170141183460469231731687303715884105728",
            "-170141183460469231731687303715884105728",
            "-170141183460469231731687303715884105729",
            "340282366920938463463374607431768211455",
            "340282366920938463463374607431768211456",
            "ffffffffffffffffffffffffffffffff",
            "100000000000000000000000000000000",
            "7fffffffffffffffffffffffffffffff",
            "-80000000000000000000000000000000",
            "-80000000000000000000000000000001",
        ];
        for radix in [2u32, 8, 10, 16, 35, 36] {
            for text in texts {
                let b = text.as_bytes();
                let signed = radix_digits(b, radix, true).and_then(|(n, m)| signed_of(n, m));
                assert_eq!(
                    signed,
                    i128::from_str_radix(text, radix).ok(),
                    "i128 {text:?} radix {radix}"
                );
                let unsigned = radix_digits(b, radix, false).map(|(n, m)| {
                    assert!(!n);
                    m
                });
                assert_eq!(
                    unsigned,
                    u128::from_str_radix(text, radix).ok(),
                    "u128 {text:?} radix {radix}"
                );
            }
        }
    }

    /// `karac_mir_str_repeat`'s doubling copy, against `str::repeat`.
    #[test]
    fn str_repeat_matches_std() {
        for s in ["", "a", "ab", "xyz", "é"] {
            for n in [-5i64, 0, 1, 2, 3, 7, 64, 1000] {
                let mut out = std::mem::MaybeUninit::<RawVec>::uninit();
                // SAFETY: `out` is writable; `s` readable for its length.
                let got = unsafe {
                    karac_mir_str_repeat(out.as_mut_ptr(), s.as_ptr(), s.len() as u64, n);
                    let v = out.assume_init();
                    let bytes = if v.len == 0 {
                        Vec::new()
                    } else {
                        std::slice::from_raw_parts(v.ptr, v.len).to_vec()
                    };
                    crate::alloc::karac_free(v.ptr);
                    bytes
                };
                assert_eq!(got, s.repeat(n.max(0) as usize).into_bytes(), "{s:?} x {n}");
            }
        }
    }

    /// The merge is stable and sorts, for a fixed element size and for
    /// one copied by length, against `slice::sort_by_key`.
    #[test]
    fn merge_sort_is_stable() {
        let mut rng = 99u64;
        for n in [
            0usize, 1, 2, 3, 4, 5, 7, 8, 9, 16, 17, 100, 1000, 1025, 3001,
        ] {
            // (key, original position): 16 bytes, and 12 with the key
            // as an i32 pair padded to an odd size.
            let mut v16: Vec<(i64, i64)> = (0..n as i64)
                .map(|i| {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    ((rng % 10) as i64, i)
                })
                .collect();
            let mut want = v16.clone();
            want.sort_by_key(|e| e.0);
            let mut key = |a: *const u8, b: *const u8| {
                // SAFETY: both point at a `(i64, i64)` element.
                let (x, y) = unsafe {
                    (
                        (a as *const i64).read_unaligned(),
                        (b as *const i64).read_unaligned(),
                    )
                };
                x.cmp(&y) as i32
            };
            // SAFETY: `v16` holds `n` elements of 16 bytes.
            unsafe { merge_sort(v16.as_mut_ptr().cast(), n, 16, &mut key) };
            assert_eq!(v16, want);
            let mut v12: Vec<[u8; 12]> = want
                .iter()
                .rev()
                .map(|&(k, i)| {
                    let mut b = [0u8; 12];
                    b[..8].copy_from_slice(&k.to_le_bytes());
                    b[8..].copy_from_slice(&(i as u32).to_le_bytes());
                    b
                })
                .collect();
            let mut w12 = v12.clone();
            w12.sort_by_key(|b| i64::from_le_bytes(b[..8].try_into().unwrap()));
            // SAFETY: `v12` holds `n` elements of 12 bytes.
            unsafe { merge_sort(v12.as_mut_ptr().cast(), n, 12, &mut key) };
            assert_eq!(v12, w12);
        }
    }

    /// With a comparator that is not an order, the two ends of a merge take
    /// the same element; the merge is then redone from the front, so every
    /// element is still there exactly once.
    #[test]
    fn merge_sort_keeps_a_permutation_for_any_comparator() {
        for n in [4usize, 5, 9, 16, 37, 100, 1025, 3000] {
            for kind in 0..3u64 {
                let mut v: Vec<u64> = (0..n as u64).collect();
                let mut calls = 0u64;
                let mut odd = |_: *const u8, _: *const u8| {
                    calls = calls.wrapping_mul(6364136223846793005).wrapping_add(1);
                    match kind {
                        0 => [1, -1][(calls & 1) as usize],
                        1 => ((calls >> 33) % 3) as i32 - 1,
                        _ => -1,
                    }
                };
                // SAFETY: `v` holds `n` elements of 8 bytes.
                unsafe { merge_sort(v.as_mut_ptr().cast(), n, 8, &mut odd) };
                v.sort_unstable();
                assert!(
                    v.iter().copied().eq(0..n as u64),
                    "n {n}, comparator {kind}"
                );
            }
        }
    }
}
