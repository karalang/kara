//! VecDeque `clear`, `insert` and `try_insert` (B-2026-10-21-26),
//! `StableHash.siphash24` (B-2026-10-21-27) and the fallible twins'
//! `AllocError` (B-2026-10-21-28) on the v2 backends: each program runs on
//! the MIR interpreter and, compiled, at -O0 and -O2, with the output
//! docs/library/collections.md gives; `clear` and the twins also run under
//! the ASan lane.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "karac-mir-collections-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The program's output and exit code on the compiled backend at `opt`;
/// `None` when the runtime archive is absent.
fn build_and_run(src: &str, opt: u8) -> Option<(String, i32)> {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::codegen::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = Scratch::new();
    let obj = dir.path().join("main.o");
    let exe = dir.path().join("main");
    karac::codegen::mir_aot::compile_object(&ir, &obj, opt).expect("object");
    common::link_or_skip(karac::codegen::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ))?;
    let out = Command::new(&exe).output().expect("run");
    Some((
        String::from_utf8(out.stdout).expect("utf-8"),
        out.status.code().unwrap_or(-1),
    ))
}

/// `src` prints `want` and exits with `code` on the interpreter and,
/// compiled, at -O0 and -O2.
fn agrees(src: &str, want: &str, code: i32) {
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want, "on the interpreter");
    let aborted = matches!(interp.outcome, karac::mir::interp::Outcome::Aborted(_));
    assert_eq!(
        aborted,
        code == 101,
        "interpreter outcome {:?}",
        interp.outcome
    );
    for opt in [0, 2] {
        if let Some((out, exit)) = build_and_run(src, opt) {
            assert_eq!(out, want, "compiled at -O{opt}");
            assert_eq!(exit, code, "exit code compiled at -O{opt}");
        }
    }
}

/// The program's stdout under the ASan lane, which fails on a leak, double
/// free or use-after-free; `None` without an ASan toolchain.
fn under_asan(src: &str) -> Option<String> {
    let dir = Scratch::new();
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-llvm-asan")
        .arg(&file)
        .output()
        .expect("karac");
    let err = String::from_utf8_lossy(&out.stderr);
    if err.contains("not found") || err.contains("fsanitize") {
        eprintln!("skipping: no ASan toolchain or runtime archive: {err}");
        return None;
    }
    assert!(!err.contains("Sanitizer"), "{err}");
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `VecDeque.insert` at the front, the back and the middle, across the
/// ring's wrap; `try_insert`; `clear`, then reuse. collections.md:
/// `insert(i, x)` shifts the rest, and `clear` empties the deque.
const DEQUE: &str = r#"struct Noisy { id: i64 }
impl Drop for Noisy {
    fn drop(mut ref self) { println(f"drop {self.id}"); }
}
fn show(d: VecDeque[i64]) -> String {
    let mut out = String.new();
    let mut i = 0;
    while i < d.len() {
        match d.get(i) { Some(x) => out.push_str(f"{x} "), None => out.push_str("? ") }
        i += 1;
    }
    out
}
fn main() {
    let mut d: VecDeque[i64] = VecDeque.new();
    d.insert(0, 2);
    d.insert(0, 1);
    d.insert(2, 4);
    d.insert(2, 3);
    println(show(d));
    d.push_front(0);
    d.push_front(-1);
    d.insert(1, 9);
    println(show(d));
    match d.try_insert(7, 5) { Ok(_) => println("ok"), Err(_) => println("err") }
    println(show(d));
    d.clear();
    println(f"{d.len()} {d.is_empty()}");
    d.push_back(6);
    println(show(d));
    let mut s: VecDeque[String] = VecDeque.new();
    s.push_back("a".to_string());
    s.push_front("b".to_string());
    s.insert(1, "c".to_string());
    println(f"{s.len()}");
    s.clear();
    s.push_back("d".to_string());
    println(f"{s.len()}");
    let mut n: VecDeque[Noisy] = VecDeque.new();
    n.push_back(Noisy { id: 1 });
    n.push_back(Noisy { id: 2 });
    n.insert(1, Noisy { id: 3 });
    n.clear();
    println("cleared");
    n.push_back(Noisy { id: 4 });
    println("end");
}
"#;

const WANT_DEQUE: &str = "1 2 3 4 \n-1 9 0 1 2 3 4 \nok\n-1 9 0 1 2 3 4 5 \n0 true\n6 \n3\n1\n\
                          drop 1\ndrop 3\ndrop 2\ncleared\nend\ndrop 4\n";

#[test]
fn vecdeque_insert_and_clear_agree_on_both_backends() {
    agrees(DEQUE, WANT_DEQUE, 0);
}

/// `clear` drops what it removes: the `String` elements are freed once,
/// and the deque is usable after.
#[test]
fn vecdeque_clear_is_clean_under_asan() {
    if let Some(out) = under_asan(DEQUE) {
        assert_eq!(out, WANT_DEQUE);
    }
}

/// An index past the length panics, as `Vec.insert` does.
#[test]
fn vecdeque_insert_past_the_end_panics() {
    agrees(
        "fn main() {\n    let mut d: VecDeque[i64] = VecDeque.new();\n    d.push_back(1);\n    \
         println(\"before\");\n    d.insert(2, 5);\n    println(\"after\");\n}\n",
        "before\n",
        101,
    );
}

/// The SipHash-2-4 paper's key `00 01 .. 0f` on the empty input and on
/// `00 .. 0e`, and a zero key on a `Str`'s bytes.
const SIPHASH: &str = r#"fn content_id(payload: Str) -> u64 {
    StableHash.siphash24(payload.bytes(), 0, 0)
}
fn main() {
    let empty: Vec[u8] = Vec.new();
    println(f"{StableHash.siphash24(empty, 506097522914230528, 1084818905618843912)}");
    let fifteen: Vec[u8] = Vec.from_fn(15, |i| i as u8);
    println(f"{StableHash.siphash24(fifteen, 506097522914230528, 1084818905618843912)}");
    println(f"{content_id("x") == content_id("x")} {content_id("x") == content_id("y")}");
}
"#;

