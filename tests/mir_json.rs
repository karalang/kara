//! `Json.parse` and `Json.stringify` on the v2 backends (B-2026-10-20-39):
//! the Kāra bodies in runtime/stdlib/source/json.kara. Each program runs on
//! the MIR interpreter and, compiled, at -O0 and -O2; both must print the
//! expected output, which is what legacy's `serde_json` prints for the same
//! program (`karac run --interp`), errors and their positions included.
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
        let dir =
            std::env::temp_dir().join(format!("karac-mir-json-test-{}-{n}", std::process::id()));
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

/// The program's output on the compiled backend at `opt`; `None` when the
/// runtime archive is absent.
fn build_and_run(src: &str, opt: u8) -> Option<String> {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = Scratch::new();
    let obj = dir.path().join("main.o");
    let exe = dir.path().join("main");
    karac::backend::mir_aot::compile_object(&ir, &obj, opt).expect("object");
    common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ))?;
    let out = Command::new(&exe).output().expect("run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).expect("utf-8"))
}

/// `src` prints `want` on the interpreter and, compiled, at -O0 and -O2.
fn agrees(src: &str, want: &str) {
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want, "on the interpreter");
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "compiled at -O{opt}");
        }
    }
}

/// Number layout (whole, fraction, the 1e16 and 1e-5 edges, exponents,
/// `-0.0`, the extremes), string escapes, `Int` against `Number` by the
/// token's syntax, a repeated key, and the common errors.
#[test]
fn json_numbers_strings_and_errors_agree_on_both_backends() {
    agrees(NUMBERS, WANT_NUMBERS);
}

/// The rest of the error surface (each message at `serde_json`'s line and
/// byte column), surrogate pairs, nesting to the recursion limit, nested
/// repeated keys, and writing a built tree with a repeated key, NaN and the
/// infinities.
#[test]
fn json_error_positions_and_limits_agree_on_both_backends() {
    agrees(EDGES, WANT_EDGES);
}

const NUMBERS: &str = r#"fn main() {
    let xs = [1.0, 0.1, 1e21, 1e-7, 123456789.0, 1.5e300, -0.0, 2.5, 1e16, 1e15, 0.00001, 0.000001, 1e17, 0.3, 5e-324, 1.7976931348623157e308, 100.0, 12345.678];
    let mut v: Vec[Json] = Vec.new();
    for x in xs { v.push(Json.Number(x)); }
    v.push(Json.Int(-9223372036854775807));
    v.push(Json.String("a\"b\\c\n\t\r\u{1}\u{1f}\u{7f}é😀/"));
    println(Json.Array(v).stringify());
    let inputs = ["7", "7.0", "-0", "1e2", "1E2", "12345678901234567890", "-9223372036854775808", "[1,2", "{\"a\":1,\"a\":2,\"b\":3}", "\"\\u00e9\\ud83d\\ude00\\/\"", "  null ", "tru", "{\"a\" 1}", "[1,]", "01", "1.", "-", "\"\\x\"", "\"a\nb\"", "[]x", ""];
    for s in inputs {
        match Json.parse(s.to_string()) {
            Ok(j) => println(f"ok {j.stringify()}"),
            Err(e) => println(f"err {e.line}:{e.column} {e.message}"),
        }
    }
}
"#;

const WANT_NUMBERS: &str = "[1.0,0.1,1e+21,1e-7,123456789.0,1.5e+300,-0.0,2.5,1e+16,1000000000000000.0,0.00001,1e-6,1e+17,0.3,5e-324,1.7976931348623157e+308,100.0,12345.678,-9223372036854775807,\"a\\\"b\\\\c\\n\\t\\r\\u0001\\u001f\u{7f}é😀/\"]\n\
ok 7\n\
ok 7.0\n\
ok -0.0\n\
ok 100.0\n\
ok 100.0\n\
ok 1.2345678901234567e+19\n\
ok -9223372036854775808\n\
err 1:4 EOF while parsing a list at line 1 column 4\n\
ok {\"a\":2,\"b\":3}\n\
ok \"é😀/\"\n\
ok null\n\
err 1:3 EOF while parsing a value at line 1 column 3\n\
err 1:6 expected `:` at line 1 column 6\n\
err 1:4 trailing comma at line 1 column 4\n\
err 1:2 invalid number at line 1 column 2\n\
err 1:2 EOF while parsing a value at line 1 column 2\n\
err 1:1 EOF while parsing a value at line 1 column 1\n\
err 1:3 invalid escape at line 1 column 3\n\
err 2:0 control character (\\u0000-\\u001F) found while parsing a string at line 2 column 0\n\
err 1:3 trailing characters at line 1 column 3\n\
err 1:0 EOF while parsing a value at line 1 column 0\n\
";

