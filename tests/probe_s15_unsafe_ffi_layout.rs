//! The probe's §15/§16 rows (unsafe, FFI, layout), pinned in one family:
//! each cell is the probe's own program, refused or run as the spec says.
//!
//! - X1 (B-2026-11-14-31): an `unsafe_op_in_unsafe_fn` error decides the exit status of
//!   `check`, `run` and `test` (it printed and was then ignored).
//! - U1b (B-2026-11-14-32): `ptr.mut(x)` is a mutable use of `x` for `unused_mut`.
//! - E1 (B-2026-11-14-33): an `extern` fn with no `with` clause has every effect.
//! - DE1 (B-2026-11-14-34): a call through a `dyn` value whose type writes no clause has
//!   every effect (a non-escaping `dyn` parameter is still charged to its
//!   callers).
//! - DE2 (B-2026-11-14-35): `dyn Trait with ()` parses, in a field and a return type.
//! - X2 (B-2026-11-14-36): `Vec.as_ptr()` runs.
//! - B1 (B-2026-11-14-37): `get_unchecked` returns `ref T`.
//! - R1 (B-2026-11-14-38): a C-like enum has `values()`.
//! - R2 (B-2026-11-14-39): `#[repr(...)]` arguments are checked.

use std::process::{Command, Output};

use karac::mir::lower::run_source;

