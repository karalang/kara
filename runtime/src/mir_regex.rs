//! The MIR backend's `std.regex` engine (runtime/stdlib/source/regex.kara,
//! B-2026-10-14-63): `RegexEngine`'s natives over the `regex` crate, the
//! same calls the interpreter makes (`regex_method` in src/mir/interp.rs),
//! kept line for line with it. Opt-in `regex` feature, so the symbol lives
//! in `libkarac_runtime_regex.a`, which `karac` links when an object
//! references a `karac_regex_*` symbol.
//!
//! A pattern is compiled once per thread and kept, so a `Regex` value is
//! only its pattern text.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::mir::RawVec;
use crate::mir_io::string_of;

thread_local! {
    static COMPILED: RefCell<HashMap<String, Option<regex::Regex>>> = RefCell::new(HashMap::new());
}

/// The text of a `(ptr, len)` view; "" for a null pointer or bytes that
/// are not UTF-8, which a Kāra `Str` never holds.
///
/// # Safety
/// `p` is null or points to `n` readable bytes.
unsafe fn text<'a>(p: *const u8, n: usize) -> &'a str {
    if p.is_null() || n == 0 {
        return "";
    }
    // SAFETY: the caller's contract.
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(p, n) }).unwrap_or("")
}

fn compiled(pattern: &str) -> Option<regex::Regex> {
    COMPILED.with(|c| {
        c.borrow_mut()
            .entry(pattern.to_string())
            .or_insert_with(|| regex::Regex::new(pattern).ok())
            .clone()
    })
}

/// `RegexEngine.find_from`: the first match at byte `at` or later, as the
/// offsets of the first `n / 2` groups (-1 for a group that did not take
/// part). An empty match ending at `last_end` is skipped and the search
/// moves on one character, as the crate's iterators skip it.
fn find_from(re: &regex::Regex, s: &str, at: i64, last_end: i64, n: usize) -> Option<Vec<i64>> {
    let mut at = at;
    loop {
        if at < 0 || at as usize > s.len() || !s.is_char_boundary(at as usize) {
            return None;
        }
        let from = at as usize;
        let mut slots = vec![-1i64; n];
        let (start, end) = if n <= 2 {
            let m = re.find_at(s, from)?;
            (m.start(), m.end())
        } else {
            let caps = re.captures_at(s, from)?;
            for (g, slot) in slots.chunks_mut(2).enumerate() {
                if let (Some(m), [a, b]) = (caps.get(g), slot) {
                    (*a, *b) = (m.start() as i64, m.end() as i64);
                }
            }
            let m = caps.get(0)?;
            (m.start(), m.end())
        };
        if start == end && end as i64 == last_end {
            at += s[from..].chars().next().map_or(1, char::len_utf8) as i64;
            continue;
        }
        if n >= 2 {
            (slots[0], slots[1]) = (start as i64, end as i64);
        }
        return Some(slots);
    }
}

/// One `RegexEngine` native, by `op`: 0 `check` (the message, or "", at
/// `out`), 1 `groups` (returned), 2 `group_name` (group `a`'s name at
/// `out`), 3 `find_from` (searching `s` from `a`, the previous end `b`,
/// into the `n` slots at `slots`; 1 when found), 4 `replace` (`s` with the
/// first `a` matches, all for 0, replaced by `rep`, at `out`), 5 `escape`
/// (`pat` escaped, at `out`).
///
/// # Safety
/// Each `(ptr, len)` pair is null or readable; `slots` has room for `n`
/// `i64`s when `op` is 3; `out` is writable for a `String` when the op
/// writes one.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn karac_regex_mir(
    op: i32,
    pat: *const u8,
    pat_len: usize,
    s: *const u8,
    s_len: usize,
    rep: *const u8,
    rep_len: usize,
    a: i64,
    b: i64,
    slots: *mut i64,
    n: usize,
    out: *mut RawVec,
) -> i64 {
    // SAFETY: the caller's contract, for every unsafe call below.
    let pattern = unsafe { text(pat, pat_len) };
    match op {
        0 => {
            let message = match regex::Regex::new(pattern) {
                Ok(_) => String::new(),
                Err(e) => e.to_string(),
            };
            unsafe { string_of(out, message.as_bytes()) };
            0
        }
        1 => compiled(pattern).map_or(0, |r| r.captures_len() as i64),
        2 => {
            let name = compiled(pattern)
                .and_then(|r| {
                    r.capture_names()
                        .nth(a as usize)
                        .flatten()
                        .map(str::to_string)
                })
                .unwrap_or_default();
            unsafe { string_of(out, name.as_bytes()) };
            0
        }
        3 => {
            let s = unsafe { text(s, s_len) };
            match compiled(pattern).and_then(|r| find_from(&r, s, a, b, n)) {
                Some(found) => {
                    for (i, v) in found.into_iter().enumerate() {
                        unsafe { *slots.add(i) = v };
                    }
                    1
                }
                None => 0,
            }
        }
        4 => {
            let s = unsafe { text(s, s_len) };
            let rep = unsafe { text(rep, rep_len) };
            let replaced = match compiled(pattern) {
                Some(r) => r.replacen(s, a.max(0) as usize, rep).into_owned(),
                None => s.to_string(),
            };
            unsafe { string_of(out, replaced.as_bytes()) };
            0
        }
        _ => {
            unsafe { string_of(out, regex::escape(pattern).as_bytes()) };
            0
        }
    }
}
