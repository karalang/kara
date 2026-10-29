//! Sockets a `Network` provider opened (core semantics §14.12).
//!
//! A `with_provider[Network](p, f)` call or a `providers` block binding
//! `Network` registers `p` and five of its methods here for as long as it
//! runs (`karac_mir_net_enter` / `karac_mir_net_exit`, emitted by
//! `providers::specialise`). A `TcpStream` or `TcpListener` the provider
//! opens holds the descriptor `-2 - index` (`karac_mir_net_adopt`), where
//! an OS socket's is not negative and -1 is none. Its reads, writes,
//! shutdown and accepts go to the provider while its override runs, and
//! fail with `ProviderUnavailable` after; its drop calls the provider's
//! `close`, or does nothing once the override has ended. Every other
//! operation on it fails with `Other(-1)`.
//!
//! The methods are the library's code-returning `__read`, `__write`,
//! `__accept` and `__shutdown` (runtime/stdlib/source/net_provider.kara),
//! and the provider's `close`, called by their bodies' pointer convention:
//! a slot for the result, then a slot per argument, each a 64-bit word.

// Without sockets (a wasm build) a program can still enter and leave an
// override; only the routing is unused.
#![cfg_attr(not(all(feature = "net", unix)), allow(dead_code))]

use std::sync::Mutex;

/// The runtime's code for `NetError.ProviderUnavailable`
/// (src/backend/mir/io.rs `NET_ERRORS`).
const PROVIDER_UNAVAILABLE: i32 = 10;

struct Extent {
    provider: u64,
    close: usize,
    read: usize,
    write: usize,
    accept: usize,
    shutdown: usize,
    live: bool,
}

struct Table {
    extents: Vec<Extent>,
    /// By the socket's index: its extent and the provider's own id.
    sockets: Vec<Option<(usize, i64)>>,
}

static TABLE: Mutex<Table> = Mutex::new(Table {
    extents: Vec::new(),
    sockets: Vec::new(),
});

fn table() -> std::sync::MutexGuard<'static, Table> {
    TABLE.lock().unwrap_or_else(|p| p.into_inner())
}

/// Whether `fd` is a socket a provider opened.
pub(crate) fn is_provided(fd: i64) -> bool {
    fd < -1
}

fn index(fd: i64) -> usize {
    (-2 - fd) as usize
}

/// A slice view as compiled code passes one: the pointer, then the length.
#[repr(C)]
struct View {
    ptr: u64,
    len: u64,
}

/// Registers the provider at `provider` for a `with_provider[Network]`
/// call or `providers` block, with its `close` and the library's
/// `__read`, `__write`, `__accept` and `__shutdown`; the id `exit` takes.
#[no_mangle]
pub extern "C" fn karac_mir_net_enter(
    provider: *mut u8,
    close: usize,
    read: usize,
    write: usize,
    accept: usize,
    shutdown: usize,
) -> i64 {
    let mut t = table();
    t.extents.push(Extent {
        provider: provider as u64,
        close,
        read,
        write,
        accept,
        shutdown,
        live: true,
    });
    t.extents.len() as i64 - 1
}

/// Ends the override `id` registered: its sockets fail from now on.
#[no_mangle]
pub extern "C" fn karac_mir_net_exit(id: i64) {
    let mut t = table();
    if let Some(x) = usize::try_from(id).ok().and_then(|i| t.extents.get_mut(i)) {
        x.live = false;
    }
}

fn adopt(t: &mut Table, extent: usize, h: i64) -> i64 {
    t.sockets.push(Some((extent, h)));
    -1 - t.sockets.len() as i64
}

/// The descriptor of the socket `h` the provider at `provider` returned
/// from its `bind` or `connect`, inside that provider's running override.
#[no_mangle]
pub extern "C" fn karac_mir_net_adopt(provider: *mut u8, h: i64) -> i64 {
    let mut t = table();
    let Some(extent) = t
        .extents
        .iter()
        .rposition(|x| x.live && x.provider == provider as u64)
    else {
        crate::fatal::write_stderr(
            b"error: a Network provider opened a socket outside its override\n",
        );
        std::process::abort();
    };
    adopt(&mut t, extent, h)
}