const EDGES: &str = r#"fn main() {
    let inputs = ["1e400", "-1.5e-3", "1E+2", "[1 2]", "{1:2}", "{\"a\":1,}", "{\"a\"", "{", "[", "\"abc", "\"\\u12\"", "\"\\uZZZZ\"", "\"\\ud800\"", "\"\\udc00\"", "\"\\ud800\\u0041\"", "\"\\ud83d\\ude00\"", "nul", "nulx", "falsy", " \n [\n1,\n x]", "1.x", "-x", "2e", "2e+", "\"é\" x", "[\"é\",x]", "{\"a\":{\"b\":1,\"b\":[2,{\"c\":3,\"c\":4}]},\"a\":5}", "{}", "[]", "[[],{}]", "  {\"k\" : [ true , false , null ] }  ", "[1,,2]", "{\"a\":1 \"b\":2}", "{\"a\":1,\"b\"}", "{,}", "[,]", "-01", "0.5e-2", "123abc", "\"\\b\\f\\/\"", "\"\t\"", "9223372036854775807", "9223372036854775808", "-9223372036854775809", "1.0e0", "0e0", "[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]", "[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]", "{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":"];
    for s in inputs {
        match Json.parse(s.to_string()) {
            Ok(j) => println(f"ok {j.stringify()}"),
            Err(e) => println(f"err {e.line}:{e.column} {e.message}"),
        }
    }
    let dup = Json.Object(Vec[("x", Json.Int(1)), ("y", Json.Null), ("x", Json.Array(Vec.new())), ("z", Json.String("\u{7f}\u{8}\u{c}"))]);
    println(dup.stringify());
    println(Json.Array(Vec[Json.Number(0.0 / 0.0), Json.Number(1.0 / 0.0), Json.Number(-1.0 / 0.0), Json.Number(-1.5), Json.Number(1e-5), Json.Number(123e-20)]).stringify());
}
"#;

const WANT_EDGES: &str = "err 1:5 number out of range at line 1 column 5\n\
ok -0.0015\n\
ok 100.0\n\
err 1:4 expected `,` or `]` at line 1 column 4\n\
err 1:2 key must be a string at line 1 column 2\n\
err 1:8 trailing comma at line 1 column 8\n\
err 1:4 EOF while parsing an object at line 1 column 4\n\
err 1:1 EOF while parsing an object at line 1 column 1\n\
err 1:1 EOF while parsing a list at line 1 column 1\n\
err 1:4 EOF while parsing a string at line 1 column 4\n\
err 1:6 EOF while parsing a string at line 1 column 6\n\
err 1:7 invalid escape at line 1 column 7\n\
err 1:8 unexpected end of hex escape at line 1 column 8\n\
err 1:7 lone leading surrogate in hex escape at line 1 column 7\n\
err 1:13 lone leading surrogate in hex escape at line 1 column 13\n\
ok \"😀\"\n\
err 1:3 EOF while parsing a value at line 1 column 3\n\
err 1:4 expected ident at line 1 column 4\n\
err 1:5 expected ident at line 1 column 5\n\
err 4:2 expected value at line 4 column 2\n\
err 1:3 invalid number at line 1 column 3\n\
err 1:2 invalid number at line 1 column 2\n\
err 1:2 EOF while parsing a value at line 1 column 2\n\
err 1:3 EOF while parsing a value at line 1 column 3\n\
err 1:6 trailing characters at line 1 column 6\n\
err 1:7 expected value at line 1 column 7\n\
ok {\"a\":5}\n\
ok {}\n\
ok []\n\
ok [[],{}]\n\
ok {\"k\":[true,false,null]}\n\
err 1:4 expected value at line 1 column 4\n\
err 1:8 expected `,` or `}` at line 1 column 8\n\
err 1:11 expected `:` at line 1 column 11\n\
err 1:2 key must be a string at line 1 column 2\n\
err 1:2 expected value at line 1 column 2\n\
err 1:3 invalid number at line 1 column 3\n\
ok 0.005\n\
err 1:4 trailing characters at line 1 column 4\n\
ok \"\\b\\f/\"\n\
err 1:2 control character (\\u0000-\\u001F) found while parsing a string at line 1 column 2\n\
ok 9223372036854775807\n\
ok 9.223372036854776e+18\n\
ok -9.223372036854776e+18\n\
ok 1.0\n\
ok 0.0\n\
ok [[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]\n\
err 1:128 recursion limit exceeded at line 1 column 128\n\
err 1:636 recursion limit exceeded at line 1 column 636\n\
{\"x\":[],\"y\":null,\"z\":\"\u{7f}\\b\\f\"}\n\
[null,null,null,-1.5,0.00001,1.23e-18]\n\
";
