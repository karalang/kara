//! A Rust-spelled receiver or borrow type gets a named parse error with the
//! machine edit to the Kāra spelling (B-2026-11-01-5): a writing receiver is
//! `mut ref self`, a borrowed one is `self`, an owned one is `own self`, and
//! in type position `&mut T` is `mut ref T` and `&T` is `ref T`. Applying the
//! edit leaves a program that checks.

fn apply(src: &str, fixes: &[karac::SourceFix]) -> String {
    let mut out = src.to_string();
    let mut fixes = fixes.to_vec();
    fixes.sort_by_key(|f| std::cmp::Reverse(f.offset));
    for f in fixes {
        out.replace_range(f.offset..f.offset + f.length, &f.replacement);
    }
    out
}

/// The one parse error for `src`, and `src` with its edit applied.
fn one_fix(src: &str) -> (String, String) {
    let diags = karac::check_source(src);
    assert_eq!(diags.len(), 1, "{diags:?}");
    let d = &diags[0];
    assert_eq!(d.phase, "parse", "{d:?}");
    assert_eq!(d.fixes.len(), 1, "{d:?}");
    let fixed = apply(src, &d.fixes);
    let after = karac::check_source(&fixed);
    assert!(after.is_empty(), "{fixed}\n{after:?}");
    (d.message.clone(), fixed)
}

fn method(receiver: &str) -> String {
    format!(
        "struct S {{ x: i64 }}\nimpl S {{\n    fn bump({receiver}) -> i64 {{\n        self.x\n    }}\n}}\nfn main() {{}}\n"
    )
}

#[test]
fn rust_receivers_get_a_named_fix() {
    for (rust, kara, says) in [
        ("mut self", "mut ref self", "`mut self` is Rust syntax"),
        ("&mut self", "mut ref self", "`&mut self` is Rust syntax"),
        (
            "ref mut self",
            "mut ref self",
            "`ref mut self` is Rust syntax",
        ),
        (
            "self: mut ref Self",
            "mut ref self",
            "a receiver's mode goes before `self`",
        ),
        ("&self", "self", "a bare `self` already borrows"),
        ("self: Self", "own self", "write `own self`"),
    ] {
        let (message, fixed) = one_fix(&method(rust));
        assert!(message.contains(says), "{rust}: {message}");
        assert_eq!(fixed, method(kara), "{rust}");
    }
}

#[test]
fn rust_borrow_types_get_a_named_fix() {
    let src = "fn f(v: &mut Vec[i64]) {\n    v.push(1);\n}\nfn main() {}\n";
    let (message, fixed) = one_fix(src);
    assert!(message.contains("`&mut T` is Rust syntax"), "{message}");
    assert!(fixed.starts_with("fn f(v: mut ref Vec[i64])"), "{fixed}");
    let src = "fn g(w: &Vec[i64]) -> i64 {\n    w.len()\n}\nfn main() {}\n";
    let (message, fixed) = one_fix(src);
    assert!(message.contains("`&T` is Rust syntax"), "{message}");
    assert!(fixed.starts_with("fn g(w: ref Vec[i64])"), "{fixed}");
}