fn karac(cmd: &str, src: &str) -> Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-probe-s15-{}-{:x}",
        std::process::id(),
        src.len() * 31 + src.bytes().map(usize::from).sum::<usize>()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([cmd, file.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// `karac check` on `src`: its error lines, empty when it passes. The exit
/// status must agree with what it printed.
fn check(src: &str) -> String {
    let out = karac("check", src);
    let err = String::from_utf8_lossy(&out.stderr);
    let errors: Vec<&str> = err.lines().filter(|l| l.starts_with("error")).collect();
    assert_eq!(out.status.success(), errors.is_empty(), "{err}");
    errors.join("\n")
}

fn warnings(src: &str) -> String {
    let out = karac("check", src);
    let err = String::from_utf8_lossy(&out.stderr);
    err.lines()
        .filter(|l| l.starts_with("warning"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn run(src: &str) -> String {
    assert_eq!(check(src), "", "{src}");
    let r = run_source(src).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(r.stderr, "", "{src}");
    r.output
}

fn refused(src: &str, needle: &str) {
    let err = check(src);
    assert!(
        err.contains(needle),
        "expected `{needle}` in:\n{err}\n--- {src}"
    );
}

// ── X1 ──────────────────────────────────────────────────────────────

const NULL_DEREF: &str = "fn main() { let p: *const i64 = ptr.null(); println(*p); }\n";

const DEREF_IN_UNSAFE_FN: &str = r#"
/// Reads.
///
/// # Safety
///
/// p valid.
unsafe fn rd(p: *const i64) -> i64 { *p }
fn main() { println(1); }
"#;

const CALL_FROM_SAFE: &str = r#"
/// Reads.
///
/// # Safety
///
/// p valid.
unsafe fn rd(p: *const i64) -> i64 {
    // Safety: caller.
    unsafe { *p }
}
fn main() { let x: i64 = 3; println(rd(ptr.const(x))); }
"#;

const CALL_FROM_UNSAFE_FN: &str = r#"
/// Reads.
///
/// # Safety
///
/// p valid.
unsafe fn rd(p: *const i64) -> i64 {
    // Safety: caller.
    unsafe { *p }
}
/// Reads twice.
///
/// # Safety
///
/// p valid.
unsafe fn rd2(p: *const i64) -> i64 { rd(p) + rd(p) }
fn main() { println(1); }
"#;

#[test]
fn an_unsafe_operation_outside_unsafe_fails_check() {
    for src in [
        NULL_DEREF,
        DEREF_IN_UNSAFE_FN,
        CALL_FROM_SAFE,
        CALL_FROM_UNSAFE_FN,
    ] {
        refused(src, "unsafe_op_in_unsafe_fn");
        let out = karac("check", src);
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains("All checks passed")
                && !String::from_utf8_lossy(&out.stderr).contains("All checks passed"),
            "{src}"
        );
    }
}

#[test]
fn an_unsafe_operation_outside_unsafe_is_not_run() {
    let out = karac("run", NULL_DEREF);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("unsafe_op_in_unsafe_fn"), "{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
}

#[test]
fn the_wrapped_operations_still_pass() {
    let ok = r#"
/// Reads.
///
/// # Safety
///
/// p valid.
unsafe fn rd(p: *const i64) -> i64 {
    // Safety: caller.
    unsafe { *p }
}
fn main() {
    let x: i64 = 3;
    // Safety: x is live.
    println(unsafe { rd(ptr.const(x)) });
}
"#;
    assert_eq!(run(ok), "3\n");
}

// ── U1b ─────────────────────────────────────────────────────────────

#[test]
fn ptr_mut_is_a_mutable_use() {
    let written = r#"
fn main() {
    let mut x: i64 = 5;
    let p: *mut i64 = ptr.mut(x);
    // Safety: p points at x.
    unsafe { p.write(7); }
    println(x);
}
"#;
    let only_taken = r#"
fn main() {
    let mut x: i64 = 5;
    let p: *mut i64 = ptr.mut(x);
    println(ptr.is_null(p as *const i64));
}
"#;
    for src in [written, only_taken] {
        let w = warnings(src);
        assert!(!w.contains("unused_mut"), "{w}\n--- {src}");
    }
    assert_eq!(run(written), "7\n");
    assert_eq!(run(only_taken), "false\n");
}

// ── E1 ──────────────────────────────────────────────────────────────

#[test]
fn an_extern_fn_with_no_clause_has_every_effect() {
    let no_effect = r#"
unsafe extern "C" { fn getpid() -> i32; }
#[no_effect(allocates(Heap))]
fn f() -> i32 { getpid() }
fn main() { println(f()); }
"#;
    refused(no_effect, "E_NO_EFFECT_VIOLATED");
    let clause = r#"
unsafe extern "C" { fn getpid() -> i32; }
fn f() -> i32 with reads(Env) { getpid() }
fn main() { println(f() > 0); }
"#;
    refused(clause, "has every effect");
    let public = r#"
unsafe extern "C" { fn getpid() -> i32; }
pub fn f() -> i32 { getpid() }
fn main() { println(f() > 0); }
"#;
    refused(public, "declare its effects on the extern");
}

#[test]
fn an_extern_fn_has_what_it_declares() {
    let declared = r#"
unsafe extern "C" { fn getpid() -> i32 with reads(Env); }
fn f() -> i32 with reads(Env) { getpid() }
fn main() { println(f() > 0); }
"#;
    assert_eq!(check(declared), "");
    let pure_extern = r#"
unsafe extern "C" { pure fn abs(x: i32) -> i32; }
#[no_effect(allocates(Heap))]
fn f(x: i32) -> i32 { abs(x) }
fn main() { println(f(-3)); }
"#;
    assert_eq!(check(pure_extern), "");
    let undeclared = r#"
unsafe extern "C" { fn getpid() -> i32 with writes(Stdout); }
fn f() -> i32 with reads(Env) { getpid() }
fn main() { println(f() > 0); }
"#;
    refused(undeclared, "writes(Stdout)");
}

// ── DE1 / DE2 ───────────────────────────────────────────────────────

#[test]
fn a_dyn_value_with_no_clause_has_every_effect() {
    let field = r#"
trait Sh { fn area(self) -> i64; }
struct Holder { s: dyn Sh }
#[no_effect(writes(Stdout))]
fn total(h: Holder) -> i64 { h.s.area() }
fn main() { println(1); }
"#;
    refused(field, "has every effect");
    let element = r#"
trait Sh { fn area(self) -> i64; }
#[no_effect(writes(Stdout))]
fn total(v: Vec[dyn Sh]) -> i64 { let mut t = 0; for s in v.iter() { t += s.area(); } t }
fn main() { println(1); }
"#;
    refused(element, "has every effect");
}

#[test]
fn a_non_escaping_dyn_parameter_is_charged_to_its_callers() {
    let quiet = r#"
trait Sh { fn area(self) -> i64; }
struct Sq { s: i64 }
impl Sh for Sq { fn area(self) -> i64 { self.s * self.s } }
#[no_effect(writes(Stdout))]
fn total(x: ref dyn Sh) -> i64 { x.area() }
fn main() { let q = Sq { s: 2 }; println(total(q)); }
"#;
    assert_eq!(run(quiet), "4\n");
    let loud = r#"
trait Sh { fn area(self) -> i64; }
struct Loud { s: i64 }
impl Sh for Loud { fn area(self) -> i64 { println("hi"); self.s } }
fn total(x: ref dyn Sh) -> i64 { x.area() }
#[no_effect(writes(Stdout))]
fn caller() -> i64 { let l = Loud { s: 1 }; total(l) }
fn main() { println(caller()); }
"#;
    refused(loud, "writes(Stdout)");
}

#[test]
fn the_empty_clause_on_a_dyn_type_parses_and_holds() {
    let field = r#"
trait Sh { fn area(self) -> i64; }
struct Sq { s: i64 }
impl Sh for Sq { fn area(self) -> i64 { self.s * self.s } }
struct Holder { s: dyn Sh with () }
#[no_effect(writes(Stdout))]
fn total(h: Holder) -> i64 { h.s.area() }
fn main() { let h = Holder { s: Sq { s: 3 } }; println(total(h)); }
"#;
    assert_eq!(run(field), "9\n");
    let ret = r#"
trait Sh { fn area(self) -> i64; }
struct Sq { s: i64 }
impl Sh for Sq { fn area(self) -> i64 { self.s * self.s } }
fn make() -> (dyn Sh with ()) { Sq { s: 2 } }
#[no_effect(writes(Stdout))]
fn use_it() -> i64 { make().area() }
fn main() { println(use_it()); }
"#;
    assert_eq!(run(ret), "4\n");
    let exceeds = r#"
trait Sh { fn area(self) -> i64; }
struct Loud { s: i64 }
impl Sh for Loud { fn area(self) -> i64 { println("hi"); self.s } }
struct Holder { s: dyn Sh with () }
fn main() { let h = Holder { s: Loud { s: 3 } }; println(h.s.area()); }
"#;
    refused(exceeds, "dyn Sh with ()");
}

// ── X2 / B1 ─────────────────────────────────────────────────────────

#[test]
fn vec_as_ptr_runs() {
    let src = r#"
fn main() {
    let n: *const i64 = ptr.null();
    let v: Vec[i64] = [10, 20, 30].to_vec();
    let p = v.as_ptr();
    // Safety: index 2 is in bounds of v.
    let x = unsafe { *p.offset(2) };
    let mut w: Vec[i64] = [1, 2, 3].to_vec();
    let q: *mut i64 = ptr.mut(w[1]);
    // Safety: q points at w[1].
    unsafe { q.write(x * 2); }
    println(f"{ptr.is_null(n)} {x} {w[1]}");
}
"#;
    assert_eq!(run(src), "true 30 60\n");
}

#[test]
fn get_unchecked_returns_a_view() {
    let deref = r#"
fn sum_strided(xs: Slice[i64], stride: i64, n: i64) -> i64 {
    let mut total: i64 = 0;
    for k in 0..n {
        // Safety: the caller guarantees k * stride < xs.len() for every k in 0..n
        unsafe { total += *xs.get_unchecked(k * stride); }
    }
    total
}
fn main() { let v: Vec[i64] = [1, 2, 3, 4, 5].to_vec(); println(sum_strided(v.as_slice(), 2, 3)); }
"#;
    assert_eq!(run(deref), "9\n");
    let on_vec = r#"
fn main() {
    let v: Vec[String] = ["a".to_string(), "b".to_string()].to_vec();
    // Safety: 1 < v.len().
    let s: ref String = unsafe { v.get_unchecked(1) };
    println(s.len());
}
"#;
    assert_eq!(run(on_vec), "1\n");
}

// ── R1 ──────────────────────────────────────────────────────────────

#[test]
fn a_c_like_enum_has_values() {
    let src = r#"
#[repr(u8)]
enum UsbClass { Audio = 0x01, Hid = 0x03, MassStorage = 0x08 }
enum Plain { A, B }
fn nm(c: UsbClass) -> String { match c { UsbClass.Audio => "Audio".to_string(), UsbClass.Hid => "Hid".to_string(), UsbClass.MassStorage => "MassStorage".to_string() } }
fn all() -> Slice[UsbClass] { UsbClass.values() }
fn main() {
    let mut o: Vec[String] = Vec.new();
    for c in UsbClass.values() { o.push(f"{c.discriminant()}:{nm(c)}"); }
    println(o.join(" "));
    println(f"{all().len()} {Plain.values().len()} {UsbClass.values()[2].discriminant()}");
}
"#;
    assert_eq!(run(src), "1:Audio 3:Hid 8:MassStorage\n3 2 8\n");
}

#[test]
fn a_payload_enum_has_no_values() {
    refused(
        "enum S { Circle { r: f64 }, Dot }\nfn main() { println(S.values().len()); }\n",
        "no finite set of values",
    );
}

#[test]
fn a_user_values_function_wins() {
    let src = r#"
enum E { A, B }
impl E { fn values() -> i64 { 7 } }
fn main() { println(E.values()); }
"#;
    assert_eq!(run(src), "7\n");
}

// ── R2 ──────────────────────────────────────────────────────────────

#[test]
fn repr_arguments_are_checked() {
    for (src, needle) in [
        (
            "#[repr(packed)]\nstruct W { v: i64 }\nfn main() { println(1); }\n",
            "E_REPR_DEFERRED",
        ),
        (
            "#[repr(align(8))]\nstruct W { v: i64 }\nfn main() { println(1); }\n",
            "E_REPR_DEFERRED",
        ),
        (
            "#[repr(u128)]\nstruct W { v: i64 }\nfn main() { println(1); }\n",
            "E_REPR_INT_ON_STRUCT",
        ),
        (
            "#[repr(frob)]\nstruct W { v: i64 }\nfn main() { println(1); }\n",
            "E_REPR_UNKNOWN",
        ),
        (
            "#[repr(u128)]\nenum E { A, B }\nfn main() { println(1); }\n",
            "E_REPR_UNKNOWN",
        ),
        (
            "#[repr(packed)]\nenum E { A, B }\nfn main() { println(1); }\n",
            "E_REPR_DEFERRED",
        ),
    ] {
        refused(src, needle);
    }
}

#[test]
fn the_v1_repr_forms_pass() {
    let src = r#"
#[repr(C)]
struct D { length: u8, kind: u8, bcd: u16, cls: u8 }
#[repr(transparent)]
struct W { v: i64 }
#[repr(u8)]
enum K { A = 1, B = 2 }
#[repr(i64)]
enum L { A, B }
#[repr(C)]
enum M { A, B }
fn main() { let d = D { length: 6, kind: 1, bcd: 0x0200, cls: 2 }; let w = W { v: 4 }; println(f"{d.length} {d.cls} {w.v} {K.B.discriminant()}"); }
"#;
    assert_eq!(run(src), "6 2 4 2\n");
}