#[test]
fn stable_siphash24_agrees_on_both_backends() {
    let want = format!(
        "{}\n{}\ntrue false\n",
        karac_hash::siphash24(&[], 0x0706050403020100, 0x0f0e0d0c0b0a0908),
        karac_hash::siphash24(
            &(0u8..15).collect::<Vec<u8>>(),
            0x0706050403020100,
            0x0f0e0d0c0b0a0908
        ),
    );
    assert_eq!(
        want,
        "8246050544436514353\n11613035633349379557\ntrue false\n"
    );
    agrees(SIPHASH, &want, 0);
}

/// The fallible twins return a failure as a value (collections.md §
/// Fallible allocation, B-2026-10-21-28): a capacity that cannot be
/// represented is `CapacityOverflow`, a request the allocator refuses is
/// `OutOfMemory`, and the collection is unchanged by either.
const TRY_TWINS: &str = r#"fn show(r: Result[(), AllocError]) -> String {
    match r {
        Ok(_) => "ok".to_string(),
        Err(AllocError.CapacityOverflow) => "overflow".to_string(),
        Err(AllocError.OutOfMemory { requested_bytes }) => f"oom {requested_bytes > 0}",
    }
}
fn main() {
    let r: Result[Vec[i64], AllocError] = Vec.try_with_capacity(i64.MAX);
    match r { Ok(_) => println("ok"), Err(e) => println(f"{e}") }
    let r2: Result[Vec[i64], AllocError] = Vec.try_with_capacity(10);
    match r2 { Ok(v) => println(f"ok {v.len()} {v.capacity()}"), Err(e) => println(f"{e}") }
    let s: Result[String, AllocError] = String.try_with_capacity(i64.MAX);
    match s { Ok(_) => println("ok"), Err(e) => println(f"{e}") }
    let q: Result[VecDeque[i64], AllocError] = VecDeque.try_with_capacity(i64.MAX / 4);
    match q { Ok(_) => println("ok"), Err(e) => println(f"{e}") }
    let q2: Result[VecDeque[i64], AllocError] = VecDeque.try_with_capacity(3);
    match q2 { Ok(d) => println(f"ok {d.len()}"), Err(e) => println(f"{e}") }
    let mut v: Vec[i64] = [1, 2];
    println(show(v.try_reserve(i64.MAX)));
    println(show(v.try_reserve_exact(i64.MAX - 1)));
    println(show(v.try_reserve(1 << 58)));
    println(show(v.try_reserve(100)));
    println(show(v.try_reserve(0)));
    println(f"{v.len()} {v.capacity() >= 102}");
    v.push(3);
    println(f"{v}");
    let mut w: Vec[String] = Vec.new();
    w.push("a".to_string());
    println(show(w.try_reserve(i64.MAX / 8)));
    println(f"{w}");
    let mut t = "ab".to_string();
    println(show(t.try_reserve(i64.MAX)));
    println(show(t.try_reserve(10)));
    t.push_str("cd");
    println(t);
}
"#;

