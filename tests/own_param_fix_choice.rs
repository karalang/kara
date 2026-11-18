//! A bare parameter consumed by a by-value method (`v.into_iter()`) is fixed
//! with `own` when no caller would break, and with `.clone()` otherwise
//! (B-2026-11-18-31). It used to be `.clone()` whenever the type had one: an
//! O(n) copy per call where the Rust it came from (`v: Vec<i32>`) moved for
//! free. `own` is right when each caller passes an owned value it does not use
//! again, or there is no caller; a caller that reuses its value, or passes a
//! value it only borrows, keeps the `.clone()`.

use karac::{check_source, run_playground};

const BODY: &str = "fn total(v: Vec[i32]) -> i32 {\n    v.into_iter().map(|x| x * 2).sum()\n}\n";

/// `src` with the one E0515 fix `karac check` offers applied; the result
/// checks clean and prints `want`.
fn fixed(src: &str, want: &str) -> String {
    let mut fixes: Vec<_> = check_source(src)
        .into_iter()
        .filter(|d| d.code.as_deref() == Some("E0515"))
        .flat_map(|d| d.fixes)
        .collect();
    assert_eq!(fixes.len(), 1, "{src}\n{fixes:#?}");
    fixes.sort_by_key(|f| std::cmp::Reverse(f.offset));
    let mut out = src.to_string();
    for f in fixes {
        out.replace_range(f.offset..f.offset + f.length, &f.replacement);
    }
    let after = run_playground(&out);
    assert!(after.ok, "{out}\n{:#?}", after.diagnostics);
    assert_eq!(after.stdout.concat(), want, "{out}");
    out
}

#[test]
fn consumed_param_takes_own_when_its_caller_hands_over_a_value() {
    let src = format!(
        "{BODY}fn main() {{\n    let v: Vec[i32] = [1, 2, 3];\n    println(total(v));\n}}\n"
    );
    let out = fixed(&src, "12\n");
    assert!(out.contains("fn total(v: own Vec[i32])"), "{out}");
    assert!(!out.contains(".clone()"), "{out}");
}

#[test]
fn consumed_param_takes_own_with_no_caller() {
    let src = "struct Solution {}\nimpl Solution {\n    pub fn total(v: Vec[i32]) -> i32 { \
               v.into_iter().map(|x| x * 2).sum() }\n}\nfn main() { println(12); }\n";
    let out = fixed(src, "12\n");
    assert!(out.contains("pub fn total(v: own Vec[i32])"), "{out}");
}

#[test]
fn consumed_param_keeps_clone_when_a_caller_uses_its_value_again() {
    let src = format!(
        "{BODY}fn main() {{\n    let v: Vec[i32] = [1, 2, 3];\n    println(total(v));\n    \
         println(v.len());\n}}\n"
    );
    let out = fixed(&src, "12\n3\n");
    assert!(out.contains("v.clone().into_iter()"), "{out}");
    assert!(!out.contains("own"), "{out}");
}

#[test]
fn consumed_param_keeps_clone_when_a_caller_passes_a_borrow() {
    let src = format!(
        "{BODY}fn outer(w: Vec[i32]) -> i32 {{ total(w) }}\nfn main() {{\n    \
         println(outer([1, 2, 3]));\n}}\n"
    );
    let out = fixed(&src, "12\n");
    assert!(out.contains("v.clone().into_iter()"), "{out}");
}
