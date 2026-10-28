//! The baked stdlib (`hash.kara`, `cli.kara`, ...) is parsed past any
//! program's text, so a trait default body checked for an impl records its
//! expression types under spans no user expression has (B-2026-10-28-7).
//! Parsed at offset 0, `Hasher.write_u8`'s `self.write` typed the user's
//! expression at the same offset as the impl's type, and `karac check`
//! refused two frozen benches with false ownership errors.

use std::process::Command;

fn check(path: &str) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(path)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `b[i]` at offset 1406 read as `TableKeyBytes`, the type of a `self` in
/// hash.kara at that offset, and was refused as a move out of a `Vec[i64]`.
#[test]
fn baked_span_base_compare_version_bench_checks() {
    let (ok, err) = check(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/corpus/katas/101-200/165-compare-version-numbers/bench/compare_version/source.kara"
    ));
    assert!(ok, "check must accept:\n{err}");
}

/// A `shared struct` handle reused after `Some(node)` was refused as a use
/// after move for the same reason.
#[test]
fn baked_span_base_partition_list_bench_checks() {
    let (ok, err) = check(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/corpus/katas/1-100/86-partition-list/bench/partition_list/source.kara"
    ));
    assert!(ok, "check must accept:\n{err}");
}

/// No expression type lands inside the program's own text unless the
/// program wrote an expression there: a program whose first 64 KiB are a
/// comment has no typed expression in them, whatever the baked stdlib holds.
/// The user `impl Hasher` makes the checker type `Hasher`'s default bodies.
#[test]
fn baked_span_base_no_library_type_inside_user_text() {
    let pad = "// padding padding padding padding padding padding padding padd\n".repeat(1024);
    let src = format!(
        "{pad}struct Sum {{ mut n: u64 }}\n\
         impl Hasher for Sum {{\n\
         \x20   fn write(mut ref self, bytes: ref Slice[u8]) {{ self.n = self.n + bytes.len() as u64; }}\n\
         \x20   fn finish(ref self) -> u64 {{ self.n }}\n\
         }}\n\
         fn main() {{\n\
         \x20   let mut s = Sum {{ n: 0 }};\n\
         \x20   s.write_u8(1);\n\
         \x20   println(s.finish());\n\
         }}\n"
    );
    let mut parsed = karac::parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    // Desugaring copies `Hasher`'s default bodies into the impl.
    let comptime = karac::desugar_program(&mut parsed.program);
    assert!(comptime.is_empty(), "{comptime:?}");
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck_with_library_source(&parsed.program, &resolved);
    let inside: Vec<_> = typed
        .expr_types
        .iter()
        .filter(|(k, _)| k.0 < pad.len())
        .collect();
    assert!(
        inside.is_empty(),
        "expression types inside the padding: {inside:?}"
    );
    assert!(
        typed
            .expr_types
            .keys()
            .any(|k| k.0 >= pad.len() && k.0 < src.len()),
        "the program's own expressions must be typed"
    );
}