const WANT_TRY_TWINS: &str = "capacity overflow\nok 0 10\ncapacity overflow\ncapacity overflow\n\
                              ok 0\noverflow\noverflow\noverflow\nok\nok\n2 true\n[1, 2, 3]\n\
                              overflow\n[a]\noverflow\nok\nabcd\n";

#[test]
fn fallible_twins_return_capacity_overflow_on_both_backends() {
    agrees(TRY_TWINS, WANT_TRY_TWINS, 0);
}

/// The twins leave nothing behind when they fail or succeed.
#[test]
fn fallible_twins_are_clean_under_asan() {
    if let Some(out) = under_asan(TRY_TWINS) {
        assert_eq!(out, WANT_TRY_TWINS);
    }
}

/// A request below the representable bound that no allocator can satisfy
/// (2^60 bytes) is `OutOfMemory` with that byte count, not an abort. Not
/// run under ASan, whose allocator aborts on such a request by default.
#[test]
fn fallible_twins_report_out_of_memory() {
    agrees(
        "fn main() {\n    let r: Result[Vec[u8], AllocError] = Vec.try_with_capacity(1 << 60);\n    \
         match r {\n        Ok(_) => println(\"ok\"),\n        \
         Err(AllocError.OutOfMemory { requested_bytes }) => println(f\"oom {requested_bytes}\"),\n        \
         Err(AllocError.CapacityOverflow) => println(\"overflow\"),\n    }\n}\n",
        "oom 1152921504606846976\n",
        0,
    );
}

/// B-2026-10-21-78: the IR handed to the JIT has had scalar replacement, so
/// one long function reaches instruction selection without a stack slot per
/// temporary (which made `__mir-llvm-run` grow faster than quadratically).
#[test]
fn jit_ir_has_its_stack_slots_replaced() {
    let mut src = String::from("fn main() {\n    let mut t = String.new();\n");
    for i in 0..300 {
        src += &format!("    t.push_str(\"x{i}\");\n");
    }
    src += "    println(t.len());\n}\n";
    let lowered = karac::mir::lower::build_source(&src).expect("build");
    let allocas = |ir: &str| ir.matches(" = alloca ").count();
    let plain = karac::codegen::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let jit =
        karac::codegen::mir::compile_mir_ir_for_jit(&lowered.program, &lowered.tys).expect("IR");
    assert!(allocas(&plain) > 300, "{}", allocas(&plain));
    assert!(
        allocas(&jit) * 10 < allocas(&plain),
        "{} vs {}",
        allocas(&jit),
        allocas(&plain)
    );
}

/// B-2026-10-21-67: `cbrt` and the inverse hyperbolic functions lower on
/// compiled MIR, at either float width, as the interpreter computes them.
/// The inexact values are checked to a tolerance: hosts' libms differ in
/// the last digit (x86 CI's newer glibc gives acosh(2) and atanh(0.5) one
/// ulp off the values first pinned here).
#[test]
fn cube_root_and_inverse_hyperbolics_agree() {
    agrees(
        "fn main() {\n    let x: f64 = 27.0;\n    let y: f32 = 8.0;\n    \
         println(f\"{x.cbrt()} {y.cbrt()} {(0.0).asinh()} {(1.0).acosh()} {(0.0).atanh()}\");\n    \
         println(f\"{((0.5).asinh() - 0.48121182505960347).abs() < 0.000000000001} \
         {((2.0).acosh() - 1.3169578969248166).abs() < 0.000000000001} \
         {((0.5).atanh() - 0.5493061443340548).abs() < 0.000000000001}\");\n}\n",
        "3.0 2.0 0.0 0.0 0.0\ntrue true true\n",
        0,
    );
}

/// B-2026-10-21-40: a view of a literal receiver chosen in an `if` outlives
/// the branch (the literal is lent from static data), on both backends; a
/// `mut` method on a literal still has a `String` of its own.
#[test]
fn literal_receiver_view_outlives_its_branch() {
    agrees(
        "fn pick(upper: bool) -> u8 {\n    let digits = if upper { \"ABC\".bytes() } else { \"abc\".bytes() };\n    digits[1]\n}\n\
         fn first(n: i64) -> Str {\n    match n { 0 => \"zero\".as_str(), _ => \"many\".as_str() }\n}\n\
         fn main() {\n    println(f\"{pick(true)} {pick(false)}\");\n    println(f\"{first(0)} {first(3)} {\"abc\".len()}\");\n}\n",
        "66 98\nzero many 3\n",
        0,
    );
}

