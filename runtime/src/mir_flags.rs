//! The run-time borrow flags of core semantics §6.2 for compiled MIR: a
//! `mut` field of a `shared` value whose type is neither `Copy` nor a handle
//! aggregate is flagged while a loan of it lives, so a conflicting access
//! through another handle to the same object panics. The borrow-flag pass
//! (`src/mir/flags.rs`) places the operations; this keeps the flags, by the
//! field's address.
//!
//! `shared` values never cross tasks, so a flag one task holds can never
//! meet another task's access: each task keeps its own [`Table`], which
//! the executor swaps into the polling thread while the task runs and back
//! out when its poll returns, as it does the task's span stack. Under the
//! pool a task can move between workers at a suspension and its table moves
//! with it, and no access takes a lock: one process-wide table behind the
//! pool's lock serialized every task that wrote a `mut` field of its own
//! `shared` values (kata 133's par lane ran 16x slower at four workers than
//! at one, B-2026-11-01-25). The flags are keyed by the field's address:
//! a recursion over `shared` data that holds a loan across its call holds
//! one flag per frame, and a list searched on every acquire made it
//! quadratic in the depth (B-2026-11-08-83). A check with none held
//! returns at once.
//!
//! A coroutine's resume body has no frame slots of its own to remember
//! what it took, so the `_in` entry points keep that here, by the
//! coroutine frame's address and the slot's index, across suspensions
//! (B-2026-10-20-14).

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// A field's address hashed by one multiply, its high half folded into
/// the low bits the table indexes by: addresses are aligned, so their own
/// low bits are mostly zero.
#[derive(Default)]
struct AddrHash(u64);

impl Hasher for AddrHash {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(self.0.rotate_left(8) ^ u64::from(b));
        }
    }
    fn write_u64(&mut self, n: u64) {
        let h = n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        self.0 = h ^ (h >> 32);
    }
    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }
}

type Flags = HashMap<usize, Flag, BuildHasherDefault<AddrHash>>;

/// One flagged field: how many readers hold it, or whether a writer does,
/// and where the borrow that took it was made: a `file:line:col` constant
/// the compiled program passes (null when the place is not the program's
/// own), which a conflict names (design.md §11, B-2026-11-04-81). Readers
/// that join a held flag keep the first one's place.
struct Flag {
    readers: u32,
    writer: bool,
    at: usize,
    at_len: u64,
}

/// A flag a coroutine frame holds: the frame, its slot, and what it took.
struct Hold {
    frame: usize,
    slot: u32,
    addr: usize,
    mutable: u8,
}

/// A task's flags and its coroutine frames' holds.
#[derive(Default)]
pub(crate) struct Table {
    flags: Flags,
    holds: Vec<Hold>,
}

thread_local! {
    /// The running task's table (outside every task, the thread's own).
    static TABLE: RefCell<Table> = const {
        RefCell::new(Table {
            flags: HashMap::with_hasher(BuildHasherDefault::new()),
            holds: Vec::new(),
        })
    };
}

/// Puts `t` in the running task's place and answers what was there: the
/// executor's swap around a task's poll.
pub(crate) fn swap_table(t: Table) -> Table {
    TABLE.with(|c| std::mem::replace(&mut *c.borrow_mut(), t))
}

fn flags<R>(f: impl FnOnce(&mut Flags) -> R) -> R {
    TABLE.with(|c| f(&mut c.borrow_mut().flags))
}

fn holds<R>(f: impl FnOnce(&mut Vec<Hold>) -> R) -> R {
    TABLE.with(|c| f(&mut c.borrow_mut().holds))
}

fn conflicts(f: &Flag, mutable: bool) -> bool {
    f.writer || (mutable && f.readers > 0)
}

