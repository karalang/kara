//! This file was split on 2026-09-21. The fixtures now live in per-area
//! files under `tests/interpreter/`, because this file had grown into an
//! append target nobody could read, review or blame. The TEST TARGET is
//! unchanged -- `cargo test --features llvm --test interpreter` still runs
//! every one of them, so CI needs no edit. A separate test target per
//! area was measured at ~118 MiB of link output each (~250 MiB at the
//! default debug level), which the session disk allowance cannot carry;
//! modules cost nothing.
//!
//! Run one area alone:  cargo test --features llvm --test interpreter <area>::

// tests/interpreter.rs

pub(crate) use karac::interpreter::DbgOutputMode;
pub(crate) use karac::{
    run_program, run_program_full, run_program_with_dbg, run_program_with_drops,
    run_program_with_trace,
};

// ── Area modules ─────────────────────────────
// Each file's header states which fixtures belong in it.
#[path = "interpreter/arrays.rs"]
mod arrays;
#[path = "interpreter/attrs_ir.rs"]
mod attrs_ir;
#[path = "interpreter/borrows.rs"]
mod borrows;
#[path = "interpreter/closures.rs"]
mod closures;
#[path = "interpreter/concurrency.rs"]
mod concurrency;
#[path = "interpreter/control.rs"]
mod control;
#[path = "interpreter/drop_order.rs"]
mod drop_order;
#[path = "interpreter/effects.rs"]
mod effects;
#[path = "interpreter/enums.rs"]
mod enums;
#[path = "interpreter/frames.rs"]
mod frames;
#[path = "interpreter/generics.rs"]
mod generics;
#[path = "interpreter/gpu_tensor.rs"]
mod gpu_tensor;
#[path = "interpreter/io_runtime.rs"]
mod io_runtime;
#[path = "interpreter/iter_range.rs"]
mod iter_range;
#[path = "interpreter/map_set.rs"]
mod map_set;
#[path = "interpreter/misc.rs"]
mod misc;
#[path = "interpreter/moves.rs"]
mod moves;
#[path = "interpreter/numerics.rs"]
mod numerics;
#[path = "interpreter/option_result.rs"]
mod option_result;
#[path = "interpreter/patterns.rs"]
mod patterns;
#[path = "interpreter/rc_shared.rs"]
mod rc_shared;
#[path = "interpreter/slices.rs"]
mod slices;
#[path = "interpreter/strings.rs"]
mod strings;
#[path = "interpreter/structs.rs"]
mod structs;
#[path = "interpreter/vecs.rs"]
mod vecs;

// ── Test Helpers ────────────────────────────────────────────────

fn run(source: &str) -> String {
    let output = run_program(source);
    output.join("")
}

/// Assert a program that prints a single f64 produces a value within a tiny
/// tolerance of `expected`. Irrational transcendentals (`asin`, `asinh`, …)
/// lower to libm, whose last-ULP result differs across platforms (macOS vs
/// Linux) — a bit-exact string assertion is NOT portable and spuriously fails
/// the macOS CI leg. The interpreter and codegen still share one platform's
/// libm within a run, so run==build holds; only the cross-platform string pin
/// was wrong. 1e-12 is far looser than one ULP yet far tighter than any real
/// regression (a wrong function is off by orders of magnitude).
fn assert_prints_float_near(source: &str, expected: f64) {
    let out = run(source);
    let got: f64 = out
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("expected a float, got {out:?}"));
    assert!(
        (got - expected).abs() <= 1e-12,
        "expected ~{expected}, got {got} (raw {out:?})"
    );
}

fn runtime_errors(source: &str) -> Vec<karac::interpreter::RuntimeError> {
    let (_out, errors, _trace, _trunc) = run_program_full(source);
    errors
}

fn run_no_errors(source: &str) -> String {
    let (out, errors, _trace, _trunc) = run_program_full(source);
    assert!(
        errors.is_empty(),
        "Expected no runtime errors, got: {:?}",
        errors
    );
    out.join("")
}

// ── Sub-step 3: NLL drop placement ─────────────────────────────

