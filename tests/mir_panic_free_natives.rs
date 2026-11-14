//! `String.repeat`, integer parsing, `sort` and `i128` printing keep std's
//! panic hook out of a MIR binary (B-2026-11-04-31), and so do float
//! printing and format specs (B-2026-11-06-2). `str::repeat`'s
//! length `expect`, core's `from_str_radix` radix assert, the scratch
//! `vec!` and `step_by` of the merge sort, `slice::sort_unstable`'s
//! total-order check and `u128`'s `Display` each kept a Rust panic
//! reachable, and with it the hook and its backtrace symbolizer: katas 297,
//! 315, 332 and 394 were 319-336 KB on macOS where other katas are 52-69
//! KB. Each program here is built and run; its output is a Python mirror's
//! and the MIR interpreter's.
#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-panic-free-natives-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn karac(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "0")
        .env_remove("KARAC_RUNTIME")
        .env_remove("KARAC_FORCE_FULL_RUNTIME")
        .current_dir(dir);
    c
}

/// Builds and runs `src`, checks its output against `expected` and the
/// interpreter's, and returns whether the binary links std's panic hook;
/// `None` (after saying so) when the runtime archive is missing and the run
/// does not require it.
fn check(tag: &str, src: &str, expected: &str) -> Option<bool> {
    let dir = scratch(tag);
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let interp = karac(&dir)
        .args(["__mir-run", "main.kara"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&interp.stdout),
        expected,
        "interpreter: {}",
        String::from_utf8_lossy(&interp.stderr)
    );
    let exe = dir.join("main.out");
    let built = karac(&dir)
        .args(["build", "main.kara", "-o"])
        .arg(&exe)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).into_owned();
    if !built.status.success()
        && err.contains("not found")
        && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none()
    {
        eprintln!("skipping: no runtime archive: {err}");
        return None;
    }
    assert!(built.status.success(), "{err}");
    let ran = Command::new(&exe).output().unwrap();
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&ran.stdout), expected);
    let bytes = std::fs::read(&exe).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    Some(bytes.windows(14).any(|w| w == b"RUST_BACKTRACE"))
}

#[test]
fn mir_string_repeat_links_no_panic_hook() {
    let src = REPEAT;
    if let Some(hook) = check("repeat", src, "ababababab\n10\n0\n0\n0\n3000 abc\nééé\n") {
        assert!(!hook, "std's panic hook is linked");
    }
}

#[test]
fn mir_integer_parsing_and_i128_printing_link_no_panic_hook() {
    if let Some(hook) = check("parse", PARSE, PARSE_OUT) {
        assert!(!hook, "std's panic hook is linked");
    }
}

#[test]
fn mir_integer_and_comparator_sorts_link_no_panic_hook() {
    let expected = "830235998\n-1073402097 1072439231 -1000 1000 0 255 -3 3\n1 100 50 0 14\nw0 w1 w99\n0 0 4 48\n";
    if let Some(hook) = check("sort", SORT, expected) {
        assert!(!hook, "std's panic hook is linked");
    }
}

/// Printing a float and formatting any hole with a spec: core's float
/// formatting (Grisu, Dragon, and the `parse` the even-tie rule read back
/// with), `format!`, `Vec<char>` in the spec parser and a `RefCell`
/// borrow each kept a Rust panic reachable, and kata 346 was 336 KB on the
/// M5 (B-2026-11-06-2). The expected output is a Python mirror's (`repr`,
/// and `format` with an `f` type for a precision).
#[test]
fn mir_float_printing_and_format_specs_link_no_panic_hook() {
    if let Some(hook) = check("floats", FLOATS, FLOATS_OUT) {
        assert!(!hook, "std's panic hook is linked");
    }
}

const FLOATS: &str = r#"
fn main() {
    let x = 7.0 / 3.0;
    println(x);
    println(f"{x:.3} {x:10.2}|{x:<8.1}|{x:+.0}|{x:08.3}");
    let half = 0.5;
    let neg = -half;
    let two = 2.5;
    let eighth = 0.125;
    println(f"{neg:.0} {two:.0} {eighth:.2} {half:.1}");
    let tiny = 5e-324;
    println(tiny);
    println(f"{tiny:.3}");
    let big = 1e300;
    println(big);
    println(f"{big:.0}");
    println(-75455689800034.625);
    println(1e16);
    println(0.00001);
    let t: f32 = 0.1;
    println(t);
    let n = 42;
    let s = "ab";
    println(f"{n:>6}|{n:<5}|{n:^7}|{n:08b}|{n:x}|{s:*^6}|{s:>4}");
    let w: i128 = -12345678901234567890123;
    println(f"{w:>30}|{w:x}");
}
"#;

const FLOATS_OUT: &str = "2.3333333333333335\n2.333       2.33|2.3     |+2|0002.333\n-0 2 0.12 0.5\n5e-324\n0.000\n1e+300\n1000000000000000052504760255204420248704468581108159154915854115511802457988908195786371375080447864043704443832883878176942523235360430575644792184786706982848387200926575803737830233794788090059368953234970799945081119038967640880074652742780142494579258788820056842838115669472196386865459400540160\n-75455689800034.62\n1e+16\n1e-05\n0.1\n    42|42   |  42   |00101010|2a|**ab**|  ab\n      -12345678901234567890123|fffffffffffffd62bd49b1898ebdbb35\n";