/// Takes the flag at `addr`, mutably when `mutable` is non-zero, for the
/// borrow made at `at` (`at_len` bytes). Returns 1, taking nothing, when
/// the flag is held in a way that conflicts.
#[no_mangle]
pub extern "C" fn karac_mir_flag_acquire(
    addr: *const u8,
    mutable: u8,
    at: *const u8,
    at_len: u64,
) -> u8 {
    let addr = addr as usize;
    let mutable = mutable != 0;
    flags(|fl| {
        match fl.get_mut(&addr) {
            Some(f) if conflicts(f, mutable) => return 1,
            Some(f) => {
                if mutable {
                    f.writer = true;
                } else {
                    f.readers += 1;
                }
            }
            None => {
                fl.insert(
                    addr,
                    Flag {
                        readers: u32::from(!mutable),
                        writer: mutable,
                        at: at as usize,
                        at_len,
                    },
                );
            }
        }
        0
    })
}

/// Gives back a flag `karac_mir_flag_acquire` took at `addr` with the same
/// `mutable`.
#[no_mangle]
pub extern "C" fn karac_mir_flag_release(addr: *const u8, mutable: u8) {
    let addr = addr as usize;
    flags(|fl| {
        let Some(f) = fl.get_mut(&addr) else {
            return;
        };
        if mutable != 0 {
            f.writer = false;
        } else {
            f.readers = f.readers.saturating_sub(1);
        }
        if f.readers == 0 && !f.writer {
            fl.remove(&addr);
        }
    })
}

/// An access that takes no borrow: returns 1 when a write or drop
/// (`mutable` non-zero) meets a held flag, or a read meets a writer's.
#[no_mangle]
pub extern "C" fn karac_mir_flag_check(addr: *const u8, mutable: u8) -> u8 {
    let addr = addr as usize;
    flags(|fl| {
        if fl.is_empty() {
            return 0;
        }
        u8::from(fl.get(&addr).is_some_and(|f| conflicts(f, mutable != 0)))
    })
}

/// Sets the message of the abort that follows a conflict at `addr`: `head`
/// (`head_len` bytes, which names the field), the place of the borrow
/// holding the flag when the program passed one, and the rule (design.md
/// §11: the panic names the field and both accesses; the abort's own place
/// is the second).
///
/// # Safety
/// `head` points at `head_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_flag_conflict(addr: *const u8, head: *const u8, head_len: u64) {
    let addr = addr as usize;
    let (at, at_len) = flags(|fl| fl.get(&addr).map_or((0, 0), |f| (f.at, f.at_len)));
    let mut msg = crate::lean_vec::LeanVec::<u8>::new();
    // SAFETY: the caller's contract.
    msg.extend_from_slice(unsafe { std::slice::from_raw_parts(head, head_len as usize) });
    if at != 0 && at_len > 0 {
        msg.extend_from_slice(b" at ");
        // SAFETY: `karac_mir_flag_acquire`'s caller passed a constant.
        msg.extend_from_slice(unsafe {
            std::slice::from_raw_parts(at as *const u8, at_len as usize)
        });
    }
    msg.extend_from_slice(" (core semantics §6.2)".as_bytes());
    crate::mir::set_panic_bytes(msg.as_slice());
}

/// `karac_mir_flag_acquire` for the coroutine frame at `frame`, at its
/// slot `slot`: gives back what that slot held first, and remembers what
/// it takes until `karac_mir_flag_release_in` or
/// `karac_mir_flag_release_frame`.
#[no_mangle]
pub extern "C" fn karac_mir_flag_acquire_in(
    addr: *const u8,
    mutable: u8,
    frame: *const u8,
    slot: u32,
    at: *const u8,
    at_len: u64,
) -> u8 {
    karac_mir_flag_release_in(frame, slot);
    if karac_mir_flag_acquire(addr, mutable, at, at_len) != 0 {
        return 1;
    }
    holds(|h| {
        h.push(Hold {
            frame: frame as usize,
            slot,
            addr: addr as usize,
            mutable,
        })
    });
    0
}

