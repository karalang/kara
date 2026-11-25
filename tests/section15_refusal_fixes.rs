//! Three rows from the §3 and §15 sentence maps (conformance/design):
//!
//! - B-2026-11-25-9: a keyword names an associated function directly after
//!   `.`, as it names a method, so `Handle.type()` calls it (design.md §3).
//! - B-2026-11-25-15: `#[allow(undocumented_unsafe)]` silences the warning
//!   at the block and module levels, not only on the function (design.md
//!   § `unsafe` blocks, "Suppression").
//! - B-2026-11-25-16: the three §15 refusals that promise a fix carry it,
//!   and `karac fix` writes what the page says.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

fn karac(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn cell(src: &str) -> PathBuf {
    let n = CALL.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-s15-fixes-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    dir
}

/// `karac check` output and what `__mir-run` prints.
fn check_and_run(src: &str) -> (String, String) {
    let dir = cell(src);
    let (_, check) = karac(&dir, &["check", "cell.kara"]);
    let (_, run) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (check, run)
}

/// The text `karac fix` leaves, and the check of it.
fn fixed(src: &str) -> (String, String) {
    let dir = cell(src);
    karac(&dir, &["fix", "cell.kara"]);
    let text = std::fs::read_to_string(dir.join("cell.kara")).unwrap();
    let (_, check) = karac(&dir, &["check", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (text, check)
}

#[test]
fn s15_keyword_assoc_fn_called_bare() {
    let (check, run) = check_and_run(
        "struct Handle { v: i64 }
impl Handle {
    fn type() -> Handle { Handle { v: 7 } }
    fn match(x: i64) -> i64 { x + 1 }
}
fn main() {
    let h = Handle.type();
    println(h.v);
    println(Handle.match(2));
}
",
    );
    assert!(!check.contains("error"), "{check}");
    assert_eq!(run, "7\n3\n");
}

#[test]
fn s15_keyword_after_a_value_root_is_still_its_method() {
    // `None.or(..)` and a constant's `.or(..)` read the value's method, as
    // before the path parser took keywords.
    let (check, run) = check_and_run(
        "const DEFAULT: Option[i64] = None;

fn main() {
    let a: Option[i64] = None.or(Some(1));
    println(a.unwrap());
    println(DEFAULT.or(Some(2)).unwrap());
}
",
    );
    assert!(!check.contains("error"), "{check}");
    assert_eq!(run, "1\n2\n");
}

const UNDOCUMENTED: &str = "    let x: i64 = 3;
    let p: *const i64 = ptr.const(x);
";

#[test]
fn s15_allow_undocumented_unsafe_on_block() {
    let (check, _) = check_and_run(&format!(
        "fn main() {{\n{UNDOCUMENTED}    #[allow(undocumented_unsafe)]\n    unsafe {{ println(f\"{{*p}}\"); }}\n}}\n"
    ));
    assert!(!check.contains("undocumented_unsafe"), "{check}");
    // The control: without the attribute, the block warns.
    let (check, _) = check_and_run(&format!(
        "fn main() {{\n{UNDOCUMENTED}    unsafe {{ println(f\"{{*p}}\"); }}\n}}\n"
    ));
    assert!(check.contains("warning[undocumented_unsafe]"), "{check}");
}

#[test]
fn s15_allow_undocumented_unsafe_on_module() {
    let (check, run) = check_and_run(&format!(
        "#![allow(undocumented_unsafe)]\nfn main() {{\n{UNDOCUMENTED}    let v = unsafe {{ *p }};\n    println(f\"{{v}}\");\n}}\n"
    ));
    assert!(!check.contains("undocumented_unsafe"), "{check}");
    assert_eq!(run, "3\n");
}

#[test]
fn s15_deny_on_a_block_wins_over_the_module_allow() {
    let (check, _) = check_and_run(&format!(
        "#![allow(undocumented_unsafe)]\nfn main() {{\n{UNDOCUMENTED}    #[deny(undocumented_unsafe)]\n    unsafe {{ println(f\"{{*p}}\"); }}\n}}\n"
    ));
    assert!(check.contains("error[undocumented_unsafe]"), "{check}");
}

#[test]
fn s15_allow_undocumented_unsafe_on_an_impl_method_still_applies() {
    let (check, _) = check_and_run(
        "struct S { x: i64 }
impl S {
    #[allow(undocumented_unsafe)]
    fn get(self) -> i64 {
        let p: *const i64 = ptr.const(self.x);
        unsafe { *p }
    }
}
fn main() { println(S { x: 4 }.get()); }
",
    );
    assert!(!check.contains("undocumented_unsafe"), "{check}");
}

#[test]
fn s15_ref_to_const_ptr_cast_takes_ptr_const() {
    let (text, check) = fixed(
        "fn addr(r: ref i64) -> *const i64 { r as *const i64 }
fn main() {
    let x: i64 = 1;
    let p = addr(x);
}
",
    );
    assert_eq!(
        text,
        "fn addr(r: ref i64) -> *const i64 { ptr.const(r) }
fn main() {
    let x: i64 = 1;
    let p = addr(x);
}
"
    );
    assert!(!check.contains("error"), "{check}");
}

#[test]
fn s15_pointer_to_integer_casts_take_ptr_addr() {
    let (text, check) = fixed(
        "fn main() {
    let mut x: i64 = 1;
    let p: *const i64 = ptr.const(x);
    let a = p as usize;
    let b = p as u64;
    let r: mut ref i64 = mut ref x;
    let q: *mut i64 = r as *mut i64;
    println(a > 0 and b > 0);
}
",
    );
    assert_eq!(
        text,
        "fn main() {
    let mut x: i64 = 1;
    let p: *const i64 = ptr.const(x);
    let a = ptr.addr(p);
    let b = ptr.addr(p) as u64;
    let r: mut ref i64 = mut ref x;
    let q: *mut i64 = ptr.mut(r);
    println(a > 0 and b > 0);
}
"
    );
    assert!(!check.contains("error"), "{check}");
}

#[test]
fn s15_ref_cast_to_another_pointee_keeps_the_message_alone() {
    // `ptr.const(r)` would be a `*const i64`, not the `*const u8` asked for.
    let dir = cell(
        "fn addr(r: ref i64) -> *const u8 { r as *const u8 }
fn main() {}
",
    );
    let (_, json) = karac(&dir, &["check", "--output=json", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    let doc: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    let d = doc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["code"] == "E_REF_TO_RAW_PTR_CAST_FORBIDDEN")
        .unwrap_or_else(|| panic!("{json}"))
        .clone();
    assert!(
        d.get("fix_diff").is_none_or(|f| f.is_null()),
        "no fix for another pointee: {d}"
    );
}

#[test]
fn s15_c_unwind_import_takes_c() {
    let (text, check) = fixed(
        "/// libc.
///
/// # Safety
///
/// The declarations match glibc.
unsafe extern \"C-unwind\" {
    pure fn labs(x: i64) -> i64;
    pure fn abs(x: i32) -> i32;
}
fn main() { println(f\"{labs(-1)}\"); }
",
    );
    assert_eq!(
        text,
        "/// libc.
///
/// # Safety
///
/// The declarations match glibc.
unsafe extern \"C\" {
    pure fn labs(x: i64) -> i64;
    pure fn abs(x: i32) -> i32;
}
fn main() { println(f\"{labs(-1)}\"); }
"
    );
    assert!(!check.contains("error"), "{check}");
}