/// B-2026-10-21-38: a health check over a connection type with a `Drop`
/// is lent each idle value (a bare closure parameter borrows it); a failing
/// one is evicted, dropped, and a fresh one minted, on both backends.
const POOL_CHECK_DROP: &str = r#"struct Conn { id: i64 }
impl Drop for Conn { fn drop(mut ref self) { println(f"close {self.id}"); } }
fn main() {
    let pool: Pool[Conn] = Pool.new(|| Conn { id: 1 }, 2, 4).with_health_check(|c| c.id > 5);
    match pool.acquire(10) { Ok(c) => println(f"a {c.val.id}"), Err(_) => println("err") }
    match pool.acquire(10) { Ok(c) => println(f"b {c.val.id}"), Err(_) => println("err") }
    let kept: Pool[Conn] = Pool.new(|| Conn { id: 9 }, 2, 4).with_health_check(|c| c.id > 5);
    match kept.acquire(10) { Ok(c) => println(f"c {c.val.id}"), Err(_) => println("err") }
    match kept.acquire(10) { Ok(c) => println(f"d {c.val.id}"), Err(_) => println("err") }
    println("end");
}
"#;

const WANT_POOL_CHECK_DROP: &str = "a 1\nclose 1\nb 1\nc 9\nd 9\nend\nclose 9\nclose 1\n";

#[test]
fn pool_check_lends_a_dropping_connection_on_both_backends() {
    agrees(POOL_CHECK_DROP, WANT_POOL_CHECK_DROP, 0);
}

#[test]
fn pool_check_of_a_dropping_connection_is_clean_under_asan() {
    if let Some(out) = under_asan(POOL_CHECK_DROP) {
        assert_eq!(out, WANT_POOL_CHECK_DROP);
    }
}

/// B-2026-10-21-37: a pool's idle values drop when its last `Pool` value and loan are gone,
/// on both backends: a pool going out of scope, one handed a connection
/// from another pool, and a closure factory with a capture.
const POOL_DROP: &str = r#"struct Conn { id: i64 }
impl Drop for Conn { fn drop(mut ref self) { println(f"close {self.id}"); } }
fn scoped() {
    let pool: Pool[Conn] = Pool.new(|| Conn { id: 3 }, 2, 4);
    match pool.acquire(10) { Ok(c) => println(f"use {c.val.id}"), Err(_) => println("err") }
    println("end of scope");
}
fn main() {
    scoped();
    println("after");
    let tag = "db".to_string();
    let pool: Pool[Conn] = Pool.new(|| Conn { id: tag.len() as i64 }, 2, 4).with_health_check(|c| c.id > 0);
    let other: Pool[Conn] = Pool.new(|| Conn { id: 40 }, 1, 1);
    match pool.acquire(10) {
        Ok(c) => {
            other.release(c);
            println("released to other");
        }
        Err(_) => println("err"),
    }
    match pool.acquire(10) { Ok(c) => println(f"next {c.val.id}"), Err(_) => println("err2") }
    match other.acquire(10) { Ok(c) => println(f"other {c.val.id}"), Err(_) => println("err3") }
    println("end");
}
"#;

const WANT_POOL_DROP: &str = "use 3\nend of scope\nclose 3\nafter\nclose 2\nreleased to other\n\
                              next 2\nother 40\nend\nclose 40\nclose 2\n";

#[test]
fn pool_drops_its_idle_values_on_both_backends() {
    agrees(POOL_DROP, WANT_POOL_DROP, 0);
}

#[test]
fn pool_drop_is_clean_under_asan() {
    if let Some(out) = under_asan(POOL_DROP) {
        assert_eq!(out, WANT_POOL_DROP);
    }
}

