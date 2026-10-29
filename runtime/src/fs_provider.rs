//! Files a `FileSystem` provider opened (core semantics §14.11).
//!
//! A `with_provider[FileSystem](p, f)` call registers `p` and three of its
//! methods here for as long as `f` runs (`karac_mir_fs_enter` /
//! `karac_mir_fs_exit`, emitted by `providers::specialise`). A `File` the
//! provider opens is a word with its low bit set, `(index + 1) << 1 | 1`,
//! where an OS file is the aligned `*mut KaracFile`. Its reads and writes go
//! to the provider while its call runs, and fail with
//! `ProviderUnavailable` after; its drop calls the provider's `close`, or
//! does nothing once the call has returned.
//!
//! The methods are called by their bodies' pointer convention: a slot for
//! the result, then a slot per argument. Every pointer slot is a 64-bit
//! word, as compiled code lays them out on every target.

use std::sync::Mutex;

use crate::mir::RawVec;

/// The runtime's code for `IoError.ProviderUnavailable`.
pub(crate) const PROVIDER_UNAVAILABLE: i32 = 9;

struct Extent {
    provider: u64,
    read: usize,
    write: usize,
    close: usize,
    live: bool,
}

struct Table {
    extents: Vec<Extent>,
    /// By the file's index: its extent and the provider's own id.
    files: Vec<Option<(usize, i64)>>,
}

static TABLE: Mutex<Table> = Mutex::new(Table {
    extents: Vec::new(),
    files: Vec::new(),
});

fn table() -> std::sync::MutexGuard<'static, Table> {
    TABLE.lock().unwrap_or_else(|p| p.into_inner())
}

/// Whether `handle` is a file a provider opened.
pub(crate) fn is_provided(handle: *const std::ffi::c_void) -> bool {
    handle as usize & 1 == 1
}

fn index(handle: *const std::ffi::c_void) -> usize {
    (handle as usize >> 1).wrapping_sub(1)
}

/// A slice view as compiled code passes one: the pointer, then the length.
#[repr(C)]
struct View {
    ptr: u64,
    len: u64,
}

/// What an operation on the provided file `handle` reaches: the provider,
/// the method (`write` picks `__write` over `__read`) and the provider's
/// id, or `None` once its call has returned or the file was closed.
fn route(handle: *const std::ffi::c_void, write: bool) -> Option<(u64, usize, i64)> {
    let t = table();
    let (extent, h) = (*t.files.get(index(handle))?)?;
    let x = &t.extents[extent];
    x.live
        .then_some((x.provider, if write { x.write } else { x.read }, h))
}

/// Registers the provider at `provider` for a `with_provider[FileSystem]`
/// call, with its `close`, `__read` and `__write`; the id `exit` takes.
#[no_mangle]
pub extern "C" fn karac_mir_fs_enter(
    provider: *mut u8,
    close: usize,
    read: usize,
    write: usize,
) -> i64 {
    let mut t = table();
    t.extents.push(Extent {
        provider: provider as u64,
        read,
        write,
        close,
        live: true,
    });
    t.extents.len() as i64 - 1
}

/// Ends the call `id` registered: its files fail from now on.
#[no_mangle]
pub extern "C" fn karac_mir_fs_exit(id: i64) {
    let mut t = table();
    if let Some(x) = usize::try_from(id).ok().and_then(|i| t.extents.get_mut(i)) {
        x.live = false;
    }
}

/// The `File` word of the id `h` the provider at `provider` returned from
/// its `open`, inside that provider's running call.
#[no_mangle]
pub extern "C" fn karac_mir_fs_adopt(provider: *mut u8, h: i64) -> i64 {
    let mut t = table();
    let Some(extent) = t
        .extents
        .iter()
        .rposition(|x| x.live && x.provider == provider as u64)
    else {
        crate::fatal::write_stderr(
            b"error: a FileSystem provider opened a file outside its with_provider call\n",
        );
        std::process::abort();
    };
    t.files.push(Some((extent, h)));
    ((t.files.len() as i64) << 1) | 1
}

