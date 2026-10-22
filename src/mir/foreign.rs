//! Calls from the MIR interpreter into native libraries: the `extern "C"`
//! functions a program declares (design.md § FFI; B-2026-10-22-10). The
//! symbol is found with `dlsym`, in the libraries `kara.toml`'s `[link]`
//! table names and then in the process itself (libc's `malloc`), and called
//! with every argument in an integer register, which is how the C ABI passes
//! the integers, `bool`s and pointers that are all an `extern` signature may
//! hold so far (the lowering refuses anything else).

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::Mutex;

extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *mut c_char;
}

const RTLD_NOW: c_int = 2;

/// A library handle, kept for the life of the process.
struct Handle(*mut c_void);

// SAFETY: a `dlopen` handle is a process-wide token that `dlsym` may use
// from any thread.
unsafe impl Send for Handle {}

/// The open libraries, the process's own handle last: opened on the first
/// foreign call, after the `[link]` table is known.
static HANDLES: Mutex<Option<Result<Vec<Handle>, String>>> = Mutex::new(None);

fn last_error() -> String {
    // SAFETY: `dlerror` returns null or a NUL-terminated message.
    let e = unsafe { dlerror() };
    if e.is_null() {
        "unknown error".to_string()
    } else {
        // SAFETY: checked non-null above.
        unsafe { CStr::from_ptr(e) }.to_string_lossy().into_owned()
    }
}

/// The file names a `[link]` library `name` may have, in the search paths
/// first: the development name, then the runtime one a system without the
/// development package still has.
fn candidates(name: &str, search_paths: &[String]) -> Vec<String> {
    let files: Vec<String> = if cfg!(target_os = "macos") {
        vec![format!("lib{name}.dylib")]
    } else {
        vec![format!("lib{name}.so"), format!("lib{name}.so.1")]
    };
    let mut out = Vec::new();
    for dir in search_paths {
        for f in &files {
            out.push(format!("{dir}/{f}"));
        }
    }
    out.extend(files);
    out
}

fn open_all() -> Result<Vec<Handle>, String> {
    let mut handles = Vec::new();
    if let Some(link) = crate::target::native_link_config() {
        for lib in &link.libs {
            let mut opened = None;
            let mut why = String::new();
            for file in candidates(lib, &link.search_paths) {
                let c = CString::new(file).map_err(|e| e.to_string())?;
                // SAFETY: a NUL-terminated file name.
                let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW) };
                if h.is_null() {
                    why = last_error();
                } else {
                    opened = Some(Handle(h));
                    break;
                }
            }
            match opened {
                Some(h) => handles.push(h),
                None => {
                    return Err(format!(
                        "cannot load the library `{lib}` that kara.toml's [link] table names: {why}"
                    ))
                }
            }
        }
    }
    // SAFETY: a null name opens the process itself.
    let own = unsafe { dlopen(std::ptr::null(), RTLD_NOW) };
    if own.is_null() {
        return Err(last_error());
    }
    handles.push(Handle(own));
    Ok(handles)
}

/// The address of the C function `name`.
pub fn symbol(name: &str) -> Result<*const c_void, String> {
    let mut held = HANDLES.lock().unwrap_or_else(|p| p.into_inner());
    let handles = held.get_or_insert_with(open_all).as_ref()?;
    let c = CString::new(name).map_err(|e| e.to_string())?;
    for h in handles {
        // SAFETY: a live handle and a NUL-terminated name.
        let f = unsafe { dlsym(h.0, c.as_ptr()) };
        if !f.is_null() {
            return Ok(f);
        }
    }
    Err(format!(
        "the `extern \"C\"` function `{name}` is in no library the program links \
         (kara.toml's [link] table) nor in the C library"
    ))
}

/// Calls the C function at `f` with `args`, each in an integer register,
/// and returns the integer register it answers in.
///
/// # Safety
///
/// `f` must be a C function taking `args.len()` integer, `bool` or pointer
/// arguments, each pointer valid for what the function does with it.
pub unsafe fn call(f: *const c_void, args: &[u64]) -> Result<u64, String> {
    type W = u64;
    let a = |i: usize| args[i];
    // SAFETY (each arm): the caller's contract; `f` is transmuted to the
    // prototype with as many integer parameters as there are arguments.
    Ok(match args.len() {
        0 => std::mem::transmute::<*const c_void, extern "C" fn() -> W>(f)(),
        1 => std::mem::transmute::<*const c_void, extern "C" fn(W) -> W>(f)(a(0)),
        2 => std::mem::transmute::<*const c_void, extern "C" fn(W, W) -> W>(f)(a(0), a(1)),
        3 => std::mem::transmute::<*const c_void, extern "C" fn(W, W, W) -> W>(f)(a(0), a(1), a(2)),
        4 => std::mem::transmute::<*const c_void, extern "C" fn(W, W, W, W) -> W>(f)(
            a(0),
            a(1),
            a(2),
            a(3),
        ),
        5 => std::mem::transmute::<*const c_void, extern "C" fn(W, W, W, W, W) -> W>(f)(
            a(0),
            a(1),
            a(2),
            a(3),
            a(4),
        ),
        6 => std::mem::transmute::<*const c_void, extern "C" fn(W, W, W, W, W, W) -> W>(f)(
            a(0),
            a(1),
            a(2),
            a(3),
            a(4),
            a(5),
        ),
        // Past six, an argument goes on the stack, where some ABIs (Apple's
        // arm64) pack it at its own size rather than a register's.
        n => return Err(format!("{n} arguments (at most six cross to C yet)")),
    })
}