/// Gives back what the coroutine frame at `frame` holds at `slot`, if
/// anything.
#[no_mangle]
pub extern "C" fn karac_mir_flag_release_in(frame: *const u8, slot: u32) {
    let frame = frame as usize;
    let held = holds(|h| {
        let i = h.iter().position(|x| x.frame == frame && x.slot == slot)?;
        Some(h.swap_remove(i))
    });
    if let Some(x) = held {
        karac_mir_flag_release(x.addr as *const u8, x.mutable);
    }
}

/// Gives back everything the coroutine frame at `frame` holds, as it is
/// dropped.
#[no_mangle]
pub extern "C" fn karac_mir_flag_release_frame(frame: *const u8) {
    let frame = frame as usize;
    let held: Vec<Hold> = holds(|h| {
        let (mine, rest) = std::mem::take(h)
            .into_iter()
            .partition(|x| x.frame == frame);
        *h = rest;
        mine
    });
    for x in held {
        karac_mir_flag_release(x.addr as *const u8, x.mutable);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_AT: *const u8 = std::ptr::null();

    #[test]
    fn readers_share_and_a_writer_excludes() {
        let a = 0x1000 as *const u8;
        assert_eq!(karac_mir_flag_acquire(a, 0, NO_AT, 0), 0);
        assert_eq!(karac_mir_flag_acquire(a, 0, NO_AT, 0), 0);
        assert_eq!(karac_mir_flag_check(a, 0), 0);
        assert_eq!(karac_mir_flag_check(a, 1), 1);
        assert_eq!(karac_mir_flag_acquire(a, 1, NO_AT, 0), 1);
        karac_mir_flag_release(a, 0);
        karac_mir_flag_release(a, 0);
        assert_eq!(karac_mir_flag_acquire(a, 1, NO_AT, 0), 0);
        assert_eq!(karac_mir_flag_check(a, 0), 1);
        karac_mir_flag_release(a, 1);
        assert_eq!(karac_mir_flag_check(a, 1), 0);
    }

    /// Many flags held at once, as a recursion holding one per frame
    /// does (B-2026-11-08-83): each is found by its own address.
    #[test]
    fn many_flags_are_kept_apart() {
        let at = |i: usize| (0x10_0000 + 24 * i) as *const u8;
        for i in 0..5000 {
            assert_eq!(
                karac_mir_flag_acquire(at(i), u8::from(i % 2 == 0), NO_AT, 0),
                0
            );
        }
        for i in 0..5000 {
            assert_eq!(karac_mir_flag_check(at(i), 1), 1);
            assert_eq!(karac_mir_flag_check(at(i), 0), u8::from(i % 2 == 0));
        }
        assert_eq!(karac_mir_flag_check(at(5000), 1), 0);
        for i in (0..5000).rev() {
            karac_mir_flag_release(at(i), u8::from(i % 2 == 0));
        }
        for i in 0..5000 {
            assert_eq!(karac_mir_flag_check(at(i), 1), 0);
        }
    }

    #[test]
    fn a_frame_keeps_its_flags_until_released_or_dropped() {
        let (a, b) = (0x2000 as *const u8, 0x3000 as *const u8);
        let f = 0x9000 as *const u8;
        assert_eq!(karac_mir_flag_acquire_in(a, 1, f, 0, NO_AT, 0), 0);
        assert_eq!(karac_mir_flag_acquire_in(b, 0, f, 1, NO_AT, 0), 0);
        assert_eq!(karac_mir_flag_check(a, 0), 1);
        // Taking slot 0 again gives back what it held first.
        assert_eq!(karac_mir_flag_acquire_in(a, 1, f, 0, NO_AT, 0), 0);
        karac_mir_flag_release_in(f, 0);
        assert_eq!(karac_mir_flag_check(a, 1), 0);
        assert_eq!(karac_mir_flag_check(b, 1), 1);
        karac_mir_flag_release_frame(f);
        assert_eq!(karac_mir_flag_check(b, 1), 0);
    }
}