/// A read (`write` false) or write of `len` bytes at `ptr` on the provided
/// file `handle`: the count at `value` and `-1`, or an error code with an
/// `Other`'s text at `msg`.
///
/// # Safety
///
/// `ptr[..len]` is valid, `value` writable for an `i64`, and `msg` for a
/// `String`.
pub(crate) unsafe fn read_write(
    handle: *const std::ffi::c_void,
    write: bool,
    ptr: *mut u8,
    len: i64,
    value: *mut i64,
    msg: *mut RawVec,
) -> i32 {
    let Some((provider, method, h)) = route(handle, write) else {
        return PROVIDER_UNAVAILABLE;
    };
    // SAFETY: the caller's contract.
    unsafe { msg.write(RawVec::new(std::ptr::null_mut(), 0, 0)) };
    let mut ret: i64 = 0;
    let (p, hs) = (provider, h);
    let view = View {
        ptr: ptr as u64,
        len: len as u64,
    };
    let m = msg as u64;
    type Raw = unsafe extern "C" fn(*mut i64, *const u64, *const i64, *const View, *const u64);
    // SAFETY: `method` is the provider's `__read` or `__write`, whose
    // pointer convention this is (`karac_mir_fs_enter`).
    unsafe {
        let f: Raw = std::mem::transmute::<usize, Raw>(method);
        f(&mut ret, &p, &hs, &view, &m);
    }
    if ret >= 0 {
        // SAFETY: the caller's contract.
        unsafe { value.write(ret) };
        -1
    } else {
        (-1 - ret) as i32
    }
}

/// Whether the provided file `handle`'s provider call still runs.
pub(crate) fn live(handle: *const std::ffi::c_void) -> bool {
    route(handle, false).is_some()
}

/// Drops the provided file `handle`: the provider's `close` while its call
/// runs, nothing after.
pub(crate) fn close(handle: *const std::ffi::c_void) {
    let call = {
        let mut t = table();
        let Some(slot) = t.files.get_mut(index(handle)) else {
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
    // convention (`karac_mir_fs_enter`); the table is not held.
    unsafe {
        let f: Close = std::mem::transmute::<usize, Close>(close);
        f(ret.as_mut_ptr(), &provider, &h);
    }
}

/// A failure a provider reported for one of its files, carried through
/// `std::io` by a `BufReader` or `BufWriter` over the file: the runtime's
/// code for its `IoError` variant and `Other`'s text.
#[derive(Debug)]
pub(crate) struct ProvidedError {
    pub(crate) code: i32,
    pub(crate) text: String,
}

impl std::fmt::Display for ProvidedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::error::Error for ProvidedError {}

/// A provider's file as `std::io` sees it, which a `BufReader` or
/// `BufWriter` over the file owns; dropping it closes the file.
pub(crate) struct ProvidedFile(pub(crate) usize);

impl ProvidedFile {
    fn call(&mut self, write: bool, ptr: *mut u8, len: usize) -> std::io::Result<usize> {
        let mut n = 0i64;
        let mut msg = RawVec::new(std::ptr::null_mut(), 0, 0);
        // SAFETY: `ptr[..len]` is the caller's buffer; `n` and `msg` are
        // writable.
        let code =
            unsafe { read_write(self.0 as *const _, write, ptr, len as i64, &mut n, &mut msg) };
        if code == -1 {
            return Ok(n.max(0) as usize);
        }
        let mut text = String::new();
        if !msg.ptr.is_null() {
            // SAFETY: the provider's `String`, which this takes and frees.
            unsafe {
                let b = std::slice::from_raw_parts(msg.ptr, msg.len);
                text = String::from_utf8_lossy(b).into_owned();
                if msg.cap > 0 {
                    crate::alloc::karac_free(msg.ptr);
                }
            }
        }
        Err(std::io::Error::other(ProvidedError { code, text }))
    }
}

impl std::io::Read for ProvidedFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.call(false, buf.as_mut_ptr(), buf.len())
    }
}

impl std::io::Write for ProvidedFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.call(true, buf.as_ptr().cast_mut(), buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if live(self.0 as *const _) {
            return Ok(());
        }
        Err(std::io::Error::other(ProvidedError {
            code: PROVIDER_UNAVAILABLE,
            text: String::new(),
        }))
    }
}

impl Drop for ProvidedFile {
    fn drop(&mut self) {
        close(self.0 as *const _);
    }
}