fn drops_in(source: &str) -> Vec<String> {
    let (_out, drops) = run_program_with_drops(source);
    drops
}

/// One-shot loopback HTTP origin for the `RequestBuilder` tests. Reads the
/// request head (and any body indicated by `Content-Length`), then replies
/// with a canned response that echoes back the request line, two named
/// request headers, and the body — so a single assertion can pin that the
/// method, both headers, the payload, and the response-header capture all
/// survived the round trip. Returns the bound port. Ephemeral port + one
/// accept, matching the origin pattern in `tests/http_server.rs`.
#[cfg(not(target_arch = "wasm32"))]
fn spawn_echo_origin() -> u16 {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral origin port");
    let port = listener.local_addr().expect("local_addr").port();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            // Read the head, then exactly `Content-Length` more bytes. A
            // read-to-EOF would deadlock: ureq holds the connection open
            // waiting for our response.
            loop {
                let n = match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(_) => return,
                };
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let head = &text[..head_end];
                    let want: usize = head
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse().ok())?
                        })
                        .unwrap_or(0);
                    if buf.len() >= head_end + 4 + want {
                        break;
                    }
                }
            }
            let text = String::from_utf8_lossy(&buf).to_string();
            let head_end = text.find("\r\n\r\n").unwrap_or(text.len());
            let head = &text[..head_end];
            let body = text.get(head_end + 4..).unwrap_or("");
            let verb = head.split_whitespace().next().unwrap_or("?").to_string();
            let pick = |name: &str| -> String {
                head.lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.trim()
                            .eq_ignore_ascii_case(name)
                            .then(|| v.trim().to_string())
                    })
                    .unwrap_or_else(|| "-".to_string())
            };
            let payload = format!("m={verb};xa={};xb={};body={body}", pick("X-A"), pick("X-B"));
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nX-Echo: yes\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });
    port
}

/// The source program for [`two_from_impls_dispatch_by_source_type`], with the
/// two `From` impls emitted in the given order.
///
/// The ORDER is a parameter because order was the whole bug: with the impls
/// collapsed onto one `AppError.from` name, each backend answered with a
/// different member of the group, so ANY single-order fixture passes on one
/// backend by luck. Only running both orders and demanding identical output
/// distinguishes "resolved correctly" from "happened to pick the right one".
fn two_from_impls_src(parse_impl_first: bool) -> String {
    let p = "impl From[ParseError] for AppError { fn from(e: ParseError) -> AppError { return AppError.Parse(e); } }";
    let d = "impl From[DbError] for AppError { fn from(e: DbError) -> AppError { return AppError.Db(e); } }";
    let (a, b) = if parse_impl_first { (p, d) } else { (d, p) };
    format!(
        r#"
struct ParseError {{ tag: String }}
struct DbError {{ code: i64 }}
enum AppError {{ Parse(ParseError), Db(DbError) }}
{a}
{b}
fn label(a: AppError) -> String {{
    match a {{
        AppError.Parse(p) => return f"PARSE:{{p.tag}}",
        AppError.Db(d) => return f"DB:{{d.code}}",
    }}
}}
fn fails_parse() -> Result[i64, ParseError] {{ return Err(ParseError {{ tag: "p" }}); }}
fn fails_db() -> Result[i64, DbError] {{ return Err(DbError {{ code: 7 }}); }}
fn q_parse() -> Result[i64, AppError] {{ let n = fails_parse()?; return Ok(n); }}
fn q_db() -> Result[i64, AppError] {{ let n = fails_db()?; return Ok(n); }}
fn main() {{
    match q_parse() {{ Ok(_) => println("ok"), Err(e) => println(label(e)), }}
    match q_db() {{ Ok(_) => println("ok"), Err(e) => println(label(e)), }}
    println(label(AppError.from(ParseError {{ tag: "p" }})));
    println(label(AppError.from(DbError {{ code: 7 }})));
    let a: AppError = (ParseError {{ tag: "p" }}).into();
    let b: AppError = (DbError {{ code: 7 }}).into();
    println(label(a));
    println(label(b));
}}
"#
    )
}

