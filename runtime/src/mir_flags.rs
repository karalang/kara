//! The run-time borrow flags of core semantics §6.2 for compiled MIR: a
//! `mut` field of a `shared` value whose type is neither `Copy` nor a handle
//! aggregate is flagged while a loan of it lives, so a conflicting access
//! through another handle to the same object panics. The borrow-flag pass
//! (`src/mir/flags.rs`) places the operations; this keeps the flags, by the
//! field's address.
//!
//! `shared` values never cross threads, so each thread keeps its own flags.
//! Few are held at once (one per live loan of a flagged field), so they sit
//! in a short list, and a check with none held returns at once.

use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// One flagged field: how many readers hold it, or whether a writer does.
struct Flag {
    addr: usize,
    readers: u32,
    writer: bool,
}

thread_local! {
    static FLAGS: RefCell<Vec<Flag>> = const { RefCell::new(Vec::new()) };
}

/// How many flags any thread holds: a check with none held is one load.
static HELD: AtomicUsize = AtomicUsize::new(0);

fn conflicts(f: &Flag, mutable: bool) -> bool {
    f.writer || (mutable && f.readers > 0)
}

/// Takes the flag at `addr`, mutably when `mutable` is non-zero. Returns 1,
/// taking nothing, when the flag is held in a way that conflicts.
#[no_mangle]
pub extern "C" fn karac_mir_flag_acquire(addr: *const u8, mutable: u8) -> u8 {
    let addr = addr as usize;
    let mutable = mutable != 0;
    FLAGS.with(|fl| {
        let mut fl = fl.borrow_mut();
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
                HELD.fetch_add(1, Ordering::Relaxed);
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
    FLAGS.with(|fl| {
        let mut fl = fl.borrow_mut();
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
            HELD.fetch_sub(1, Ordering::Relaxed);
        }
    })
}

/// An access that takes no borrow: returns 1 when a write or drop
/// (`mutable` non-zero) meets a held flag, or a read meets a writer's.
#[no_mangle]
pub extern "C" fn karac_mir_flag_check(addr: *const u8, mutable: u8) -> u8 {
    if HELD.load(Ordering::Relaxed) == 0 {
        return 0;
    }
    let addr = addr as usize;
    FLAGS.with(|fl| {
        let fl = fl.borrow();
        u8::from(
            fl.iter()
                .any(|f| f.addr == addr && conflicts(f, mutable != 0)),
        )
    })
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
}
