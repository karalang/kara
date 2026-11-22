//! B-2026-11-22-22: `.raw()` of a borrowed `distinct type` over a raw pointer
//! copies the pointer, as for any `Copy` base, so design.md's opaque-handle
//! `Drop` idiom checks. B-2026-11-22-23: a `break` whose value type disagrees
//! with an earlier one is reported at that `break`, not at the loop.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// `karac check main.kara` on `src`: (success, stderr).
fn check(src: &str) -> (bool, String) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-rawbrk-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "main.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

#[test]
fn raw_of_borrowed_distinct_pointer_is_the_pointer() {
    let (ok, out) = check(
        "distinct type P = *mut u8;\n\
         fn take(p: *mut u8) {}\n\
         fn f(x: P) { take(x.raw()); }\n\
         fn g(x: mut ref P) { take(x.raw()); }\n\
         struct S { p: P }\n\
         impl S { fn h(mut ref self) { take(self.p.raw()); } }\n\
         impl Drop for P { fn drop(mut ref self) { if not ptr.is_null(self.raw()) { take(self.raw()); } } }\n\
         fn main() {}\n",
    );
    assert!(ok, "{out}");
}

#[test]
fn break_type_mismatch_points_at_the_break() {
    let (ok, out) = check(
        "fn main() {\n    let a = true;\n    let z = loop {\n        if a { break 5i32; }\n        if a { break \"hello\"; }\n    };\n}\n",
    );
    assert!(!ok);
    assert!(
        out.contains("main.kara:5:16"),
        "want the second break (5:16):\n{out}"
    );
    let (ok, out) = check(
        "fn main() {\n    let a = true;\n    let z = 'b: {\n        if a { break 'b 1i32; }\n        if a { break 'b \"x\"; }\n        2i32\n    };\n}\n",
    );
    assert!(!ok);
    assert!(
        out.contains("main.kara:5:16"),
        "want the second break (5:16):\n{out}"
    );
}
