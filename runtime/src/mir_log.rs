//! `std.log`'s runtime (docs/library/log.md; B-2026-10-27-20): the backend
//! `log.init` installed, the lowest level kept, the lock held around a
//! backend's `emit`, and the running task's id. Codegen is
//! src/backend/mir/log.rs; the interpreter keeps the same state in
//! src/mir/interp.rs.
//!
//! The lock is the runtime's, not the backend's: a backend never sees two
//! records at once and a line is never torn. A record logged from inside a
//! backend's own `emit` goes to the default backend rather than waiting on
//! the lock its thread holds.

use std::cell::{Cell, UnsafeCell};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

/// `void(sink, call)`: hands the call's record to the backend at `sink`,
/// consuming the call.
type Emit = unsafe extern "C" fn(*mut u8, *mut u8);
/// `void(sink)`: drops the backend and frees its box.
type DropSink = unsafe extern "C" fn(*mut u8);
/// `void(call)`: the default backend, consuming the call.
type Default = unsafe extern "C" fn(*mut u8);

/// A kept call an auto-par task logged (B-2026-10-27-22), on its way to
/// the backend in statement order (`mir_task`'s `Hold`): a copy of the
/// call in its own allocation, and the default backend it was given.
pub(crate) struct Held {
    call: *mut u8,
    size: usize,
    default: Default,
}

impl Held {
    fn layout(size: usize) -> std::alloc::Layout {
        std::alloc::Layout::from_size_align(size.max(1), 16).expect("a log call's layout")
    }

    /// Hands the call to the backend, and frees its copy.
    pub(crate) fn release(self) {
        // SAFETY: `call` is the copy `karac_mir_log_dispatch` made,
        // consumed once here.
        unsafe {
            emit(self.call, self.default);
            std::alloc::dealloc(self.call, Self::layout(self.size));
        }
    }
}

// SAFETY: a held call is owned data, moved between workers with its task.
unsafe impl Send for Held {}

struct Installed {
    sink: *mut u8,
    emit: Emit,
    drop: DropSink,
}

/// The installed backend, read and written only under [`lock`].
struct Slot(UnsafeCell<Option<Installed>>);

// SAFETY: every access is under `lock`.
unsafe impl Sync for Slot {}

static BACKEND: Slot = Slot(UnsafeCell::new(None));
/// The lowest rank kept: debug 0, info 1, warn 2, error 3.
static MIN: AtomicI64 = AtomicI64::new(1);
/// The thread holding the lock (its [`me`]), or 0.
static OWNER: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static TOKEN: u8 = const { 0 };
    static HELD: Cell<bool> = const { Cell::new(false) };
}

/// A non-zero number no other live thread has.
fn me() -> usize {
    TOKEN.with(|t| t as *const u8 as usize)
}

/// Takes the lock; `false`, without taking it, when this thread holds it
/// already.
fn lock() -> bool {
    if HELD.with(Cell::get) {
        return false;
    }
    let me = me();
    while OWNER
        .compare_exchange_weak(0, me, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        std::thread::yield_now();
    }
    HELD.with(|h| h.set(true));
    true
}

fn unlock() {
    HELD.with(|h| h.set(false));
    OWNER.store(0, Ordering::Release);
}

/// Whether a record of `rank` is kept: 1 or 0.
#[no_mangle]
pub extern "C" fn karac_mir_log_enabled(rank: i64) -> i64 {
    i64::from(rank >= MIN.load(Ordering::Relaxed))
}

/// Installs the backend at `sink` and keeps records of `rank` and above,
/// dropping the backend before it. Called from inside a backend's `emit`,
/// it drops the new backend instead.
///
/// # Safety
/// `sink` is a box `emit` and `drop` take.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_log_install(
    sink: *mut u8,
    emit: Emit,
    drop: DropSink,
    rank: i64,
) {
    if !lock() {
        // SAFETY: the caller's contract.
        unsafe { drop(sink) };
        return;
    }
    // SAFETY: under the lock.
    let old = unsafe { (*BACKEND.0.get()).replace(Installed { sink, emit, drop }) };
    MIN.store(rank, Ordering::Relaxed);
    if let Some(old) = old {
        // SAFETY: the box `install` was given, now no longer installed.
        unsafe { (old.drop)(old.sink) };
    }
    unlock();
}

/// Hands the kept call at `call`, of `size` bytes, to the installed
/// backend under the lock, or to `default` when there is none. A call an
/// auto-par task made is copied, to reach the backend in statement order
/// (docs/library/log.md: auto-par is invisible to logging).
///
/// # Safety
/// `call` is a call `emit` and `default` take.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_log_dispatch(call: *mut u8, size: usize, default: Default) {
    if HELD.with(Cell::get) {
        // From inside a backend's `emit`.
        // SAFETY: the caller's contract.
        unsafe { default(call) };
        return;
    }
    if crate::mir_task::holding_logs() {
        // SAFETY: a fresh allocation of `size` bytes, and the call moved
        // into it; the caller gave the call up.
        let copy = unsafe {
            let copy = std::alloc::alloc(Held::layout(size));
            if copy.is_null() {
                std::alloc::handle_alloc_error(Held::layout(size));
            }
            std::ptr::copy_nonoverlapping(call, copy, size);
            copy
        };
        crate::mir_task::hold_log(Held {
            call: copy,
            size,
            default,
        });
        return;
    }
    // SAFETY: the caller's contract.
    unsafe { emit(call, default) };
}

/// Hands `call` to the installed backend under the lock, or to `default`
/// when there is none or this thread is inside a backend's `emit`.
///
/// # Safety
/// As [`karac_mir_log_dispatch`].
unsafe fn emit(call: *mut u8, default: Default) {
    if !lock() {
        // SAFETY: the caller's contract.
        unsafe { default(call) };
        return;
    }
    // SAFETY: under the lock.
    match unsafe { &*BACKEND.0.get() } {
        // SAFETY: the installed backend's own functions.
        Some(b) => unsafe { (b.emit)(b.sink, call) },
        // SAFETY: the caller's contract.
        None => unsafe { default(call) },
    }
    unlock();
}

/// The running task's id, or 0 outside every task (`main` included). An
/// auto-par task answers its spawner's.
#[no_mangle]
pub extern "C" fn karac_mir_log_task_id() -> i64 {
    crate::mir_task::log_task_id() as i64
}
