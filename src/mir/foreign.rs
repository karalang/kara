//! Calls from the MIR interpreter into native libraries: the `extern "C"`
//! functions a program declares (design.md § FFI; B-2026-10-22-10). The
//! symbol is found with `dlsym`, in the libraries `kara.toml`'s `[link]`
//! table names and then in the process itself (libc's `malloc`), and called
//! with every argument in an integer register, which is how the C ABI passes
//! the integers, `bool`s and pointers, or, when an `f32` or `f64` crosses
//! too, through [`call_mixed`] (B-2026-10-31-32). The lowering refuses
//! anything else.

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
    let mut handles = open_libs(RTLD_NOW)?;
    // SAFETY: a null name opens the process itself.
    let own = unsafe { dlopen(std::ptr::null(), RTLD_NOW) };
    if own.is_null() {
        return Err(last_error());
    }
    handles.push(Handle(own));
    Ok(handles)
}

/// `RTLD_GLOBAL`: a library's symbols join the process's own.
const RTLD_GLOBAL: c_int = if cfg!(target_os = "macos") {
    0x8
} else {
    0x100
};

/// Opens the `[link]` table's libraries into the process's own symbols, for
/// `karac_jit_runner`, whose JIT resolves a compiled program's `extern "C"`
/// calls by looking in the process (the mir-llvm backend, B-2026-10-22-10).
pub fn load_link_libraries_globally() -> Result<(), String> {
    open_libs(RTLD_NOW | RTLD_GLOBAL).map(|_| ())
}

