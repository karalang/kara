//! `karac build --mir`: the MIR backend compiled ahead of time (redesign M2).
//!
//! The IR `codegen::mir::compile_mir_ir` produces goes through
//! `codegen::mir_aot::compile_object` and the ordinary runtime link, and the
//! binary must print what the MIR interpreter prints, at every optimisation
//! level the build offers.
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
            std::env::temp_dir().join(format!("karac-mir-build-test-{}-{n}", std::process::id()));
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

const PROGRAM: &str = r#"
struct P { name: String, n: i64 }
fn main() {
    let mut v: Vec[P] = Vec.new();
    for i in 0..5 { v.push(P { name: f"p{i}", n: i * i }); }
    let total = v.iter().map(|p| p.n).sum();
    let mut m: Map[String, i64] = Map.new();
    for p in v.iter() { m[p.name.clone()] = p.n; }
    println(f"hello {total} {m.len()} {v[3].name}");
}
"#;

/// Build `src` at `opt`, link and run it: its stdout, or `None` to skip.
fn build_and_run(src: &str, opt: u8) -> Option<String> {
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
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).expect("utf-8"))
}

#[test]
fn mir_build_matches_the_interpreter_at_every_opt_level() {
    for opt in 0..=3 {
        if let Some(out) = build_and_run(PROGRAM, opt) {
            assert_eq!(out, "hello 30 5 p3\n", "at -O{opt}");
        }
    }
}