/// B-2026-10-21-35: `try_resize` growing past what can be represented is an
/// `Err`, the vector unchanged and the fill value dropped; shrinking, a
/// negative length and a representable growth are `Ok`.
const TRY_RESIZE: &str = r#"fn show(r: Result[(), AllocError]) -> String {
    match r {
        Ok(_) => "ok".to_string(),
        Err(AllocError.CapacityOverflow) => "overflow".to_string(),
        Err(AllocError.OutOfMemory { requested_bytes }) => f"oom {requested_bytes}",
    }
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    println(show(v.try_resize(i64.MAX, 0)));
    println(f"{v.len()}");
    println(show(v.try_resize(3, 7)));
    println(f"{v}");
    println(show(v.try_resize(1, 0)));
    println(f"{v}");
    println(show(v.try_resize(-2, 0)));
    println(f"{v.len()}");
    let mut w: Vec[String] = Vec.new();
    println(show(w.try_resize(i64.MAX, "x".to_string())));
    println(show(w.try_resize(2, "y".to_string())));
    println(f"{w}");
    println(show(w.try_resize(1 << 57, "z".to_string())));
    println(f"{w.len()}");
}
"#;

const WANT_TRY_RESIZE: &str =
    "overflow\n0\nok\n[7, 7, 7]\nok\n[7]\nok\n0\noverflow\nok\n[y, y]\noverflow\n2\n";

#[test]
fn try_resize_returns_capacity_overflow_on_both_backends() {
    agrees(TRY_RESIZE, WANT_TRY_RESIZE, 0);
}

#[test]
fn try_resize_is_clean_under_asan() {
    if let Some(out) = under_asan(TRY_RESIZE) {
        assert_eq!(out, WANT_TRY_RESIZE);
    }
}

/// B-2026-10-21-45: `close` drops the idle values at once, a loan's value
/// drops when it comes back instead of going back, `acquire` answers
/// `PoolClosed` from then on, and closing twice (or a hand-built pool) does
/// nothing more.
const POOL_CLOSE: &str = r#"struct Conn { id: i64 }
impl Drop for Conn { fn drop(mut ref self) { println(f"close {self.id}"); } }
fn show(r: Result[PooledConnection[Conn], PoolError]) -> String {
    match r {
        Ok(c) => f"got {c.val.id}",
        Err(PoolError.PoolClosed) => "closed".to_string(),
        Err(PoolError.Timeout) => "timeout".to_string(),
        Err(_) => "other".to_string(),
    }
}
fn main() {
    let pool: Pool[Conn] = Pool.new(|| Conn { id: 1 }, 2, 4);
    match pool.acquire(10) {
        Ok(held) => {
            {
                let idle = pool.acquire(10);
            }
            println("one idle, one lent");
            pool.close();
            println("closed");
            println(show(pool.acquire(10)));
            pool.close();
            println(f"still holding {held.val.id}");
        }
        Err(_) => println("err"),
    }
    println("loan returned");
    println(show(pool.acquire(10)));
    let fake: Pool[Conn] = Pool { handle_id: 0 };
    fake.close();
    println(show(fake.acquire(10)));
}
"#;

const WANT_POOL_CLOSE: &str = "one idle, one lent\nclose 1\nclosed\nclosed\nstill holding 1\n\
                               close 1\nloan returned\nclosed\nclosed\n";

#[test]
fn pool_close_agrees_on_both_backends() {
    agrees(POOL_CLOSE, WANT_POOL_CLOSE, 0);
}

#[test]
fn pool_close_is_clean_under_asan() {
    if let Some(out) = under_asan(POOL_CLOSE) {
        assert_eq!(out, WANT_POOL_CLOSE);
    }
}