fn open_libs(flags: c_int) -> Result<Vec<Handle>, String> {
    let mut handles = Vec::new();
    if let Some(link) = crate::target::native_link_config() {
        for lib in &link.libs {
            let mut opened = None;
            let mut why = String::new();
            for file in candidates(lib, &link.search_paths) {
                let c = CString::new(file).map_err(|e| e.to_string())?;
                // SAFETY: a NUL-terminated file name.
                let h = unsafe { dlopen(c.as_ptr(), flags) };
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
    Ok(handles)
}

/// The address of the C function `name`.
pub fn symbol(name: &str) -> Result<*const c_void, String> {
    // The libm names a Rust link shadows (`compiler_builtins`' `cbrt`): the
    // function a compiled program links, which `dlsym` does not see, so an
    // `extern "C"` call of one answers as `karac build`'s does (FF1; the
    // JIT republishes the same addresses, `PUBLISHED_LIBM_SYMBOLS`).
    let (cbrt, cbrtf) = crate::float_math::cbrt_libm_addrs();
    match name {
        "cbrt" => return Ok(cbrt as *const c_void),
        "cbrtf" => return Ok(cbrtf as *const c_void),
        _ => {}
    }
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

/// The integer argument registers a C function reads: six on x86-64
/// System V, eight (x0 to x7) on AArch64.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const INT_REGS: usize = if cfg!(target_arch = "aarch64") { 8 } else { 6 };

/// A C function's result register: an integer one, or a float one for an
/// `f32` / `f64` result.
pub enum Ret {
    Int(u64),
    Float(f64),
}

/// Calls the C function at `f` with integer, `bool` and pointer `ints` and
/// floating-point `floats`, each kind in its own register sequence, in
/// order. On x86-64 (System V) and AArch64 a non-variadic function takes
/// its first six (x86-64) or eight integer arguments and its first eight
/// float arguments in two independently numbered register files, whatever
/// their interleaving in the signature, so one prototype with six integer
/// and eight `f64` slots serves every signature of at most that many: the
/// slots the function does not read are ignored. An `f32` travels in the
/// low half of its register, the bits [`f32_slot`] puts there.
///
/// # Safety
///
/// `f` must be a non-variadic C function whose integer-class and
/// float-class parameters are `ints` and `floats` in order, each pointer
/// valid for what the function does with it.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub unsafe fn call_mixed(
    f: *const std::ffi::c_void,
    ints: &[u64],
    floats: &[f64],
    float_ret: bool,
) -> Result<Ret, String> {
    type W = u64;
    type F = f64;
    if ints.len() > INT_REGS || floats.len() > 8 {
        return Err(format!(
            "{} integer and {} float arguments (at most {INT_REGS} and eight cross to C yet)",
            ints.len(),
            floats.len()
        ));
    }
    let mut i = [0u64; 8];
    i[..ints.len()].copy_from_slice(ints);
    let mut x = [0f64; 8];
    x[..floats.len()].copy_from_slice(floats);
    // SAFETY: the caller's contract; the extra slots are registers the
    // function does not read.
    Ok(if float_ret {
        Ret::Float(regs_call::<F>(f, &i, &x))
    } else {
        Ret::Int(regs_call::<W>(f, &i, &x))
    })
}

/// Calls `f` with every integer and float argument register loaded from
/// `i` and `x`, the result read as a `T`: six integer registers on x86-64
/// System V, eight (x0 to x7) on AArch64, and eight float ones on both.
///
/// # Safety
///
/// As for [`call_mixed`].
#[cfg(target_arch = "aarch64")]
unsafe fn regs_call<T>(f: *const std::ffi::c_void, i: &[u64; 8], x: &[f64; 8]) -> T {
    type W = u64;
    type F = f64;
    let g = std::mem::transmute::<
        *const std::ffi::c_void,
        extern "C" fn(W, W, W, W, W, W, W, W, F, F, F, F, F, F, F, F) -> T,
    >(f);
    g(
        i[0], i[1], i[2], i[3], i[4], i[5], i[6], i[7], x[0], x[1], x[2], x[3], x[4], x[5], x[6],
        x[7],
    )
}

/// [`regs_call`] on x86-64.
///
/// # Safety
///
/// As for [`call_mixed`].
#[cfg(target_arch = "x86_64")]
unsafe fn regs_call<T>(f: *const std::ffi::c_void, i: &[u64; 8], x: &[f64; 8]) -> T {
    type W = u64;
    type F = f64;
    let g = std::mem::transmute::<
        *const std::ffi::c_void,
        extern "C" fn(W, W, W, W, W, W, F, F, F, F, F, F, F, F) -> T,
    >(f);
    g(
        i[0], i[1], i[2], i[3], i[4], i[5], x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7],
    )
}

/// The registers a C function's result comes back in, for
/// [`call_shaped`]: what [`super::c_abi`] makes of the result type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetShape {
    /// One integer register.
    Word,
    /// One float register, its 64 bits.
    Float,
    /// Two registers, each a float one when its flag is set (x86-64: `rax`
    /// / `rdx` / `xmm0` / `xmm1` by eightbyte; AArch64: `x0`, `x1`).
    Pair(bool, bool),
    /// `n` float registers (an AArch64 HFA), `f32` or `f64` each.
    Hfa { n: usize, f32: bool },
    /// Through the hidden pointer (`sret`) to a slot of this many bytes.
    Memory(usize),
}

/// The bytes of the result slot [`RetShape::Memory`] reads back into: as
/// large as a struct may be that crosses by value on the interpreter.
pub const MEMORY_RESULT_MAX: usize = 512;

/// [`call_mixed`] for any [`RetShape`]: the result's register images in
/// order, each 8 bytes (4 for an `f32` HFA member), or the `Memory` slot's
/// bytes.
///
/// # Safety
///
/// As for [`call_mixed`], and the function's result is the C type `shape`
/// was classified from.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub unsafe fn call_shaped(
    f: *const std::ffi::c_void,
    ints: &[u64],
    floats: &[f64],
    stack: &[u64],
    shape: RetShape,
) -> Result<Vec<u8>, String> {
    type W = u64;
    type F = f64;
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct P2<A, B>(A, B);
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct P3<A>(A, A, A);
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct P4<A>(A, A, A, A);
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Mem([u64; MEMORY_RESULT_MAX / 8]);
    /// The stack arguments, in order: on x86-64 System V every one takes
    /// whole eightbytes, so their words laid end to end are the stack.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Blob<const N: usize>([u64; N]);
    if ints.len() > INT_REGS || floats.len() > 8 {
        return Err(format!(
            "{} integer and {} float arguments (at most {INT_REGS} and eight cross to C yet)",
            ints.len(),
            floats.len()
        ));
    }
    if !stack.is_empty() && (cfg!(not(target_arch = "x86_64")) || stack.len() > 16) {
        return Err(format!(
            "{} words of stack arguments (the interpreter passes up to 16, on x86-64)",
            stack.len()
        ));
    }
    let mut i = [0u64; 8];
    i[..ints.len()].copy_from_slice(ints);
    let mut x = [0f64; 8];
    x[..floats.len()].copy_from_slice(floats);
    // With a hidden result pointer, x86-64 passes it in the first integer
    // register, so five are left for the arguments before the stack ones.
    let five = matches!(shape, RetShape::Memory(_)) && cfg!(target_arch = "x86_64");
    // SAFETY (both): the caller's contract, as for `call_mixed`; every
    // integer register is taken, so the blob goes on the stack.
    unsafe fn go_n<T, const N: usize>(
        f: *const std::ffi::c_void,
        i: &[u64; 8],
        x: &[f64; 8],
        s: &[u64],
        five: bool,
    ) -> T {
        let mut b = [0u64; N];
        b.copy_from_slice(s);
        let b = Blob(b);
        if five {
            let g = std::mem::transmute::<
                *const std::ffi::c_void,
                extern "C" fn(W, W, W, W, W, F, F, F, F, F, F, F, F, Blob<N>) -> T,
            >(f);
            g(
                i[0], i[1], i[2], i[3], i[4], x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7], b,
            )
        } else {
            let g = std::mem::transmute::<
                *const std::ffi::c_void,
                extern "C" fn(W, W, W, W, W, W, F, F, F, F, F, F, F, F, Blob<N>) -> T,
            >(f);
            g(
                i[0], i[1], i[2], i[3], i[4], i[5], x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7],
                b,
            )
        }
    }
    unsafe fn go<T>(
        f: *const std::ffi::c_void,
        i: &[u64; 8],
        x: &[f64; 8],
        s: &[u64],
        five: bool,
    ) -> T {
        macro_rules! blob {
            ($($n:literal)*) => {
                match s.len() {
                    $($n => return go_n::<T, $n>(f, i, x, s, five),)*
                    _ => {}
                }
            };
        }
        blob!(1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16);
        // AArch64's `sret` pointer goes in x8, apart from the argument
        // registers.
        regs_call::<T>(f, i, x)
    }
    let w = |v: u64| v.to_ne_bytes().to_vec();
    let d = |v: f64| v.to_bits().to_ne_bytes().to_vec();
    let s = |v: f32| v.to_bits().to_ne_bytes().to_vec();
    Ok(match shape {
        RetShape::Word => w(go::<W>(f, &i, &x, stack, five)),
        RetShape::Float => d(go::<F>(f, &i, &x, stack, five)),
        RetShape::Pair(false, false) => {
            let r = go::<P2<W, W>>(f, &i, &x, stack, five);
            [w(r.0), w(r.1)].concat()
        }
        RetShape::Pair(false, true) => {
            let r = go::<P2<W, F>>(f, &i, &x, stack, five);
            [w(r.0), d(r.1)].concat()
        }
        RetShape::Pair(true, false) => {
            let r = go::<P2<F, W>>(f, &i, &x, stack, five);
            [d(r.0), w(r.1)].concat()
        }
        RetShape::Pair(true, true) => {
            let r = go::<P2<F, F>>(f, &i, &x, stack, five);
            [d(r.0), d(r.1)].concat()
        }
        RetShape::Hfa { n: 1, f32: false } => d(go::<F>(f, &i, &x, stack, five)),
        RetShape::Hfa { n: 1, f32: true } => s(go::<f32>(f, &i, &x, stack, five)),
        RetShape::Hfa { n: 2, f32: true } => {
            let r = go::<P2<f32, f32>>(f, &i, &x, stack, five);
            [s(r.0), s(r.1)].concat()
        }
        RetShape::Hfa { n: 3, f32: false } => {
            let r = go::<P3<F>>(f, &i, &x, stack, five);
            [d(r.0), d(r.1), d(r.2)].concat()
        }
        RetShape::Hfa { n: 3, f32: true } => {
            let r = go::<P3<f32>>(f, &i, &x, stack, five);
            [s(r.0), s(r.1), s(r.2)].concat()
        }
        RetShape::Hfa { n: 4, f32: false } => {
            let r = go::<P4<F>>(f, &i, &x, stack, five);
            [d(r.0), d(r.1), d(r.2), d(r.3)].concat()
        }
        RetShape::Hfa { n: 4, f32: true } => {
            let r = go::<P4<f32>>(f, &i, &x, stack, five);
            [s(r.0), s(r.1), s(r.2), s(r.3)].concat()
        }
        RetShape::Hfa { n: 2, f32: false } => {
            let r = go::<P2<F, F>>(f, &i, &x, stack, five);
            [d(r.0), d(r.1)].concat()
        }
        RetShape::Hfa { n, .. } => return Err(format!("a result of {n} float registers")),
        RetShape::Memory(n) if n <= MEMORY_RESULT_MAX => {
            let r = go::<Mem>(f, &i, &x, stack, five);
            r.0.iter().flat_map(|v| v.to_ne_bytes()).take(n).collect()
        }
        RetShape::Memory(n) => {
            return Err(format!(
                "a result of {n} bytes (at most {MEMORY_RESULT_MAX} cross back from C here)"
            ))
        }
    })
}

/// [`call_shaped`] elsewhere: refused.
///
/// # Safety
///
/// None needed; it calls nothing.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub unsafe fn call_shaped(
    _f: *const std::ffi::c_void,
    _ints: &[u64],
    _floats: &[f64],
    _stack: &[u64],
    _shape: RetShape,
) -> Result<Vec<u8>, String> {
    Err("a struct crosses to C on the interpreter only on x86-64 and AArch64; compile it".into())
}

/// [`call_mixed`] elsewhere: refused.
///
/// # Safety
///
/// None needed; it calls nothing.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub unsafe fn call_mixed(
    _f: *const std::ffi::c_void,
    _ints: &[u64],
    _floats: &[f64],
    _float_ret: bool,
) -> Result<Ret, String> {
    Err("a float crosses to C on the interpreter only on x86-64 and AArch64; compile it".into())
}

/// The `f64` register image of an `f32` argument: its bits in the low half,
/// where the C callee reads a `float`.
pub fn f32_slot(v: f32) -> f64 {
    f64::from_bits(u64::from(v.to_bits()))
}

/// The `f32` a C function returned in the low half of its float register.
pub fn f32_of(r: f64) -> f32 {
    f32::from_bits(r.to_bits() as u32)
}