/// `karac_mir_tcp`'s `op` on the provided socket `fd`, with its result
/// as that function returns one: `-1` and the count or descriptor at
/// `value`, or the `NetError` code with `Other`'s or `Tls`'s payload at
/// `value`.
///
/// # Safety
///
/// `ptr[..len]` is valid for a read or write, and `value` writable.
pub(crate) unsafe fn tcp(op: i32, fd: i64, ptr: *mut u8, len: i64, value: *mut i64) -> i32 {
    let route = {
        let t = table();
        t.sockets
            .get(index(fd))
            .copied()
            .flatten()
            .map(|(extent, h)| (extent, h, &t.extents[extent]))
            .filter(|(_, _, x)| x.live)
            .map(|(extent, h, x)| (extent, h, x.provider, x.read, x.write, x.accept, x.shutdown))
    };
    let Some((extent, h, p, read, write, accept, shutdown)) = route else {
        return PROVIDER_UNAVAILABLE;
    };
    let mut v: i64 = 0;
    let vp = &mut v as *mut i64 as u64;
    // A method's `0`, or `-1 - code`.
    let mut ret: i64 = 0;
    type Moved = unsafe extern "C" fn(*mut i64, *const u64, *const i64, *const View, *const u64);
    type Plain = unsafe extern "C" fn(*mut i64, *const u64, *const i64, *const u64);
    let view = |at: *mut u8, n: i64| View {
        ptr: at as u64,
        len: n.max(0) as u64,
    };
    // SAFETY: each method is the provider's `__read`, `__write`,
    // `__accept` or `__shutdown`, whose pointer convention this is
    // (`karac_mir_net_enter`); the table is not held while it runs.
    unsafe {
        match op {
            2 => {
                let f: Plain = std::mem::transmute::<usize, Plain>(accept);
                f(&mut ret, &p, &h, &vp);
            }
            4 => {
                let f: Plain = std::mem::transmute::<usize, Plain>(shutdown);
                f(&mut ret, &p, &h, &vp);
            }
            5 | 6 => {
                let m = if op == 5 { read } else { write };
                let f: Moved = std::mem::transmute::<usize, Moved>(m);
                f(&mut ret, &p, &h, &view(ptr, len), &vp);
            }
            7 => {
                // `write_all`: partial writes go round again.
                let f: Moved = std::mem::transmute::<usize, Moved>(write);
                let mut done = 0;
                while done < len && ret >= 0 {
                    f(
                        &mut ret,
                        &p,
                        &h,
                        &view(ptr.add(done as usize), len - done),
                        &vp,
                    );
                    if ret >= 0 && v == 0 {
                        break;
                    }
                    if ret >= 0 {
                        done += v;
                    }
                }
                if ret >= 0 {
                    v = done;
                }
            }
            _ => {
                // No `try_clone` of a provider's socket.
                value.write(-1);
                return 1;
            }
        }
    }
    if ret < 0 {
        // SAFETY: the caller's contract.
        unsafe { value.write(v) };
        return (-1 - ret) as i32;
    }
    if op == 2 {
        // The accepted socket is the provider's too.
        v = adopt(&mut table(), extent, v);
    }
    // SAFETY: the caller's contract.
    unsafe { value.write(v) };
    -1
}

/// Drops the provided socket `fd`: the provider's `close` while its
/// override runs, nothing after.
pub(crate) fn close(fd: i64) {
    let call = {
        let mut t = table();
        let Some(slot) = t.sockets.get_mut(index(fd)) else {
            return;
        };
        let Some((extent, h)) = slot.take() else {
            return;
        };
        let x = &t.extents[extent];
        x.live.then_some((x.provider, x.close, h))
    };
    let Some((provider, close, h)) = call else {
        return;
    };
    let mut ret = [0u64; 2];
    type Close = unsafe extern "C" fn(*mut u64, *const u64, *const i64);
    // SAFETY: `close` is the provider's `close`, called by its pointer
    // convention (`karac_mir_net_enter`); the table is not held.
    unsafe {
        let f: Close = std::mem::transmute::<usize, Close>(close);
        f(ret.as_mut_ptr(), &provider, &h);
    }
}
