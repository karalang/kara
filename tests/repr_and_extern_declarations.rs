//! B-2026-11-19-55: `#[repr(transparent)]` accepted a zero-sized neighbour
//! aligned above one byte (`Array[u64, 0]`), and the wrapper took its
//! alignment, so it no longer had its field's layout. Such a neighbour is now
//! refused, as Rust refuses it.
//!
//! B-2026-11-19-56: an `extern` block's ABI string and its by-value parameter
//! and result types were checked only at a call ("call of unknown function",
//! E0516 "not lowered yet"). Both are now refused where they are written.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

const DOC: &str = "/// x.\n///\n/// # Safety\n///\n/// x.\n";

/// `karac check` on `src`: (success, stderr).
fn check(src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-repr-extern-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", f.to_str().unwrap()])
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

#[test]
fn transparent_refuses_an_over_aligned_zero_sized_neighbour() {
    let (ok, text) = check(
        "#[repr(transparent)]\nstruct Q { raw: i32, a: Array[u64, 0] }\nfn main() { println(Q.size_of()); }\n",
    );
    assert!(!ok, "{text}");
    assert!(
        text.contains("E_REPR_TRANSPARENT_NEIGHBOUR_ALIGN"),
        "{text}"
    );
    assert!(text.contains("'a'"), "{text}");
    // A non-empty array is a second data field, not a zero-sized neighbour:
    // the library's `Array` entry has no fields, and it read as an empty
    // unit struct.
    let (ok, text) = check(
        "#[repr(transparent)]\nstruct Q { raw: i32, a: Array[u64, 4] }\nfn main() { println(Q.size_of()); }\n",
    );
    assert!(!ok, "{text}");
    assert!(
        text.contains("E_REPR_TRANSPARENT_REQUIRES_SINGLE_FIELD"),
        "{text}"
    );
}

#[test]
fn transparent_keeps_alignment_one_neighbours() {
    for neighbour in ["Array[u8, 0]", "()", "Array[bool, 0]", "E"] {
        let src = format!(
            "struct E {{}}\n#[repr(transparent)]\nstruct Q {{ raw: i32, a: {neighbour} }}\nfn main() {{ println(Q.size_of()); }}\n"
        );
        let (ok, text) = check(&src);
        assert!(ok, "{neighbour}: {text}");
    }
}

#[test]
fn extern_block_refuses_an_abi_other_than_c() {
    for abi in ["system", "Rust"] {
        let src = format!(
            "{DOC}unsafe extern \"{abi}\" {{\n    pure fn abs(x: i32) -> i32;\n}}\nfn main() {{}}\n"
        );
        let (ok, text) = check(&src);
        assert!(!ok, "{abi}: {text}");
        assert!(
            text.contains(&format!("unsupported ABI \"{abi}\"")),
            "{text}"
        );
    }
    let src = format!(
        "{DOC}unsafe extern \"C\" {{\n    pure fn abs(x: i32) -> i32;\n}}\nfn main() {{}}\n"
    );
    let (ok, text) = check(&src);
    assert!(ok, "{text}");
}

#[test]
fn extern_declaration_refuses_a_type_that_does_not_cross() {
    for (decl, says) in [
        ("fn puts(s: String) -> i32;", "take `*const u8`"),
        ("pure fn abs(x: M) -> i32;", "has no `#[repr]`"),
        ("pure fn abs(x: P) -> i32;", "carries data"),
        (
            "pure fn pair(x: i32) -> (i32, i32);",
            "a tuple has no C layout",
        ),
        (
            "pure fn first(v: Vec[i32]) -> i32;",
            "`Vec` is a Kāra library type",
        ),
    ] {
        let src = format!(
            "enum M {{ A, B, C }}\nenum P {{ A(i32), B }}\n{DOC}unsafe extern \"C\" {{\n    {decl}\n}}\nfn main() {{}}\n"
        );
        let (ok, text) = check(&src);
        assert!(!ok, "{decl}: {text}");
        assert!(
            text.contains("E_EXTERN_TYPE_NOT_FFI_SAFE"),
            "{decl}: {text}"
        );
        assert!(text.contains(says), "{decl}: {text}");
    }
}

#[test]
fn extern_declaration_keeps_types_that_cross() {
    let src = format!(
        "#[repr(i32)]\nenum M {{ A, B, C }}\n#[repr(C)]\nstruct S {{ x: i32, y: f64 }}\n{DOC}unsafe extern \"C\" {{\n    pure fn f(a: M, b: S, c: char, d: bool, e: *const u8) -> M;\n}}\nfn main() {{}}\n"
    );
    let (ok, text) = check(&src);
    assert!(ok, "{text}");
}

/// B-2026-11-19-138 (review ruling on -56): a by-value struct crosses only
/// with `#[repr(C)]` (or `transparent`); without it its field order and
/// padding are unspecified. The refusal carries a fix inserting `#[repr(C)]`.
#[test]
fn extern_declaration_refuses_a_struct_without_repr_c() {
    let src = format!(
        "struct P {{ x: i32, y: f64 }}\n{DOC}unsafe extern \"C\" {{\n    pure fn f(p: P) -> i32;\n}}\nfn main() {{}}\n"
    );
    let (ok, text) = check(&src);
    assert!(!ok, "{text}");
    assert!(text.contains("E_EXTERN_TYPE_NOT_FFI_SAFE"), "{text}");
    assert!(
        text.contains("the struct `P` has no `#[repr(C)]`"),
        "{text}"
    );

    let dir = std::env::temp_dir().join(format!(
        "karac-repr-extern-fix-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, &src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["fix", f.to_str().unwrap()])
        .output()
        .expect("karac");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let fixed = std::fs::read_to_string(&f).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(fixed.starts_with("#[repr(C)]\nstruct P {"), "{fixed}");
    let (ok, text) = check(&fixed);
    assert!(ok, "{text}");
}
