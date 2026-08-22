//! Unicode normalization FFI — the AOT backend for `String.normalize(form)`
//! (design.md § Strings, Equality). Gated behind the opt-in `unicode` feature,
//! which is built into `libkarac_runtime_unicode.a` and auto-selected by
//! `karac` whenever the emitted object references a `karac_unicode_*` symbol —
//! mirroring the opt-in `regex` / `arrow` archives so the ICU normalization
//! tables (~100 KB) never touch the lean/full/wasm archives.
//!
//! **The interpreter links the same `icu_normalizer` version** (it is already
//! in `karac`'s dependency graph via `ureq → url → idna → idna_adapter`), so
//! the two backends agree by construction rather than by convention — the same
//! reason the Arrow IPC twin is byte-identical, and the reason
//! `karac_string_to_lowercase` exists at all instead of open-coding case
//! mapping in codegen. A test asserts the equality on all four forms.
//!
//! Unlike case mapping, normalization is NOT available from `std`: Rust ships
//! case-mapping tables but no normalization tables. That is the whole reason
//! this archive is a separate artifact and `to_lowercase` is not.

use icu_normalizer::{ComposingNormalizerBorrowed, DecomposingNormalizerBorrowed};
use std::borrow::Cow;

/// Normalization form selector. The integer values are the discriminants of
/// `runtime/stdlib/normalization_form.kara`'s `NormalizationForm` enum in
/// declaration order, which is what codegen passes and what the interpreter
/// mirrors — keep the three in step.
const NFC: i32 = 0;
const NFD: i32 = 1;
const NFKC: i32 = 2;
const NFKD: i32 = 3;
// (Rust consts are SCREAMING_SNAKE by its own convention; the Kāra variants
// they mirror are `Nfc`/`Nfd`/`Nfkc`/`Nfkd` per design.md CN-4.)

/// `String.normalize(form)` — return a fresh owned buffer holding the
/// normalized text. Normalization can change the byte length in either
/// direction (`é` NFC→NFD grows 2→3; NFD→NFC shrinks 3→2), so the result is
/// always a new allocation and never an in-place edit of the receiver.
///
/// An unrecognized `form` cannot arise from a well-typed program — the
/// parameter is a four-variant enum — so it degrades to returning the input
/// unchanged rather than trapping, matching the "no match / invalid" posture
/// the regex entrypoints take on impossible input.
///
/// # Safety
/// `data`/`len` are a Kāra String body; `out_len` must be writable. See
/// [`crate::clone::alloc_string_result`], whose allocation contract this shares
/// (NUL-terminated, `null` + `0` for an empty result).
#[no_mangle]
pub unsafe extern "C" fn karac_unicode_normalize(
    data: *const u8,
    len: i64,
    form: i32,
    out_len: *mut i64,
) -> *mut u8 {
    unsafe {
        let s = crate::clone::str_from_raw(data, len);
        let normalized = normalize_str(s, form);
        crate::clone::alloc_string_result(normalized.as_bytes(), out_len)
    }
}

/// The pure half of [`karac_unicode_normalize`], shared with the unit tests.
/// Returns ICU's `Cow`: already-normalized input (the overwhelmingly common
/// case, since most text is NFC already) borrows instead of allocating, and the
/// caller copies out of it either way when it builds the Kāra String body.
fn normalize_str(s: &str, form: i32) -> Cow<'_, str> {
    match form {
        NFC => ComposingNormalizerBorrowed::new_nfc().normalize(s),
        NFD => DecomposingNormalizerBorrowed::new_nfd().normalize(s),
        NFKC => ComposingNormalizerBorrowed::new_nfkc().normalize(s),
        NFKD => DecomposingNormalizerBorrowed::new_nfkd().normalize(s),
        _ => Cow::Borrowed(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact hazard design.md's Equality bullet warns about: `e` +
    /// COMBINING ACUTE (NFD, 3 bytes) and `é` (NFC, 2 bytes) are different
    /// byte strings that normalize to one another.
    #[test]
    fn nfc_and_nfd_round_trip_the_spec_hazard() {
        let nfd = "e\u{0301}";
        let nfc = "\u{00e9}";
        assert_ne!(nfd, nfc, "the hazard itself: raw bytes differ");
        assert_eq!(normalize_str(nfd, NFC), nfc);
        assert_eq!(normalize_str(nfc, NFD), nfd);
        assert_eq!(normalize_str(nfd, NFC), normalize_str(nfc, NFC));
    }

    /// The compatibility forms differ from the canonical ones: U+FB01 LATIN
    /// SMALL LIGATURE FI is unchanged by NFC/NFD and becomes `fi` under
    /// NFKC/NFKD. Without this, a K/non-K mix-up would pass every canonical
    /// test.
    #[test]
    fn compatibility_forms_decompose_where_canonical_ones_do_not() {
        let ligature = "\u{FB01}";
        assert_eq!(normalize_str(ligature, NFC), ligature);
        assert_eq!(normalize_str(ligature, NFD), ligature);
        assert_eq!(normalize_str(ligature, NFKC), "fi");
        assert_eq!(normalize_str(ligature, NFKD), "fi");
    }

    /// An out-of-range selector is unreachable from a well-typed program;
    /// pin the degradation so a future discriminant renumber fails loudly in
    /// the tests rather than silently normalizing to the wrong form.
    #[test]
    fn an_unknown_form_returns_the_input_unchanged() {
        assert_eq!(normalize_str("e\u{0301}", 99), "e\u{0301}");
    }

    #[test]
    fn empty_and_ascii_are_unchanged_by_every_form() {
        for form in [NFC, NFD, NFKC, NFKD] {
            assert_eq!(normalize_str("", form), "");
            assert_eq!(normalize_str("plain ascii", form), "plain ascii");
        }
    }
}