const REPEAT: &str = r#"
fn main() {
    let t = "ab".repeat(5);
    println(t);
    println(t.len());
    println("".repeat(1000).len());
    println("xyz".repeat(0).len());
    println("q".repeat(-3).len());
    let big = "abc".repeat(1000);
    println(f"{big.len()} {big.substring(2997, 3000)}");
    println("é".repeat(3));
}
"#;

/// `i64`, `u64`, `u8`, `i8`, `i128` and `u128` through `parse` and
/// `from_str_radix`: signs, both cases of digit, a digit past the radix,
/// empty and sign-only text, the extremes and one past them.
const PARSE: &str = r#"
fn show(r: Option[i64]) {
    match r {
        Some(v) => println(v),
        None => println("none"),
    }
}

fn main() {
    show(i64.from_str_radix("ff", 16));
    show(i64.from_str_radix("-Zz", 36));
    show(i64.from_str_radix("+101", 2));
    show(i64.from_str_radix("12", 2));
    show(i64.from_str_radix("", 10));
    show(i64.from_str_radix("-", 16));
    show(i64.from_str_radix("+-1", 16));
    show(i64.parse("  9223372036854775807 ").ok());
    show(i64.parse("-9223372036854775808").ok());
    show(i64.parse("9223372036854775808").ok());
    show(i64.parse("99999999999999999999999999999999999999999").ok());
    match u64.parse("18446744073709551615") {
        Ok(v) => println(v),
        Err(_) => println("none"),
    }
    match u8.from_str_radix("-0", 10) {
        Some(v) => println(v),
        None => println("none"),
    }
    match i8.from_str_radix("-80", 16) {
        Some(v) => println(v),
        None => println("none"),
    }
    match i128.parse("-170141183460469231731687303715884105728") {
        Ok(v) => println(v),
        Err(_) => println("none"),
    }
    match i128.parse("170141183460469231731687303715884105728") {
        Ok(v) => println(v),
        Err(_) => println("none"),
    }
    match u128.from_str_radix("ffffffffffffffffffffffffffffffff", 16) {
        Some(v) => println(v),
        None => println("none"),
    }
    match u128.parse("-1") {
        Ok(v) => println(v),
        Err(_) => println("none"),
    }
}
"#;

/// Python: `int(text, radix)` within each type's range, else none.
const PARSE_OUT: &str = "255\n-1295\n5\nnone\nnone\nnone\nnone\n9223372036854775807\n-9223372036854775808\nnone\nnone\n18446744073709551615\nnone\n-128\n-170141183460469231731687303715884105728\nnone\n340282366920938463463374607431768211455\nnone\n";

/// Integer sorts of four widths (random, ascending, descending, and short
/// enough for the insertion sort), a `Vec[String]` sort and a `sort_by`.
const SORT: &str = r#"
fn main() {
    let mut a: Vec[i64] = Vec.new();
    let mut b: Vec[i32] = Vec.new();
    let mut c: Vec[u8] = Vec.new();
    let mut d: Vec[i16] = Vec.new();
    let mut x: i64 = 12345;
    for i in 0..3000 {
        x = (x * 1103515245 + 12345) % 2147483648;
        a.push(x - 1073741824);
        b.push((x % 2001 - 1000) as i32);
        c.push((x % 256) as u8);
        d.push((x % 7 - 3) as i16);
    }
    let mut up: Vec[i64] = Vec.new();
    let mut down: Vec[i64] = Vec.new();
    let mut tiny: Vec[i64] = Vec.new();
    for i in 0..100 {
        up.push(i);
        down.push(100 - i);
    }
    for i in 0..15 {
        tiny.push((i * 7) % 15);
    }
    a.sort();
    b.sort();
    c.sort();
    d.sort();
    up.sort();
    down.sort();
    tiny.sort();
    let mut h: i64 = 0;
    for i in 0..3000 {
        h = (h * 31 + a[i] + b[i] as i64 * 7 + c[i] as i64 * 11 + d[i] as i64 * 13) % 1000000007;
    }
    println(h);
    println(f"{a[0]} {a[2999]} {b[0]} {b[2999]} {c[0]} {c[2999]} {d[0]} {d[2999]}");
    println(f"{down[0]} {down[99]} {up[50]} {tiny[0]} {tiny[14]}");
    let mut words: Vec[String] = Vec.new();
    for i in 0..200 {
        words.push(f"w{(i * 37) % 200}");
    }
    words.sort();
    println(f"{words[0]} {words[1]} {words[199]}");
    let mut pairs: Vec[(i64, i64)] = Vec.new();
    for i in 0..50 {
        pairs.push(((i * 13) % 5, i));
    }
    pairs.sort_by(|p, q| p.0.cmp(q.0));
    println(f"{pairs[0].0} {pairs[0].1} {pairs[49].0} {pairs[49].1}");
}
"#;
