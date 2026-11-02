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
//! at one, B-2026-11-01-25). Few flags are held at once (one per live loan
//! of a flagged field), so they sit in a short list, and a check with none
//! held returns at once.
//!
//! A coroutine's resume body has no frame slots of its own to remember
//! what it took, so the `_in` entry points keep that here, by the
//! coroutine frame's address and the slot's index, across suspensions
//! (B-2026-10-20-14).

use std::cell::RefCell;

/// One flagged field: how many readers hold it, or whether a writer does.
struct Flag {
    addr: usize,
    readers: u32,
    writer: bool,
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
    flags: Vec<Flag>,
    holds: Vec<Hold>,
}

thread_local! {
    /// The running task's table (outside every task, the thread's own).
    static TABLE: RefCell<Table> = const {
        RefCell::new(Table {
            flags: Vec::new(),
            holds: Vec::new(),
        })
    };
}

/// Puts `t` in the running task's place and answers what was there: the
/// executor's swap around a task's poll.
pub(crate) fn swap_table(t: Table) -> Table {
    TABLE.with(|c| std::mem::replace(&mut *c.borrow_mut(), t))
}

fn flags<R>(f: impl FnOnce(&mut Vec<Flag>) -> R) -> R {
    TABLE.with(|c| f(&mut c.borrow_mut().flags))
}

fn holds<R>(f: impl FnOnce(&mut Vec<Hold>) -> R) -> R {
    TABLE.with(|c| f(&mut c.borrow_mut().holds))
}

fn conflicts(f: &Flag, mutable: bool) -> bool {
    f.writer || (mutable && f.readers > 0)
}

/// Takes the flag at `addr`, mutably when `mutable` is non-zero. Returns 1,
/// taking nothing, when the flag is held in a way that conflicts.
#[no_mangle]
pub extern "C" fn karac_mir_flag_acquire(addr: *const u8, mutable: u8) -> u8 {
    let addr = addr as usize;
    let mutable = mutable != 0;
    flags(|fl| {
        match fl.iter_mut().find(|f| f.addr == addr) {
            Some(f) if conflicts(f, mutable) => return 1,
            Some(f) => {
                if mutable {
                    f.writer = true;
                } else {
                    f.readers += 1;
                }
            }
            None => {
                fl.push(Flag {
                    addr,
                    readers: u32::from(!mutable),
                    writer: mutable,
                });
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
        let Some(i) = fl.iter().position(|f| f.addr == addr) else {
            return;
        };
        let f = &mut fl[i];
        if mutable != 0 {
            f.writer = false;
        } else {
            f.readers = f.readers.saturating_sub(1);
        }
        if f.readers == 0 && !f.writer {
            fl.swap_remove(i);
        }
    })
}

/// An access that takes no borrow: returns 1 when a write or drop
/// (`mutable` non-zero) meets a held flag, or a read meets a writer's.
#[no_mangle]
pub extern "C" fn karac_mir_flag_check(addr: *const u8, mutable: u8) -> u8 {
    let addr = addr as usize;
    flags(|fl| {
        u8::from(
            fl.iter()
                .any(|f| f.addr == addr && conflicts(f, mutable != 0)),
        )
    })
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
) -> u8 {
    karac_mir_flag_release_in(frame, slot);
    if karac_mir_flag_acquire(addr, mutable) != 0 {
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

    #[test]
    fn readers_share_and_a_writer_excludes() {
        let a = 0x1000 as *const u8;
        assert_eq!(karac_mir_flag_acquire(a, 0), 0);
        assert_eq!(karac_mir_flag_acquire(a, 0), 0);
        assert_eq!(karac_mir_flag_check(a, 0), 0);
        assert_eq!(karac_mir_flag_check(a, 1), 1);
        assert_eq!(karac_mir_flag_acquire(a, 1), 1);
        karac_mir_flag_release(a, 0);
        karac_mir_flag_release(a, 0);
        assert_eq!(karac_mir_flag_acquire(a, 1), 0);
        assert_eq!(karac_mir_flag_check(a, 0), 1);
        karac_mir_flag_release(a, 1);
        assert_eq!(karac_mir_flag_check(a, 1), 0);
    }

    #[test]
    fn a_frame_keeps_its_flags_until_released_or_dropped() {
        let (a, b) = (0x2000 as *const u8, 0x3000 as *const u8);
        let f = 0x9000 as *const u8;
        assert_eq!(karac_mir_flag_acquire_in(a, 1, f, 0), 0);
        assert_eq!(karac_mir_flag_acquire_in(b, 0, f, 1), 0);
        assert_eq!(karac_mir_flag_check(a, 0), 1);
        // Taking slot 0 again gives back what it held first.
        assert_eq!(karac_mir_flag_acquire_in(a, 1, f, 0), 0);
        karac_mir_flag_release_in(f, 0);
        assert_eq!(karac_mir_flag_check(a, 1), 0);
        assert_eq!(karac_mir_flag_check(b, 1), 1);
        karac_mir_flag_release_frame(f);
        assert_eq!(karac_mir_flag_check(b, 1), 0);
    }
}