/// B-2026-10-21-65: `iN.checked_from(f)` is `Some` of `f` truncated toward
/// zero when that is in the type's range (both ends exact), and `None` for
/// NaN, an infinity or a value past either end; an `f32` argument widens.
/// `f32.parse` parses at `f32`, so a value halfway between two `f32`s in
/// `f64` terms still rounds once (`1.0000001`, not `1.0`).
const CHECKED_FROM: &str = r#"fn show_i8(o: Option[i8]) -> String { match o { Some(x) => f"{x}", None => "None".to_string() } }
fn show_i64(o: Option[i64]) -> String { match o { Some(x) => f"{x}", None => "None".to_string() } }
fn show_u64(o: Option[u64]) -> String { match o { Some(x) => f"{x}", None => "None".to_string() } }
fn show_u128(o: Option[u128]) -> String { match o { Some(x) => f"{x}", None => "None".to_string() } }
fn show_f32(r: Result[f32, ParseError]) -> String {
    match r {
        Ok(x) => f"{x}",
        Err(ParseError.Empty) => "Empty".to_string(),
        Err(ParseError.Invalid) => "Invalid".to_string(),
        Err(_) => "other".to_string(),
    }
}
fn main() {
    println(f"{show_i8(i8.checked_from(127.9))} {show_i8(i8.checked_from(-128.5))} {show_i8(i8.checked_from(128.0))} {show_i8(i8.checked_from(-129.0))}");
    println(f"{show_i64(i64.checked_from(-9223372036854775808.0))} {show_i64(i64.checked_from(9223372036854775808.0))} {show_i64(i64.checked_from(f64.INFINITY))} {show_i64(i64.checked_from(f64.NAN))}");
    println(f"{show_u64(u64.checked_from(18446744073709549568.0))} {show_u64(u64.checked_from(18446744073709551616.0))} {show_u64(u64.checked_from(-0.9))} {show_u64(u64.checked_from(-1.0))}");
    println(f"{show_u128(u128.checked_from(1e30))} {show_u128(u128.checked_from(1e39))}");
    let h: f32 = -7.75;
    println(f"{show_i64(i64.checked_from(h))}");
    println(f"{show_f32(f32.parse("0.1"))} {show_f32(f32.parse("1.000000059604644775390625000001"))} {show_f32(f32.parse(""))} {show_f32(f32.parse("abc"))}");
}
"#;
const WANT_CHECKED_FROM: &str = "127 -128 None None\n-9223372036854775808 None None None\n18446744073709549568 None 0 None\n1000000000000000019884624838656 None\n-7\n0.1 1.0000001 Empty Invalid\n";

#[test]
fn checked_from_and_f32_parse_agree_on_both_backends() {
    agrees(CHECKED_FROM, WANT_CHECKED_FROM, 0);
}

/// strings.md's `CStr` names: `bytes` and `to_str`, beside `to_string`; a
/// `CString` has `bytes` too (B-2026-10-21-31).
const CSTR_NAMES: &str = r#"fn main() {
    let c = c"hello";
    println(c.bytes().len());
    println(c.bytes()[0]);
    match c.to_str() {
        Ok(s) => println(f"Ok({s})"),
        Err(_) => println("Err"),
    }
    match c"a\xffb".to_str() {
        Ok(s) => println(f"Ok({s})"),
        Err(_) => println("Err"),
    }
    match c.to_string() {
        Ok(s) => println(f"Ok({s.len()})"),
        Err(_) => println("Err"),
    }
    match "xyz".to_string().to_cstring() {
        Ok(cs) => println(cs.bytes().len()),
        Err(_) => println("nul"),
    }
}
"#;

#[test]
fn cstr_bytes_and_to_str_agree_on_both_backends() {
    agrees(CSTR_NAMES, "5\n104\nOk(hello)\nErr\nOk(5)\n3\n", 0);
}

/// `.into()` from a `VarError` to an `IoError` goes through the library's
/// `From[VarError] for IoError`, as `?` does (io.md; B-2026-10-21-16).
const VAR_ERROR_INTO: &str = r#"fn missing() -> VarError { VarError.NotPresent }
fn main() {
    let v = VarError.NotUnicode;
    let e1: IoError = v.into();
    println(e1);
    let e2: IoError = missing().into();
    println(e2);
    let e3: IoError = IoError.from(VarError.NotUnicode);
    println(e3);
}
"#;

#[test]
fn var_error_into_io_error_agrees_on_both_backends() {
    agrees(VAR_ERROR_INTO, "InvalidUtf8\nNotFound\nInvalidUtf8\n", 0);
}

/// An `Atomic[T]` operand that is a bare literal is a `T`: `new`, `store`,
/// `fetch_add`, `fetch_sub` and `compare_exchange` on `i32` / `u64` atomics,
/// struct fields included (concurrency.md § Atomics; B-2026-10-21-73).
const ATOMIC_LITERALS: &str = r#"import std.sync.{Atomic};
sync struct Metrics {
    requests: Atomic[u64],
    small: Atomic[i32],
}
fn main() {
    let m = Metrics { requests: Atomic.new(0), small: Atomic.new(-5) };
    m.requests.fetch_add(3);
    m.requests.store(18000000000000000000);
    m.small.fetch_sub(-2);
    println(f"{m.requests.load()} {m.small.load()}");
    let a: Atomic[i32] = Atomic.new(2147483647);
    let p = a.fetch_add(1);
    println(f"{p} {a.load()}");
    let r = m.small.compare_exchange(-3, 40);
    println(f"{r.is_ok()} {m.small.load()}");
    let u: Atomic[u32] = Atomic.new(0);
    println(f"{u.fetch_sub(1)} {u.load()}");
}
"#;

