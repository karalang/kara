//! Rows found by the sentence-map audits of design.md §3 (Lexical
//! structure) and §15 (Unsafe, FFI and layout control), 2026-11-25. Each
//! test runs the cell its row cites, from `conformance/design/lexical/` or
//! `conformance/design/ffi/`, against the expectation written from the page.

use std::path::{Path, PathBuf};
use std::process::Command;

fn cell(dir: &str, name: &str) -> (String, String) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("conformance/design")
        .join(dir);
    let src = std::fs::read_to_string(root.join(format!("{name}.kara"))).unwrap();
    let want = std::fs::read_to_string(root.join(format!("{name}.out"))).unwrap_or_default();
    (src, want)
}

fn karac(tag: &str, args: &[&str], src: &str) -> (bool, String, String) {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "karac-lex-ffi-{tag}-{}-{}",
        args[0].trim_start_matches('_'),
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("main.kara")
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The cell prints its `.out` on the interpreter and, with the backend,
/// compiled.
fn runs(dir: &str, name: &str) {
    let (src, want) = cell(dir, name);
    let mut modes = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        modes.push("__mir-llvm-run");
    }
    for m in modes {
        let (ok, out, err) = karac(name, &[m], &src);
        assert!(ok, "{name} {m}: {err}");
        assert_eq!(out, want, "{name} {m}");
    }
}

/// `karac check` output of the cell (stdout and stderr).
fn check(dir: &str, name: &str) -> String {
    let (src, _) = cell(dir, name);
    let (_, out, err) = karac(name, &["check"], &src);
    out + &err
}

/// B-2026-11-25-3: `r#true` names a binding; bare `true` stays the literal.
#[test]
fn raw_true_binding_leaves_the_literal() {
    runs("lexical", "raw_ident_true_false_literals_still_literals");
}

/// B-2026-11-25-4: `const n` is refused, and the help names `N` (CN-7).
#[test]
fn one_letter_const_help_names_the_capital() {
    let out = check("lexical", "cn7_one_letter_lowercase_const");
    assert!(out.contains("consider renaming to `N`"), "{out}");
    assert!(
        !out.contains("no capitalization change can make it"),
        "{out}"
    );
}

/// B-2026-11-25-5: the byte-literal `\u{...}` error is the page's sentence.
#[test]
fn byte_unicode_escape_error_is_the_pages_sentence() {
    let out = check("lexical", "byte_unicode_escape_full_message");
    assert!(
        out.contains(
            "Unicode escapes are not permitted in byte literals; use \\xFF for byte 0xFF."
        ),
        "{out}"
    );
}

/// B-2026-11-25-6: `String.from_utf8` takes a `b"..."` array by value.
#[test]
fn from_utf8_takes_a_byte_string_by_value() {
    runs("lexical", "byte_string_from_utf8");
}

/// B-2026-11-25-7: a `c"..."` literal's NUL follows its bytes.
#[test]
fn c_string_literal_ends_in_a_nul() {
    runs("lexical", "c_string_nul_terminated");
}

/// B-2026-11-25-8: on native a `host fn` is an `extern "C"` call.
#[test]
fn host_fn_on_native_calls_c() {
    runs("ffi", "host_fn_native_lowers_to_c");
}

/// B-2026-11-25-13: a derive on an opaque type is a trait impl, refused as
/// one (design.md §15, 6812).
#[test]
fn opaque_type_derive_is_refused() {
    let out = check("ffi", "opaque_derive_refused");
    assert!(
        out.contains("E_OPAQUE_TYPE_NO_INHERENT_OR_TRAIT_IMPLS"),
        "{out}"
    );
}

/// B-2026-11-25-13: a struct pattern over an opaque type is field access by
/// another name, refused with the field code (design.md §15, 6811).
#[test]
fn opaque_type_struct_pattern_is_refused() {
    let out = check("ffi", "opaque_pattern_match_refused");
    assert!(out.contains("E_OPAQUE_TYPE_NO_FIELDS"), "{out}");
    assert!(!out.contains("E0516"), "{out}");
}

/// B-2026-11-25-10: a primitive type name cannot name an item, since
/// `u8(..)` in first position names the type (design.md §3, 224).
#[test]
fn primitive_type_name_cannot_name_a_function() {
    let out = check("lexical", "primitive_name_as_fn_refused");
    assert!(out.contains("error[E0102]"), "{out}");
    assert!(out.contains("`u8` is a primitive type name"), "{out}");
    assert!(out.contains("read_u8"), "{out}");
}

/// B-2026-11-25-10: `MutFn` and `OnceFn` are reserved like `Fn`.
#[test]
fn mutfn_and_oncefn_are_reserved_type_names() {
    for (cell_name, name) in [
        ("mutfn_type_name_reserved_enum", "MutFn"),
        ("oncefn_type_name_reserved_trait", "OnceFn"),
    ] {
        let out = check("lexical", cell_name);
        assert!(out.contains("error[E0102]"), "{out}");
        assert!(out.contains(&format!("'{name}' is reserved")), "{out}");
    }
}

/// B-2026-11-25-10, review ruling 2026-11-25: a field or method may use a
/// primitive type name, since it is reached after `.`.
#[test]
fn primitive_type_name_may_name_a_field_or_method() {
    runs("lexical", "primitive_name_as_field_or_method_allowed");
}

/// Drift (§15, 6395): `ptr.null()` with nothing to fix its pointee is an
/// inference error, not E0516 "is not lowered yet"; a later use fixes it.
#[test]
fn ptr_null_without_a_pointee_is_an_inference_error() {
    let out = check("ffi", "ptr_null_needs_an_expected_type_refused");
    assert!(
        out.contains("cannot infer type parameter 'T' of `*const T`"),
        "{out}"
    );
    assert!(!out.contains("E0516"), "{out}");
    runs("ffi", "ptr_null_pointee_fixed_by_a_later_use");
}

/// Drift (§15, 6459): `mem.uninitialized` names `MaybeUninit[T]`, as an
/// import and as a call.
#[test]
fn mem_uninitialized_points_at_maybe_uninit() {
    for name in [
        "mem_uninitialized_import_unresolved_refused",
        "mem_uninitialized_call_unresolved_refused",
    ] {
        let out = check("ffi", name);
        assert!(out.contains("MaybeUninit[T]"), "{out}");
        assert!(!out.contains("did you mean `std.web`"), "{out}");
        assert!(!out.contains("undefined name 'mem'\n"), "{out}");
    }
}
