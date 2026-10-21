//! The MIR backend's `std.crypto` digests (runtime/stdlib/std_crypto*.kara,
//! B-2026-10-15-17): the `CryptoSha2`, `CryptoHmac`, `CryptoLegacy` and
//! `CryptoCt` natives over `ring`, the same calls the interpreter makes
//! (`crypto_method` in src/mir/interp.rs), kept line for line with it.
//! `ring` comes with the `tls` feature, so the symbol lives in the full
//! archive, which `karac` links when an object references a
//! `karac_crypto_*` symbol.
//!
//! A running hash or MAC is a box whose address is its handle; `finish`
//! and `free` both end it.

/// One running hash or MAC. src/mir/interp.rs has the same type.
enum CryptoState {
    Digest(ring::digest::Context),
    Mac(ring::hmac::Context),
}

impl CryptoState {
    /// A hash of `alg` bits (160 is SHA-1), or with `key` its HMAC.
    fn start(alg: i64, key: Option<&[u8]>) -> Option<CryptoState> {
        use ring::{digest, hmac};
        Some(match (alg, key) {
            (160, None) => {
                CryptoState::Digest(digest::Context::new(&digest::SHA1_FOR_LEGACY_USE_ONLY))
            }
            (256, None) => CryptoState::Digest(digest::Context::new(&digest::SHA256)),
            (384, None) => CryptoState::Digest(digest::Context::new(&digest::SHA384)),
            (512, None) => CryptoState::Digest(digest::Context::new(&digest::SHA512)),
            (256, Some(k)) => CryptoState::Mac(hmac::Context::with_key(&hmac::Key::new(
                hmac::HMAC_SHA256,
                k,
            ))),
            (384, Some(k)) => CryptoState::Mac(hmac::Context::with_key(&hmac::Key::new(
                hmac::HMAC_SHA384,
                k,
            ))),
            (512, Some(k)) => CryptoState::Mac(hmac::Context::with_key(&hmac::Key::new(
                hmac::HMAC_SHA512,
                k,
            ))),
            _ => return None,
        })
    }

    fn update(&mut self, data: &[u8]) {
        match self {
            CryptoState::Digest(c) => c.update(data),
            CryptoState::Mac(c) => c.update(data),
        }
    }

    fn finish(self) -> Vec<u8> {
        match self {
            CryptoState::Digest(c) => c.finish().as_ref().to_vec(),
            CryptoState::Mac(c) => c.sign().as_ref().to_vec(),
        }
    }
}

/// `std.crypto.constant_time_eq`: false on unequal lengths, else every byte
/// pair is compared, with no early exit, so the time depends only on the
/// length. src/mir/interp.rs has the same function.
fn crypto_ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

/// The bytes of a `(ptr, len)` view; empty for a null pointer.
///
/// # Safety
/// `p` is null or points to `n` readable bytes.
unsafe fn bytes<'a>(p: *const u8, n: usize) -> &'a [u8] {
    if p.is_null() || n == 0 {
        return &[];
    }
    // SAFETY: the caller's contract.
    unsafe { std::slice::from_raw_parts(p, n) }
}

/// Stores a finished digest into `out`, which the library sizes to it.
///
/// # Safety
/// `out` points to `out_len` writable bytes.
unsafe fn write(out: *mut u8, out_len: usize, digest: &[u8]) {
    let n = digest.len().min(out_len);
    // SAFETY: the caller's contract; `n` is within both.
    unsafe { std::ptr::copy_nonoverlapping(digest.as_ptr(), out, n) };
}

/// The `std.crypto` natives. `op`: 0 digest `a` into `out`; 1 the MAC of
/// `b` under key `a` into `out`; 2 start a hash, 3 start a MAC under key
/// `a` (each returns its handle, 0 for an unknown `alg`); 4 add `a` to
/// handle `h`; 5 finish `h` into `out`; 6 free `h`; 7 whether `a` equals
/// `b` in constant time (1 or 0).
///
/// # Safety
///
/// `a` / `b` describe readable bytes (or are null with a zero length),
/// `out` is writable for `out_len` bytes, and `h` is 0 or a handle op 2 or
/// 3 returned that no op 5 or 6 has ended.
#[no_mangle]
pub unsafe extern "C" fn karac_crypto_mir(
    op: i32,
    alg: i64,
    a: *const u8,
    a_len: usize,
    b: *const u8,
    b_len: usize,
    h: i64,
    out: *mut u8,
    out_len: usize,
) -> i64 {
    // SAFETY: the caller's contract, for every unsafe call below.
    let (a, b) = unsafe { (bytes(a, a_len), bytes(b, b_len)) };
    let state = |h: i64| h as *mut CryptoState;
    match op {
        0 | 1 => {
            let (key, data) = if op == 1 { (Some(a), b) } else { (None, a) };
            if let Some(mut st) = CryptoState::start(alg, key) {
                st.update(data);
                unsafe { write(out, out_len, &st.finish()) };
            }
            0
        }
        2 | 3 => match CryptoState::start(alg, (op == 3).then_some(a)) {
            Some(st) => Box::into_raw(Box::new(st)) as i64,
            None => 0,
        },
        4 => {
            if h != 0 {
                unsafe { (*state(h)).update(a) };
            }
            0
        }
        5 => {
            if h != 0 {
                let st = unsafe { Box::from_raw(state(h)) };
                unsafe { write(out, out_len, &st.finish()) };
            }
            0
        }
        6 => {
            if h != 0 {
                drop(unsafe { Box::from_raw(state(h)) });
            }
            0
        }
        7 => i64::from(crypto_ct_eq(a, b)),
        _ => 0,
    }
}