#[test]
fn atomic_literal_operands_take_the_atomic_type() {
    agrees(
        ATOMIC_LITERALS,
        "18000000000000000000 -3\n2147483647 -2147483648\ntrue 40\n0 4294967295\n",
        0,
    );
}

/// The `ascii_chars` pass: a search over an ASCII literal's chars, a
/// cursor stepped by hand, and the texts it must leave alone (a wide char,
/// a text that grows, a backwards walk) agree on both backends.
#[test]
fn ascii_literal_chars_agree_on_both_backends() {
    agrees(
        r#"
fn find(n: i64) -> char {
    let a: String = "abcdefghijklmnopqrstuvwxyz";
    let mut i = 0;
    for ch in a.chars() {
        if i == n {
            return ch;
        }
        i = i + 1;
    }
    '?'
}
fn wide(n: i64) -> char {
    let a: String = "aéb€c";
    let mut i = 0;
    for ch in a.chars() {
        if i == n {
            return ch;
        }
        i = i + 1;
    }
    '?'
}
fn grown() -> String {
    let mut a: String = "ab";
    a.push('é');
    let mut out: String = "";
    for ch in a.chars() {
        out.push(ch);
    }
    out
}
fn manual() -> String {
    let a: String = "xyz";
    let mut it = a.chars();
    let mut out: String = "";
    loop {
        match it.next() {
            Some(c) => out.push(c),
            None => break,
        }
    }
    out
}
fn backwards() -> String {
    let a: String = "abc";
    let mut out: String = "";
    for ch in a.chars().rev() {
        out.push(ch);
    }
    out
}
fn main() {
    println(f"{find(0)} {find(25)} {find(26)} {wide(1)} {wide(3)} {grown()} {manual()} {backwards()}");
}
"#,
        "a z ? é € abé xyz cba\n",
        0,
    );
}

/// The collections.md surface B-2026-10-21-30 found missing: the `from`
/// constructors, `PriorityQueue` from `collect`, `VecDeque`'s `Display`,
/// `==` and `<`, `Vec.remove_first`, `f64.total_cmp` as a comparator,
/// `clone_from_slice`, `Array.from_fn` and an `Array`'s in-place `Slice`
/// methods, on `String` elements where an element can be.
const COLLECTIONS_PAGE: &str = r#"fn main() {
    let primes = Set.from([2, 3, 5, 7, 7]);
    let ages = Map.from([("ann", 31), ("bo", 27)]);
    println(f"{primes.len()} {primes.contains(5)} {ages["ann"]} {ages.len()}");
    let pq: PriorityQueue[i64] = [3, 1, 2].into_iter().collect();
    println(f"{pq.into_sorted_vec()}");
    let mut d: VecDeque[String] = VecDeque.new();
    d.push_back("b".to_string());
    d.push_front("a".to_string());
    let e: VecDeque[String] = ["a".to_string(), "b".to_string()].into_iter().collect();
    let empty: VecDeque[i64] = VecDeque.new();
    println(f"{d} {empty} {d == e} {d < e}");
    e.push_back("c".to_string());
    println(f"{d < e} {d != e}");
    let mut v: Vec[String] = ["ab".to_string(), "cde".to_string(), "fg".to_string()];
    match v.remove_first(|s| s.len() == 3) {
        Some(x) => println(f"{x} {v}"),
        None => println("none"),
    }
    match v.remove_first(|s| s.len() > 9) {
        Some(x) => println(x),
        None => println(f"none {v}"),
    }
    let mut w: Vec[f64] = [2.5, -1.5, f64.NAN, 0.5];
    w.sort_by(f64.total_cmp);
    println(f"{w}");
    let other: Vec[String] = ["x".to_string(), "y".to_string()];
    v[0..2].clone_from_slice(other);
    println(f"{v} {other}");
    let mut a: Array[String, 3] = Array.from_fn(3, |i| f"s{3 - i}");
    a.sort();
    println(f"{a}");
    a.sort_by_key(|s| s.len());
    a.reverse();
    a.swap(0, 2);
    println(f"{a}");
    let mut n = Array.from_fn(4, |i| i * i);
    n.fill(7);
    println(f"{n} {n.len()}");
}
"#;

const WANT_COLLECTIONS_PAGE: &str = "4 true 31 2\n[1, 2, 3]\n[a, b] [] true false\n\
true true\ncde [ab, fg]\nnone [ab, fg]\n[-1.5, 0.5, 2.5, NaN]\n[x, y] [x, y]\n\
[s1, s2, s3]\n[s1, s2, s3]\n[7, 7, 7, 7] 4\n";