// ── B-2026-08-21-8: Map / Set are hash-indexed and Arc-backed ────
//
// The representation moved from a bare `Vec` (a by-value association list) to
// `Arc<RwLock<MapData>>` / `Arc<RwLock<SetData>>` — insertion-ordered storage
// with a hash index beside it. VALUE SEMANTICS is what the change put at risk,
// because the derived `Clone` now shares storage the way `Array`'s always has,
// and only `deep_clone_value` at binding sites keeps two bindings independent.
//
// These tests ORIGINALLY also pinned the observable order to insertion order.
// They no longer can, and the reason is B-2026-08-21-6: the observable walk is
// now ordered by the per-process-seeded hash, because design.md § Map says
// "iteration order is unspecified and varies across process runs". A test that
// pinned a particular order would fail on most runs — and, worse, asserting one
// would re-assert the very contract the spec denies, which is how a whole
// codebase quietly grows an order dependency. What survives is what is actually
// promised: every entry is visited exactly once, and every survivor of a
// removal is still findable. Hence the sorting below.

/// Sort the first `n` lines of `out` and rejoin, so an assertion can pin
/// CONTENTS without pinning the unspecified iteration order. The remaining
/// lines (lookups, membership checks) keep their exact positions, since those
/// ARE ordered — they are separate statements.
fn sorted_prefix(out: &str, n: usize) -> String {
    let mut lines: Vec<&str> = out.lines().collect();
    let n = n.min(lines.len());
    lines[..n].sort_unstable();
    let mut joined = lines.join("\n");
    if out.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

// ── B-2026-08-22-6: a USER-written hasher, interpreted ───────────────
//
// `MapData::hash_key` is a `&self` method reached through an `RwLock` guard,
// with no interpreter anywhere in sight, so running user code from there needs
// `interpreter::user_hasher`: the key `Value` is flattened to a canonical byte
// string and fed to a sub-interpreter that drives `build` / `write` / `finish`.
// These pin the two halves — that the map still WORKS, and that the user's
// permutation is what orders it.

/// FNV-1a and a 31-multiplier hash, as a source prefix.
const USER_HASHERS: &str = "\
struct Fnv { h: u64 }\n\
impl Hasher for Fnv {\n\
    fn write(mut ref self, bytes: ref Slice[u8]) {\n\
        for b in bytes { self.h = (self.h ^ (b as u64)).wrapping_mul(1099511628211u64); }\n\
    }\n\
    fn finish(ref self) -> u64 { self.h }\n\
}\n\
struct FnvBuild { }\n\
impl BuildHasher for FnvBuild {\n\
    type Hasher = Fnv;\n\
    fn build(ref self) -> Fnv { Fnv { h: 14695981039346656037u64 } }\n\
}\n\
struct Sum { h: u64 }\n\
impl Hasher for Sum {\n\
    fn write(mut ref self, bytes: ref Slice[u8]) {\n\
        for b in bytes { self.h = self.h.wrapping_mul(31u64).wrapping_add(b as u64); }\n\
    }\n\
    fn finish(ref self) -> u64 { self.h }\n\
}\n\
struct SumBuild { }\n\
impl BuildHasher for SumBuild {\n\
    type Hasher = Sum;\n\
    fn build(ref self) -> Sum { Sum { h: 0u64 } }\n\
}\n";

// ── B-2026-08-24-7: a fault outranks the statement it faulted inside ──

/// The load-bearing assertion for this family: after the fault, the
/// interpreter must have executed NOTHING further. `errors` alone is not
/// enough — the pre-fix interpreter recorded the error too, and *still*
/// ran the caller to completion on a value that does not exist.
fn assert_stops_at_fault(source: &str, needle: &str, forbidden: &str) {
    let (out, errors, _trace, _trunc) = run_program_full(source);
    assert!(
        errors.iter().any(|e| e.message.contains(needle)),
        "expected a runtime error containing {needle:?}, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    let joined = out.join("");
    assert!(
        !joined.contains(forbidden),
        "execution continued past the fault: output {joined:?} contains {forbidden:?}"
    );
}