/// The CLI: `karac build --mir <file> -o <path>` writes the executable there.
#[test]
fn karac_build_mir_writes_the_executable() {
    let dir = Scratch::new();
    let src = dir.path().join("hello.kara");
    std::fs::write(&src, PROGRAM).unwrap();
    let exe = dir.path().join("hello");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([
            "build",
            "--mir",
            src.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if common::link_or_skip(if out.status.success() {
        Ok(())
    } else {
        Err(stderr.to_string())
    })
    .is_none()
    {
        return;
    }
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("Built: {}\n", exe.display())
    );
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hello 30 5 p3\n");
}

/// `#[derive(Serialize, Deserialize)]` and `Json` compiled: the same text
/// as on the MIR interpreter (`tests/mir_serde.rs`), at -O0 and -O2.
const SERDE_PROGRAM: &str = r#"
#[derive(Serialize, Deserialize)]
struct Config {
    #[serde(rename: "server_name")]
    name: String,
    #[serde(skip)]
    cache: Vec[i64],
    #[serde(default: 8080)]
    port: i64,
    shapes: Vec[Shape],
    sorted: SortedMap[i64, String],
    nick: Option[String],
}

#[derive(Serialize, Deserialize)]
enum Shape { Dot, Circle(i64), Rect(i64, i64), Named { label: String } }

#[derive(Serialize, Deserialize)]
#[serde(tag: "type")]
enum Event { Ping, Move { x: i64, y: i64 } }

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Value { Nothing, Num(i64), Text(String) }

fn main() {
    let mut sorted: SortedMap[i64, String] = SortedMap.new();
    sorted.insert(2, "two".to_string());
    sorted.insert(1, "one".to_string());
    let c = Config {
        name: "a\"b".to_string(),
        cache: vec![1],
        port: 9,
        shapes: vec![Shape.Dot, Shape.Circle(2), Shape.Rect(3, 4), Shape.Named { label: "t".to_string() }],
        sorted: sorted,
        nick: None,
    };
    let j = Json.serialize(c);
    println(j);
    let back: Result[Config, DeserializeError] = Json.deserialize(j);
    match back {
        Ok(b) => println(f"{b.name} {b.cache.len()} {b.port} {b.shapes.len()} {b.sorted.len()}"),
        Err(e) => println(e.message),
    }
    let sparse: Result[Config, DeserializeError] =
        Json.deserialize("{\"server_name\": \"s\", \"shapes\": [], \"sorted\": {}}");
    match sparse { Ok(b) => println(f"{b.port}"), Err(e) => println(e.message) }
    let evs = vec![Event.Ping, Event.Move { x: 1, y: 2 }];
    let je = Json.serialize(evs);
    let evs2: Result[Vec[Event], DeserializeError] = Json.deserialize(je);
    println(f"{je} {evs2.unwrap().len()}");
    let vs = vec![Value.Nothing, Value.Num(3), Value.Text("t".to_string())];
    let jv = Json.serialize(vs);
    let vs2: Result[Vec[Value], DeserializeError] = Json.deserialize(jv);
    println(f"{jv} {vs2.unwrap().len()}");
}
"#;

#[test]
fn mir_build_runs_derived_serde() {
    let want = "{\"server_name\":\"a\\\"b\",\"port\":9,\"shapes\":[\"Dot\",{\"Circle\":2},{\"Rect\":[3,4]},{\"Named\":{\"label\":\"t\"}}],\"sorted\":{\"1\":\"one\",\"2\":\"two\"},\"nick\":null}\n\
                a\"b 0 9 4 2\n\
                8080\n\
                [{\"type\":\"Ping\"},{\"type\":\"Move\",\"x\":1,\"y\":2}] 2\n\
                [null,3,\"t\"] 3\n";
    let interp = karac::mir::lower::run_source(SERDE_PROGRAM).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SERDE_PROGRAM, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// `for x in v.into_iter()` takes each element out of the `Vec` once,
/// front to back, without moving the rest up each round; what a `break` or
/// a `return` leaves drops front to back, compiled as interpreted.
const INTO_ITER_PROGRAM: &str = r#"
struct D { n: i64 }
impl Drop for D {
    fn drop(mut ref self) { println(f"drop {self.n}"); }
}
fn first_big(v: Vec[D]) -> i64 {
    for d in v.into_iter() {
        if d.n > 1 { return d.n; }
    }
    -1
}
fn first_own(v: own Vec[D]) -> i64 {
    for d in v.into_iter() {
        if d.n > 1 { return d.n; }
    }
    -1
}
fn main() {
    let mut v: Vec[D] = Vec.new();
    for i in 0..5 { v.push(D { n: i }); }
    for d in v.into_iter() {
        println(f"got {d.n}");
        if d.n == 2 { break; }
    }
    let mut w: Vec[D] = Vec.new();
    for i in 0..4 { w.push(D { n: 10 + i }); }
    for (i, d) in w.into_iter().enumerate() {
        if i == 1 { continue; }
        println(f"at {i} {d.n}");
    }
    let mut u: Vec[D] = Vec.new();
    for i in 0..4 { u.push(D { n: i }); }
    println(f"fb {first_big(u)}");
    let mut o: Vec[D] = Vec.new();
    for i in 0..4 { o.push(D { n: 20 + i }); }
    println(f"fo {first_own(o)}");
    let mut big: Vec[Vec[i64]] = Vec.new();
    for i in 0..1000 { big.push(vec![i, i]); }
    let mut s = 0;
    for x in big.into_iter() { s = s + x[0] + x[1]; }
    println(f"sum {s}");
}
"#;

#[test]
fn mir_build_into_iter_takes_front_to_back() {
    let want = "got 0\n\
                drop 0\n\
                got 1\n\
                drop 1\n\
                got 2\n\
                drop 2\n\
                drop 3\n\
                drop 4\n\
                at 0 10\n\
                drop 10\n\
                drop 11\n\
                at 2 12\n\
                drop 12\n\
                at 3 13\n\
                drop 13\n\
                fb 2\n\
                drop 20\n\
                drop 21\n\
                drop 22\n\
                drop 23\n\
                fo 20\n\
                sum 999000\n\
                drop 0\n\
                drop 1\n\
                drop 2\n\
                drop 3\n";
    let interp = karac::mir::lower::run_source(INTO_ITER_PROGRAM).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(INTO_ITER_PROGRAM, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// `karac build --mir` of `src`, saved as `main.kara` in `dir`, with `envs`
/// set: the executable, or `None` to skip when there is no runtime to link.
fn cli_build(dir: &Scratch, src: &str, envs: &[(&str, &str)]) -> Option<PathBuf> {
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    let exe = dir.path().join("main");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([
            "build",
            "--mir",
            file.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .envs(envs.iter().copied())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    common::link_or_skip(if out.status.success() {
        Ok(())
    } else {
        Err(stderr.to_string())
    })?;
    Some(exe)
}

/// A compiled runtime error says where it happened, as the MIR interpreter
/// does: the statement in the program's own code, or, for a panic inside
/// the library (`unwrap`), the program's call into it.
#[test]
fn mir_build_reports_where_a_panic_happened() {
    let cases = [
        (
            "fn pick(v: ref Vec[i64], i: i64) -> i64 {\n    v[i]\n}\n\
             fn main() {\n    let v = vec![1, 2, 3];\n    println(f\"{pick(v, 7)}\");\n}\n",
            "runtime error: index out of bounds\n  at {f}:2:5\nError return trace:\n  {f}:2:5\n",
        ),
        (
            "fn main() {\n    let o: Option[i64] = None;\n    println(\"before\");\n    \
             let x = o.unwrap();\n    println(f\"{x}\");\n}\n",
            "runtime error: called unwrap() on None\n  at {f}:4:13\nError return trace:\n  {f}:4:13\n",
        ),
        (
            "fn main() {\n    let x = 3;\n    if x > 2 { panic(\"too big\"); }\n}\n",
            "runtime error: too big\n  at {f}:3:16\nError return trace:\n  {f}:3:16\n",
        ),
    ];
    for (src, want) in cases {
        for opt in ["0", "2"] {
            let dir = Scratch::new();
            let Some(exe) = cli_build(&dir, src, &[("KARAC_MIR_OPT", opt)]) else {
                return;
            };
            let run = Command::new(&exe).output().unwrap();
            assert_eq!(run.status.code(), Some(101), "at -O{opt}");
            let f = dir.path().join("main.kara");
            let want = want.replace("{f}", &f.to_string_lossy());
            assert_eq!(String::from_utf8_lossy(&run.stderr), want, "at -O{opt}");
        }
    }
}

/// `KARAC_DEBUG_INFO=1` gives the compiled program line tables: gdb stops
/// at a Kāra line and its backtrace names the Kāra lines of each frame, at
/// -O0 and through -O2's inlining. Skipped without a usable gdb.
#[test]
fn mir_build_line_tables_drive_gdb() {
    let gdb_ok = Command::new("gdb")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !gdb_ok {
        eprintln!("skipping: no gdb");
        return;
    }
    let src = "fn pick(v: ref Vec[i64], i: i64) -> i64 {\n    v[i]\n}\n\n\
               fn main() {\n    let v = vec![1, 2, 3];\n    println(f\"{pick(v, 1)}\");\n}\n";
    for opt in ["0", "2"] {
        let dir = Scratch::new();
        let envs = [("KARAC_MIR_OPT", opt), ("KARAC_DEBUG_INFO", "1")];
        let Some(exe) = cli_build(&dir, src, &envs) else {
            return;
        };
        let out = Command::new("gdb")
            .args([
                "-batch",
                "-ex",
                "break main.kara:2",
                "-ex",
                "run",
                "-ex",
                "bt",
            ])
            .arg(&exe)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        if text.contains("ptrace") || text.contains("Operation not permitted") {
            eprintln!("skipping: gdb cannot trace here\n{text}");
            return;
        }
        let frames: Vec<&str> = text.lines().filter(|l| l.starts_with('#')).collect();
        assert!(
            frames.len() >= 2
                && frames[0].ends_with("main.kara:2")
                && frames[1].ends_with("main.kara:7"),
            "at -O{opt}:\n{text}"
        );
    }
}

/// A string cut past the end, or inside a char, panics as a bounds error
/// naming the cut, compiled and interpreted alike, not as an explicit
/// `panic(..)` (B-2026-10-13-13).
#[test]
fn mir_string_slice_panics_name_the_cut() {
    let cases = [
        (
            "fn main() {\n    let s = String.from(\"abc\");\n    let n = s.len() + 2;\n    \
             println(f\"{s[1..n]}\");\n}\n",
            "runtime error: string slice bounds 1..5 out of range (len 3)\n",
        ),
        (
            "fn main() {\n    let s = String.from(\"h\\u{e9}llo\");\n    let n = s.len() - 4;\n    \
             println(f\"{s[0..n]}\");\n}\n",
            "runtime error: E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY: byte range 0..2 does not fall \
             on UTF-8 char boundaries of \"h\u{e9}llo\"\n",
        ),
    ];
    for (src, want) in cases {
        let dir = Scratch::new();
        let file = dir.path().join("main.kara");
        std::fs::write(&file, src).unwrap();
        let interp = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["__mir-run", file.to_str().unwrap()])
            .output()
            .unwrap();
        let first = |b: &[u8]| {
            String::from_utf8_lossy(b)
                .lines()
                .next()
                .unwrap_or("")
                .to_string()
                + "\n"
        };
        assert_eq!(first(&interp.stderr), want, "__mir-run");
        let Some(exe) = cli_build(&dir, src, &[]) else {
            return;
        };
        let run = Command::new(&exe).output().unwrap();
        assert_eq!(run.status.code(), Some(101));
        assert_eq!(first(&run.stderr), want, "compiled");
    }
}

/// `a.ref_eq(b)` on a `shared` type, and the free `ref_eq(a, b)`, compare
/// the handles, not the values, on the interpreter and compiled alike
/// (B-2026-10-13-17).
#[test]
fn mir_ref_eq_compares_shared_handles() {
    const SRC: &str = "shared struct N { val: i64 }\n\
        fn same(x: N, y: N) -> bool { x.ref_eq(y) }\n\
        fn main() {\n    let a = N { val: 1 };\n    let b = a;\n    \
        let c = N { val: 1 };\n    \
        println(f\"{a.ref_eq(b)} {a.ref_eq(c)} {same(a, c)} {same(c, c)}\");\n    \
        println(f\"{ref_eq(a, b)} {ref_eq(a, c)}\");\n}\n";
    let want = "true false false true\ntrue false\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A program's own items named as the library's are (B-2026-10-12-21): a
/// `struct Peekable`, a variant `IterMap`, a function `serde_next`. The
/// library's keep working beside them, `peekable()` among them.
const LIBRARY_NAMES_PROGRAM: &str = r#"
struct Peekable { n: i64 }
enum Token { Letter(u8), IterMap(i64) }
fn serde_next(n: i64) -> i64 { n + 1 }
fn main() {
    let s = Peekable { n: 4 };
    let t = Token.IterMap(serde_next(2));
    match t {
        Token.IterMap(n) => println(f"map {n}"),
        Token.Letter(c) => println(f"letter {c}"),
    }
    let v = vec![1, 2, 3];
    let mut it = v.iter().peekable();
    match it.peek() {
        Some(x) => println(f"{x} {s.n}"),
        None => println("none"),
    }
    let d: Vec[i64] = v.iter().map(|x| x * s.n).collect();
    println(f"{d}");
}
"#;

#[test]
fn mir_program_items_may_take_library_names() {
    let want = "map 3\n1 4\n[4, 8, 12]\n";
    let interp = karac::mir::lower::run_source(LIBRARY_NAMES_PROGRAM).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(LIBRARY_NAMES_PROGRAM, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-13-19: `Vec.extend` takes any owned iterable, as the spec's
/// `fn extend[I: IntoIterator[Item = T]]` does: an iterator chain, a range,
/// a type's own `Iterator`, another collection, and lent `Copy` items.
#[test]
fn mir_vec_extend_takes_any_iterable() {
    const SRC: &str = "struct Count { n: i64 }\n\
        impl Iterator for Count {\n    type Item = i64;\n    \
        fn next(mut ref self) -> Option[i64] {\n        \
        if self.n == 0 { return None; }\n        self.n = self.n - 1;\n        Some(self.n)\n    }\n}\n\
        struct D { id: i64 }\n\
        impl Drop for D { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }\n\
        fn main() {\n    let v: Vec[i64] = vec![1, 2];\n    let mut d = v.clone();\n    \
        d.extend(v.iter().cloned());\n    d.extend(v.iter().map(|x| *x * 10));\n    \
        d.extend(v.iter());\n    d.extend(0..2);\n    d.extend(Count { n: 2 });\n    \
        let mut q: VecDeque[i64] = VecDeque.new();\n    q.push_back(9);\n    q.push_front(8);\n    \
        d.extend(q);\n    let mut s: SortedSet[i64] = SortedSet.new();\n    s.insert(5);\n    s.insert(4);\n    \
        d.extend(s);\n    d.extend(v.into_iter());\n    println(f\"{d}\");\n    \
        let mut m: SortedMap[i64, bool] = SortedMap.new();\n    m.insert(2, true);\n    m.insert(1, false);\n    \
        let mut e: Vec[(i64, bool)] = Vec.new();\n    e.extend(m);\n    println(f\"{e}\");\n    \
        let mut names: Vec[String] = vec![\"a\".to_string()];\n    \
        names.extend((0..2).map(|i| f\"n{i}\"));\n    println(f\"{names}\");\n    \
        let src: Vec[D] = vec![D { id: 1 }, D { id: 2 }];\n    let mut out: Vec[D] = Vec.new();\n    \
        out.extend(src.into_iter().filter(|x| x.id > 1));\n    println(f\"len {out.len()}\");\n}\n";
    let want = "[1, 2, 1, 2, 10, 20, 1, 2, 0, 1, 1, 0, 8, 9, 4, 5, 1, 2]\n\
        [(1, false), (2, true)]\n[a, n0, n1]\ndrop 1\nlen 1\ndrop 2\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A cast to a narrower type into a wider binding or return slot is the
/// cast, then the implicit widening (B-2026-10-13-21).
#[test]
fn mir_a_cast_into_a_wider_place_widens() {
    const SRC: &str = "fn code(c: char) -> i64 { c as i32 }\n\
        fn low(x: i64) -> i64 { x as u8 }\n\
        fn main() {\n    let x: i64 = 300;\n    let y: i64 = x as u8;\n    \
        let z: i64 = -x as i32;\n    let w: i64 = 'a' as i32;\n    \
        let f: f64 = x as f32;\n    \
        println(f\"{y} {z} {w} {code('b')} {low(259)} {f}\");\n}\n";
    let want = "44 -300 97 98 3 300.0\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-13-22: `x.powi(n)` raises a float to an `i32` power by repeated
/// squaring, with the same multiplications on both backends, so the bits
/// agree (design.md's `(x2 - x1).powi(2)`, kata 509's Binet arm).
#[test]
fn mir_float_powi_agrees_on_both_backends() {
    const SRC: &str = "fn main() {\n    let x: f64 = 1.5;\n    \
        println(f\"{x.powi(2)} {x.powi(0)} {x.powi(-3)} {2.0.powi(10)}\");\n    \
        let phi: f64 = (1.0 + 5.0.sqrt()) / 2.0;\n    let root5: f64 = 5.0.sqrt();\n    \
        let n: i64 = 19;\n    println(f\"{(phi.powi(n as i32) / root5).round() as i64}\");\n    \
        let y: f64 = 1.1;\n    println(f\"{y.powi(10).to_bits()} {y.powi(-7).to_bits()}\");\n    \
        let z: f32 = 1.1;\n    println(f\"{z.powi(10).to_bits32()} {z.powi(-2).to_bits32()}\");\n    \
        let big: f64 = 0.999999;\n    println(f\"{big.powi(2147483647)} {2.0.powi(-2147483648)}\");\n    \
        let (x1, y1, x2, y2) = (1.0, 2.0, 4.0, 6.0);\n    \
        println(f\"{((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt()}\");\n}\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    let want = interp.output.clone();
    let lines: Vec<&str> = want.lines().collect();
    assert_eq!(lines[0], "2.25 1.0 0.2962962962962963 1024.0");
    assert_eq!(lines[1], "4181");
    // Repeated squaring, in this order, as plain Rust computes it.
    let sq = |x: f64, n: i32| -> f64 {
        let (mut r, mut b, mut e) = (1.0, x, n.unsigned_abs());
        while e != 0 {
            if e & 1 == 1 {
                r *= b;
            }
            b *= b;
            e >>= 1;
        }
        if n < 0 {
            1.0 / r
        } else {
            r
        }
    };
    assert_eq!(
        lines[2],
        format!("{} {}", sq(1.1, 10).to_bits(), sq(1.1, -7).to_bits())
    );
    assert_eq!(lines[4], "0.0 0.0");
    assert_eq!(lines[5], "5.0");
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-12-5: the string-receiver `.parse()` sugar takes a `Str` (a
/// `Str` parameter, a slice of one, `as_str()`) as it takes a `String`.
#[test]
fn mir_str_receiver_parse() {
    const SRC: &str = "fn whole(s: Str) -> Option[i64] {\n    s.parse()\n}\n\
        fn head(s: Str) -> i64 {\n    let m: Option[i64] = s[0..1].parse();\n    m.unwrap()\n}\n\
        fn byref(s: ref String) -> Option[u8] {\n    s.parse()\n}\n\
        fn main() {\n    let t = String.from(\"42\");\n    \
        let x: Option[f64] = t.as_str().parse();\n    \
        println(f\"{whole(\"42\").unwrap()} {head(\"42\")} {byref(t).unwrap()} {x.unwrap()}\");\n    \
        let bad: Option[i64] = \"4x\".parse();\n    println(f\"{bad.is_none()}\");\n}\n";
    let want = "42 4 42 42.0\ntrue\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// `c.into_iter()` on an owned set, map or deque of non-`Copy` elements
/// moves them out, in a `for`, a `collect` and an `extend`; a `break`
/// drops what is left (B-2026-10-13-14).
const INTO_ITER_COLLECTIONS: &str = r#"
struct D { id: i64 }
impl Drop for D { fn drop(mut ref self) { println(f"drop {self.id}"); } }
fn main() {
    let mut found: Set[Vec[i64]] = Set.new();
    found.insert(vec![1, 2]);
    found.insert(vec![3]);
    let mut out: Vec[Vec[i64]] = Vec.new();
    for s in found.into_iter() {
        out.push(s);
    }
    println(f"{out.len()} {out[0].len() + out[1].len()}");
    let mut names: Set[String] = Set.new();
    names.insert("a".to_string());
    let got: Vec[String] = names.into_iter().collect();
    println(f"{got}");
    let mut q: VecDeque[String] = VecDeque.new();
    q.push_back("x".to_string());
    q.push_front("w".to_string());
    for s in q.into_iter() { println(s); }
    let mut m: Map[i64, String] = Map.new();
    m.insert(1, "one".to_string());
    for (k, v) in m.into_iter() { println(f"{k} {v}"); }
    let mut sm: SortedMap[i64, D] = SortedMap.new();
    sm.insert(2, D { id: 20 });
    sm.insert(1, D { id: 10 });
    sm.insert(3, D { id: 30 });
    for (k, d) in sm.into_iter() {
        if k == 2 { break; }
        println(f"got {k} {d.id}");
    }
    println("after");
    let mut ss: SortedSet[String] = SortedSet.new();
    ss.insert("b".to_string());
    ss.insert("a".to_string());
    let mut e: Vec[String] = Vec.new();
    e.extend(ss);
    println(f"{e}");
    let mut c: Set[i64] = Set.new();
    c.insert(3);
    for x in c.into_iter() { println(f"{x}"); }
}
"#;

#[test]
fn mir_into_iter_moves_out_of_every_collection() {
    let want = "2 3
[a]
w
x
1 one
got 1 10
drop 10
drop 20
drop 30
after
[a, b]
3
";
    let interp = karac::mir::lower::run_source(INTO_ITER_COLLECTIONS).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(INTO_ITER_COLLECTIONS, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

// B-2026-10-12-13 — a string literal where a `Str` is expected is the view of
// its static data on both backends: a `let` annotation, a `vec!` element, a
// tuple slot, a tail expression, an assignment, a struct field, a match arm.
#[test]
fn mir_string_literal_takes_an_expected_str() {
    const SRC: &str = r#"struct P { name: Str, n: i64 }
fn f(s: Str) -> i64 { s.len() }
fn g() -> Str { "abc" }
fn pick(b: bool) -> Str { match b { true => "yes", false => "no" } }
fn main() {
    let x: Str = "abc";
    let v: Vec[Str] = vec!["ab", "c"];
    let t: (Str, i64) = ("ab", 1);
    let mut y: Str = "a";
    y = "bc";
    let p = P { name: "lbl", n: 2 };
    println(f"{f(x)} {x} {v[0]} {f(v[1])} {t.0} {f(t.0)} {g()} {y} {f(y)}");
    println(f"{p.name} {p.n} {pick(true)} {pick(false)}");
}
"#;
    let want = "3 abc ab 1 ab 2 abc bc 2\nlbl 2 yes no\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

// B-2026-10-12-16 — matching through a tuple of `Map.get` results binds the
// `ref`s each `get` lends, on both backends.
#[test]
fn mir_tuple_scrutinee_of_map_gets() {
    const SRC: &str = r#"fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert(String.from("a"), 1);
    m.insert(String.from("b"), 2);
    match (m.get("a"), m.get("b")) {
        (Some(i), Some(j)) => println(f"{*i + *j}"),
        _ => println("none"),
    }
    match (m.get("a"), m.get("q")) {
        (Some(i), None) | (None, Some(i)) => println(f"{*i}"),
        _ => println("both or none"),
    }
    match ((m.get("b"), 1), 2) {
        ((Some(i), k), l) => println(f"{*i + k + l}"),
        _ => println("none"),
    }
}
"#;
    let want = "3\n1\n5\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A strong handle or an `Option` of one stored into a `weak` field from a
/// call, `Some(a)`, another weak field or a struct literal is downgraded:
/// the object goes when its last strong handle does (B-2026-10-13-23).
const WEAK_STORES: &str = r#"
shared struct Node { val: i64, mut parent: weak Node }
fn get(n: Node) -> Node { n }
fn find(n: Node) -> Option[Node] { Some(n) }
fn main() {
    let c = Node { val: 3, parent: None };
    let b = Node { val: 2, parent: None };
    {
        let a = Node { val: 1, parent: None };
        b.parent = a;
        c.parent = get(a);
        println(f"{c.parent.expect("set").val}");
        c.parent = b.parent.expect("b");
        c.parent = b.parent;
        c.parent = Some(a);
        c.parent = find(a);
        let d = Node { val: 4, parent: get(a) };
        println(f"{d.parent.expect("d").val} {c.parent.is_some()}");
    }
    println(f"{c.parent.is_none()} {b.parent.is_none()}");
}
"#;

#[test]
fn mir_weak_stores_downgrade_every_value() {
    let want = "1
1 true
true true
";
    let interp = karac::mir::lower::run_source(WEAK_STORES).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(WEAK_STORES, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

// B-2026-10-13-26 — `chars()` on a `Str` is an iterator value on both
// backends, as it is on a `String`: adaptors, `rev`, `next`, a collect into a
// `String`, multi-byte chars, and the plain `for` and `Vec[char]` forms.
#[test]
fn mir_str_chars_is_an_iterator() {
    const SRC: &str = r#"fn e(w: Str) { for (i, c) in w.chars().enumerate() { print(f"{i}{c} "); } println(""); }
fn fc(w: Str) -> i64 { w.chars().filter(|c| c.is_ascii_uppercase()).count() }
fn r(w: Str) { for c in w.chars().rev() { print(f"{c} "); } println(""); }
fn a(w: Str) -> bool { w.chars().all(|c| c.is_ascii_uppercase()) }
fn n(w: Str) -> Option[char] { let mut it = w.chars(); it.next() }
fn m(w: Str) -> String { let s: String = w.chars().map(|c| c.to_ascii_lowercase()).collect(); s }
fn plain(w: Str) -> i64 { let mut k = 0; for c in w.chars() { if c == 'é' { k += 10; } k += 1; } k }
fn v(w: Str) -> Vec[char] { w.chars().collect() }
fn main() {
    e("Ab");
    println(f"{fc("Ab")} {a("Ab")} {n("Ab")} {n("")}");
    r("Ab");
    println(m("Ab"));
    let owned = String.from("xé€y");
    e(owned.as_str());
    r(owned.as_str());
    println(f"{plain(owned.as_str())} {v("hé")}");
    let s: Str = "QRS";
    let back: String = s.chars().rev().collect();
    println(back);
}
"#;
    let want = "0A 1b \n1 false Some(A) None\nb A \nab\n0x 1é 2€ 3y \ny € é x \n14 [h, é]\nSRQ\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

// B-2026-10-13-27 — `T.default()` on a scalar, `String` or a collection, a
// derived `Default` over collection, `Option` and tuple fields, and
// `take(place)` of a collection place, on both backends.
#[test]
fn mir_standard_types_are_default() {
    const SRC: &str = r#"#[derive(Default)]
struct Config { names: Vec[String], count: i64, title: String, tags: Map[String, i64], best: Option[String], pair: (i64, String), q: VecDeque[u8], seen: Set[i64] }
#[derive(Default)]
struct Outer { c: Config, on: bool }
fn main() {
    let c = Config.default();
    println(f"{c.names.len()} {c.count} [{c.title}] {c.tags.len()} {c.best} {c.pair.0} [{c.pair.1}] {c.q.len()} {c.seen.len()}");
    let o = Outer.default();
    let inner = o.c;
    println(f"{inner.names.len()} {o.on}");
    let mut w: Vec[Vec[i64]] = vec![vec![1, 2], vec![3]];
    let t = take(mut w[0]);
    let s = String.default();
    let v: Vec[i64] = Vec.default();
    println(f"{t.len()} {w[0].len()} [{s}] {v.len()}");
    let x = i64.default();
    let b = bool.default();
    let f = f64.default();
    let ch = char.default();
    let u = u8.default();
    println(f"{x} {b} {f} {ch as u32} {u}");
}
"#;
    let want = "0 0 [] 0 None 0 [] 0 0\n0 false\n2 0 [] 0\n0 false 0.0 0 0\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

// B-2026-10-13-25 — `mem.swap` / `mem.replace` / `mem.take` on both
// backends, on locals and on places, and a local `mem` with its own `take`.
#[test]
fn mir_mem_module_functions() {
    const SRC: &str = r#"struct Node { kids: Vec[i64], name: String }
struct Mem { n: i64 }
impl Mem { fn take(mut ref self, k: i64) -> i64 { self.n -= k; self.n } }
fn other() -> i64 { let mut mem = Mem { n: 10 }; mem.take(3) }
fn main() {
    let mut a: Vec[i64] = vec![1];
    let mut b: Vec[i64] = vec![2, 3];
    mem.swap(mut a, mut b);
    let old = mem.replace(mut a, vec![9]);
    let gone = mem.take(mut b);
    println(f"{a.len()} {b.len()} {old.len()} {gone.len()}");
    let mut nodes: Vec[Node] = vec![Node { kids: vec![1, 2], name: String.from("x") }];
    let k = mem.take(mut nodes[0].kids);
    let n = mem.replace(mut nodes[0].name, String.from("y"));
    println(f"{k.len()} {n} {nodes[0].kids.len()} {nodes[0].name} {other()}");
}
"#;
    let want = "1 0 2 1\n2 x 0 y 7\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

// B-2026-10-13-5 — a conditional impl is used where its bounds hold
// (nested, through a where clause, and from a generic caller), and a
// program whose argument misses a bound is refused at type check rather
// than reaching the builder with an unresolved `.show`.
#[test]
fn mir_conditional_impl_bounds_are_checked() {
    const PRELUDE: &str = r#"trait Show { fn show(ref self) -> String; }
impl Show for i64 { fn show(ref self) -> String { "i".to_string() } }
struct Wrap[T] { v: T }
impl[T: Show] Show for Wrap[T] { fn show(ref self) -> String { f"w({self.v.show()})" } }
struct Pair[A, B] { a: A, b: B }
impl[A, B] Show for Pair[A, B] where A: Show, B: Show {
    fn show(ref self) -> String { f"{self.a.show()},{self.b.show()}" }
}
struct Plain { n: i64 }
fn print_it[X: Show](x: ref X) { println(x.show()); }
"#;
    let src = format!(
        "{PRELUDE}fn lift[T: Show](x: own T) {{ print_it(Wrap {{ v: x }}); }}
fn main() {{
    print_it(Wrap {{ v: 3 }});
    print_it(Wrap {{ v: Wrap {{ v: 4 }} }});
    print_it(Pair {{ a: 1, b: Wrap {{ v: 2 }} }});
    lift(5);
}}
"
    );
    let want = "w(i)\nw(w(i))\ni,w(i)\nw(i)\n";
    let interp = karac::mir::lower::run_source(&src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(&src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
    let bad = format!("{PRELUDE}fn main() {{ print_it(Wrap {{ v: Plain {{ n: 1 }} }}); }}\n");
    let err = karac::mir::lower::run_source(&bad).expect_err("refused");
    assert!(
        format!("{err:?}").contains("`Wrap[Plain]` does not implement `Show`"),
        "{err:?}"
    );
}

// B-2026-10-14-3: the interpreter's decoded calls of small functions
// (tuple arguments read through a `Vec` element's reference, a tuple or
// scalar result) and its checked steps folded into their overflow switch
// give what the general steps give, and an overflow inside such a call
// still aborts.
#[test]
fn mir_decoded_calls_and_checked_steps() {
    const SRC: &str = r#"fn dist(a: (i64, i64), b: (i64, i64)) -> i64 {
    (a.0 - b.0) * (a.0 - b.0) + (a.1 - b.1) * (a.1 - b.1)
}
fn mid(a: (i64, i64), b: (i64, i64)) -> (i64, i64) {
    ((a.0 + b.0) / 2, (a.1 + b.1) / 2)
}
fn square(x: i64) -> i64 { x * x }
fn main() {
    let ps: Vec[(i64, i64)] = vec![(0, 0), (3, 4), (-6, 8)];
    let mut total = 0;
    for i in 0..ps.len() {
        for j in 0..ps.len() {
            total += dist(ps[i], ps[j]);
        }
    }
    let m = mid(ps[1], ps[2]);
    println(f"{total} {m.0} {m.1} {square(-7)}");
}
"#;
    let want = "444 -1 6 49\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
    const OVERFLOW: &str = r#"fn square(x: i64) -> i64 { x * x }
fn main() {
    let mut t = 0;
    for i in 0..3 {
        t += square(i);
    }
    println(f"{t}");
    println(f"{square(3037000500)}");
}
"#;
    let r = karac::mir::lower::run_source(OVERFLOW).expect("MIR interpreter");
    assert_eq!(r.output, "5\n");
    assert!(
        matches!(r.outcome, karac::mir::interp::Outcome::Aborted(ref why) if format!("{why:?}").contains("Overflow")),
        "{:?}",
        r.outcome
    );
}

// B-2026-10-13-33 — a `mut` field of a `shared` value reached by indexing a
// borrowed `Vec` is written through the handle (core-semantics §5.9): the
// element is read with `index`, not borrowed with `index_mut`, so a bare
// `Vec` parameter, a `ref` local and a field of a `ref` struct all take
// the write; an owned `Vec` still does.
#[test]
fn mir_shared_field_write_through_an_indexed_borrow() {
    const SRC: &str = r#"shared struct Node {
    mut val: i64,
    mut next: Option[Node],
}

struct Holder {
    mut ns: Vec[Node],
}

fn set(nodes: Vec[Node]) {
    nodes[1].val = 5;
    nodes[0].val += 10;
}

fn bump(h: ref Holder) {
    h.ns[0].val = 40;
}

fn main() {
    let nodes: Vec[Node] = vec![Node { val: 1, next: None }, Node { val: 2, next: None }];
    set(nodes);
    println(f"{nodes[0].val} {nodes[1].val}");
    let lists: Vec[Vec[Node]] = vec![vec![Node { val: 7, next: None }]];
    let ns = ref lists[0];
    ns[0].val = 8;
    ns[0].next = Some(Node { val: 9, next: None });
    println(f"{lists[0][0].val} {lists[0][0].next.unwrap().val}");
    let h = Holder { ns: vec![Node { val: 3, next: None }] };
    bump(h);
    println(f"{h.ns[0].val}");
    let mut owned: Vec[Node] = vec![Node { val: 1, next: None }];
    owned[0].val = 2;
    println(f"{owned[0].val}");
}
"#;
    let want = "11 5
8 9
40
2
";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

// B-2026-10-14-2 — a `ref char` from `Vec[char].iter()` is read as its value
// by text's char-taking methods (`push`, `find`, `split`), on a `String` and
// on a `Str`, as it is by a user fn's `char` parameter (§5.10).
#[test]
fn mir_text_methods_read_a_ref_char() {
    const SRC: &str = r#"fn main() {
    let days: Vec[char] = vec!['a', 'b', ','];
    let mut s = String.new();
    for c in days.iter() {
        s.push(c);
    }
    println(s);
    let t = "x,y,ab";
    let owned = String.from("ba,c");
    for c in days.iter() {
        println(f"{t.find(c)} {t.split(c).len()} {owned.find(c)} {owned.split(c).len()}");
    }
}
"#;
    let want = "ab,\nSome(4) 2 Some(1) 2\nSome(5) 2 Some(0) 2\nSome(1) 3 Some(2) 2\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

// B-2026-10-14-4 — `join` and `concat` reach a `Slice[String]` (a range
// index, `as_slice()`), with a literal or a `Str` separator, as they reach
// a `Vec[String]`.
#[test]
fn mir_join_and_concat_of_a_string_slice() {
    const SRC: &str = r#"fn main() {
    let parts: Vec[String] = vec![String.from("a"), String.from("b"), String.from("c")];
    let rest = parts[1..].join("/");
    println(f"{rest}");
    println(parts.as_slice().join(", "));
    println(parts[1..].concat());
    let sep = String.from("-");
    println(parts[..2].join(sep.as_str()));
    println(parts.join("|"));
    let empty: Vec[String] = Vec.new();
    println(f"[{empty[..].join(",")}]");
}
"#;
    let want = "b/c\na, b, c\nbc\na-b\na|b|c\n[]\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

// B-2026-10-14-17 — `collect()` into a `VecDeque` builds it front to back
// on both MIR backends, through the library's `FromIterator` impl, as a
// `Vec`, `Set`, `Map` or `String` target does.
#[test]
fn mir_collect_into_a_vec_deque() {
    const SRC: &str = r#"fn main() {
    let v: Vec[i64] = vec![3, 1, 2, 3];
    let d: VecDeque[i64] = v.iter().map(|x| x * 2).collect();
    let words: Vec[String] = vec!["b".to_string(), "a".to_string()];
    let ds: VecDeque[String] = words.iter().map(|w| w.to_uppercase()).collect();
    println(f"{d.len()} {d.front()} {d.back()} {ds.front()} {ds.back()}");
}
"#;
    let want = "4 Some(6) Some(6) Some(B) Some(A)\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

// B-2026-10-14-11 — a `for` over a `Vec[ref Node]` binds each element, the
// reference itself (it was read through once more, a `Node` copied into a
// `ref Node` slot), and the element borrows the tree, not the `Vec`, so a
// breadth-first walk can replace its level with the next one.
#[test]
fn mir_for_over_a_vec_of_references() {
    const SRC: &str = r#"struct Node {
    val: i64,
    children: Vec[Node],
}

fn mk(v: i64) -> Node {
    Node { val: v, children: vec![Node { val: v + 1, children: Vec.new() }, Node { val: v + 2, children: Vec.new() }] }
}

fn depth(root: ref Node) -> i64 {
    let mut level: Vec[ref Node] = Vec.new();
    level.push(root);
    let mut d = 0;
    while level.len() > 0 {
        d += 1;
        let mut next: Vec[ref Node] = Vec.new();
        for node in level {
            for child in node.children.iter() {
                next.push(child);
            }
        }
        level = next;
    }
    d
}

fn main() {
    let roots = vec![mk(1), mk(10)];
    let mut v: Vec[ref Node] = Vec.new();
    for r in roots.iter() {
        v.push(r);
    }
    let mut total = 0;
    for n in v {
        total += n.val;
    }
    for n in v.iter() {
        total += n.children.len();
    }
    for (i, n) in v.iter().enumerate() {
        total += i * n.val;
    }
    println(f"{total} {depth(roots[0])}");
}
"#;
    let want = "25 2\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

// B-2026-10-14-20 — `collect()` into a `SortedSet` or `SortedMap` builds it
// in key order on both MIR backends, through the library's `FromIterator`.
#[test]
fn mir_collect_into_sorted_set_and_map() {
    const SRC: &str = r#"fn main() {
    let v: Vec[i64] = vec![3, 1, 2, 3];
    let ss: SortedSet[i64] = v.iter().map(|x| x * 2).collect();
    let sm: SortedMap[i64, i64] = v.iter().map(|x| (x * 1, x * 10)).collect();
    let words: Vec[String] = vec!["b".to_string(), "a".to_string(), "b".to_string()];
    let ws: SortedSet[String] = words.iter().map(|w| w.to_uppercase()).collect();
    let mut keys: Vec[i64] = Vec.new();
    for k in ss.iter() { keys.push(*k); }
    let mut names: Vec[String] = Vec.new();
    for w in ws.iter() { names.push(w.clone()); }
    println(f"{keys} {sm.len()} {sm.get(2)} {names}");
}
"#;
    let want = "[2, 4, 6] 3 Some(20) [A, B]\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

// B-2026-10-14-15 — `chunks` and `windows` on a `Vec` receiver find the
// `Slice` methods on the MIR interpreter, as on the compiled backend.
#[test]
fn mir_vec_chunks_and_windows() {
    const SRC: &str = r#"fn main() {
    let mut nums: Vec[i64] = vec![1, 4, 3, 2];
    nums.sort();
    let s: i64 = nums.chunks(2).iter().map(|pair| pair[0]).sum();
    let v = vec![1, 2, 3, 4, 5];
    let c = v.chunks(2);
    for w in v.windows(2) {
        println(f"{w[0]} {w[1]}");
    }
    println(f"{s} {c.len()} {c[2].len()} {v.windows(6).len()}");
}
"#;
    let want = "1 2\n2 3\n3 4\n4 5\n4 3 1 0\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-14-6 — a `map` closure's bare `collect()` takes its target from
/// the outer `let` annotation's element type on the MIR pipeline too.
#[test]
fn mir_map_closure_collect_takes_the_outer_element_type() {
    const SRC: &str = r#"fn main() {
    let words: Vec[String] = vec!["ab".to_string(), "cd".to_string()];
    let r: Vec[Vec[char]] = words.iter().map(|w| w.chars().rev().collect()).collect();
    println(f"{r}");
    let s: Vec[String] = words.iter().map(|w| w.chars().rev().collect()).collect();
    println(f"{s}");
    let t: Vec[String] = words.iter().map(|w| { let n = w.len(); w.chars().take(n - 1).collect() }).collect();
    println(f"{t}");
}
"#;
    let want = "[[b, a], [d, c]]\n[ba, dc]\n[a, c]\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-14-19 — a `collect()` that is an element of a tuple literal
/// takes its target from the tuple's slot, written on a `let` or named by
/// the `Vec` it is pushed into.
#[test]
fn mir_collect_in_a_tuple_literal_takes_the_slot_type() {
    const SRC: &str = r#"fn main() {
    let s = "abc".to_string();
    let t: (Vec[char], i64) = (s.chars().collect(), 1);
    let mut pairs: Vec[(Vec[char], i64)] = Vec.new();
    pairs.push((s.chars().collect(), 2));
    let u: (String, i64) = (s.chars().rev().collect(), 3);
    println(f"{t.0.len()} {pairs[0].0.len()} {pairs[0].1} {u.0} {u.1}");
}
"#;
    let want = "3 3 2 cba 3\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

// B-2026-10-14-30 — a user type's operator impls: `a + b` calls its `add`
// (by value or by `ref`, `Self` and `Self.Output` results), `a += b` is
// `a = a + b`, the bitwise traits dispatch the same way, and a `T: Add`
// body instantiated at the user type calls the impl.
#[test]
fn mir_user_operator_impls() {
    const SRC: &str = r#"struct V2 {
    x: i64,
    tag: String,
}

impl Add for V2 {
    fn add(own self, rhs: own V2) -> V2 {
        V2 { x: self.x + rhs.x, tag: self.tag + rhs.tag }
    }
}

impl Sub for V2 {
    fn sub(ref self, rhs: ref V2) -> Self {
        V2 { x: self.x - rhs.x, tag: self.tag.clone() }
    }
}

impl Mul for V2 {
    type Output = i64;
    fn mul(ref self, rhs: ref V2) -> Self.Output {
        self.x * rhs.x
    }
}

#[derive(Copy, Clone)]
struct Bits {
    b: i64,
}

impl BitOr for Bits {
    fn bitor(own self, rhs: own Bits) -> Bits {
        Bits { b: self.b | rhs.b }
    }
}

impl Shl for Bits {
    fn shl(own self, rhs: own Bits) -> Bits {
        Bits { b: self.b << rhs.b }
    }
}

fn sum2[T: Add](a: own T, b: own T) -> T {
    a + b
}

fn main() {
    let a = V2 { x: 3, tag: String.from("a") };
    let b = V2 { x: 10, tag: String.from("b") };
    let d = a - b;
    println(f"{a * b} {d.x} {d.tag}");
    let c = a + b;
    println(f"{c.x} {c.tag}");
    let mut acc = V2 { x: 0, tag: String.from("") };
    for i in 0..4 {
        acc += V2 { x: i, tag: f"{i}" };
    }
    acc -= d;
    let s = sum2(acc, V2 { x: 100, tag: String.from("!") });
    println(f"{s.x} {s.tag} {sum2(3, 4)}");
    let mut m = Bits { b: 1 } | Bits { b: 4 };
    m <<= Bits { b: 1 };
    println(f"{m.b} {(m << Bits { b: 2 }).b}");
}
"#;
    let want = "30 -7 a\n13 ab\n113 0123! 7\n10 40\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

// B-2026-10-14-31: `if let` / `while let` on a bare `None` take the
// payload's type from the body. The compiled backend panicked in
// src/codegen/mir/body.rs on the `()` payload typed HIR used to give it.
#[test]
fn mir_if_let_on_a_bare_none() {
    const SRC: &str = r#"fn main() {
    let c = 5;
    let mut best = 0;
    if let Some(below) = None {
        best = c + below;
    }
    let o = if let Some(b) = None { c + b } else { 1 };
    while let Some(b) = None {
        best = c + b;
    }
    println(f"{best} {o}");
}
"#;
    let want = "0 1\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-14-13 — an owned local pushed into a `Vec[ref Node]` is lent,
/// not moved: it stays usable, and the tree is dropped once, by its owner.
#[test]
fn mir_owned_local_pushed_into_a_vec_of_references_is_lent() {
    const SRC: &str = r#"struct Node { val: i64, children: Vec[Node] }
fn mk(v: i64) -> Node { Node { val: v, children: vec![Node { val: v + 1, children: Vec.new() }] } }
fn main() {
    let t = mk(1);
    let u = mk(10);
    let mut v: Vec[ref Node] = Vec.new();
    v.push(t);
    v.push(t);
    v.push(u);
    let mut total = 0;
    for n in v.iter() { total += n.val + n.children.len(); }
    println(f"{v.len()} {total} {t.val} {t.children[0].val}");
}
"#;
    let want = "3 15 1 2\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    assert!(
        !matches!(interp.outcome, karac::mir::interp::Outcome::Error(_)),
        "{:?}",
        interp.outcome
    );
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}