#[test]
fn collections_page_surface_agrees_on_both_backends() {
    agrees(COLLECTIONS_PAGE, WANT_COLLECTIONS_PAGE, 0);
}

#[test]
fn collections_page_surface_is_clean_under_asan() {
    if let Some(out) = under_asan(COLLECTIONS_PAGE) {
        assert_eq!(out, WANT_COLLECTIONS_PAGE);
    }
}

/// `clone_from_slice` panics when the lengths differ, before writing.
#[test]
fn clone_from_slice_of_another_length_panics() {
    agrees(
        r#"fn main() {
    let mut v: Vec[String] = ["a".to_string(), "b".to_string(), "c".to_string()];
    let other: Vec[String] = ["x".to_string()];
    println("before");
    v[0..2].clone_from_slice(other);
    println(f"{v}");
}
"#,
        "before\n",
        101,
    );
}

/// `Array.from_fn` over an element with a `Drop`: each is built once and
/// dropped once, and a capture is borrowed, not moved (B-2026-10-21-30).
const ARRAY_FROM_FN_DROP: &str = r#"struct Noisy { n: i64 }
impl Drop for Noisy {
    fn drop(mut ref self) { println(f"drop {self.n}"); }
}
fn main() {
    let tag = "t".to_string();
    let a: Array[Noisy, 3] = Array.from_fn(3, |i| Noisy { n: i + tag.len() });
    println(f"{a[0].n} {a[2].n}");
}
"#;

#[test]
fn array_from_fn_drops_each_element_once() {
    agrees(ARRAY_FROM_FN_DROP, "1 3\ndrop 1\ndrop 2\ndrop 3\n", 0);
    if let Some(out) = under_asan(ARRAY_FROM_FN_DROP) {
        assert_eq!(out, "1 3\ndrop 1\ndrop 2\ndrop 3\n");
    }
}

/// A count that is not the array's length panics before any element is
/// built.
#[test]
fn array_from_fn_of_the_wrong_count_panics() {
    agrees(
        r#"struct Noisy { n: i64 }
impl Drop for Noisy {
    fn drop(mut ref self) { println(f"drop {self.n}"); }
}
fn count() -> i64 { 4 }
fn main() {
    let s: Array[String, 2] = Array.from_fn(2, |i| f"x{i}");
    println(f"{s}");
    let a: Array[Noisy, 3] = Array.from_fn(count(), |i| Noisy { n: i });
    println(f"{a[0].n}");
}
"#,
        "[x0, x1]\n",
        101,
    );
}

/// A `chars()` walk over a borrowed text with 2-, 3- and 4-byte chars,
/// forward and through `rev()`, agrees on both backends now that the
/// runtime's decode step is marked as reading only its text.
#[test]
fn wide_char_walks_over_a_parameter_agree() {
    agrees(
        r#"
fn count(s: ref String, c: char) -> i64 {
    let mut n = 0;
    for ch in s.chars() {
        if ch == c {
            n = n + 1;
        }
    }
    n
}
fn main() {
    let s: String = "héllo wörld € 𝄞 é";
    let mut back: String = "";
    for ch in s.chars().rev() {
        back.push(ch);
    }
    println(f"{count(s, 'é')} {count(s, 'l')} {count(s, '𝄞')} {back}");
}
"#,
        "2 3 1 é 𝄞 € dlröw olléh\n",
        0,
    );
}

/// A range `for` that pushes once per trip reserves its count up front on
/// both backends (B-2026-10-18-51): 26, 26 and 10 where growth by doubling
/// would leave 32 and 16.
#[test]
fn range_for_fills_reserve_their_count_on_both_backends() {
    agrees(
        r#"
fn fill(n: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    for _ in 0..n { v.push(0); }
    v
}
fn lit() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    for _ in 0..26 { v.push(1); }
    v
}
fn incl(n: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    for i in 0..=n { v.push(i); }
    v
}
fn main() {
    let a = fill(26);
    let b = lit();
    let c = incl(9);
    let (al, ac, bl, bc, cl, cc) = (a.len(), a.capacity(), b.len(), b.capacity(), c.len(), c.capacity());
    println(f"{al} {ac} {bl} {bc} {cl} {cc}");
}"#,
        "26 26 26 26 10 10\n",
        0,
    );
}
