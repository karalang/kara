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

/// A program cut into codegen units (`KARAC_MIR_CODEGEN_UNITS`, which a
/// large program gets by default) runs as the whole one does: each unit
/// keeps its own bodies and declares the rest, and the objects are merged.
#[test]
fn karac_build_mir_in_codegen_units_matches_one_unit() {
    let dir = Scratch::new();
    let src = dir.path().join("hello.kara");
    std::fs::write(&src, PROGRAM).unwrap();
    for units in ["1", "2", "3", "5"] {
        let exe = dir.path().join(format!("hello{units}"));
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .env("KARAC_MIR_CODEGEN_UNITS", units)
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
        let run = Command::new(&exe).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            "hello 30 5 p3\n",
            "in {units} units"
        );
    }
}

/// A table of literals only borrowed is a static, built once
/// (`mir::literal_tables`); one pushed to or returned stays a local `Vec`.
/// Compiled, both print what the interpreter prints.
#[test]
fn mir_build_literal_tables_match_the_interpreter() {
    const SRC: &str = r#"
fn morse(word: String) -> String {
    let codes = [".-", "-...", "-.-.", "-.."];
    let mut out = String.new();
    for b in word.bytes() { out.push_str(codes[(b - 97) as i64]); }
    out
}
fn step(k: i64) -> i64 {
    let d = [1, 0, -1, 0];
    d[k] + d.len() as i64
}
fn grown() -> Vec[String] {
    let mut t = ["x", "y"];
    t.push("z".to_string());
    t
}
fn main() {
    let mut n = 0;
    for i in 0..1000 { n = n + morse("abcd".to_string()).len() + step(i % 4); }
    println(f"{n} {morse("dcba".to_string())} {grown().len()}");
}
"#;
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, "17000 -..-.-.-....- 3\n", "at -O{opt}");
        }
    }
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
/// -O0 and through -O2's inlining. Skipped without a usable gdb. The
/// `Vec` is collected and the index comes from the program's arguments,
/// so -O2 cannot fold the call away now that it sees a literal `Vec`'s
/// allocation and contents.
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
               fn main() {\n    let v: Vec[i64] = \"1 2 3\".split(\" \").map(|s| i64.parse(s).unwrap()).collect();\n    println(f\"{pick(v, env.args().len())}\");\n}\n";
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
// by text's char-taking methods (`push`, `to_string`), on a `String` and
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
        let sep = c.to_string();
        println(f"{t.find(c.to_string())} {t.split(sep.as_str()).count()} {owned.find(c.to_string())} {owned.split(sep.as_str()).count()}");
    }
}
"#;
    // `split` and `find` take a `Str` pattern (docs/library/strings.md,
    // B-2026-10-20-22), so the char is given as text there; `push` and
    // `to_string` read the `ref char`.
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

// B-2026-10-15-76 — an operator impl's `Output` is the operator's result
// type and `Rhs` picks the impl (design.md § Operator traits): `Instant -
// Instant` is a `Duration`, `Instant + Duration` an `Instant`, `Duration *
// i64` and `Duration * Duration` call different impls, a type's `impl Add`
// and `impl Mul` each keep their own `Output`, and `T.Output` at a number is
// the number.
#[test]
fn mir_operator_impls_take_their_output_and_rhs() {
    const SRC: &str = r#"struct Duration {
    ns: i64,
}

struct Instant {
    ns: i64,
}

impl Sub for Instant {
    type Output = Duration;
    fn sub(own self, rhs: own Instant) -> Duration {
        Duration { ns: self.ns - rhs.ns }
    }
}

impl Add[Duration] for Instant {
    type Output = Instant;
    fn add(own self, rhs: own Duration) -> Instant {
        Instant { ns: self.ns + rhs.ns }
    }
}

impl Mul for Duration {
    type Output = Duration;
    fn mul(own self, rhs: own Duration) -> Self.Output {
        Duration { ns: self.ns * rhs.ns }
    }
}

impl Mul[i64] for Duration {
    type Output = Duration;
    fn mul(own self, rhs: own i64) -> Duration {
        Duration { ns: self.ns * rhs }
    }
}

struct V {
    x: i64,
}

impl Add for V {
    type Output = V;
    fn add(own self, rhs: own V) -> Self.Output {
        V { x: self.x + rhs.x }
    }
}

impl Mul for V {
    type Output = i64;
    fn mul(own self, rhs: own V) -> Self.Output {
        self.x * rhs.x
    }
}

fn sum2[T: Add](a: own T, b: own T) -> T.Output {
    a + b
}

fn main() {
    let later = Instant { ns: 50 };
    let d = later - Instant { ns: 20 };
    let dn = d.ns;
    let mut t = Instant { ns: 1 } + d;
    t += Duration { ns: 4 };
    let e = Duration { ns: 3 } * 4;
    let f = Duration { ns: 5 } * Duration { ns: 6 };
    println(f"{dn} {t.ns} {e.ns} {f.ns}");
    let v = V { x: 2 } + V { x: 3 };
    let n = V { x: 2 } * V { x: 3 };
    println(f"{v.x} {n} {sum2(3, 4)} {sum2(1.5, 2.0)}");
}
"#;
    let want = "30 35 12 30\n5 6 7 3.5\n";
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

/// B-2026-10-14-26 — `ref place` on a field (design.md § `*` and `ref` in
/// expressions) borrows that place on the MIR pipeline, bound to a local or
/// returned, as § Returns spells it.
#[test]
fn mir_ref_of_a_field_borrows_it() {
    const SRC: &str = r#"
struct User {
    name: String,
    age: i64,
}

struct Node { val: i64, next: Option[Node] }

struct List { head: Node }

impl List {
    fn head(self) -> ref Node { ref self.head }
    fn last(self) -> ref Node {
        let mut node = ref self.head;
        while let Some(n) = node.next {
            node = n;
        }
        node
    }
}

fn name(user: User) -> ref String {
    ref user.name
}

fn main() {
    let u = User { name: "ada".to_string(), age: 36 };
    println(f"{name(u)} {u.age}");
    let h = ref u.name;
    println(f"{h} {h.len()} {u.age}");
    let l = List { head: Node { val: 1, next: Some(Node { val: 2, next: Some(Node { val: 3, next: None }) }) } };
    println(f"{l.head().val} {l.last().val}");
    let p = (u, 5);
    let q = ref p.0.name;
    println(f"{q} {p.1}");
}
"#;
    let want = "ada 36\nada 3 36\n1 3\nada 5\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// An array receiver of the library's `Slice` methods is viewed as a slice,
/// on a local and on a temporary alike (B-2026-10-14-41).
#[test]
fn mir_array_receiver_of_a_slice_method_is_viewed() {
    const SRC: &str = r#"
fn mk() -> Array[i64, 3] { return [10, 20, 30]; }

fn main() {
    let m = mk();
    println(m.iter().count());
    let s: i64 = m.iter().sum();
    println(s);
    println(mk().iter().count());
    let t: i64 = mk().iter().map(|x| x * 2).sum();
    println(t);
    for x in mk().iter() {
        println(x);
    }
}
"#;
    let want = "3\n60\n3\n120\n10\n20\n30\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-14-40 — `??` reads a `ref` of a `Copy` `Option` / `Result`
/// through the reference on the compiled backend too.
#[test]
fn mir_coalesce_reads_a_ref_of_a_copy_wrapper() {
    const SRC: &str = r#"fn f(o: mut ref Option[i64]) -> i64 { o ?? 5 }
fn main() {
    let values: Vec[Option[i64]] = vec![Some(3), None, Some(4)];
    let mut sum = 0;
    for v in values.iter() { sum += v ?? 0; }
    let r = ref values[0];
    sum += r ?? 0;
    let rs: Vec[Result[i64, i64]] = vec![Ok(7), Err(1)];
    for x in rs.iter() { sum += x ?? 100; }
    let mut o: Option[i64] = None;
    sum += f(mut o);
    println(f"{sum}");
}
"#;
    let want = "122\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A `Vec` or array passed for a generic `Slice[T]` parameter solves `T`
/// from its element (B-2026-10-14-42).
#[test]
fn mir_vec_for_a_generic_slice_param_solves_its_element() {
    const SRC: &str = r#"
fn first[T: Clone](s: Slice[T]) -> T { s[0].clone() }
fn count[T](s: Slice[T]) -> i64 { s.len() }
fn bump[T](s: mut Slice[T], v: own T) { s[0] = v; }

fn main() {
    let vi: Vec[i64] = [10, 20, 30];
    println(first(vi));
    println(count(vi));
    let vs: Vec[String] = ["a".to_string(), "b".to_string()];
    println(first(vs));
    let a: Array[f64, 2] = [1.5, 2.5];
    println(first(a));
    let mut w: Vec[i64] = [1, 2];
    bump(mut w, 7);
    println(w[0]);
}
"#;
    let want = "10\n3\na\n1.5\n7\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-14-37 — a user type's `impl Index` / `impl IndexMut` on the
/// compiled backend.
#[test]
fn mir_user_index_impls() {
    const SRC: &str = r#"struct Grid { w: i64, cells: Vec[i64] }
impl Index[(i64, i64)] for Grid {
    type Output = i64;
    fn index(self, idx: (i64, i64)) -> ref i64 {
        let (r, c) = idx;
        ref self.cells[r * self.w + c]
    }
}
impl IndexMut[(i64, i64)] for Grid {
    type Output = i64;
    fn index_mut(mut ref self, idx: (i64, i64)) -> mut ref i64 {
        let (r, c) = idx;
        self.cells[r * self.w + c]
    }
}
struct Names { all: Vec[String] }
impl Index[i64] for Names {
    type Output = String;
    fn index(self, i: i64) -> ref String { ref self.all[i] }
}
struct Stack[T] { items: Vec[T] }
impl[T] Index[i64] for Stack[T] {
    type Output = T;
    fn index(self, i: i64) -> ref T { ref self.items[self.items.len() as i64 - 1 - i] }
}
fn main() {
    let mut g = Grid { w: 2, cells: vec![1, 2, 3, 4] };
    g[1, 0] = 30;
    g[0, 1] += 5;
    println(f"{g[1, 0]} {g[0, 1]} {g[(1, 1)]}");
    let n = Names { all: vec!["ada".to_string(), "bob".to_string()] };
    println(f"{n[1]} {n[0].len()}");
    let c = n[0].clone();
    println(c);
    let s = Stack { items: vec![1, 2, 3] };
    let t = Stack { items: vec!["a".to_string(), "b".to_string()] };
    println(f"{s[0]} {s[2]} {t[0]} {t[1]}");
}
"#;
    let want = "30 7 4\nbob 3\nada\n3 1 b a\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-14-47 — `c[k] = v` on a user type with an `impl IndexSet`
/// calls `index_set`, so a key that is not present yet is added; `+=` still
/// goes through `IndexMut`. A `String` key lent to `index` / `index_mut` is
/// dropped with the statement.
#[test]
fn mir_user_index_set_stores_a_whole_element() {
    const SRC: &str = r#"struct Registry { keys: Vec[String], vals: Vec[i64] }
impl Registry {
    fn new() -> Registry { Registry { keys: Vec.new(), vals: Vec.new() } }
    fn find(ref self, k: ref String) -> Option[i64] {
        let mut i = 0;
        while i < self.keys.len() {
            if self.keys[i] == *k { return Some(i); }
            i += 1;
        }
        None
    }
}
impl Index[String] for Registry {
    type Output = i64;
    fn index(ref self, k: ref String) -> ref i64 {
        let i = self.find(k).expect("missing key");
        ref self.vals[i]
    }
}
impl IndexMut[String] for Registry {
    type Output = i64;
    fn index_mut(mut ref self, k: ref String) -> mut ref i64 {
        let i = self.find(k).expect("missing key");
        mut ref self.vals[i]
    }
}
impl IndexSet[String] for Registry {
    type Output = i64;
    fn index_set(mut ref self, k: own String, v: own i64) {
        match self.find(k) {
            Some(i) => { self.vals[i] = v; }
            None => { self.keys.push(k); self.vals.push(v); }
        }
    }
}
struct Sparse[V] { keys: Vec[i64], vals: Vec[V] }
impl[V] Sparse[V] {
    fn new() -> Sparse[V] { Sparse { keys: Vec.new(), vals: Vec.new() } }
    fn len(ref self) -> i64 { self.keys.len() }
}
impl[V] IndexSet[i64] for Sparse[V] {
    type Output = V;
    fn index_set(mut ref self, k: own i64, v: own V) {
        let mut i = 0;
        while i < self.keys.len() {
            if self.keys[i] == k {
                self.vals[i] = v;
                return;
            }
            i += 1;
        }
        self.keys.push(k);
        self.vals.push(v);
    }
}
fn main() {
    let mut r = Registry.new();
    r["a".to_string()] = 1;
    r["b".to_string()] = 2;
    r["a".to_string()] = 10;
    r["b".to_string()] += 5;
    println(f"{r["a".to_string()]} {r["b".to_string()]} {r.keys.len()}");
    let mut s: Sparse[String] = Sparse.new();
    s[7] = "seven".to_string();
    s[1000000] = "big".to_string();
    s[7] = "SEVEN".to_string();
    println(f"{s.len()} {s.vals[0]} {s.vals[1]}");
}
"#;
    let want = "10 7 2\n2 SEVEN big\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A float cast to `i128` saturates at `i128`'s own bounds instead of
/// overflowing the interpreter's range computation (B-2026-10-18-46), and an
/// `f32` prints its own shortest digits on both backends (B-2026-10-18-47).
#[test]
fn mir_f64_to_i128_and_f32_display() {
    const SRC: &str = r#"fn main() {
    let f: f64 = 1e22;
    let w = f as i128;
    println(f"{w}");
    let g: f64 = -1e40;
    println(f"{g as i128} {1e40 as i128} {(0.0 / 0.0) as i128}");
    let z: f32 = 0.1;
    println(f"{z}");
    println(z.to_string());
    let v: Vec[f32] = vec![0.1, 2.5, 1.0e-7];
    println(f"{v}");
    let d: f64 = 0.1;
    println(f"{d} {z as f64}");
}
"#;
    let want = "10000000000000000000000\n\
        -170141183460469231731687303715884105728 170141183460469231731687303715884105727 0\n\
        0.1\n0.1\n[0.1, 2.5, 1e-07]\n0.1 0.10000000149011612\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// `collect`'s `C: FromIterator[V]` holds for the collection the context
/// names: `String` collects `char`s, not `String`s or numbers
/// (B-2026-10-15-2).
#[test]
fn mir_collect_checks_from_iterator_for_the_context_type() {
    for src in [
        "fn main() { let w: Vec[String] = [\"ab\".to_string()]; let s: String = w.iter().map(|x| x.to_uppercase()).collect(); println(s); }",
        "fn main() { let v: Vec[i64] = [1, 2]; let s: String = v.into_iter().collect(); println(s); }",
    ] {
        let err = karac::mir::lower::build_source(src).err().expect("refused");
        assert!(err.contains("FromIterator"), "{err}");
    }
    const SRC: &str = r#"
fn main() {
    let s = "hi";
    let up: String = s.chars().map(|c| c.to_ascii_uppercase()).collect();
    let v: Vec[i64] = [3, 1, 3];
    let w: Vec[i64] = v.iter().map(|x| x * 2).collect();
    let st: Set[i64] = v.iter().map(|x| x).collect();
    let m: Map[i64, i64] = v.iter().map(|x| (x, x + 1)).collect();
    println(f"{up} {w.len()} {st.len()} {m.len()}");
}
"#;
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, "HI 3 2 2\n");
}

/// A type error stopping the MIR pipeline says where it is
/// (B-2026-10-14-46).
#[test]
fn mir_type_error_carries_its_line_and_column() {
    const SRC: &str =
        "fn main() {\n    let n: i64 = 1;\n    let s: String = n;\n    println(s);\n}\n";
    let err = karac::mir::lower::build_source(SRC).err().expect("refused");
    assert!(err.starts_with("typecheck: 3:"), "{err}");
}

/// A constant array's `Slice` methods take a view of it, as a local's do
/// (B-2026-10-14-52).
#[test]
fn mir_const_array_receiver_of_a_slice_method_is_viewed() {
    const SRC: &str = r#"
const AA: Array[i64, 3] = [1, 2, 3];
const PS: Array[(Str, i64), 2] = [("a", 1), ("b", 2)];

fn main() {
    let mut s = 0;
    for x in AA.iter() {
        s += *x;
    }
    println(f"{s}");
    println(AA.iter().count());
    for (k, v) in PS.iter() {
        println(f"{k}={v}");
    }
}
"#;
    let want = "6\n3\na=1\nb=2\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A stored function value is called through a `ref` (design.md § Function
/// values; B-2026-10-14-61).
#[test]
fn mir_function_value_is_called_through_a_ref() {
    const SRC: &str = r#"
fn dbl(n: i64) -> i64 { n * 2 }
fn inc(n: i64) -> i64 { n + 1 }
fn apply(f: ref Fn(i64) -> i64, x: i64) -> i64 { f(x) }

fn main() {
    let fs: Vec[Fn(i64) -> i64] = vec![dbl, inc];
    let mut t = 0;
    for f in fs.iter() {
        t += f(10);
    }
    println(f"{t}");
    let k = 3;
    let gs: Vec[Fn(i64) -> i64] = vec![|x| x + k, dbl];
    let mut u = 0;
    for g in gs.iter() {
        u += g(1);
    }
    println(f"{u} {apply(fs[0], 7)}");
}
"#;
    let want = "31\n6 14\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// An `if` or `while` condition reading a field of a call's owned result is
/// read before that result drops (B-2026-10-14-64).
#[test]
fn mir_condition_on_a_field_of_a_temporary() {
    const SRC: &str = r#"
struct A { ok: bool, s: String }

fn mk(n: i64) -> A { A { ok: n > 1, s: "x".to_string() } }

fn main() {
    if mk(2).ok {
        println("yes");
    }
    if mk(0).ok {
        println("no");
    } else {
        println("else");
    }
    let mut i = 0;
    while mk(3 - i).ok {
        i += 1;
    }
    println(f"end {i}");
}
"#;
    let want = "yes\nelse\nend 2\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A `match` on a field reached through a call that returns a `ref` is
/// matched where it lies, through that reference (B-2026-10-14-56).
#[test]
fn mir_match_on_a_field_through_a_returned_reference() {
    const SRC: &str = r#"
enum V { T(String), L(Vec[String]) }
struct It { v: V, n: i64 }
fn first(v: ref Vec[It]) -> ref It { ref v[0] }
fn main() {
    let mut m: Map[String, It] = Map.new();
    m.insert("b".to_string(), It { v: V.L(vec!["p".to_string(), "q".to_string()]), n: 4 });
    let t = match m.get("b").unwrap().v { V.T(_) => 1, V.L(q) => q.len() };
    let xs: Vec[It] = vec![It { v: V.T("qq".to_string()), n: 1 }];
    let u = match first(xs).v { V.T(s) => s.len(), V.L(q) => q.len() };
    let o: Option[ref It] = Some(first(xs));
    let w = match o.unwrap().v { V.T(_) => 1, V.L(_) => 2 };
    if let V.T(s) = first(xs).v { println(s); }
    println(f"{t} {u} {w} {xs.len()} {m.len()}");
}
"#;
    let want = "qq
2 2 1 1 1
";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A closure's early `return None` / `return Err(..)` agrees with its tail
/// once each fills in what the other leaves open (B-2026-10-14-59), and with
/// the binding's annotated return type (B-2026-10-14-65).
#[test]
fn mir_closure_early_return_takes_the_closure_return_type() {
    for (src, want) in [
        (
            r#"
fn main() {
    let lim = 3;
    let f = |n: i64| { if n < 0 { return None; } if n > lim { Some(f"big {n}") } else { None } };
    let h = |n: i64| { if n < 0 { return Some("neg".to_string()); } None };
    println(f"{f(5).unwrap()} {f(-1).is_none()} {h(-2).unwrap()} {h(1).is_none()}");
    let g = |n: i64| { if n == 0 { return Err("zero".to_string()); } Ok(10 / n) };
    println(f"{g(2).unwrap()} {g(0).is_err()}");
}
"#,
            "big 5 true neg true
5 true
",
        ),
        (
            r#"
fn main() {
    let w: Vec[Str] = "a 3 b".split_whitespace().collect();
    let amount: Fn(i64) -> Result[i64, String] = |i: i64| {
        if w.len() <= i {
            return Err("missing".to_string());
        }
        Ok(w[i].len() as i64)
    };
    let r = match w[0].clone() { "a" => amount(1), "b" => amount(2), _ => Ok(0) };
    println(f"{r.unwrap()}");
    println(f"{amount(9).is_err()}");
}
"#,
            "1
true
",
        ),
    ] {
        let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
        assert_eq!(interp.output, want);
        for opt in [0, 2] {
            if let Some(out) = build_and_run(src, opt) {
                assert_eq!(out, want, "at -O{opt}");
            }
        }
    }
}

/// A closure inside a method captures `self` like any other place it reads
/// (B-2026-10-15-4): by reference for a call or a read, through a nested
/// closure, and mutably when the body writes one of its fields.
#[test]
fn mir_closure_in_a_method_captures_self() {
    let src = r#"
struct S { k: i64, name: String }
impl S {
    fn add(self, x: i64) -> i64 { x + self.k }
    fn all(self, v: ref Vec[i64]) -> Vec[i64] {
        v.iter().map(|x| self.add(*x)).collect()
    }
    fn nested(self, v: ref Vec[i64]) -> i64 {
        v.iter().map(|x| v.iter().map(|y| self.add(*x * *y)).sum()).sum()
    }
    fn tag(self, v: ref Vec[i64]) -> Vec[String] {
        v.iter().filter(|x| *x > self.k).map(|x| f"{self.name}{x}").collect()
    }
    fn bump(mut ref self, v: ref Vec[i64]) {
        v.iter().for_each(|x| { self.k += *x; });
    }
}
fn main() {
    let mut s = S { k: 10, name: "n".to_string() };
    let v: Vec[i64] = vec![1, 2, 11];
    let r = s.all(v);
    println(f"{r[0]} {r[1]} {r[2]}");
    println(f"{s.nested(v)}");
    let t = s.tag(v);
    println(f"{t.len()} {t[0]}");
    s.bump(v);
    println(f"{s.k}");
}
"#;
    let want = "11 12 21\n286\n1 n11\n24\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A user variant named like a prelude one (`Err`, `Ok`, `None`) does not
/// capture the prelude constructors or patterns (B-2026-10-15-3): the
/// value's enum decides which variant a bare name means, whether or not the
/// user's enum is used anywhere else and whatever its variant indices are.
#[test]
fn mir_user_variant_named_err_does_not_capture_result() {
    for (src, want) in [
        (
            r#"
enum RedirKind { In, Out, Append, Err }
fn tk(s: Str) -> Result[i64, String] {
    if s.is_empty() { return Err("empty".to_string()); }
    Ok(2)
}
fn main() {
    let k = RedirKind.In;
    match k { RedirKind.Err => println("e"), _ => println("other") }
    match tk("ab") { Ok(t) => println(f"{t}"), Err(e) => println(e) }
    match tk("") { Ok(t) => println(f"{t}"), Err(e) => println(e) }
}
"#,
            "other\n2\nempty\n",
        ),
        (
            r#"
enum Level { Err, Ok }
fn f(n: i64) -> Result[i64, String] {
    if n < 0 { return Err("neg".to_string()); }
    Ok(n * 2)
}
fn main() {
    let l = Level.Ok;
    match l { Level.Err => println("lerr"), Level.Ok => println("lok") }
    match f(2) { Ok(t) => println(f"{t}"), Err(e) => println(e) }
    match f(-1) { Ok(t) => println(f"{t}"), Err(e) => println(e) }
}
"#,
            "lok\n4\nneg\n",
        ),
        (
            r#"
enum RedirKind { In, Out, Append, Err }
enum Opt { None, Some(i64) }
fn tk(s: Str) -> Result[i64, String] {
    if s.is_empty() { return Err("empty".to_string()); }
    Ok(2)
}
fn g(b: bool) -> Option[i64] { if b { Some(3) } else { None } }
fn main() {
    match tk("ab") { Ok(t) => println(f"{t}"), Err(e) => println(e) }
    match tk("") { Ok(t) => println(f"{t}"), Err(e) => println(e) }
    match g(false) { Some(x) => println(f"{x}"), None => println("none") }
    let r: Result[i64, String] = Err("x".to_string());
    if let Err(e) = r { println(e); }
}
"#,
            "2\nempty\nnone\nx\n",
        ),
    ] {
        let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
        assert_eq!(interp.output, want);
        for opt in [0, 2] {
            if let Some(out) = build_and_run(src, opt) {
                assert_eq!(out, want, "at -O{opt}");
            }
        }
    }
}

/// A `Copy` payload bound out of an element (`match v[i] { J.I(n) => .. }`)
/// reads through `*n` as `karac check`'s `ref` binding does
/// (B-2026-10-15-24), in a free function and on `self.vals[i]`.
#[test]
fn mir_deref_of_a_copy_payload_of_an_element() {
    let src = r#"
enum J { I(i64), S(String) }
struct H { vals: Vec[J] }
impl H {
    fn first(ref self) -> Option[i64] {
        match self.vals[0] { J.I(n) => Some(*n), _ => None }
    }
}
fn first_int(vals: ref Vec[J]) -> Option[i64] {
    match vals[0] {
        J.I(n) => Some(*n),
        _ => None,
    }
}
fn plain(vals: ref Vec[J]) -> i64 {
    match vals[0] { J.I(n) => n + 1, _ => 0 }
}
fn main() {
    let mut v: Vec[J] = Vec.new();
    v.push(J.I(7));
    v.push(J.S("s".to_string()));
    println(f"{first_int(v).unwrap_or(0)} {plain(v)}");
    let h = H { vals: v };
    println(f"{h.first().unwrap_or(0)}");
}
"#;
    let want = "7 8\n7\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A `mut ref name` pattern binding (core semantics §4.6) borrows its part
/// mutably, so a write through it lands in the element, the variant
/// payload or the local it was matched in (B-2026-10-15-13).
#[test]
fn mir_mut_ref_pattern_binding_writes_through() {
    let src = r#"
struct P { n: i64 }
enum E { A(P), B }
fn main() {
    let mut ps = vec![P { n: 1 }, P { n: 2 }];
    match ps[1] {
        mut ref p => { p.n += 5; }
    }
    println(f"{ps[1].n}");
    let mut es = vec![E.A(P { n: 3 }), E.B];
    match es[0] {
        E.A(mut ref p) => { p.n *= 10; }
        E.B => {}
    }
    if let E.A(mut ref p) = es[0] { p.n += 1; }
    match es[0] { E.A(p) => println(f"{p.n}"), E.B => println("b") }
    let mut q = P { n: 4 };
    match q { mut ref r => { r.n = 9; } }
    println(f"{q.n}");
}
"#;
    let want = "7\n31\n9\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A function value pushed into a `Vec[Fn(..)]` or inserted into a
/// `Map[K, Fn(..)]` is erased to the element type, as `vec![f]` erases it,
/// so calling it back through an index or `get` works (B-2026-10-15-26):
/// a named fn, closures with and without captures, and a `let`-bound one.
#[test]
fn mir_function_value_stored_by_push_is_callable() {
    let src = r#"
fn w(n: i64) -> i64 { n + 1 }
fn main() {
    let mut v: Vec[Fn(i64) -> i64] = Vec.new();
    v.push(w);
    v.push(|n| n * 2);
    let k = 5;
    v.push(|n| n + k);
    println(f"{v[0](7)} {v[1](7)} {v[2](7)}");
    let mut s: Vec[Fn(i64) -> String with allocates(Heap)] = Vec.new();
    let tag = "t".to_string();
    s.push(|n| f"{tag}{n}");
    let f: Fn(i64) -> String with allocates(Heap) = |n| f"f{n}";
    s.push(f);
    println(f"{s[0](1)} {(s[1])(2)} {s.len()}");
    let mut m: Map[String, Fn(i64) -> i64] = Map.new();
    m.insert("w".to_string(), w);
    m.insert("d".to_string(), |n| n * 2);
    println(f"{m.get("w").unwrap()(1)} {m.get("d").unwrap()(4)}");
}
"#;
    let want = "8 14 12\nt1 f2 2\n2 8\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A `let`-bound closure that is later stored where it escapes (a struct
/// field pushed into a returned `Vec`, a returned struct) captures by move
/// (core semantics §9.3), so its captured `String` outlives the function
/// (B-2026-10-15-27); one that is only called still borrows.
#[test]
fn mir_let_bound_closure_stored_in_a_returned_value_captures_by_move() {
    let src = r#"
struct T { f: Fn(i64) -> String with allocates(Heap) }
fn make(host: own String) -> Vec[T] {
    let mut v: Vec[T] = Vec.new();
    let f: Fn(i64) -> String with allocates(Heap) = |n| f"{host}-{n}";
    v.push(T { f: f });
    v
}
fn one(host: own String) -> T {
    let g = |n: i64| f"{host}:{n}";
    T { f: g }
}
fn local_only(host: own String) -> i64 {
    let h = |n: i64| host.len() as i64 + n;
    let r = h(1);
    r + host.len() as i64
}
fn main() {
    let v = make("web-1".to_string());
    println((v[0].f)(7));
    let t = one("db".to_string());
    println((t.f)(3));
    println(f"{local_only("abc".to_string())}");
}
"#;
    let want = "web-1-7\ndb:3\n7\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A borrow check error says where it is, as a type error does
/// (B-2026-10-15-16).
#[test]
fn mir_borrow_error_carries_its_line_and_column() {
    const SRC: &str = r#"
fn main() {
    let mut m: SortedMap[String, String] = SortedMap.new();
    m.insert("a".to_string(), "1".to_string());
    let v = m.get("a").unwrap_or(String.new());
    println(f"{v}");
}
"#;
    let err = karac::mir::lower::build_source(SRC).err().expect("refused");
    assert!(err.starts_with("borrow check main: 5:"), "{err}");
    assert!(err.contains("(borrow at bb"), "{err}");
    assert!(err.contains(" at 5:"), "{err}");
}

/// B-2026-10-15-40: a user variant `None` and Option's bare `None` are told apart by the value's type.
#[test]
fn mir_user_variant_named_none_does_not_capture_option() {
    let src = r#"
enum V { None, Int(i64) }
fn half(x: i64) -> Option[i64] {
    if x % 2 == 0 {
        return Some(x / 2);
    }
    None
}
fn main() {
    let v = V.Int(3);
    match v {
        V.None => println("none"),
        V.Int(i) => println(f"int {i}"),
    }
    let w = V.None;
    match w {
        V.None => println("none"),
        V.Int(i) => println(f"int {i}"),
    }
    let o: Option[i64] = None;
    println(f"{half(4).unwrap_or(-1)} {half(3).unwrap_or(-1)} {o.is_none()}");
}
"#;
    let want = "int 3\nnone\n2 -1 true\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-15-34: a user `fn format` (or `format_spec`, `panic_message`) does not take the f-string's call.
#[test]
fn mir_user_fn_named_like_a_builder_native_is_not_an_f_string() {
    let src = r#"
fn format(n: i64) -> String {
    let s = f"<{n}>";
    s
}
fn format_spec(a: ref String, b: i64) -> String { f"{a}/{b:>4}" }
fn panic_message(n: i64) -> i64 { n * 2 }
fn main() {
    let names = vec!["x".to_string()];
    println(format(3));
    println(f"scripts/{names[0]}.lua {format_spec(names[0], 2)} {panic_message(4)}");
}
"#;
    let want = "<3>\nscripts/x.lua x/   2 8\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-15-22: `let mut t = since` with `since: ref i64` binds an `i64` that `t = t + 1` writes.
#[test]
fn mir_let_mut_of_a_ref_to_a_copy_value_is_a_counter() {
    let src = r#"
fn look(m: ref SortedMap[String, i64], k: Str, dflt: i64) -> i64 {
    let since = m.get(k).unwrap_or(dflt);
    let mut t = since;
    t = t + 1;
    t += 2;
    t
}
fn main() {
    let mut m: SortedMap[String, i64] = SortedMap.new();
    m.insert("a".to_string(), 5);
    let since = m.get("a").unwrap_or(9);
    let mut t = since;
    t = t + 1;
    println(f"{t} {since}");
    let mut u = m.get("a").unwrap_or(9);
    u = since;
    u += 1;
    println(f"{u} {look(m, "a", 0)} {look(m, "b", 10)}");
}
"#;
    let want = "6 5\n6 8 13\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-15-19: a `String` passes as a `Str` parameter of a method called through a trait bound.
#[test]
fn mir_string_arg_to_a_str_param_through_a_trait_bound() {
    let src = r#"
trait Store {
    fn get(self, key: Str) -> Option[i64];
}
struct Mem { n: i64 }
impl Store for Mem {
    fn get(self, key: Str) -> Option[i64] { if key == "a" { Some(self.n) } else { None } }
}
struct Line { sku: String }
struct Service[S: Store] { store: S }
impl[S: Store] Service[S] {
    fn price(self, l: ref Line) -> i64 { self.store.get(l.sku).unwrap_or(-1) }
}
fn main() {
    let s = Service { store: Mem { n: 7 } };
    let a = Line { sku: "a".to_string() };
    let b = Line { sku: "b".to_string() };
    println(f"{s.price(a)} {s.price(b)}");
}
"#;
    let want = "7 -1\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-15-33: methods of a `shared struct` called on a borrowed handle, and on one bound by a pattern over a borrowed enum.
#[test]
fn mir_methods_on_a_borrowed_shared_handle() {
    let src = r#"
enum Val { Int(i64), Nil }
shared struct T {
    id: i64,
    mut arr: Vec[i64],
}
impl T {
    fn noop(ref self, v: i64) {
        let _ = v;
    }
    fn size(ref self) -> i64 {
        self.arr.len()
    }
    fn put(ref self, v: own Val) -> bool {
        match v {
            Val.Int(n) => { self.arr.push(n); true }
            Val.Nil => false,
        }
    }
    fn get(ref self, k: ref Val) -> Val {
        match k {
            Val.Int(i) => if *i < self.arr.len() { Val.Int(self.arr[*i]) } else { Val.Nil },
            Val.Nil => Val.Nil,
        }
    }
}
enum E { A(T), B }
fn f(t: ref T) -> i64 {
    t.noop(5);
    let n = t.size();
    t.put(Val.Int(9));
    let ok = t.put(Val.Nil);
    match t.get(Val.Int(0)) {
        Val.Int(x) => println(f"got {x} {ok}"),
        Val.Nil => println("nil"),
    }
    n + t.size()
}
fn g(er: ref E) {
    match er {
        E.A(h) => { h.put(Val.Int(2)); println(f"h {h.size()}"); }
        E.B => {}
    }
}
fn main() {
    let t = T { id: 1, arr: Vec.new() };
    println(f"{f(t)}");
    let e = E.A(t);
    g(e);
}
"#;
    let want = "got 9 false\n1\nh 2\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-15-37: a parameter named `ast` or `compiler` is the parameter, not the comptime module.
#[test]
fn mir_a_local_named_ast_or_compiler_shadows_the_comptime_module() {
    let src = r#"
struct A { xs: Vec[i64] }
impl A {
    fn add(mut ref self, x: i64) -> i64 {
        self.xs.push(x);
        self.xs.len()
    }
}
fn f(ast: mut ref A) -> i64 {
    ast.add(3)
}
fn g(compiler: ref A) -> i64 {
    compiler.xs.len()
}
fn main() {
    let mut a = A { xs: Vec.new() };
    println(f"{f(mut a)}");
    let ast = 5;
    println(f"{ast} {g(a)}");
}
"#;
    let want = "1\n5 1\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-14-66: width and alignment apply to a `Str` hole.
#[test]
fn mir_format_width_on_a_str_hole() {
    let src = r#"
fn row(name: Str, n: i64) -> String { f"[{name:<6}|{name:>5}|{name:^7}|{n:>4}]" }
fn main() {
    let c: Str = "abc";
    println(f"[{c:<6}]");
    let s = "hello".to_string();
    println(row(s, 7));
    println(row("ab", 12));
}
"#;
    let want = "[abc   ]\n[hello |hello| hello |   7]\n[ab    |   ab|  ab   |  12]\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-15-41 and B-2026-10-15-42: a program's own `Range` or `IoError`
/// is its own type, and range literals and file errors keep the library's.
#[test]
fn mir_a_user_type_named_range_or_io_error_is_its_own() {
    let src = r#"
struct Range { lo: i64, hi: i64 }
impl Range {
    fn width(self) -> i64 { self.hi - self.lo }
}
struct IoError { n: Vec[i64] }
fn mid(r: ref Range) -> i64 { (r.lo + r.hi) / 2 }
fn main() {
    let r = Range { lo: 1, hi: 3 };
    let v: Vec[i64] = vec![10, 20, 30];
    let mut s = 0;
    for i in 0..v.len() {
        s += v[i];
    }
    for i in 0..=1 {
        s += i;
    }
    println(f"{s} {r.lo} {r.width()} {mid(r)} {v[1..3].len()}");
    match fs.read_to_string("nonexistent_zz.txt") {
        Ok(_) => println("read"),
        Err(_) => println("failed"),
    }
    let u = IoError { n: vec![1, 2] };
    println(f"{u.n.len()}");
}
"#;
    let want = "61 1 2 2 2\nfailed\n2\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
    // An error names the program's type by its own name.
    let err = karac::mir::lower::build_source(
        &src.replace("    println(f\"{u.n.len()}\");", "    let bad: i64 = r;"),
    )
    .err()
    .expect("refused");
    assert!(err.contains("found 'Range'"), "{err}");
}

/// B-2026-10-18-10: a program's own `Client`, `Response`, `HttpError` or
/// `RequestBuilder` is its own type, not the library's HTTP client, so its
/// methods are found, inside its own `impl` too.
#[test]
fn mir_a_user_type_named_like_the_http_client_is_its_own() {
    let src = r#"
struct Client { out: String }
impl Client {
    fn error(mut ref self, msg: Str) { self.out.push_str(msg); }
    fn warn(mut ref self, msg: Str) { self.error(msg); }
}
struct Response { code: i64 }
impl Response {
    fn status(self) -> i64 { self.code }
    fn summary(self) -> String { f"{self.status()} ok" }
}
enum HttpError { Timeout, Refused(i64) }
impl HttpError {
    fn describe(self) -> String {
        match self {
            HttpError.Timeout => "timeout".to_string(),
            HttpError.Refused(p) => f"refused {p}",
        }
    }
}
struct RequestBuilder { parts: Vec[String] }
impl RequestBuilder {
    fn header(own self, h: Str) -> RequestBuilder {
        let mut parts = self.parts;
        parts.push(h.to_string());
        RequestBuilder { parts: parts }
    }
    fn send(self) -> Result[Response, HttpError] {
        if self.parts.len() > 1 { Ok(Response { code: 200 }) } else { Err(HttpError.Refused(80)) }
    }
}
fn main() {
    let mut c = Client { out: String.new() };
    c.warn("x");
    println(c.out);
    for n in [2, 1] {
        let mut r = RequestBuilder { parts: Vec.new() };
        for i in 0..n {
            r = r.header(f"h{i}");
        }
        match r.send() {
            Ok(resp) => println(resp.summary()),
            Err(e) => println(e.describe()),
        }
    }
    println(HttpError.Timeout.describe());
}
"#;
    let want = "x\n200 ok\nrefused 80\ntimeout\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
    // An error names the program's type by its own name.
    let err = karac::mir::lower::build_source(
        &src.replace("    println(c.out);", "    let bad: i64 = c;"),
    )
    .err()
    .expect("refused");
    assert!(err.contains("found 'Client'"), "{err}");
}

/// B-2026-10-14-67: `join` and `concat` take any element with a `Display`
/// form, on a `Vec` and on a slice of one.
#[test]
fn mir_join_of_str_and_number_elements() {
    let src = r#"
fn main() {
    let s: Str = "a";
    let mut v: Vec[Str] = Vec.new();
    v.push(s); v.push(s);
    println(v.join(" "));
    println(v[0..2].join(" "));
    let n: Vec[i64] = vec![1, 2];
    println(n.join(","));
    let w = vec!["x".to_string(), "y".to_string()];
    println(f"{w.join("-")} {w[0..2].join("+")} {w.concat()}");
}
"#;
    let want = "a a\na a\n1,2\nx-y x+y xy\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-14-67: `join` and `concat` take any element with a `Display`
/// form, on a `Vec` and on a slice of one.
#[test]
fn mir_join_of_any_display_elements() {
    let src = r#"
struct P { x: i64 }
impl Display for P {
    fn to_string(ref self) -> String { f"<{self.x}>" }
}
fn main() {
    let f: Vec[f64] = vec![1.5, 2.0];
    let c: Vec[char] = vec!['a', 'b'];
    let b: Vec[bool] = vec![true, false];
    let p = vec![P { x: 1 }, P { x: 2 }];
    let e: Vec[i64] = Vec.new();
    println(f"{f.join(";")}|{c.concat()}|{b.join(" ")}|{p.join(", ")}|{e.join(",")}|");
}
"#;
    let want = "1.5;2.0|ab|true false|<1>, <2>||\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// `rotate_left` / `rotate_right` lower to LLVM's funnel shifts, the amount
/// taken modulo the width, as Rust's (B-2026-10-15-67).
#[test]
fn mir_build_rotates_at_every_width() {
    let src = r#"
fn main() {
    let a: u32 = 0x80000001u32;
    let b: u64 = 0x0000000000000003u64;
    println(f"{a.rotate_left(1)} {b.rotate_right(1)}");
    println(f"{a.rotate_right(33)} {b.rotate_left(64)} {a.rotate_left(0)}");
    let c: u8 = 0x81u8;
    println(f"{c.rotate_left(1)} {c.rotate_right(9)}");
}
"#;
    // The interpreter's natives agree (B-2026-10-15-67).
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(
        interp.output,
        "3 9223372036854775809\n3221225472 3 2147483649\n3 192\n"
    );
    for opt in [0, 2] {
        let Some(out) = build_and_run(src, opt) else {
            return;
        };
        assert_eq!(
            out, "3 9223372036854775809\n3221225472 3 2147483649\n3 192\n",
            "at -O{opt}"
        );
    }
}

/// A bare variant pattern whose name a struct also holds names the
/// scrutinee's variant, not the struct (B-2026-10-15-35); the struct's own
/// patterns still mean the struct.
#[test]
fn mir_bare_variant_pattern_named_like_a_struct() {
    let src = r#"
enum Block { Heading(i64), Rule, Para { text: String } }
struct Heading { level: i64 }
struct Para { text: String }
fn show(b: Block) -> String {
    match b {
        Heading(n) => f"h{n}",
        Para { text } => f"p:{text}",
        Rule => "rule",
    }
}
fn lvl(h: Heading) -> i64 {
    match h {
        Heading { level } => level,
    }
}
fn txt(p: ref Para) -> String {
    let Para { text } = p;
    text.clone()
}
fn main() {
    println(show(Block.Heading(2)));
    println(show(Block.Para { text: "x".to_string() }));
    println(show(Block.Rule));
    println(f"{lvl(Heading { level: 3 })}");
    println(txt(Para { text: "y".to_string() }));
    let b = Block.Heading(4);
    if let Heading(k) = b { println(f"k{k}"); }
}
"#;
    let want = "h2\np:x\nrule\n3\ny\nk4\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-15-54: unsuffixed literals past `i64::MAX` in `u64` and `u128`
/// places, as values, operands, arguments, shifts and patterns, print the
/// same on both backends.
#[test]
fn mir_unsuffixed_literals_past_i64_agree_on_both_backends() {
    const SRC: &str = "fn f(x: u64) -> u64 { x }\n\
        fn main() {\n    let a: u64 = 0xFFFF_FFFF_FFFF_FFFF;\n    \
        let b: u128 = 18446744073709551616;\n    let c: u64 = 9223372036854775808;\n    \
        println(f\"{a} {b} {c + 1} {a == 18446744073709551615} {f(18446744073709551614)}\");\n    \
        let s: u128 = 1 << 64;\n    \
        let m: u128 = (0xFFFF_FFFF_FFFF_FFFF << 64) | 0xFFFF_FFFF_FFFF_FFFF;\n    \
        println(f\"{s} {m}\");\n    let x: u64 = 5;\n    \
        println(f\"{x.max(18446744073709551615)} {x.clamp(9223372036854775808, 18446744073709551615)} {x.checked_add(18446744073709551610)}\");\n    \
        let w: i128 = 170141183460469231731687303715884105727;\n    \
        let v: Vec[u64] = vec![18446744073709551615, 1];\n    \
        let k = match a { 9223372036854775808..=18446744073709551614 => \"upper\", 18446744073709551615 => \"max\", _ => \"lower\" };\n    \
        println(f\"{w} {v[0]} {k}\");\n}\n";
    let want =
        "18446744073709551615 18446744073709551616 9223372036854775809 true 18446744073709551614\n\
        18446744073709551616 340282366920938463463374607431768211455\n\
        18446744073709551615 9223372036854775808 Some(18446744073709551615)\n\
        170141183460469231731687303715884105727 18446744073709551615 max\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A `const` initialized by an integer literal past i64 takes the literal's
/// full value, suffixed or not, and one out of range for its type is an
/// error rather than a panic of the checker (B-2026-10-18-2).
#[test]
fn mir_const_literals_past_i64() {
    const SRC: &str = "const BIG: i128 = 18446744073709551615;\n\
        const UU: u64 = 18446744073709551615;\n\
        const WW: u64 = 18446744073709551615u64;\n\
        const HH: i128 = 9223372036854775808;\n\
        const XX: u128 = 340282366920938463463374607431768211455u128;\n\
        const MM: i128 = -170141183460469231731687303715884105728;\n\
        fn main() {\n    println(f\"{BIG} {UU} {WW} {HH} {XX} {MM}\");\n    \
        println(f\"{BIG - 1} {UU / 2}\");\n}\n";
    let want = "18446744073709551615 18446744073709551615 18446744073709551615 \
        9223372036854775808 340282366920938463463374607431768211455 \
        -170141183460469231731687303715884105728\n\
        18446744073709551614 9223372036854775807\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
    for (src, msg) in [
        (
            "const BB: i64 = 9223372036854775808;\nfn main() { println(f\"{BB}\"); }\n",
            "integer literal 9223372036854775808 out of range for 'i64'",
        ),
        (
            "const BB: u64 = 18446744073709551616;\nfn main() { println(f\"{BB}\"); }\n",
            "integer literal 18446744073709551616 out of range for 'u64'",
        ),
    ] {
        let err = karac::mir::lower::build_source(src).err().expect("refused");
        assert!(err.contains(msg), "{err}");
    }
}

/// `T.Output` under `T: Add` is the `Output` of the solved type's `impl Add`
/// even when the type has `impl Mul` and `impl Index` too, in a generic
/// function and in a generic impl's own binding (`W[T.Output]`)
/// (B-2026-10-18-1).
#[test]
fn mir_a_projection_reads_its_bound_traits_output() {
    const SRC: &str = "struct V { x: i64 }\n\
        impl Add for V { type Output = V; fn add(self, rhs: V) -> V { V { x: self.x + rhs.x } } }\n\
        impl Mul for V { type Output = i64; fn mul(self, rhs: V) -> i64 { self.x * rhs.x } }\n\
        impl Index[i64] for V { type Output = i64; fn index(ref self, i: i64) -> ref i64 { ref self.x } }\n\
        struct W[T] { v: T }\n\
        impl[T: Add] Add for W[T] { type Output = W[T.Output]; \
        fn add(self, rhs: W[T]) -> W[T.Output] { W { v: self.v + rhs.v } } }\n\
        impl[T] Mul for W[T] { type Output = bool; fn mul(self, rhs: W[T]) -> bool { true } }\n\
        fn sum2[T: Add](a: own T, b: own T) -> T.Output { a + b }\n\
        fn sum_let[T: Add](a: own T, b: own T) -> T.Output {\n    \
        let r: T.Output = a + b;\n    r\n}\n\
        fn prod2[T: Mul](a: own T, b: own T) -> T.Output { a * b }\n\
        fn main() {\n    \
        println(f\"{sum2(V { x: 1 }, V { x: 2 }).x}\");\n    \
        println(f\"{sum_let(V { x: 5 }, V { x: 6 }).x}\");\n    \
        println(f\"{prod2(V { x: 3 }, V { x: 4 })}\");\n    \
        let w = sum2(W { v: V { x: 1 } }, W { v: V { x: 9 } });\n    \
        println(f\"{w.v.x}\");\n    \
        println(f\"{prod2(W { v: 1 }, W { v: 2 })}\");\n    \
        println(f\"{sum2(3, 4)}\");\n}\n";
    let want = "3\n11\n12\n10\ntrue\n7\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A projection that reaches a generic call or constructor as an argument
/// is the caller's, and an operator on a generic impl reads the impl's own
/// parameters at the operand's arguments (B-2026-10-18-44, B-2026-10-18-43).
#[test]
fn mir_a_callers_projection_passes_through_a_generic_call() {
    const SRC: &str = "enum E[T] { A(T), N }\n\
        struct V { x: i64 }\n\
        impl Add for V { type Output = V; fn add(self, rhs: V) -> V { V { x: self.x + rhs.x } } }\n\
        impl Neg for V { type Output = V; fn neg(self) -> V { V { x: 0 - self.x } } }\n\
        struct W[T] { v: T }\n\
        impl[T: Add] Add for W[T] { type Output = W[T.Output]; fn add(self, rhs: W[T]) -> W[T.Output] { W { v: self.v + rhs.v } } }\n\
        impl[T: Neg] Neg for W[T] { type Output = W[T.Output]; fn neg(self) -> W[T.Output] { W { v: -self.v } } }\n\
        fn pair[T: Add](a: own T, b: own T) -> Option[T.Output] { Some(a + b) }\n\
        fn tagged[T: Add](a: own T, b: own T) -> E[T.Output] { let s = a + b; E.A(s) }\n\
        fn id[T](x: own T) -> T { x }\n\
        fn through[T: Add](a: own T, b: own T) -> T.Output { let s = a + b; id(s) }\n\
        fn twice[T: Add](a: own W[T], b: own W[T]) -> W[T.Output] { a + b }\n\
        fn main() {\n    \
        println(f\"{pair(3, 4).unwrap()} {through(5, 6)}\");\n    \
        match tagged(V { x: 1 }, V { x: 2 }) {\n        \
        E.A(v) => println(f\"{v.x}\"),\n        \
        E.N => println(\"none\"),\n    \
        }\n    \
        let w = W { v: V { x: 1 } } + W { v: V { x: 9 } };\n    \
        let n = -W { v: V { x: 4 } };\n    \
        let i = W { v: 3 } + W { v: 4 };\n    \
        let t = twice(W { v: V { x: 2 } }, W { v: V { x: 5 } });\n    \
        println(f\"{w.v.x} {n.v.x} {i.v} {t.v.x}\");\n\
        }\n";
    let want = "7 11\n3\n10 -4 7 7\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// An unsuffixed `i128::MIN` is legal where the type is `i128` and refused
/// as out of range for `i64` where nothing gives it a type
/// (B-2026-10-15-73).
#[test]
fn mir_unsuffixed_i128_min_takes_its_type_from_inference() {
    const SRC: &str = "fn main() {\n    \
        let w: i128 = -170141183460469231731687303715884105728;\n    \
        println(f\"{w} {w + 1}\");\n}\n";
    let want =
        "-170141183460469231731687303715884105728 -170141183460469231731687303715884105727\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
    let Err(err) = karac::mir::lower::run_source(
        "fn main() {\n    let w = -170141183460469231731687303715884105728;\n    println(f\"{w}\");\n}\n",
    ) else {
        panic!("an untyped i128::MIN is out of range for i64");
    };
    assert!(err.contains("out of range for 'i64'"), "{err}");
}

/// A string literal is a `Str` wherever a `Str` is expected, including a
/// method argument checked after it is inferred, a tuple after a `Str`
/// element and a tuple inside `Some` (B-2026-10-14-68).
#[test]
fn mir_string_literal_into_str_slots() {
    let src = r#"
const DB: Str = "kv.db";
const AB: Str = "ab";
fn g() -> Option[(Str, Str)] { Some(("a", "b")) }
fn f(s: ref Str) -> i64 { s.len() }
fn main() {
    let mut v: Vec[Str] = Vec.new();
    v.push("a");
    v.push(AB);
    for p in [DB, "kv.wal"].iter() { println(p); }
    let files = vec![("one", AB), ("two", "cd")];
    for (n, x) in files.iter() { println(f"{n}={x}"); }
    match g() { Some((a, b)) => println(f"{a}{b}"), None => {} }
    let mut m: Map[Str, i64] = Map.new();
    m.insert("k", 1);
    let mut d: VecDeque[Str] = VecDeque.new();
    d.push_back("x");
    println(f"{m.len()} {v.len()} {v.contains("a")} {v.contains("z")} {d.len()} {f("abc")}");
}
"#;
    let want = "kv.db\nkv.wal\none=ab\ntwo=cd\nab\n1 2 true false 1 3\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-15-57: the borrow checker's state is per held loan, not per
/// local times loan, so a function of thousands of statements, each with a
/// loan of its own, checks in linear-ish time. Before, 1000 `push_str`
/// lines took 2 GB and 4000 ran out of memory; the bound here is loose.
#[test]
fn mir_borrowck_scales_to_long_functions() {
    let mut src = String::from("fn build() -> String {\n    let mut s = String.new();\n");
    for i in 0..3000u64 {
        src.push_str(&format!("    s.push_str(\"{:096x}\");\n", i * 7919 + 12345));
    }
    src.push_str("    s\n}\nfn main() {\n    println(f\"{build().len()}\");\n}\n");
    let start = std::time::Instant::now();
    karac::mir::lower::build_source(&src).expect("MIR");
    let took = start.elapsed();
    assert!(took < std::time::Duration::from_secs(60), "took {took:?}");
}

/// The MIR interpreter rotates signed and 128-bit integers too, and
/// `partial_cmp` orders floats, with `None` for NaN (B-2026-10-15-67,
/// B-2026-10-15-68), and so does the compiled backend, integer
/// `partial_cmp` included (B-2026-10-15-74).
#[test]
fn mir_interp_rotates_and_partially_orders() {
    let src = r#"
fn main() {
    let d: i32 = -2;
    let e: u128 = 1u128;
    let c: u8 = 0x81u8;
    println(f"{d.rotate_left(1)} {d.rotate_right(1)} {e.rotate_right(1)} {c.rotate_left(64)}");
    match 2.5.partial_cmp(1.0) {
        Some(Ordering.Greater) => println("greater"),
        Some(_) => println("other"),
        None => println("unordered"),
    }
    let n = 0.0 / 0.0;
    println(f"{n.partial_cmp(1.0).is_none()}");
    let mut v = [2.5, 1.0, 3.0];
    v.sort_by(|p, q| p.partial_cmp(q).unwrap());
    let x: f32 = 1.5;
    println(f"{v[0]} {v[1]} {v[2]} {x.partial_cmp(1.5) == Some(Ordering.Equal)}");
    let i: i64 = -3;
    let u: u64 = 18446744073709551615u64;
    let w: i128 = -1i128;
    println(f"{i.partial_cmp(2) == Some(Ordering.Less)} {u.partial_cmp(1u64) == Some(Ordering.Greater)} {w.rotate_left(7)} {e.rotate_left(130)}");
}
"#;
    let want = "-3 2147483647 170141183460469231731687303715884105728 129\ngreater\ntrue\n\
                1.0 2.5 3.0 true\ntrue true -1 4\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// `i128` / `u128` `parse` and `from_str_radix` on both backends, at and
/// past the bounds (B-2026-10-18-11).
#[test]
fn mir_parses_128_bit_integers() {
    let src = r#"
fn main() {
    println(f"{i128.parse("18446744073709551616").unwrap()}");
    println(f"{i128.parse(" -170141183460469231731687303715884105728 ").unwrap()}");
    println(f"{i128.parse("170141183460469231731687303715884105728").is_none()}");
    println(f"{u128.parse("340282366920938463463374607431768211455").unwrap()}");
    println(f"{u128.parse("-1").is_none()} {u128.parse("x").is_none()}");
    println(f"{i128.from_str_radix("-ff", 16).unwrap()} {u128.from_str_radix("zz", 36).unwrap()}");
}
"#;
    let want = "18446744073709551616\n-170141183460469231731687303715884105728\ntrue\n\
                340282366920938463463374607431768211455\ntrue true\n-255 1295\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// `String.clear` empties the string, `with_capacity` reserves exactly
/// its slots, and `binary_search` finds std's index, duplicates and an
/// empty Vec included (B-2026-10-14-36, B-2026-10-14-14, B-2026-10-18-9).
#[test]
fn mir_string_clear_with_capacity_and_binary_search() {
    let src = r#"
fn main() {
    let mut s = String.new();
    s.push_str("abc");
    s.clear();
    s.push('x');
    let mut t = "lit".to_string();
    t.clear();
    t.push_str("yz");
    println(f"{s} {s.len()} {t} {t.len()}");
    let mut v: Vec[i64] = Vec.with_capacity(5);
    println(f"{v.capacity()} {v.len()}");
    v.push(1);
    let e: Vec[i64] = Vec.with_capacity(0);
    let mut w = String.with_capacity(3);
    w.push_str("hello");
    println(f"{v.capacity()} {e.capacity()} {w}");
    let mut b: Vec[i64] = Vec.new();
    for x in [1, 2, 2, 2, 2, 3, 5, 5, 8] { b.push(x); }
    let mut r = 0;
    for k in 0..10 {
        match b.binary_search(k) { Some(i) => r = r * 10 + i, None => r = r * 10 + 9 }
    }
    let empty: Vec[i64] = Vec.new();
    println(f"{r} {empty.binary_search(3).is_none()}");
}
"#;
    let want = "x 1 yz 2\n5 0\n5 0 hello\n9045979989 true\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A variant constructed through a type alias is the target's variant:
/// `Err(OldE.B(7))`, `Older.C { code: 3 }` and `Older.A` with `type OldE =
/// NetE` (B-2026-10-15-50).
#[test]
fn mir_variant_constructed_through_a_type_alias() {
    let src = r#"
enum NetE { A, B(i32), C { code: i64 } }
type OldE = NetE;
type Older = OldE;
struct Pt { x: i64 }
impl Pt { fn origin() -> Pt { Pt { x: 0 } } }
type P = Pt;
fn f(x: i64) -> Result[i64, OldE] { if x > 0 { Ok(x) } else { Err(OldE.B(7)) } }
fn main() {
    match f(0) { Ok(_) => println("ok"), Err(NetE.B(n)) => println(f"b {n}"), Err(_) => println("other") }
    let e = Older.C { code: 3 };
    match e { OldE.C { code } => println(f"c {code}"), _ => println("x") }
    let a = Older.A;
    match a { NetE.A => println("a"), _ => println("x") }
    println(f"{P.origin().x}");
}
"#;
    let want = "b 7\nc 3\na\n0\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// `f64.partial_cmp` / `f32.partial_cmp`: `Some` of the `Ordering`, `None`
/// when either side is NaN, and the comparator a float sort is written
/// with (B-2026-10-15-68).
#[test]
fn mir_build_float_partial_cmp() {
    let src = r#"
fn main() {
    let a = 2.5;
    let b = 1.0;
    match a.partial_cmp(b) {
        Some(Ordering.Greater) => println("greater"),
        Some(_) => println("other"),
        None => println("unordered"),
    }
    let nan = 0.0 / 0.0;
    match nan.partial_cmp(1.0) {
        Some(_) => println("ordered"),
        None => println("unordered"),
    }
    match b.partial_cmp(b) {
        Some(Ordering.Equal) => println("equal"),
        _ => println("other"),
    }
    let mut v = [2.5, 1.0, 3.0, -4.0];
    v.sort_by(|p, q| p.partial_cmp(q).unwrap());
    println(f"{v[0]} {v[1]} {v[2]} {v[3]}");
    let x: f32 = 1.5f32;
    println(f"{x.partial_cmp(2.0f32) == Some(Ordering.Less)}");
}
"#;
    for opt in [0, 2] {
        let Some(out) = build_and_run(src, opt) else {
            return;
        };
        assert_eq!(
            out, "greater\nunordered\nequal\n-4.0 1.0 2.5 3.0\ntrue\n",
            "at -O{opt}"
        );
    }
}

/// A width on a `ref Str` from iterating a `Vec[Str]` pads the view the
/// reference is, rather than reading the view's bytes as a pointer
/// (B-2026-10-14-66).
#[test]
fn mir_build_width_on_a_borrowed_str() {
    let src = r#"
fn main() {
    let v: Vec[Str] = vec!["ab", "cde"];
    for c in v.iter() {
        println(f"[{c:<6}][{c:>6}][{c:^7}]");
    }
    let owned: Vec[String] = vec!["xy".to_string()];
    for o in owned.iter() {
        println(f"[{o:<4}]");
    }
}
"#;
    let want = "[ab    ][    ab][  ab   ]\n[cde   ][   cde][  cde  ]\n[xy  ]\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A struct update whose base is a call evaluates the base once, after the
/// written fields (B-2026-10-15-47), on a plain and a generic struct.
#[test]
fn mir_struct_update_with_a_call_base() {
    let src = r#"
#[derive(Clone)]
struct D { a: i64, b: String, c: bool }
fn base() -> D { println("base"); D { a: 1, b: "x".to_string(), c: true } }
fn two() -> i64 { println("two"); 2 }
struct W[T] { v: T, n: i64 }
fn wb() -> W[String] { W { v: "w".to_string(), n: 4 } }
fn main() {
    let d = D { a: 5, ..base() };
    println(f"{d.a} {d.b} {d.c}");
    let e = D { a: two(), b: "y".to_string(), ..base() };
    println(f"{e.a} {e.b} {e.c}");
    let w = W { n: 9, ..wb() };
    println(f"{w.v} {w.n}");
    let h = D { c: false, ..d };
    println(f"{h.a} {h.b} {h.c}");
}
"#;
    let want = "base\n5 x true\ntwo\nbase\n2 y true\nw 9\n5 x false\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// `*x` on the items of `zip` and `chain` over `.iter()` (B-2026-10-15-1).
#[test]
fn mir_zip_and_chain_items_deref() {
    let src = r#"
fn same(a: Vec[i64], b: Vec[i64]) -> bool {
    a.len() == b.len() and a.iter().zip(b.iter()).all(|(x, y)| *x == *y)
}
fn main() {
    let a: Vec[i64] = vec![1, 2, 3];
    let b: Vec[i64] = vec![1, 2, 4];
    let mut d = 0;
    for (x, y) in a.iter().zip(b.iter()) { d += *y - *x; }
    for p in a.iter().zip(b.iter()) { d += *p.1 - *p.0; }
    let mut s = 0;
    for x in a.iter().chain(b.iter()) { s += *x; }
    let c: Vec[i64] = vec![4, 5, 6];
    for (x, y) in a.iter().zip(c.into_iter()) { s += *x + y; }
    println(f"{same(a, b)} {d} {s}");
}
"#;
    let want = "false 2 34\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_unsuffixed_literal_takes_its_peers_integer_type() {
    // B-2026-10-15-51 / -52: an unsuffixed literal on the left of `& | ^`,
    // or in the first branch of an `if` or `match`, takes the other
    // operand's or branch's type rather than defaulting to `i64`.
    let src = r#"
fn enc(c: u8) -> Vec[u8] {
    let mut out: Vec[u8] = Vec.new();
    out.push(0xC0 | (c >> 6));
    out.push(0x80 | (c & 0x3F));
    out
}
fn f(c: bool, y: u128) -> u128 {
    let x = if c { 0 } else { y };
    x
}
fn g(c: bool, y: u8) -> u8 {
    let x = if c { y } else { 7 };
    let z = if c { 1 } else if y > 3 { 2 } else { y };
    x + z
}
fn m(k: i64, y: u128) -> u128 {
    let x = match k { 0 => 0, 1 => { 3 } _ => y };
    x
}
fn h(y: u16) -> u16 {
    y | (1 << 8) | (3 & 1)
}
fn top() -> u128 {
    (0xFFFF_FFFF_FFFF_FFFF << 64) | 0xFFFF_FFFF_FFFF_FFFF
}
fn main() {
    let c: u8 = 20;
    let d: u8 = 0xC0 ^ c;
    let x: u8 = 0x80 | (c & 0x3F);
    let e = enc(233);
    println(f"{d} {x} {e[0]} {e[1]}");
    println(f"{f(true, 5)} {f(false, 5)} {g(true, 9)} {g(false, 9)} {m(0, 5)} {m(1, 5)} {m(2, 5)}");
    println(f"{h(1)} {top()}");
}
"#;
    let want = "212 148 195 169\n0 5 10 9 0 3 5\n257 340282366920938463463374607431768211455\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_string_literal_patterns_on_a_str_scrutinee() {
    // B-2026-10-14-43: a string literal pattern takes the `Str` type of its
    // scrutinee, as a string literal expression does where a `Str` is
    // expected.
    let src = r#"
fn size(name: String) -> i64 {
    match name.as_str() {
        "Year" => 4,
        "Month" => 7,
        _ => 19,
    }
}
fn size2(name: Str) -> i64 {
    match name {
        "Year" => 4,
        "Month" => 7,
        _ => 19,
    }
}
fn kind(v: Vec[String], i: i64) -> String {
    let r = match v[i].as_str() {
        "a" | "b" => "early",
        "z" => "late",
        _ => "other",
    };
    r.to_string()
}
fn main() {
    println(f"{size(String.from("Year"))} {size(String.from("Month"))} {size(String.from("Day"))}");
    println(f"{size2("Year")} {size2("Month")} {size2("Day")}");
    let v: Vec[String] = vec![String.from("a"), String.from("z"), String.from("q")];
    println(f"{kind(v, 0)} {kind(v, 1)} {kind(v, 2)}");
}
"#;
    let want = "4 7 19\n4 7 19\nearly late other\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_array_reaches_slice_read_methods() {
    // B-2026-10-15-60: an `Array[T, N]` receiver also finds `Slice[T]`'s
    // methods (design.md § Method resolution): `to_vec`, `binary_search`,
    // `windows` and `chunks` on a local array and on a `const` one.
    let src = r#"
const MAGIC: Array[i64, 3] = [1, 2, 3];
fn main() {
    let a: Array[i64, 5] = [1, 2, 3, 4, 5];
    let v = a.to_vec();
    let header: Vec[i64] = vec![1, 2, 3];
    println(f"{v.len()} {header == MAGIC.to_vec()} {a.binary_search(4)}");
    let s: Array[String, 2] = ["x".to_string(), "y".to_string()];
    let t = s.to_vec();
    println(f"{t} {s[1]}");
    for w in a.windows(2) { print(f"{w[0] + w[1]} "); }
    for c in a.chunks(2) { print(f"{c.len()} "); }
    println("");
}
"#;
    let want = "5 true Some(3)\n[x, y] y\n3 5 7 9 2 2 1 \n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_method_on_a_const_element_at_a_computed_index() {
    // B-2026-10-06-107: `NM[i].len()` on a `const` was parsed as the
    // type-qualified call `Type[Args].method()`, and `NAMES[k as i64]` was a
    // parse error. A Const-class name is never a type, so it indexes.
    let src = r#"
const NM: Array[String, 2] = ["ab", "cde"];
const NUMS: Array[i64, 2] = [-3, 4];
const NAMES: Array[Str, 2] = ["x", "yz"];
fn main() {
    let i = 1;
    let k: i32 = 0;
    println(f"{NM[i].clone()} {NM[i + 0].len()} {NM[i].len()} {NM[1].clone()}");
    println(f"{NUMS[i - 1].abs()} {NUMS[i] + 1}");
    println(f"{NAMES[k as i64].to_string()} {NAMES[i].len()}");
    let v: Vec[i64] = Vec[i64].new();
    println(f"{v.len()}");
}
"#;
    let want = "cde 3 3 cde\n3 5\nx 2\n0\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_value_enum_carries_a_value_enum_payload() {
    // B-2026-10-06-86: the one-level enum-nesting limit was a legacy layout
    // limit. `karac check` stopped applying it to v2 core, but the MIR
    // pipeline still refused `enum Tok { Open(Kind) }`, a unit-only `Kind`
    // included, and a generic enum instantiated at a value enum.
    let src = r#"
enum Kind { Round, Square, Curly }
enum Tok { Open(Kind), Close(Kind) }
enum V { Nil, Int(i64), Str(String) }
enum Place { Global(String), Field(V, V) }
enum Opt2[T] { No, Yes(T) }
fn closes(a: Kind, b: Kind) -> bool {
    match (a, b) {
        (Kind.Round, Kind.Round) => true,
        (Kind.Square, Kind.Square) => true,
        (Kind.Curly, Kind.Curly) => true,
        _ => false,
    }
}
fn lex(s: Str) -> Vec[Tok] {
    let mut out: Vec[Tok] = Vec.new();
    for c in s.chars() {
        match c {
            '(' => out.push(Tok.Open(Kind.Round)),
            ')' => out.push(Tok.Close(Kind.Round)),
            '[' => out.push(Tok.Open(Kind.Square)),
            ']' => out.push(Tok.Close(Kind.Square)),
            '{' => out.push(Tok.Open(Kind.Curly)),
            _ => out.push(Tok.Close(Kind.Curly)),
        }
    }
    out
}
fn valid(s: Str) -> bool {
    let mut st: Vec[Kind] = Vec.new();
    for t in lex(s).into_iter() {
        match t {
            Tok.Open(k) => st.push(k),
            Tok.Close(k) => match st.pop() {
                Some(o) => if not closes(o, k) { return false; },
                None => return false,
            },
        }
    }
    st.is_empty()
}
fn show(v: own V) -> String {
    match v {
        V.Nil => "nil".to_string(),
        V.Int(n) => f"{n}",
        V.Str(s) => s,
    }
}
fn main() {
    println(f"{valid("([]{})")} {valid("([)]")} {valid("((")}");
    let p = Place.Field(V.Int(1), V.Str("x".to_string()));
    match p {
        Place.Global(n) => println(n),
        Place.Field(a, b) => println(f"field {show(a)} {show(b)}"),
    }
    let q: Opt2[V] = Opt2.Yes(V.Str("deep".to_string()));
    match q {
        Opt2.Yes(v) => println(show(v)),
        Opt2.No => println("no"),
    }
}
"#;
    let want = "true false false\nfield 1 x\ndeep\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_concrete_impl_on_a_generic_type_keeps_its_args() {
    // B-2026-09-27-115 / B-2026-10-01-48: inside `impl G[R]` the checker
    // erased `self` to the bare `G`, so a payload binding was the declared
    // `T` (`t.id`, `v.len()` and `let w = v` were refused) and `self.n()`
    // missed the methods registered on `G1[String]`. The receiver rebind,
    // forward and hand-back spellings each drop the payload once.
    let src = r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.name}"); } }
enum G[T] { X(T), Z }
impl G[R] {
    fn peek(ref self) -> i64 { match self { G.X(t) => t.id, G.Z => -1 } }
    fn reb(own self) -> i64 {
        let e = self;
        match e { G.X(t) => { return t.id; } G.Z => { return 0; } }
    }
    fn fwd(own self) -> i64 { self.reb() + 100 }
    fn id(own self) -> G[R] { return self }
}
enum G1[T] { A(T), B }
impl G1[String] {
    fn n(ref self) -> i64 {
        match self { G1.A(v) => { return v.len() as i64; } G1.B => { return -1; } }
    }
    fn twice(ref self) -> i64 { self.n() * 2 }
    fn m(own self) -> i64 {
        match self { G1.A(v) => { let w = v; println(w); return 5; } G1.B => { return 0; } }
    }
}
struct Bx[T] { v: T }
impl Bx[String] {
    fn n(ref self) -> i64 { self.v.len() as i64 }
    fn twice(ref self) -> i64 { self.n() * 2 }
}
fn main() {
    let h: G1[String] = G1.A("abc".to_string());
    let z: G1[String] = G1.B;
    println(f"{h.n()} {h.twice()} {z.twice()}");
    let c: G1[String] = G1.A(f"s-{1}");
    println(f"r={c.m()}");
    let b = Bx { v: "hey".to_string() };
    println(f"{b.n()} {b.twice()}");
    let a: G[R] = G.X(R { id: 1, name: "a".to_string() });
    println(f"{a.reb()}");
    let e: G[R] = G.X(R { id: 2, name: "e".to_string() });
    println(f"{e.fwd()}");
    let d = G.X(R { id: 3, name: "d".to_string() }).id();
    println(f"{d.peek()}");
    let y: G[R] = G.Z;
    println(f"{y.fwd()}");
}
"#;
    let want = "3 6 -2\ns-1\nr=5\n3 6\ndrop a\n1\ndrop e\n102\n3\n100\ndrop d\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_vec_of_an_ord_element_orders_lexicographically() {
    // B-2026-10-06-92: `Vec[T: Ord]` was not `Ord` to the checker, so a
    // `SortedSet` / `SortedMap` key, `a < b`, `iter().max()` and
    // `a.cmp(b)` over `Vec`s were refused, and the MIR builder had no `Vec`
    // ordering to lower.
    let src = r#"
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct S { v: Vec[i64], n: i64 }
fn part2() {
    let a = S { v: vec![1, 2], n: 9 };
    let b = S { v: vec![1, 3], n: 0 };
    println(f"{a < b}");
    let x: Vec[i64] = vec![2];
    let y: Vec[i64] = vec![1, 5];
    match x.cmp(y) {
        Ordering.Less => println("less"),
        Ordering.Equal => println("eq"),
        Ordering.Greater => println("greater"),
    }
    let mut ss: Vec[S] = vec![b, a];
    ss.sort();
    println(f"{ss[0].n}");
}
fn main() {
    let a: Vec[i64] = vec![1, 2];
    let b: Vec[i64] = vec![1, 3];
    let p: Vec[i64] = vec![1];
    println(f"{a < b} {b < a} {p < a} {a <= a} {a > p} {a >= b}");
    let mut s: SortedSet[Vec[i64]] = SortedSet.new();
    s.insert(b.clone());
    s.insert(a.clone());
    s.insert(p.clone());
    s.insert(a.clone());
    for k in s.iter() { print(f"{k} "); }
    println("");
    let mut g: SortedMap[Vec[String], i64] = SortedMap.new();
    g.insert(vec!["b".to_string()], 2);
    g.insert(vec!["a".to_string(), "z".to_string()], 1);
    g.insert(vec!["a".to_string()], 0);
    for (k, v) in g.iter() { print(f"{k}={v} "); }
    println("");
    let v: Vec[Vec[i64]] = vec![vec![2], vec![1, 9], vec![3, 0, 0], vec![]];
    println(f"{v.iter().max().unwrap()} {v.iter().min().unwrap()}");
    let w: Vec[Vec[Vec[i64]]] = vec![vec![vec![1]], vec![vec![0, 5]]];
    println(f"{w[0] > w[1]}");
    part2();
}
"#;
    let want = "true false true true true false\n[1] [1, 2] [1, 3] \n[a]=0 [a, z]=1 [b]=2 \n[3, 0, 0] []\ntrue\ntrue\ngreater\n9\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_a_returned_ref_of_a_copy_value_is_read_into_the_slot() {
    // B-2026-10-18-4: `fn f(..) -> i64 { m.get(k).unwrap_or(0) }` assigned
    // the call's `ref i64` straight into the `i64` return slot, so the
    // compiled backends returned the reference's address. Tail, `return`
    // and a branch tail.
    let src = r#"
fn get_or_zero(m: ref Map[String, i64], k: Str) -> i64 {
    m.get(k).unwrap_or(0)
}
fn first_or_zero(v: ref Vec[i64]) -> i64 {
    return v.first().unwrap_or(0);
}
fn pick(m: ref Map[String, i64], k: Str, c: bool) -> i64 {
    if c { m.get(k).unwrap_or(0) } else { match m.get("b") { Some(x) => *x, None => -1 } }
}
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("a".to_string(), 5);
    m.insert("b".to_string(), 8);
    println(f"{get_or_zero(m, "a")} {get_or_zero(m, "z")}");
    let v: Vec[i64] = vec![4, 2];
    let e: Vec[i64] = Vec.new();
    println(f"{first_or_zero(v)} {first_or_zero(e)}");
    println(f"{pick(m, "a", true)} {pick(m, "a", false)}");
    let x: i64 = m.get("a").unwrap_or(0);
    println(f"{x}");
}
"#;
    let want = "5 0\n4 0\n5 8\n5\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_a_ref_result_outliving_its_referent_is_copied_out() {
    // B-2026-10-18-19: a tail `v.first().unwrap()` of a LOCAL `Vec` (or a
    // `PriorityQueue`'s `peek`) returned the reference into the `i64`
    // slot, so it dangled once the local died: `__mir-run` reported a use
    // after free and the compiled lanes printed addresses. Fixed by
    // B-2026-10-18-4's read-through in the built MIR.
    let src = r#"
fn top() -> i64 {
    let v: Vec[i64] = vec![7];
    v.first().unwrap()
}
fn topq() -> i64 {
    let mut q: PriorityQueue[i64] = PriorityQueue.new();
    q.push(7);
    q.peek().unwrap_or(0)
}
fn main_a() {
    let words = vec![4, 5];
    let t = top();
    println(f"{t} {words} {topq()}");
}
struct Least {
    mut top: PriorityQueue[i64],
}
impl Least {
    fn add(mut ref self, val: i64) -> i64 {
        self.top.push(val);
        self.top.peek().unwrap()
    }
}
fn answers(adds: Vec[i64]) -> Vec[i64] {
    let mut s = Least { top: PriorityQueue.new() };
    let mut out: Vec[i64] = Vec.new();
    for a in adds.iter() {
        out.push(s.add(*a));
    }
    out
}
fn main_b() {
    println(f"{answers(vec![5, 3, 8])}");
}
fn main() {
    main_a();
    main_b();
}
"#;
    let want = "7 [4, 5] 7\n[5, 3, 3]\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_a_match_arm_ref_result_is_read_into_the_value_slot() {
    // B-2026-10-18-13: `None => b.get(k).unwrap_or(0)` beside `Some(n) => *n`
    // stored the arm's `ref i64` into the `i64` match result. Fixed by
    // B-2026-10-18-4's read-through.
    let src = r#"
fn main() {
    let mut a: Map[char, i64] = Map.new();
    let mut b: Map[char, i64] = Map.new();
    b.insert('x', 2);
    a.insert('y', 5);
    let numb = match a.get('x') {
        Some(n) => *n,
        None => b.get('x').unwrap_or(0),
    };
    println(f"{numb - 1}");
}
"#;
    let want = "1\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_a_generic_function_passed_as_a_value_takes_its_instance_from_the_slot() {
    // B-2026-10-18-12: `count(v, never)` with `fn never[T](x: T) -> bool`
    // was refused with "a generic function as a value is not lowered yet".
    // The slot's `Fn` type (the callee's parameter, or a `let`
    // annotation) fixes the item's instance, and a call through the value
    // runs that instance.
    let src = r#"
fn never[T](x: T) -> bool {
    false
}
fn count[T](v: ref Vec[T], keep: Fn(T) -> bool) -> i64 {
    let mut n: i64 = 0;
    for x in v.iter() {
        if not keep(x) {
            n += 1;
        }
    }
    n
}
fn main0() {
    let v: Vec[char] = vec!['a', 'b'];
    println(f"{count(v, never)}");
}
fn len_of[T](v: ref Vec[T]) -> i64 { v.len() as i64 }
fn same[T](x: own T) -> T { x }
fn is_big(x: i64) -> bool { x > 10 }
fn apply(f: Fn(ref Vec[i64]) -> i64, v: Vec[i64]) -> i64 { f(v) }
fn twice(f: Fn(i64) -> i64, x: i64) -> i64 { f(f(x)) }
fn main1() {
    let g: Fn(i64) -> bool = is_big;
    let k: Fn(char) -> char = same;
    println(f"{apply(len_of, vec![1, 2, 3])} {twice(same, 4)} {g(11)} {k('z')}");
}
fn keep_all[T](x: ref T) -> bool { true }
fn count_kept[T](v: ref Vec[T], keep: Fn(ref T) -> bool) -> i64 {
    let mut n: i64 = 0;
    for x in v.iter() {
        if keep(x) { n += 1; }
    }
    n
}
fn wrap[T: Clone](x: ref T) -> Vec[T] { vec![x.clone(), x.clone()] }
fn main2() {
    let v: Vec[String] = vec!["a".to_string(), "bb".to_string()];
    let w: Fn(ref String) -> Vec[String] = wrap;
    let s = "q".to_string();
    println(f"{count_kept(v, keep_all)} {w(s).len()} {s}");
}
fn main() {
    main0();
    main1();
    main2();
}
"#;
    let want = "2\n3 4 true z\n2 2 q\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_a_bare_none_argument_leaves_the_callee_param_to_the_other_arguments() {
    // B-2026-10-18-24: `fold_from(it, f, None)` with `start: own Option[T]`
    // and `T` fixed by `I: Iterator[Item = T]` instanced `fold_from` at
    // `T = ()`: the bare `None` typed as `Option[T]` with Option's own `T`
    // bound the callee's `T` to it before the bound could solve it, and the
    // compiled lanes printed `()` (asan: stack-buffer-overflow).
    let src = r#"
struct VecIter[T] {
    items: Vec[T],
    pos: i64,
}

impl[T: Clone] Iterator for VecIter[T] {
    type Item = T;
    fn next(mut ref self) -> Option[T] {
        if self.pos >= self.items.len() {
            return None;
        }
        self.pos += 1;
        Some(self.items[self.pos - 1].clone())
    }
}

fn fold_from[I: Iterator[Item = T], T](it: own I, f: Fn(own T, own T) -> T, start: own Option[T]) -> Option[T] {
    let mut it = it;
    let mut acc = start;
    while let Some(x) = it.next() {
        acc = match acc {
            None => Some(x),
            Some(a) => Some(f(a, x)),
        };
    }
    acc
}

fn main0() {
    let v: Vec[i64] = vec![1, 2, 3];
    match fold_from(VecIter { items: v, pos: 0 }, |a: i64, b: i64| a + b, None) {
        Some(t) => println(f"{t}"),
        None => println("none"),
    }
}

enum Slot[T] {
    Empty,
    Full(T),
}

fn first[T](a: own Option[T], b: own Option[T]) -> Option[T] {
    match a {
        Some(x) => Some(x),
        None => b,
    }
}

fn pick[T](d: own T, s: own Slot[T]) -> T {
    match s {
        Slot.Full(x) => x,
        Slot.Empty => d,
    }
}

fn count[T](o: own Option[T]) -> i64 {
    match o {
        Some(_) => 1,
        None => 0,
    }
}

fn main1() {
    match first(None, Some("b".to_string())) {
        Some(s) => println(s),
        None => println("none"),
    }
    match first(Some(2.5), None) {
        Some(s) => println(f"{s}"),
        None => println("none"),
    }
    println(f"{pick(7, Slot.Empty)}");
    println(pick("d".to_string(), Slot.Empty));
    println(f"{count(None)}");
}

fn main() {
    main0();
    main1();
}
"#;
    let want = "6\nb\n2.5\n7\nd\n0\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_a_generic_types_associated_fn_takes_its_args_from_the_slot() {
    // B-2026-10-18-18: `Bag.new()` whose type arguments come only from where
    // it goes (`bags.push(Bag.new())`, `bags.insert(0, Bag.new())`, a
    // generic call's argument, or `let mut b = Bag.new()` inside
    // `impl[T] Bag[T]`) stopped with "a call with no resolved callee".
    let src = r#"
struct Bag[T] { items: Vec[T] }
impl[T: Clone] Bag[T] {
    fn new() -> Bag[T] { Bag { items: Vec.new() } }
    fn with(x: own T) -> Bag[T] {
        let mut b = Bag.new();
        b.items.push(x);
        b
    }
}
fn fill[U](x: own U, b: own Bag[U]) -> Bag[U] {
    let mut b = b;
    b.items.push(x);
    b
}
fn main() {
    let b = Bag.with(5);
    let mut bags: Vec[Bag[i64]] = Vec.new();
    bags.push(Bag.new());
    bags.insert(0, Bag.new());
    let mut m: Map[String, Bag[i64]] = Map.new();
    m.insert("a".to_string(), Bag.new());
    let c = fill("s".to_string(), Bag.new());
    println(f"{b.items.len()} {bags.len()} {m.len()} {c.items[0]}");
}
"#;
    let want = "1 2 1 s\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

// B-2026-10-15-15 — the `Str` searchers and cutters beside `find` and
// `split`: `rfind`, `split_once`, `rsplit_once`, `strip_prefix`,
// `strip_suffix` and `splitn`, each answering views into the text, on a
// `Str` and through a `String`.
#[test]
fn mir_str_cutters_answer_views() {
    const SRC: &str = r#"fn show(o: Option[(Str, Str)]) -> String {
    match o {
        Some((a, b)) => f"[{a}|{b}]",
        None => "none",
    }
}
fn main() {
    let s = "a/b/c.txt";
    println(f"{s.rfind("/")} {s.rfind("x")} {s.rfind("")} {"ééa".rfind("é")}");
    println(f"{show(s.split_once("/"))} {show(s.rsplit_once("/"))} {show(s.split_once("?"))}");
    println(f"{s.strip_prefix("a/")} {s.strip_suffix(".txt")} {s.strip_prefix("z")}");
    let parts: Vec[Str] = s.splitn(2, "/").collect();
    println(f"{parts.len()} {parts[0]} {parts[1]}");
    let e: Vec[Str] = "abc".splitn(3, "").collect();
    println(f"{e.len()} [{e[0]}] [{e[1]}] [{e[2]}]");
    let z: Vec[Str] = s.splitn(0, "/").collect();
    let one: Vec[Str] = s.splitn(1, "/").collect();
    let many: Vec[Str] = s.splitn(9, "/").collect();
    println(f"{z.len()} {one[0]} {many.len()} {many[2]}");
    let owned = "key=value=x".to_string();
    match owned.split_once("=") {
        Some((k, v)) => println(f"{k} {v}"),
        None => println("none"),
    }
    println(f"{owned.rfind("=")} {owned.strip_prefix("key")} {owned.trim_end()}");
    let w: Vec[Str] = owned.splitn(2, "=").collect();
    println(f"{w[1]}");
}
"#;
    let want = "Some(3) Some(7) Some(9) Some(2)\n[a|b/c.txt] [a/b|c.txt] none\nSome(b/c.txt) Some(a/b/c) None\n2 a b/c.txt\n3 [] [a] [bc]\n0 a/b/c.txt 3 c.txt\nkey value=x\nSome(9) Some(=value=x) key=value=x\nvalue=x\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

// B-2026-10-15-15 — `std.path`: the lexical path functions, imported item
// by item, on both MIR backends.
#[test]
fn mir_std_path_is_lexical() {
    const SRC: &str = r#"import std.path.{parent, file_name, extension, stem, join, normalize, is_absolute, components};
fn show(o: Option[Str]) -> String {
    match o {
        Some(s) => f"[{s}]",
        None => "none",
    }
}
fn main() {
    for p in ["a/b/c.txt", "a", "/a", "/", "", "a/b/", "a//b", "../x", "dir/.bashrc", "f.tar.gz", "f.", "a/.."].iter() {
        println(f"{p}: {show(parent(p))} {show(file_name(p))} {show(extension(p))} {show(stem(p))} {is_absolute(p)}");
    }
    println(join("a", "b"));
    println(join("a/", "b"));
    println(join("a", "/etc"));
    println(join("", "b"));
    println(join("a", "../../x"));
    for p in ["a/./b/../c", "/../a", "../../a/..", "a/b/../..", "/", "", ".//x//"].iter() {
        println(f"{p} -> {normalize(p)}");
    }
    for p in ["/usr//lib/./x", "./a/b", "a/../b", "."].iter() {
        let cs = components(p);
        let mut line = String.new();
        for c in cs.iter() {
            line.push_str(f"<{c}>");
        }
        println(f"{p}: {line}");
    }
}
"#;
    let want = "a/b/c.txt: [a/b] [c.txt] [txt] [c] false\na: [] [a] none [a] false\n/a: [/] [a] none [a] true\n/: none none none none true\n: none none none none false\na/b/: [a] [b] none [b] false\na//b: [a] [b] none [b] false\n../x: [..] [x] none [x] false\ndir/.bashrc: [dir] [.bashrc] none [.bashrc] false\nf.tar.gz: [] [f.tar.gz] [gz] [f.tar] false\nf.: [] [f.] [] [f] false\na/..: [a] none none none false\na/b\na/b\n/etc\nb\na/../../x\na/./b/../c -> a/c\n/../a -> /a\n../../a/.. -> ../..\na/b/../.. -> .\n/ -> /\n -> .\n.//x// -> x\n/usr//lib/./x: </><usr><lib><x>\n./a/b: <.><a><b>\na/../b: <a><..><b>\n.: <.>\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-15-17: `std.encoding` against RFC 4648's test vectors, with the
/// strict-decoding errors, identical on the MIR interpreter and both opt levels.
#[test]
fn mir_std_encoding_round_trips_rfc4648() {
    const SRC: &str = r#"import std.encoding.{base64_encode, base64_decode, base64url_encode, base64url_decode, hex_encode, hex_decode, DecodeError};
fn show(r: Result[Vec[u8], DecodeError]) -> String {
    match r {
        Ok(v) => f"ok:{String.from_utf8(v.as_slice()).unwrap_or("?".to_string())}",
        Err(e) => f"err:{e}",
    }
}
fn main() {
    for s in ["", "f", "fo", "foo", "foob", "fooba", "foobar"].iter() {
        let e = base64_encode(s.bytes());
        let u = base64url_encode(s.bytes());
        println(f"[{s}] {e} {u} {show(base64_decode(e.as_str()))} {show(base64url_decode(u.as_str()))} {hex_encode(s.bytes())}");
    }
    println(base64_encode([251u8, 255u8, 191u8].as_slice()));
    println(base64url_encode([251u8, 255u8, 191u8].as_slice()));
    println(show(base64_decode("Zm9v!mFy")));
    println(show(base64_decode("Zm9vYg")));
    println(show(base64_decode("Zm9vYh==")));
    println(show(base64url_decode("Zm9vYg==")));
    println(show(hex_decode("666F6f")));
    println(show(hex_decode("666")));
    println(show(hex_decode("6x")));
}
"#;
    let want = "[]   ok: ok: \n[f] Zg== Zg ok:f ok:f 66\n[fo] Zm8= Zm8 ok:fo ok:fo 666f\n[foo] Zm9v Zm9v ok:foo ok:foo 666f6f\n[foob] Zm9vYg== Zm9vYg ok:foob ok:foob 666f6f62\n[fooba] Zm9vYmE= Zm9vYmE ok:fooba ok:fooba 666f6f6261\n[foobar] Zm9vYmFy Zm9vYmFy ok:foobar ok:foobar 666f6f626172\n+/+/\n-_-_\nerr:invalid byte at offset 4\nerr:invalid length\nerr:invalid byte at offset 5\nok:foob\nok:foo\nerr:invalid length\nerr:invalid byte at offset 1\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A wildcard import of a gated module names its public items, and the
/// program's own `join` beats the wildcard's (design.md § Wildcards).
#[test]
fn mir_gated_wildcard_import_names_public_items() {
    const SRC: &str = r#"import std.path.*;
import std.encoding.{hex_encode};

fn join(a: Str, b: Str) -> String {
    f"{a}+{b}"
}

fn main() {
    println(parent("a/b") ?? "none");
    println(join("x", "y"));
    println(normalize("a/./b/../c"));
    println(hex_encode("A".bytes()));
}
"#;
    let want = "a\nx+y\na/c\n41\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A string literal after `??` is a `Str` when the left side is an
/// `Option[Str]`, and stays a `String` after an `Option[String]`.
#[test]
fn mir_nil_coalesce_str_literal_fallback() {
    const SRC: &str = r#"fn pick(v: Vec[String], i: i64) -> Option[Str] {
    if i < v.len() { Some(v[i].as_str()) } else { None }
}

fn main() {
    let v = ["x".to_string()];
    let a = pick(v, 0) ?? "none";
    let b = pick(v, 5) ?? "none";
    let o: Option[String] = None;
    let c = o ?? "owned";
    println(f"{a} {b} {c} {c.len()}");
}
"#;
    let want = "x none owned 5\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-15-47: a struct update whose base is a call evaluates the base
/// once, after the listed fields, and copies the rest from it.
#[test]
fn mir_struct_update_from_a_call() {
    const SRC: &str = r#"struct D { a: i64, name: String, c: bool }

fn base() -> D {
    println("base");
    D { a: 1, name: "b".to_string(), c: true }
}

fn five() -> i64 {
    println("five");
    5
}

struct H { d: D }

fn main() {
    let d = D { a: five(), ..base() };
    println(f"{d.a} {d.name} {d.c}");
    let e = D { c: false, ..base() };
    println(f"{e.a} {e.name} {e.c}");
    let h = H { d: D { a: 2, ..base() } };
    println(f"{h.d.a} {h.d.name}");
    let v = [D { a: 9, ..base() }, D { name: "x".to_string(), ..base() }];
    println(f"{v[0].a} {v[1].name} {v[1].a}");
}
"#;
    let want = "five\nbase\n5 b true\nbase\n1 b false\nbase\n2 b\nbase\nbase\n9 x 1\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-15-10: `std.time`'s `Duration` and `sleep`, `std.random` and
/// `std.uuid`, beside the `Clock` global.
#[test]
fn mir_std_time_random_uuid() {
    const SRC: &str = r#"import std.time.{Clock, Duration, SystemTime, sleep};
import std.random.{next_u64};
import std.uuid.{v4};

fn main() {
    let d = Duration.ms(1500);
    println(f"{d.as_ms()} {Duration.secs(2).as_ms()} {d < Duration.secs(2)} {d == Duration.ms(1500)}");
    let t0 = SystemTime.now();
    sleep(Duration.ms(2));
    let t1 = SystemTime.now();
    println(f"{t0.since_epoch().as_ms() > 1700000000000} {t0 < t1}");
    let u = v4();
    let parts: Vec[Str] = u.split("-").collect();
    println(f"{u.len()} {parts.len()} {parts[2].slice(0, 1)} {next_u64() != next_u64()} {Clock.now() > 0}");
}
"#;
    let want = "1500 2000 true true\ntrue true\n36 5 4 true true\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-15-112: `std.time.Instant` on the monotonic clock.
#[test]
fn mir_std_time_instant() {
    const SRC: &str = r#"import std.time.{Duration, Instant, sleep};

fn main() {
    let i0 = Instant.now();
    sleep(Duration.ms(2));
    let i1 = Instant.now();
    let later = i0.add(Duration.ms(5));
    println(f"{i1.duration_since(i0).as_ms() >= 2} {i0 < i1} {later.duration_since(i0).as_ms()} {i0.elapsed().as_ms() >= 2}");
}
"#;
    let want = "true true 5 true\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-15-51: an unsuffixed literal on the left of `&`, `|` or `^`
/// takes the other operand's integer type, as it does under `+`.
#[test]
fn mir_bitwise_literal_left_operand_takes_the_peer_type() {
    const SRC: &str = r#"fn main() {
    let c: u8 = 20;
    let d: u8 = 0xC0 ^ c;
    let g: u8 = (c >> 2) | 0x80;
    let x: u8 = 0x80 | (c & 0x3F);
    let mut out: Vec[u8] = Vec.new();
    out.push(0xC0 | (c >> 6));
    println(f"{d} {g} {x} {out[0]}");
}
"#;
    let want = "212 133 148 192\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-15-52: a literal branch of an `if` or `match` takes the type the
/// other branches agree on, whichever branch comes first.
#[test]
fn mir_literal_branch_takes_the_other_branch_type() {
    const SRC: &str = r#"fn f(c: bool, y: u128) -> u128 {
    let x = if c { 0 } else { y };
    x
}

fn g(k: i64, y: u8) -> u8 {
    let z = match k { 0 => 255, 1 => y, _ => { 7 } };
    z
}

fn main() {
    let y: u8 = 7;
    let z: u8 = if true { 1 } else { y };
    println(f"{f(true, 5)} {f(false, 5)} {z} {g(0, 3)} {g(1, 3)} {g(2, 3)}");
}
"#;
    let want = "0 5 1 255 3 7\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-15-50: a variant is constructed through a type alias of its enum.
#[test]
fn mir_variant_constructed_through_a_type_alias_in_a_result() {
    const SRC: &str = r#"enum NetE { A, B(i32) }
type OldE = NetE;

fn f(x: i64) -> Result[i64, OldE] {
    if x > 0 { Ok(x) } else { Err(OldE.B(7)) }
}

fn main() {
    match f(0) { Ok(_) => println("ok"), Err(NetE.B(n)) => println(f"b {n}"), Err(_) => println("other") }
    let u = OldE.A;
    match u { NetE.A => println("a"), _ => println("x") }
}
"#;
    let want = "b 7\na\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-15-60: an `Array[T, N]` takes `Slice[T]`'s `to_vec`, for a
/// const table, a local and a non-`Copy` element.
#[test]
fn mir_array_to_vec_copies_the_elements() {
    const SRC: &str = r#"const TBL: Array[u8, 3] = [1, 2, 3];

fn main() {
    let a: Array[i64, 3] = [4, 5, 6];
    let mut v = a.to_vec();
    v.push(7);
    let s: Array[String, 2] = ["x".to_string(), "yz".to_string()];
    let t = s.to_vec();
    println(f"{v} {TBL.to_vec()} {a[0]} {t} {s[1]}");
}
"#;
    let want = "[4, 5, 6, 7] [1, 2, 3] 4 [x, yz] yz\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-14-68: a string literal in a tuple inside `Some` / `Ok` takes
/// the `Str` its slot names.
#[test]
fn mir_str_literal_in_a_tuple_payload() {
    const SRC: &str = r#"fn g() -> Option[(Str, Str)] { Some(("a", "b")) }
fn h() -> Result[(Str, (i64, Str)), String] { Ok(("c", (1, "d"))) }

fn main() {
    let (x, y) = g().unwrap();
    let (z, (n, w)) = h().unwrap();
    println(f"{x}{y}{z}{n}{w}");
}
"#;
    let want = "abc1d\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_a_named_fn_with_a_borrowed_param_solves_the_slots_return() {
    // B-2026-10-18-26: `check(n).map_err(text)?` with `fn text(e: E)` (a
    // borrowed parameter) for `map_err[F]`'s `OnceFn(own E) -> F` failed the
    // function type's unify at the parameter and never read the return, so
    // `F` stayed unsolved: "'?' cannot propagate error 'F' as 'String'".
    let src = r#"
enum E {
    Bad(i64),
}

fn check(n: i64) -> Result[i64, E] {
    if n < 0 { Err(E.Bad(n)) } else { Ok(n * 2) }
}

fn text(e: E) -> String {
    match e {
        E.Bad(n) => f"bad {n}",
    }
}

fn run(n: i64) -> Result[i64, String] {
    let v = check(n).map_err(text)?;
    Ok(v + 1)
}

fn main0() {
    match run(4) {
        Ok(v) => println(f"{v}"),
        Err(e) => println(e),
    }
    match run(-1) {
        Ok(v) => println(f"{v}"),
        Err(e) => println(e),
    }
}

fn len_of(s: String) -> i64 { s.len() }
fn shout(s: String) -> String { s.to_uppercase() }
fn twice(n: i64) -> i64 { n * 2 }
fn parse(s: String) -> Result[i64, String] {
    let n = s.len();
    if n > 3 { Err(f"long {s}") } else { Ok(n) }
}
fn main1() {
    let o: Option[String] = Some("abc".to_string());
    let n = o.clone().map(len_of);
    let u = o.map(shout);
    let r: Result[i64, String] = Ok(4);
    let t = r.map(twice);
    let p = parse("hello".to_string()).map_err(shout);
    println(f"{n.unwrap()} {u.unwrap()} {t.unwrap()}");
    match p {
        Ok(v) => println(f"{v}"),
        Err(e) => println(e),
    }
}

fn main() {
    main0();
    main1();
}
"#;
    let want = "9\nbad -1\n3 ABC 8\nLONG HELLO\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// `T.parse` reads one to 18 digits inline and hands every other text to
/// the runtime (B-2026-10-18-34): both paths, at three widths, give the
/// MIR interpreter's answers.
#[test]
fn mir_build_parse_agrees_with_the_interpreter() {
    let src = r#"
fn show(o: Option[i64]) -> String {
    match o {
        Some(v) => f"{v}",
        None => "none".to_string(),
    }
}
fn show8(o: Option[i8]) -> String {
    match o {
        Some(v) => f"{v}",
        None => "none".to_string(),
    }
}
fn showu8(o: Option[u8]) -> String {
    match o {
        Some(v) => f"{v}",
        None => "none".to_string(),
    }
}
fn main() {
    let cases = ["0", "12", "-12", "+7", " 12", "12\n", "", "-", "1a", "999999999999999999", "9223372036854775807", "9223372036854775808", "-9223372036854775808", "127", "128", "255", "256", "-0", "007"];
    for c in cases {
        println(f"[{c}] {show(i64.parse(c).ok())} {show8(i8.parse(c).ok())} {showu8(u8.parse(c).ok())}");
    }
}
"#;
    let want = karac::mir::lower::run_source(src).expect("interp").output;
    assert!(want.contains("[255] 255 none 255\n"), "{want}");
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// `s[lo..hi]` as a view is cut inline when both ends are char
/// boundaries (B-2026-10-18-34): ASCII, multi-byte, empty and end cuts
/// give the MIR interpreter's answers.
#[test]
fn mir_build_str_view_cuts_agree_with_the_interpreter() {
    let src = r#"
fn cut(t: Str, lo: i64, hi: i64) -> Str {
    t[lo..hi]
}
fn main() {
    let s = "héllo wörld";
    let e = "";
    println(f"[{cut(s, 0, 1)}] [{cut(s, 0, 3)}] [{cut(s, 3, 7)}] [{cut(s, 13, 13)}] [{cut(s, 0, 13)}] [{cut(e, 0, 0)}] [{cut(s, 5, 5)}]");
    println(f"[{s[1..3]}] [{s[8..12]}]");
}
"#;
    let want = karac::mir::lower::run_source(src).expect("interp").output;
    assert_eq!(want, "[h] [hé] [llo ] [] [héllo wörld] [] []\n[é] [örl]\n");
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_char_try_from_reads_a_borrowed_integer() {
    // B-2026-10-18-33: `char.try_from(x)` with `x` a `ref i64` from
    // `for x in v.iter()` was refused ("expects an integer codepoint, got
    // `ref i64`"); it reads the value, as the other integer uses of `x` do.
    let src = r#"
fn main() {
    let b: Vec[i64] = vec![104, 105, 55296];
    let mut s = String.new();
    for x in b.iter() {
        match char.try_from(x) {
            Ok(c) => s.push(c),
            Err(n) => s.push_str(f"<{n}>"),
        }
    }
    let u: Vec[u32] = vec![79, 75];
    for y in u.iter() {
        s.push(char.try_from(y).unwrap());
    }
    let w: Vec[u8] = vec![33];
    for z in w.iter() {
        s.push(char.try_from(z).unwrap());
    }
    println(s);
}
"#;
    let want = "hi<55296>OK!\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_a_collect_inside_a_constructor_takes_the_expected_target() {
    // B-2026-10-18-37: `Ok(s.split_whitespace().map(..).collect())` as the
    // tail of a `Result[Vec[String], String]` fn left `collect`'s `C`
    // unsolved: the expected type seeded `Ok`'s `T`, but the argument was
    // inferred rather than checked against it.
    let src = r#"
fn words(text: ref String) -> Result[Vec[String], String] {
    Ok(text.split_whitespace().map(|p| p.to_string()).collect())
}
fn evens(v: ref Vec[i64]) -> Option[Vec[i64]] {
    if v.is_empty() {
        return None;
    }
    return Some(v.iter().filter(|x| *x % 2 == 0).map(|x| *x).collect());
}
fn uniq(v: ref Vec[i64]) -> Result[Set[i64], String] {
    Ok(v.iter().map(|x| *x).collect())
}
fn upper(s: ref String) -> Option[String] {
    Some(s.chars().map(|c| c.to_ascii_uppercase()).collect())
}
fn main() {
    match words("a b c".to_string()) {
        Ok(v) => println(f"{v.len()} {v[2]}"),
        Err(e) => println(e),
    }
    let v: Vec[i64] = vec![1, 2, 4, 2];
    let s = "ab".to_string();
    println(f"{evens(v).unwrap().len()} {uniq(v).unwrap().len()} {upper(s).unwrap()}");
}
"#;
    let want = "3 c\n3 3 AB\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_a_programs_own_field_and_variant_leave_reflection_alone() {
    // B-2026-10-18-45: a program declaring its own `struct Field` that also
    // pulls in protobuf (`derive(Message)`, or just the word `Message`)
    // failed with "no field 'ty' on struct 'Field'": the library's schema
    // walk over `T.fields()` read the program's `Field` for the reflection
    // record. The program's `Field` and `Variant` are renamed apart.
    let src = r#"
struct Field { name: String, number: i64 }
enum Variant { A, B(i64) }
fn show(v: Variant) -> String {
    match v {
        Variant.A => "a".to_string(),
        Variant.B(n) => f"b{n}",
    }
}

#[derive(Message)]
struct Person {
    name: String,
    age: i64,
}

fn main() {
    let fs: Vec[Field] = vec![Field { name: "name".to_string(), number: 1 }];
    let p = Person { name: "Ada".to_string(), age: 36 };
    let q = Person.decode(p.encode());
    println(f"{q.name} {q.age} {fs[0].name} {fs[0].number} {show(Variant.B(2))}");
}
"#;
    let want = "Ada 36 name 1 b2\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// `sort_by` / `sort_by_key` run an emitted merge sort that makes the
/// same comparator calls in the same order as the MIR interpreter,
/// including past the 1024-element block (B-2026-10-13-18). The third
/// sort's comparator counts and prints its calls and is not an order,
/// so any difference in the call schedule changes the output.
#[test]
fn mir_build_sort_by_call_schedule_agrees_with_the_interpreter() {
    let src = r#"
fn make(n: i64, seed: i64) -> Vec[(i64, i64)] {
    let mut v: Vec[(i64, i64)] = Vec.new();
    let mut s = seed;
    let mut i = 0;
    while i < n {
        s = (s * 1103515245 + 12345) & 2147483647;
        v.push((s % 97, i));
        i += 1;
    }
    v
}
fn sum(v: ref Vec[(i64, i64)]) -> i64 {
    let mut h = 0;
    for (k, x) in v.iter() {
        h = (h * 31 + k * 1000 + x) % 1000000007;
    }
    h
}
fn main() {
    for n in [5, 1025, 3000] {
        let mut a = make(n, n);
        a.sort_by(|x, y| x.0.cmp(y.0));
        let mut b = make(n, n + 1);
        b.sort_by_key(|x| 0 - x.0);
        let mut c = make(n, n + 2);
        let mut calls = 0;
        c.sort_by(|x, y| {
            calls += 1;
            if calls % 1000 == 0 {
                println(f"call {calls} {x.1} {y.1}");
            }
            if (x.1 * 7 + y.1) % 5 < 2 { Ordering.Less } else { Ordering.Greater }
        });
        let s: Vec[i64] = make(n, 3).iter().map(|p| p.0).collect();
        let mut t = s.clone();
        t.sort_by(|x, y| y.cmp(x));
        println(f"{n} {sum(a)} {sum(b)} {sum(c)} {calls} {t[0]} {t[n - 1]}");
    }
}
"#;
    let want = karac::mir::lower::run_source(src).expect("interp").output;
    assert!(want.contains("call 1000 "), "{want}");
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A long `sort_by` / `sort_by_key` over few distinct keys is
/// partitioned rather than merged (kata 1665's bench ran 2.8x legacy on
/// the merge), by the interpreter's `sort_order` and the compiled
/// `part_sort` alike: the same comparator calls in the same order, so
/// they agree from one key to a million, on sorted, reversed and
/// half-tied inputs, for a comparator that prints, and for one that
/// panics on a pair only the partition compares (element 10 against the
/// middle one).
#[test]
fn mir_build_sort_by_partitions_few_keys_like_the_interpreter() {
    let pure = r#"
fn make(n: i64, seed: i64, keys: i64) -> Vec[(i64, i64)] {
    let mut v: Vec[(i64, i64)] = Vec.new();
    let mut s = seed;
    let mut i = 0;
    while i < n {
        s = (s * 1103515245 + 12345) & 2147483647;
        v.push((s % keys, i));
        i += 1;
    }
    v
}
fn sum(v: ref Vec[(i64, i64)]) -> i64 {
    let mut h = 0;
    for (k, x) in v.iter() {
        h = (h * 31 + k * 1000 + x) % 1000000007;
    }
    h
}
fn main() {
    for n in [4097, 20000] {
        for keys in [1, 2, 7, 50, 1000000] {
            let mut a = make(n, n + keys, keys);
            a.sort_by(|x, y| x.0.cmp(y.0));
            let mut b = make(n, n + keys + 1, keys);
            b.sort_by_key(|x| 0 - x.0);
            let mut c = make(n, keys, keys);
            c.sort_by(|x, y| y.0.cmp(x.0));
            let mut d = c.clone();
            d.sort_by(|x, y| x.0.cmp(y.0));
            println(f"{n} {keys} {sum(a)} {sum(b)} {sum(c)} {sum(d)}");
        }
    }
    let mut m = make(20000, 9, 1000000);
    let mut i = 0;
    while i < 20000 {
        if i % 2 == 0 {
            m[i] = (500000, i);
        }
        i += 1;
    }
    m.sort_by(|x, y| x.0.cmp(y.0));
    println(f"mixed {sum(m)}");
}
"#;
    let printing = r#"
fn main() {
    let mut v: Vec[(i64, i64)] = Vec.new();
    let mut i = 0;
    while i < 6000 {
        v.push(((i * 7) % 5, i));
        i += 1;
    }
    let mut calls = 0;
    v.sort_by(|x, y| {
        calls += 1;
        if calls % 5000 == 0 {
            println(f"call {calls} {x.1} {y.1}");
        }
        x.0.cmp(y.0)
    });
    println(f"{calls} {v[0].1} {v[5999].1}");
}
"#;
    let panicky = r#"
fn main() {
    let mut v: Vec[(i64, i64)] = Vec.new();
    let mut i = 0;
    while i < 6000 {
        v.push(((i * 7) % 5, i));
        i += 1;
    }
    let mut calls = 0;
    v.sort_by(|x, y| {
        calls += 1;
        if calls % 2000 == 0 {
            println(f"call {calls}");
        }
        if x.1 == 10 and y.1 == 3000 {
            panic(f"pair {x.1} {y.1}");
        }
        x.0.cmp(y.0)
    });
    println(f"{calls}");
}
"#;
    let lowered = karac::mir::lower::build_source(pure).expect("MIR");
    let ir = karac::codegen::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    assert!(ir.contains("part_sort."), "sort_by runs the partition sort");
    for src in [pure, printing] {
        let want = karac::mir::lower::run_source(src).expect("interp").output;
        for opt in [0, 2] {
            if let Some(out) = build_and_run(src, opt) {
                assert_eq!(out, want, "at -O{opt}");
            }
        }
    }
    let dir = Scratch::new();
    let file = dir.path().join("main.kara");
    std::fs::write(&file, panicky).unwrap();
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
    };
    let why = first(&interp.stderr);
    assert!(why.contains("pair 10 3000"), "{why}");
    let Some(exe) = cli_build(&dir, panicky, &[]) else {
        return;
    };
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(run.status.code(), Some(101));
    assert_eq!(first(&run.stderr), why, "compiled");
    assert_eq!(run.stdout, interp.stdout, "compiled");
}

/// `Display` for an `f32` shows its own shortest digits, not the widened
/// `f64`'s (B-2026-10-18-47), in a bare hole, `to_string` and inside a
/// `Vec`; a precision and the `f64`s are unchanged.
#[test]
fn mir_build_f32_display_shows_its_own_shortest_digits() {
    let src = r#"
fn main() {
    let z: f32 = 0.1;
    let w: f32 = 3.0;
    let big: f32 = 16777216.0;
    let d: f64 = 0.1;
    println(f"{z} {w} {big} {d}");
    println(z.to_string());
    println(f"[{z:.3}]");
    let v: Vec[f32] = [0.1, 0.2, 1.5];
    println(f"{v}");
    let x = z * 3.0;
    println(f"{x}");
}
"#;
    let want = karac::mir::lower::run_source(src).expect("interp").output;
    assert_eq!(
        want,
        "0.1 3.0 16777216.0 0.1\n0.1\n[0.100]\n[0.1, 0.2, 1.5]\n0.3\n",
    );
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_a_field_reached_through_a_mut_ref_binds_its_payload_mutably() {
    // B-2026-10-18-41: `match it.value { V.List(q) => q.push(..) }` with
    // `it` a `mut ref` was refused by the borrow checker ("mutable borrow
    // of (*_16) through a shared reference"): the field was matched
    // through a shared reborrow. A place reached only through `mut ref`s
    // is now reborrowed mutably, so `q` is a `mut ref`, while a `Copy`
    // payload (`Count(n)`) still copies and the arm may overwrite the
    // field it came from.
    let src = r#"
enum Value { Text(String), List(Vec[String]) }
struct Item { value: Value }
enum St { Idle, Count(i64), Named(String) }
struct M { state: St, tag: String }

fn add(it: mut ref Item) {
    match it.value {
        Value.List(q) => q.push("x".to_string()),
        Value.Text(_) => {}
    }
}

fn step(m: mut ref M) {
    match m.state {
        St.Idle => m.state = St.Count(1),
        St.Count(n) => {
            m.state = St.Named(f"c{n}");
            println(f"was {n}");
        }
        St.Named(s) => {
            s.push_str("!");
            println(f"{s} {m.tag}");
        }
    }
}

fn main() {
    let mut items: Map[String, Item] = Map.new();
    items.insert("k".to_string(), Item { value: Value.List(Vec.new()) });
    let it = items.get_mut("k".to_string()).unwrap();
    match it.value {
        Value.List(q) => q.push("x".to_string()),
        Value.Text(_) => {}
    }
    add(mut items.get_mut("k".to_string()).unwrap());
    match items.get("k".to_string()).unwrap().value {
        Value.List(q) => println(f"{q.len()}"),
        Value.Text(_) => {}
    }
    let mut m = M { state: St.Idle, tag: "t".to_string() };
    step(mut m);
    step(mut m);
    step(mut m);
}
"#;
    let want = "2\nwas 1\nc1! t\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A fresh `Vec` is allocated in line (kata 28's bench): empty, zero-sized
/// element, `with_capacity`, a literal, `filled` and a clone all give
/// the MIR interpreter's answers.
#[test]
fn mir_build_inline_vec_allocation_agrees_with_the_interpreter() {
    let src = r#"
fn main() {
    let a: Vec[u8] = Vec.filled(0, 1u8);
    let mut b: Vec[i64] = Vec.with_capacity(0);
    b.push(7);
    let c: Vec[()] = Vec.filled(3, ());
    let d: Vec[i64] = vec![1, 2, 3];
    let mut e: Vec[i64] = Vec.with_capacity(2);
    for i in 0..5 {
        e.push(i * i);
    }
    let f = e.clone();
    let g: Vec[String] = Vec.filled(2, "x".to_string());
    println(f"{a.len()} {b} {c.len()} {d} {e} {f} {g}");
}
"#;
    let want = karac::mir::lower::run_source(src).expect("interp").output;
    assert_eq!(
        want,
        "0 [7] 3 [1, 2, 3] [0, 1, 4, 9, 16] [0, 1, 4, 9, 16] [x, x]\n"
    );
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_a_generic_types_field_typed_by_a_projection_takes_its_instance() {
    // B-2026-10-18-23: a generic ADT with a field typed by a projection of
    // its parameter (`prev: Option[I.Item]`) stopped every MIR build that
    // used it with "an associated type of `I` is not lowered yet": the
    // definition's field types were converted with `I` still a parameter.
    // The projection now stays in the definition and each instance
    // normalizes it (`VecIter[String].Item` = `String`), for a struct and
    // an enum payload, nested, and under a `for` loop.
    let src = r#"
struct VecIter[T] {
    items: Vec[T],
    pos: i64,
}

impl[T: Clone] Iterator for VecIter[T] {
    type Item = T;
    fn next(mut ref self) -> Option[T] {
        if self.pos >= self.items.len() {
            return None;
        }
        self.pos += 1;
        Some(self.items[self.pos - 1].clone())
    }
}

// Yields each item with the one before it, if any.
struct WithPrev[I: Iterator] {
    it: I,
    prev: Option[I.Item],
}

fn with_prev[I: Iterator](it: own I) -> WithPrev[I] {
    WithPrev { it: it, prev: None }
}

impl[I: Iterator] Iterator for WithPrev[I] {
    type Item = (Option[I.Item], I.Item);
    fn next(mut ref self) -> Option[(Option[I.Item], I.Item)] {
        match self.it.next() {
            Some(x) => {
                let p = self.prev.take();
                Some((p, x))
            }
            None => None,
        }
    }
}

enum Slot[I: Iterator] {
    Empty,
    Held(I.Item),
}

fn first_slot[I: Iterator](it: own I) -> Slot[I] {
    let mut it = it;
    match it.next() {
        Some(x) => Slot.Held(x),
        None => Slot.Empty,
    }
}

fn main() {
    let words: Vec[String] = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let mut w = with_prev(VecIter { items: words, pos: 0 });
    w.prev = Some("start".to_string());
    while let Some((p, x)) = w.next() {
        match p {
            Some(p) => println(f"{p}>{x}"),
            None => println(f"_>{x}"),
        }
    }
    let mut nested = with_prev(with_prev(VecIter { items: vec![1, 2], pos: 0 }));
    while let Some((outer, (inner, x))) = nested.next() {
        let o = match outer { Some(_) => "o", None => "-" };
        let i = match inner { Some(v) => v, None => 0 };
        println(f"{o} {i} {x}");
    }
    let s = first_slot(VecIter { items: vec!["z".to_string()], pos: 0 });
    match s {
        Slot.Held(x) => println(f"held {x}"),
        Slot.Empty => println("empty"),
    }
    let none: Vec[String] = Vec.new();
    let e = first_slot(VecIter { items: none, pos: 0 });
    match e {
        Slot.Held(x) => println(f"held {x}"),
        Slot.Empty => println("empty"),
    }
    let mut n = 0;
    for (p, x) in with_prev(VecIter { items: vec![7, 8, 9], pos: 0 }) {
        n += x;
        match p { Some(_) => n += 100, None => {} }
    }
    println(f"{n}");
}
"#;
    let want = "start>a\n_>b\n_>c\n- 0 1\n- 0 2\nheld z\nempty\n24\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// `String.pop` takes the last char off the end, whatever its UTF-8 width,
/// and `String.truncate` keeps the first `n` bytes (a no-op past the end, as
/// `Vec.truncate`), on both backends (B-2026-10-19-2).
#[test]
fn mir_string_pop_and_truncate() {
    const SRC: &str = "fn strip_newline(s: mut ref String) {\n    \
        if s.ends_with(\"\\n\") { let _ = s.pop(); }\n}\n\
        fn main() {\n    \
        let mut s = \"abc\\n\".to_string();\n    \
        strip_newline(mut s);\n    \
        println(f\"[{s}] {s.len()}\");\n    \
        let mut t = \"h\\u{e9}llo\\u{2192}\\u{20ac}\\u{1f600}\".to_string();\n    \
        let a = t.pop().unwrap();\n    \
        let b = t.pop().unwrap();\n    \
        let c = t.pop().unwrap();\n    \
        println(f\"{a} {b} {c} {t} {t.len()}\");\n    \
        t.truncate(3);\n    \
        println(f\"[{t}] {t.len()}\");\n    \
        t.truncate(10);\n    \
        t.push('!');\n    \
        println(f\"[{t}] {t.len()}\");\n    \
        t.truncate(-1);\n    \
        let none = t.pop().is_none();\n    \
        println(f\"[{t}] {t.is_empty()} {none}\");\n    \
        let mut w = \"ab\".to_string();\n    \
        w.push('c');\n    \
        while let Some(ch) = w.pop() { print(f\"{ch}\"); }\n    \
        println(f\" {w.len()}\");\n\
        }\n";
    let want = "[abc] 3\n\u{1f600} \u{20ac} \u{2192} h\u{e9}llo 6\n[h\u{e9}] 3\n[h\u{e9}!] 4\n[] true true\ncba 0\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A `String.truncate` length inside a char panics, with the same message on
/// both backends (B-2026-10-19-2).
#[test]
fn mir_string_truncate_inside_a_char_panics() {
    let src = "fn main() {\n    let mut t = \"h\\u{e9}\".to_string();\n    \
               println(\"before\");\n    t.truncate(2);\n    println(t);\n}\n";
    let want = "runtime error: String.truncate: the length is not on a UTF-8 char boundary\n";
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
    assert_eq!(interp.status.code(), Some(101), "__mir-run");
    assert_eq!(
        String::from_utf8_lossy(&interp.stdout),
        "before\n",
        "__mir-run"
    );
    assert_eq!(first(&interp.stderr), want, "__mir-run");
    let Some(exe) = cli_build(&dir, src, &[]) else {
        return;
    };
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(run.status.code(), Some(101));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "before\n");
    assert_eq!(first(&run.stderr), want, "compiled");
}

#[test]
fn mir_a_projection_bound_by_a_where_clause_takes_its_methods() {
    // B-2026-10-19-8: `x.clone()` on `x: I.Item` under `where I.Item: Clone`
    // checked as an error type, so a `Pairwise` adapter, which keeps a copy
    // of each item, stopped on MIR with "a type that failed to check". A
    // projection a where clause bounds now takes that bound's methods, in a
    // generic function and in a generic impl, nested and under `for`.
    let src = r#"
struct VecIter[T] {
    items: Vec[T],
    pos: i64,
}

impl[T: Clone] Iterator for VecIter[T] {
    type Item = T;
    fn next(mut ref self) -> Option[T] {
        if self.pos >= self.items.len() {
            return None;
        }
        self.pos += 1;
        Some(self.items[self.pos - 1].clone())
    }
}

struct Pairwise[I: Iterator] {
    it: I,
    prev: Option[I.Item],
}

fn pairwise[I: Iterator](it: own I) -> Pairwise[I] {
    Pairwise { it: it, prev: None }
}

impl[I: Iterator] Iterator for Pairwise[I] where I.Item: Clone {
    type Item = (I.Item, I.Item);
    fn next(mut ref self) -> Option[(I.Item, I.Item)] {
        if self.prev.is_none() {
            self.prev = self.it.next();
        }
        match self.it.next() {
            Some(x) => {
                let p = self.prev.take().unwrap();
                self.prev = Some(x.clone());
                Some((p, x))
            }
            None => None,
        }
    }
}

enum Slot[I: Iterator] {
    Empty,
    Held(I.Item),
}

fn first_slot[I: Iterator](it: own I) -> Slot[I] {
    let mut it = it;
    match it.next() {
        Some(x) => Slot.Held(x),
        None => Slot.Empty,
    }
}

fn main() {
    let words: Vec[String] = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let mut pw = pairwise(VecIter { items: words, pos: 0 });
    while let Some((a, b)) = pw.next() {
        println(f"{a}{b}");
    }
    let nums: Vec[i64] = vec![1, 2, 3, 4];
    let mut nested = pairwise(pairwise(VecIter { items: nums, pos: 0 }));
    while let Some(((a, b), (c, d))) = nested.next() {
        println(f"{a}{b}-{c}{d}");
    }
    let s = first_slot(VecIter { items: vec!["z".to_string()], pos: 0 });
    match s {
        Slot.Held(x) => println(f"held {x}"),
        Slot.Empty => println("empty"),
    }
    for (a, b) in pairwise(VecIter { items: vec![7, 8, 9], pos: 0 }) {
        println(f"{a}+{b}={a + b}");
    }
}
"#;
    let want = "ab\nbc\n12-23\n23-34\nheld z\n7+8=15\n8+9=17\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
    // A user trait's method through the same kind of bound.
    let src = r#"
struct VecIter[T] {
    items: Vec[T],
    pos: i64,
}

impl[T: Clone] Iterator for VecIter[T] {
    type Item = T;
    fn next(mut ref self) -> Option[T] {
        if self.pos >= self.items.len() {
            return None;
        }
        self.pos += 1;
        Some(self.items[self.pos - 1].clone())
    }
}

trait Shout {
    fn shout(self) -> String;
}
impl Shout for String {
    fn shout(self) -> String { self.to_uppercase() }
}
fn loudest[I: Iterator](it: own I) -> String where I.Item: Shout {
    let mut it = it;
    let mut out = "".to_string();
    while let Some(x) = it.next() {
        out = out + x.shout();
    }
    out
}
fn main() {
    println(loudest(VecIter { items: vec!["a".to_string(), "b".to_string()], pos: 0 }));
}
"#;
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, "AB\n");
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, "AB\n", "at -O{opt}");
        }
    }
}

/// `karac build --mir` compiles for the CPU baseline `karac build` does
/// (`KARAC_TARGET_CPU`, then the manifest, then `v3`), not LLVM's bare
/// `x86-64`: a bogus `KARAC_TARGET_CPU` is refused by name, and a valid
/// one builds a binary that runs.
#[test]
fn mir_build_takes_the_cpu_baseline_of_karac_build() {
    let dir = Scratch::new();
    let src = dir.path().join("cpu.kara");
    std::fs::write(&src, PROGRAM).unwrap();
    let exe = dir.path().join("cpu");
    let build = |cpu: &str| {
        Command::new(env!("CARGO_BIN_EXE_karac"))
            .args([
                "build",
                "--mir",
                src.to_str().unwrap(),
                "-o",
                exe.to_str().unwrap(),
            ])
            .env("KARAC_TARGET_CPU", cpu)
            .output()
            .unwrap()
    };
    let bad = build("mir-bogus-cpu");
    assert!(!bad.status.success());
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(stderr.contains("unknown CPU 'mir-bogus-cpu'"), "{stderr}");
    let cpu = if cfg!(target_arch = "x86_64") {
        "x86-64"
    } else {
        "generic"
    };
    let good = build(cpu);
    let stderr = String::from_utf8_lossy(&good.stderr);
    if common::link_or_skip(if good.status.success() {
        Ok(())
    } else {
        Err(stderr.to_string())
    })
    .is_none()
    {
        return;
    }
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hello 30 5 p3\n");
}

#[test]
fn mir_an_empty_vec_in_a_generic_struct_literal_takes_the_calls_type() {
    // B-2026-10-19-9: `hold(Bag { items: Vec.new(), n: 0 })` into a
    // `Holder[Bag[String]]` checked, and MIR stopped on the `Vec.new()` with
    // an unknown `T`: the literal's own `T` came back from the field and
    // fixed it as `Bag[T]` before the call could say `String`. The element
    // now comes from whatever decides it: the call's expected type, a later
    // argument, or a push.
    let src = r#"
struct Bag[T] { items: Vec[T], n: i64 }
struct Holder[I] { v: I }
fn hold[I](x: own I) -> Holder[I] { Holder { v: x } }
fn first[I](a: own I, b: own I) -> I { a }
fn main() {
    let h: Holder[Bag[String]] = hold(Bag { items: Vec.new(), n: 0 });
    println(f"{h.v.items.len()} {h.v.n}");
    let mut v: Vec[String] = Vec.new();
    v.push("x");
    let b = first(Bag { items: Vec.new(), n: 4 }, Bag { items: v, n: 5 });
    println(f"{b.items.len()} {b.n}");
    let mut c = Bag { items: Vec.new(), n: 2 };
    c.items.push("y");
    println(f"{c.items.len()} {c.items[0]} {c.n}");
}
"#;
    let want = "0 0\n0 4\n1 y 2\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// The `checked_` / `wrapping_` / `saturating_` / `overflowing_` `div`, `rem`,
/// `neg`, `shl`, `shr` and `pow` methods and `x.total_cmp(y)`, at 8, 64 and
/// 128 bits, signed and unsigned, on both backends (B-2026-10-19-11). The
/// expected output is what Rust's methods of those names print for the same
/// operands.
#[test]
fn mir_integer_overflow_families() {
    const SRC: &str = r#"fn o(x: Ordering) -> String {
    match x {
        Ordering.Less => "Less",
        Ordering.Equal => "Equal",
        Ordering.Greater => "Greater",
    }
}

fn main() {
    let a: i8 = -128;
    let b: i8 = 7;
    let z: i8 = 0;
    let m: i8 = -1;
    println(f"{a.checked_div(m).is_none()} {b.checked_div(z).is_none()} {a.checked_div(b).unwrap()} {a.checked_rem(m).is_none()} {b.checked_rem(2).unwrap()}");
    println(f"{a.wrapping_div(m)} {a.wrapping_rem(m)} {a.wrapping_neg()} {b.wrapping_neg()} {b.wrapping_shl(9)} {a.wrapping_shr(9)} {b.wrapping_pow(3)}");
    println(f"{a.saturating_div(m)} {a.saturating_neg()} {b.saturating_pow(3)} {a.saturating_pow(3)} {m.saturating_pow(3)}");
    println(f"{a.overflowing_div(m)} {a.overflowing_rem(m)} {a.overflowing_neg()} {b.overflowing_shl(8)} {b.overflowing_shr(3)} {b.overflowing_pow(3)}");
    println(f"{a.checked_neg().is_none()} {b.checked_shl(8).is_none()} {b.checked_shl(6).unwrap()} {a.checked_shr(7).unwrap()} {b.checked_pow(2).unwrap()} {b.checked_pow(3).is_none()}");
    let u: u8 = 200;
    let w: u8 = 3;
    println(f"{u.checked_neg().is_none()} {u.wrapping_neg()} {u.overflowing_neg()} {u.wrapping_div(w)} {u.wrapping_rem(w)} {u.saturating_pow(2)} {w.wrapping_pow(6)} {u.wrapping_shr(10)} {u.overflowing_shl(1)}");
    let n: i64 = -9223372036854775807 - 1;
    println(f"{n.checked_div(-1).is_none()} {n.wrapping_div(-1)} {n.saturating_div(-1)} {n.overflowing_pow(2)} {n.wrapping_neg()} {n.checked_pow(1).unwrap()} {(-2i64).checked_pow(63).unwrap()} {3i64.wrapping_pow(41)}");
    let q: u64 = 18446744073709551615;
    println(f"{q.wrapping_pow(2)} {q.overflowing_neg()} {q.saturating_pow(2)} {q.wrapping_shl(65)}");
    let x: i128 = 170141183460469231731687303715884105727;
    println(f"{x.checked_pow(2).is_none()} {x.wrapping_pow(2)} {x.wrapping_neg()} {x.saturating_pow(3)} {(-x - 1).saturating_div(-1)}");
    let f: f64 = 1.5;
    println(f"{o(f.total_cmp(2.0))} {o(f.total_cmp(1.5))} {o((-0.0f64).total_cmp(0.0))}");
}
"#;
    let want = "true true -18 true 1\n-128 0 -128 -7 14 -64 87\n127 127 127 -128 -1\n(-128, true) (0, true) (-128, true) (7, true) (0, false) (87, true)\ntrue true -64 -1 49 true\ntrue 56 (56, true) 66 2 255 217 50 (144, false)\ntrue -9223372036854775808 9223372036854775807 (0, true) -9223372036854775808 -9223372036854775808 -9223372036854775808 -420491770248316829\n1 (1, true) 18446744073709551615 18446744073709551614\ntrue 1 -170141183460469231731687303715884105727 170141183460469231731687303715884105727 170141183460469231731687303715884105727\nLess Equal Less\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A zero divisor panics in the `wrapping_` family, with the same message on
/// both backends; `checked_div` gives `None` instead (B-2026-10-19-11).
#[test]
fn mir_wrapping_div_by_zero_panics() {
    let src = "fn main() {\n    let z: i32 = 0;\n    \
               println(f\"{7i32.checked_div(z).is_none()}\");\n    \
               println(f\"{7i32.wrapping_div(z)}\");\n}\n";
    let want = "runtime error: division by zero\n";
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
    assert_eq!(interp.status.code(), Some(101), "__mir-run");
    assert_eq!(
        String::from_utf8_lossy(&interp.stdout),
        "true\n",
        "__mir-run"
    );
    assert_eq!(first(&interp.stderr), want, "__mir-run");
    let Some(exe) = cli_build(&dir, src, &[]) else {
        return;
    };
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(run.status.code(), Some(101));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "true\n");
    assert_eq!(first(&run.stderr), want, "compiled");
}

/// `later - earlier` is a `Duration` and `i + d` an `Instant`, through the
/// library's `impl Sub for Instant` and `impl Add[Duration] for Instant`,
/// (B-2026-10-15-112).
#[test]
fn mir_std_time_instant_operators() {
    const SRC: &str = r#"import std.time.{Instant, Duration};

fn main() {
    let a = Instant.now();
    let b = a + Duration.ms(1500);
    let d = b - a;
    let mut t = a;
    t += Duration.secs(2);
    println(f"{d.as_ms()} {b > a} {a == a} {(t - a).as_ms()} {(t - b).as_ms()}");
    let z = a - a;
    println(f"{z.as_ms()} {a + z == a}");
}
"#;
    let want = "1500 true true 2000 500\n0 true\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// `{v:N}` and `{v:0N}` on integers take the runtime's fast path; the
/// text is the general `FormatSpec`'s, on both backends, for negatives,
/// widths narrower than the number and unsigned extremes.
#[test]
fn mir_build_padded_integers_agree_with_the_interpreter() {
    let src = r#"
fn main() {
    let xs: Vec[i64] = vec![0, 7, -7, 42, -12345, 9223372036854775807];
    for x in xs.iter() {
        println(f"[{x:5}] [{x:05}] [{x:2}] [{x:02}] [{x:0}]");
    }
    let u: u64 = 18446744073709551615;
    let z: u8 = 3;
    println(f"[{u:03}] [{z:03}] [{z:3}] [{-3:010}]");
}
"#;
    let want = karac::mir::lower::run_source(src).expect("interp").output;
    assert_eq!(
        want,
        "[    0] [00000] [ 0] [00] [0]\n\
         [    7] [00007] [ 7] [07] [7]\n\
         [   -7] [-0007] [-7] [-7] [-7]\n\
         [   42] [00042] [42] [42] [42]\n\
         [-12345] [-12345] [-12345] [-12345] [-12345]\n\
         [9223372036854775807] [9223372036854775807] [9223372036854775807] [9223372036854775807] [9223372036854775807]\n\
         [18446744073709551615] [003] [  3] [-000000003]\n"
    );
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_a_value_fallback_of_an_optional_reference_is_borrowed() {
    // B-2026-10-18-53: `opt ?? ""` with `opt: Option[ref String]` is a
    // `ref String`, and the literal fallback was stored where the reference
    // belongs, so a `None` crashed at the first read on every MIR surface
    // ("deref of non-reference" on the interpreter, SEGV at 0x8 when built).
    // The fallback is now held in a temporary the result borrows, which a
    // `let` keeps to the end of its block.
    let src = r#"
fn count(tops: ref Map[String, String], key: String) -> i64 {
    let choices = tops.get(key) ?? "";
    let mut n = 0;
    for c in choices.chars() {
        n += 1;
    }
    n
}
fn show(o: Option[ref String]) {
    println(f"<{o ?? "zz"}>");
}
fn main() {
    let none: Option[ref String] = None;
    let s = none ?? "";
    println(f"[{s}] {s.len()}");
    let mut tops: Map[String, String] = Map.new();
    tops.insert("AB", "CD");
    println(f"{count(tops, "AB")} {count(tops, "XY")}");
    let mut i = 0;
    while i < 2 {
        let t = tops.get("Q") ?? "n";
        println(f"{t}{i}");
        i += 1;
    }
    show(None);
    let w = "w".to_string();
    show(Some(w));
}
"#;
    let want = "[] 0\n2 0\nn0\nn1\n<zz>\n<w>\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// A `u128` past `i128::MAX` orders unsigned on the interpreter, as on the
/// compiled backend: `sort`, `sort_by_key`, a tuple sort, `is_sorted`,
/// `binary_search`, a `SortedSet`, `max` / `min`, and the checked,
/// saturating, overflowing and `abs_diff` methods. The expected output is
/// what Rust prints for the same program (B-2026-10-19-22).
#[test]
fn mir_u128_orders_unsigned() {
    const SRC: &str = r#"fn main() {
    let big: u128 = 1u128 << 127;
    let one: u128 = 1;
    let mut v: Vec[u128] = vec![5, big, 1];
    v.sort();
    println(f"{v[0]} {v[1]} {v[2]} {v.is_sorted()} {v.binary_search(big).unwrap()}");
    let mut w: Vec[u128] = vec![5, big, 1];
    w.sort_by_key(|k| k);
    let mut t: Vec[(u128, i64)] = vec![(big, 1), (3, 2)];
    t.sort();
    println(f"{w[0]} {w[2]} {t[0].0} {t[1].0}");
    let mut s: SortedSet[u128] = SortedSet.new();
    s.insert(big);
    s.insert(7);
    s.insert(big + 1);
    for x in s.iter() {
        print(f"{x} ");
    }
    println("");
    println(f"{big.max(5)} {big.min(5)} {v.iter().max().unwrap()} {v.iter().min().unwrap()}");
    println(f"{big.checked_add(one).unwrap()} {big.checked_sub(one).unwrap()} {big.checked_mul(2).is_none()} {big.saturating_add(big)} {one.saturating_sub(big)}");
    println(f"{big.abs_diff(one)} {one.abs_diff(big)} {big.overflowing_add(big)} {big.overflowing_mul(3)}");
    println(f"{big.checked_div(2).unwrap()} {big.wrapping_shl(1)} {big.overflowing_neg()} {big.saturating_pow(2)} {(3u128).wrapping_pow(81)}");
}
"#;
    let want = "1 5 170141183460469231731687303715884105728 true 2\n1 170141183460469231731687303715884105728 3 170141183460469231731687303715884105728\n7 170141183460469231731687303715884105728 170141183460469231731687303715884105729 \n170141183460469231731687303715884105728 5 170141183460469231731687303715884105728 1\n170141183460469231731687303715884105729 170141183460469231731687303715884105727 true 340282366920938463463374607431768211455 0\n170141183460469231731687303715884105727 170141183460469231731687303715884105727 (0, true) (170141183460469231731687303715884105728, true)\n85070591730234615865843651857942052864 0 (170141183460469231731687303715884105728, true) 340282366920938463463374607431768211455 103144121322099306484875023187381681347\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// `pow` at 128 bits on the interpreter: a `u128` is raised as a `u128`
/// (`2u128.pow(127)` was `i128::MAX`), and a result past 128 bits panics
/// with "integer overflow" as `pow` at 64 bits does, rather than printing
/// `i128::MAX`. The first line is what Rust prints (B-2026-10-19-28). The
/// compiled backend does not lower 128-bit `pow` yet (B-2026-10-19-27).
#[test]
fn mir_u128_pow_overflow_panics_on_the_interpreter() {
    let src = "fn main() {\n    let t: u128 = 3;\n    let s: i128 = -3;\n    \
               println(f\"{t.pow(80)} {s.pow(79)} {(2u128).pow(127)} {(-2i128).pow(127)}\");\n    \
               let x: u128 = 1 << 100;\n    println(f\"{x.pow(2)}\");\n}\n";
    let dir = Scratch::new();
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(101));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "147808829414345923316083210206383297601 -49269609804781974438694403402127765867 \
         170141183460469231731687303715884105728 -170141183460469231731687303715884105728\n"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.starts_with("runtime error: integer overflow\n"),
        "{err}"
    );
}

#[test]
fn mir_an_impl_listing_its_parameters_in_another_order_takes_them_by_name() {
    // B-2026-10-18-48: `impl[T: Clone, I: Iterator] Iterator for Tagged[I, T]`
    // built `self` as `Tagged[T, I]`, the impl's parameters put into the
    // target by position, so `self.tag.clone()` was lowered at
    // `T = VecIter[i64]` ("clone of VecIter[i64] needs its Clone body").
    // An impl's arguments now go where its target writes each parameter;
    // the same holds for an enum and for a method that returns the target
    // with its arguments swapped.
    let src = r#"
struct VecIter[T] {
    items: Vec[T],
    pos: i64,
}

impl[T: Clone] Iterator for VecIter[T] {
    type Item = T;
    fn next(mut ref self) -> Option[T] {
        if self.pos >= self.items.len() {
            return None;
        }
        self.pos += 1;
        Some(self.items[self.pos - 1].clone())
    }
}

struct Tagged[I, T] { it: I, tag: T }

impl[T: Clone, I: Iterator] Iterator for Tagged[I, T] {
    type Item = (T, I.Item);
    fn next(mut ref self) -> Option[(T, I.Item)] {
        match self.it.next() {
            Some(x) => Some((self.tag.clone(), x)),
            None => None,
        }
    }
}

fn main() {
    let mut t = Tagged { it: VecIter { items: vec![1, 2], pos: 0 }, tag: "a".to_string() };
    while let Some((s, x)) = t.next() {
        println(f"{s} {x}");
    }
}
"#;
    let want = "a 1\na 2\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
    let src = r#"
enum Either[L, R] { Left(L), Right(R) }

impl[R: Clone, L: Clone] Either[L, R] {
    fn right_or(self, d: own R) -> R {
        match self {
            Either.Left(_) => d,
            Either.Right(r) => r.clone(),
        }
    }
    fn flip(self) -> Either[R, L] {
        match self {
            Either.Left(l) => Either.Right(l.clone()),
            Either.Right(r) => Either.Left(r.clone()),
        }
    }
}

struct Pair[A, B] { a: A, b: B }

impl[B: Clone, A: Clone] Pair[A, B] {
    fn second(self) -> B { self.b.clone() }
    fn swap(self) -> Pair[B, A] { Pair { a: self.b.clone(), b: self.a.clone() } }
}

fn main() {
    let e: Either[i64, String] = Either.Right("r".to_string());
    println(e.right_or("d".to_string()));
    let l: Either[i64, String] = Either.Left(4);
    println(l.right_or("d".to_string()));
    match l.flip() {
        Either.Left(s) => println(f"L {s}"),
        Either.Right(n) => println(f"R {n}"),
    }
    let p = Pair { a: 1, b: "two".to_string() };
    println(p.second());
    let q = p.swap();
    println(f"{q.a} {q.b}");
}
"#;
    let want = "r\nd\nR 4\ntwo\ntwo 1\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// `?` is postfix, so a tuple index straight after it reads the
/// propagated value: `h()?.1.0` is `(h()?).1.0`, on a `Result` and an
/// `Option`, and a `None` returns early (B-2026-10-19-30). It was a parse
/// error, "Expected identifier, found Integer(0, None)".
#[test]
fn mir_question_then_tuple_index() {
    const SRC: &str = r#"fn h() -> Result[(i64, (i64, i64)), String] { Ok((4, (5, 6))) }
fn k() -> Option[(String, i64)] { Some(("ab".to_string(), 2)) }
fn run() -> Result[i64, String] {
    let y = h()?.1.0;
    let z = h()?.0;
    Ok(y * 10 + z)
}
fn opt() -> Option[i64] {
    let n = k()?.1;
    let w = k()?.0.len();
    Some(n * 10 + w as i64)
}
fn none() -> Option[i64] {
    let p: Option[(i64, i64)] = None;
    let n = p?.0;
    Some(n + 100)
}
fn main() {
    match run() { Ok(v) => println(f"{v}"), Err(e) => println(e) }
    println(f"{opt().unwrap_or(-1)} {none().is_none()}");
}
"#;
    let want = "54\n22 true\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_a_function_typed_field_over_a_projection_is_normalized() {
    // B-2026-10-18-49: `f: Fn(own I.Item) -> I.Item` in a struct over
    // `I: Iterator` kept `VecIter[i64].Item` unnormalized in the field's
    // instance type (the projection search did not look inside function
    // types), so building the struct cast the closure to it: invalid MIR.
    let src = r#"
struct VecIter[T] {
    items: Vec[T],
    pos: i64,
}

impl[T: Clone] Iterator for VecIter[T] {
    type Item = T;
    fn next(mut ref self) -> Option[T] {
        if self.pos >= self.items.len() {
            return None;
        }
        self.pos += 1;
        Some(self.items[self.pos - 1].clone())
    }
}

struct MapIt[I: Iterator] {
    it: I,
    f: Fn(own I.Item) -> I.Item,
}

impl[I: Iterator] Iterator for MapIt[I] {
    type Item = I.Item;
    fn next(mut ref self) -> Option[I.Item] {
        let x = self.it.next()?;
        Some((self.f)(x))
    }
}

fn main() {
    let v: Vec[i64] = vec![1, 2, 3];
    let mut m = MapIt { it: VecIter { items: v, pos: 0 }, f: |x: i64| x * 10 };
    while let Some(x) = m.next() {
        println(f"{x}");
    }
}
"#;
    let want = "10\n20\n30\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
    // A predicate over `ref I.Item` beside an `Option[I.Item]` field.
    let src = r#"
struct VecIter[T] {
    items: Vec[T],
    pos: i64,
}

impl[T: Clone] Iterator for VecIter[T] {
    type Item = T;
    fn next(mut ref self) -> Option[T] {
        if self.pos >= self.items.len() {
            return None;
        }
        self.pos += 1;
        Some(self.items[self.pos - 1].clone())
    }
}

struct Filt[I: Iterator] {
    it: I,
    keep: Fn(ref I.Item) -> bool,
    last: Option[I.Item],
}

impl[I: Iterator] Iterator for Filt[I] {
    type Item = I.Item;
    fn next(mut ref self) -> Option[I.Item] {
        while let Some(x) = self.it.next() {
            if (self.keep)(x) {
                return Some(x);
            }
        }
        None
    }
}

fn main() {
    let words: Vec[String] = vec!["a".to_string(), "bb".to_string(), "ccc".to_string()];
    let mut f = Filt { it: VecIter { items: words, pos: 0 }, keep: |s: ref String| s.len() != 2, last: None };
    while let Some(w) = f.next() {
        println(w);
    }
    let mut g = Filt { it: VecIter { items: vec![1, 2, 3, 4], pos: 0 }, keep: |n: ref i64| *n % 2 == 0, last: None };
    while let Some(n) = g.next() {
        println(f"{n}");
    }
}
"#;
    let want = "a\nccc\n2\n4\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// `get_mut` on a `Map` or `SortedMap` takes the key's view, as `get` does:
/// a `Str` for a `String` key (a literal, a `Str` parameter, `as_str()` of
/// a temporary that reads the map) and a `Slice` for a `Vec` key, with an
/// owned key still the library's `get_mut`. The compiled backend looked the
/// view up as if it were a `String` (B-2026-10-18-39).
#[test]
fn mir_get_mut_takes_the_keys_view() {
    const SRC: &str = r#"fn bump(m: mut ref Map[String, i64], k: Str) {
    if let Some(v) = m.get_mut(k) {
        *v = *v + 1;
    }
}

fn add(m: mut ref SortedMap[String, Vec[i64]], k: Str, x: i64) {
    match m.get_mut(k) {
        Some(v) => v.push(x),
        None => {}
    }
}

fn first(m: Map[String, i64]) -> String {
    "a".to_string()
}

fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("a".to_string(), 1);
    bump(mut m, "a");
    bump(mut m, "zz");
    let s = "a".to_string();
    bump(mut m, s.as_str());
    if let Some(v) = m.get_mut(first(m).as_str()) {
        *v += 40;
    }
    let missing = m.get_mut("q").is_none();
    println(f"{m.get("a").unwrap_or(0)} {m.len()} {missing}");
    let mut sm: SortedMap[String, Vec[i64]] = SortedMap.new();
    sm.insert("k".to_string(), vec![1]);
    add(mut sm, "k", 2);
    add(mut sm, "j", 3);
    println(f"{sm.get("k").unwrap().len()} {sm.len()}");
    let mut vm: Map[Vec[i64], i64] = Map.new();
    vm.insert(vec![1, 2], 10);
    let w: Vec[i64] = vec![0, 1, 2];
    if let Some(n) = vm.get_mut(w[1..3]) {
        *n += 5;
    }
    let mut own: Map[String, i64] = Map.new();
    own.insert("o".to_string(), 7);
    if let Some(n) = own.get_mut("o".to_string()) {
        *n *= 2;
    }
    println(f"{vm.get(w[1..3]).unwrap_or(0)} {own.get("o").unwrap_or(0)}");
}
"#;
    let want = "43 1 true\n2 1\n15 14\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_a_none_borrowed_for_a_ref_option_parameter_is_option_none() {
    // B-2026-10-19-31: a user enum with a `None` variant made the bare
    // `None` handed to a `ref Option[T]` parameter build `Option.Some {}`.
    let src = r#"
enum V {
    None,
    Int(i64),
}

fn f(x: ref Option[i64]) -> i64 {
    match x {
        Some(n) => *n,
        None => -1,
    }
}

fn name(x: ref Option[String]) -> String {
    match x {
        Some(s) => s.clone(),
        None => "none".to_string(),
    }
}

fn v(x: ref V) -> i64 {
    match x {
        V.None => 0,
        V.Int(n) => *n,
    }
}

fn main() {
    println(f"{f(None)} {f(Some(4))}");
    println(name(None));
    println(name(Some("w".to_string())));
    println(f"{v(V.None)} {v(V.Int(7))}");
}
"#;
    let want = "-1 4\nnone\nw\n0 7\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// `cmp`, `partial_cmp` and the operator traits are registered for `i128`
/// and `u128` as for the other widths, so a 128-bit value calls `cmp` and
/// meets an `Ord` or `Add` bound, with a `u128` past `i128::MAX` ordered
/// unsigned (B-2026-10-19-71).
#[test]
fn mir_i128_u128_have_the_integer_traits() {
    const SRC: &str = r#"fn o(x: Ordering) -> Str {
    match x { Ordering.Less => "lt", Ordering.Equal => "eq", Ordering.Greater => "gt" }
}
fn biggest[T: Ord + Copy](a: T, b: T) -> T {
    if a > b { a } else { b }
}
fn total[T: Add[Output = T] + Copy](a: T, b: T) -> T {
    a + b
}
fn main() {
    let a: u128 = 1u128 << 127;
    let b: u128 = 5;
    let c: i128 = -3;
    println(f"{o(a.cmp(b))} {o(c.cmp(2))} {a.partial_cmp(b).is_some()} {o(b.cmp(b))}");
    println(f"{biggest(a, b)} {biggest(c, 7)} {total(a, b)} {total(c, -4)}");
    let mut v: Vec[u128] = vec![b, a, 1];
    v.sort_by(|x, y| x.cmp(y));
    println(f"{v[0]} {v[2]}");
}
"#;
    let want = "gt lt true eq\n170141183460469231731687303715884105728 7 170141183460469231731687303715884105733 -7\n1 170141183460469231731687303715884105728\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_atomics_take_no_ordering() {
    // B-2026-10-19-67: every atomic operation is sequentially consistent and
    // takes no ordering; the deprecated trailing `MemoryOrdering` still runs.
    const SRC: &str = r#"sync struct Counter { count: Atomic[i64] }
fn bump(c: ref Counter) { let _ = c.count.fetch_add(1); }
fn main() {
    let c = Counter { count: Atomic.new(0) };
    bump(c);
    c.count.fetch_add(4);
    let old = c.count.swap(9);
    let hit = c.count.compare_exchange(9, 11);
    let miss = c.count.compare_exchange(9, 12);
    c.count.fetch_sub(1, MemoryOrdering.Relaxed);
    let f = Atomic.new(false);
    let was = f.swap(true);
    println(f"{old} {c.count.load()} {hit.is_ok()} {miss.is_err()} {was} {f.load(MemoryOrdering.Acquire)}");
}
"#;
    let want = "5 10 true true false true\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_a_closure_through_a_mut_ref_function_parameter_keeps_its_writes() {
    // B-2026-10-19-68: a `mut ref MutFn()` parameter is specialised to the
    // closure it is handed, which it takes by the reference its body wants.
    for (src, want) in [
        (
            r#"fn twice(f: mut ref MutFn()) {
    f();
    f();
}

fn main() {
    let mut n = 0;
    twice(mut || { n += 1; });
    println(f"{n}");
}
"#,
            "2\n",
        ),
        (
            r#"struct Acc {
    total: i64,
}

impl Acc {
    fn run(mut ref self, f: mut ref MutFn(i64) -> i64) {
        self.total += f(self.total);
    }
}

fn twice(f: mut ref MutFn()) {
    f();
    f();
}

fn thrice(f: mut ref MutFn()) {
    twice(mut f);
    f();
}

fn sum(f: ref Fn(i64) -> i64) -> i64 {
    f(1) + f(2)
}

fn main() {
    let mut count = 0;
    let mut c = || { count += 1; };
    twice(mut c);
    thrice(mut c);
    c();
    println(f"{count}");

    let mut v: Vec[i64] = Vec.new();
    twice(mut || { v.push(v.len() as i64); });
    println(f"{v.len()} {v[1]}");

    let k = 10;
    let g = |x: i64| x * k;
    println(f"{sum(g)} {sum(|x: i64| x + k)}");

    let mut calls = 0;
    let mut h: MutFn() = || { calls += 1; };
    twice(mut h);
    h();
    println(f"{calls}");

    let mut a = Acc { total: 1 };
    let mut seen = 0;
    a.run(mut |t: i64| { seen += t; t * 2 });
    a.run(mut |t: i64| { seen += t; t * 2 });
    println(f"{a.total} {seen}");
}
"#,
            "6\n2 1\n30 23\n3\n9 4\n",
        ),
    ] {
        let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
        assert_eq!(interp.output, want);
        for opt in [0, 2] {
            if let Some(out) = build_and_run(src, opt) {
                assert_eq!(out, want, "at -O{opt}");
            }
        }
    }
}

#[test]
fn mir_interp_reads_an_array_local_in_place() {
    // B-2026-10-19-70: `Len` of an `Array` local copied the whole array, so
    // a loop over `[false; 100001]` was quadratic (about 100 s). It runs in
    // under a second now; the bound only catches the quadratic.
    let src = r#"fn main() {
    let mut seen = [false; 100001];
    let mut n = 0;
    for i in 0..100001 {
        if i % 7 == 0 {
            seen[i] = true;
        }
    }
    for i in 0..100001 {
        if seen[i] {
            n += 1;
        }
    }
    println(f"{n} {seen.len()}");
}
"#;
    let start = std::time::Instant::now();
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, "14286 100001\n");
    let took = start.elapsed();
    assert!(took.as_secs() < 30, "took {took:?}");
}

/// B-2026-10-19-53: a nested index read evaluates the base's index first,
/// `v[p(1)][p(0)]` prints `p1 p0`, as the same place does as an assignment
/// target (core semantics §8.1), on the interpreter and compiled alike.
#[test]
fn mir_nested_index_reads_left_to_right() {
    let cases = [
        (
            r#"fn p(x: i64) -> i64 { println(f"p{x}"); x }
fn main() {
    let t = (p(1), p(2));
    let a = [p(3), p(4)];
    let v: Vec[Vec[i64]] = [[10, 20], [30, 40]];
    let e = v[p(1)][p(0)];
    println(f"{p(5)} {p(6)}");
    println(f"{t.0} {a[1]} {e}");
}
"#,
            "p1\np2\np3\np4\np1\np0\np5\np6\n5 6\n1 4 30\n",
        ),
        (
            r#"fn p(x: i64) -> i64 { println(f"p{x}"); x }
fn main() {
    let mut w: Vec[Vec[i64]] = [[10, 20], [30, 40]];
    w[p(1)][p(0)] = p(7);
    w[p(0)][p(1)] += p(2);
    let a: Array[Array[i64, 2], 2] = [[1, 2], [3, 4]];
    let e = a[p(1)][p(0)] + w[p(1)][p(0)] + w[p(0)][p(1)];
    println(f"{e}");
}
"#,
            "p1\np0\np7\np0\np1\np2\np1\np0\np1\np0\np0\np1\n32\n",
        ),
    ];
    for (src, want) in cases {
        let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
        assert_eq!(interp.output, want, "__mir-run");
        let dir = Scratch::new();
        let Some(exe) = cli_build(&dir, src, &[]) else {
            continue;
        };
        let run = Command::new(&exe).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&run.stdout), want, "compiled");
    }
}

/// B-2026-10-19-52: a `let ... else` initializer's temporaries drop before
/// the else block runs, and at the `;` when the pattern matches (core
/// semantics §7.4), on the interpreter and compiled alike.
#[test]
fn mir_let_else_temporaries_drop_before_the_else_block() {
    let cases = [
        (
            r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}"); } }
fn make(id: i64) -> R { R { id: id } }
fn find(r: R) -> Option[i64] { if r.id > 5 { Some(r.id) } else { None } }
fn f(id: i64) {
    let Some(v) = find(make(id)) else {
        println("else");
        return;
    };
    println(f"got{v}");
}
fn main() {
    f(1);
    f(9);
}
"#,
            "d1\nelse\nd9\ngot9\n",
        ),
        (
            r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}{self.s}"); } }
fn make(id: i64) -> R { R { id: id, s: f"s{id}" } }
fn find(r: R) -> Option[String] { if r.id > 5 { Some(r.s.clone()) } else { None } }
fn pick(v: ref Vec[String], i: i64) -> Option[String] { if i < v.len() { Some(v[i].clone()) } else { None } }
fn main() {
    let names: Vec[String] = ["a".to_string(), "b".to_string()];
    for i in 0..4 {
        let Some(v) = find(make(i * 3)) else {
            println("skip");
            continue;
        };
        println(f"got {v}");
        let Some(n) = pick(names, i) else {
            println("no name");
            break;
        };
        println(f"name {n}");
    }
}
"#,
            "d0s0\nskip\nd3s3\nskip\nd6s6\ngot s6\nno name\n",
        ),
    ];
    for (src, want) in cases {
        let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
        assert_eq!(interp.output, want, "__mir-run");
        let dir = Scratch::new();
        let Some(exe) = cli_build(&dir, src, &[]) else {
            continue;
        };
        let run = Command::new(&exe).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&run.stdout), want, "compiled");
    }
}

/// B-2026-10-19-54: a `?` that fails in one `par` branch cancels no other
/// branch; every branch runs and the block fails with the source-earliest
/// error (core semantics §11.7). Nested blocks, `String` errors dropped by a
/// later failing branch, and `Option`, on the interpreter and compiled alike.
#[test]
fn mir_question_in_a_par_branch_runs_the_other_branches() {
    let cases = [
        (
            r#"enum E { Bad(i64) }
fn fail(n: i64) -> Result[i64, E] { Err(E.Bad(n)) }
fn work() -> Result[i64, E] {
    println("work done");
    Ok(7)
}
fn one() -> Result[i64, E] {
    let (x, y) = par { fail(1)?, work()? };
    Ok(x + y)
}
fn two() -> Result[i64, E] {
    let (x, y) = par { fail(1)?, fail(2)? };
    Ok(x + y)
}
fn show(r: Result[i64, E]) {
    match r {
        Ok(v) => println(f"ok {v}"),
        Err(E.Bad(n)) => println(f"err {n}"),
    }
}
fn main() {
    show(one());
    show(two());
}
"#,
            "work done\nerr 1\nerr 1\n",
        ),
        (
            r#"fn fail(n: i64) -> Result[String, String] { Err(f"e{n}") }
fn work(n: i64) -> Result[String, String] {
    Ok(f"v{n}")
}
fn one() -> Result[String, String] {
    let (x, y, z) = par { work(1)?, fail(2)?, fail(3)? };
    Ok(f"{x}{y}{z}")
}
fn nested() -> Result[String, String] {
    let (x, y) = par {
        {
            let (a, b) = par { fail(4)?, work(5)? };
            f"{a}{b}"
        },
        work(6)?,
    };
    Ok(f"{x}{y}")
}
fn fine() -> Result[String, String] {
    let (x, y) = par { work(9)?, work(10)? };
    Ok(f"{x}{y}")
}
fn show(r: Result[String, String]) {
    match r {
        Ok(v) => println(f"ok {v}"),
        Err(e) => println(f"err {e}"),
    }
}
fn main() {
    show(one());
    show(nested());
    show(fine());
}
"#,
            "err e2\nerr e4\nok v9v10\n",
        ),
        (
            r#"fn none(n: i64) -> Option[i64] { if n > 0 { None } else { Some(n) } }
fn some(n: i64) -> Option[i64] {
    println(f"some {n}");
    Some(n)
}
fn h(k: i64) -> Option[i64] {
    let (a, b) = par { none(k)?, some(5)? };
    Some(a + b)
}
fn main() {
    println(f"{h(1)} {h(0)}");
}
"#,
            "some 5\nsome 5\nNone Some(5)\n",
        ),
    ];
    for (src, want) in cases {
        let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
        assert_eq!(interp.output, want, "__mir-run");
        let dir = Scratch::new();
        let Some(exe) = cli_build(&dir, src, &[]) else {
            continue;
        };
        let run = Command::new(&exe).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&run.stdout), want, "compiled");
    }
}

/// B-2026-10-19-58: a generic `shared struct` keeps its type arguments, so
/// a `Slot[i64]`'s `T` fields lower; handles alias and count like a
/// non-generic shared struct's, on the interpreter and compiled alike.
#[test]
fn mir_generic_shared_struct_lowers() {
    let cases = [
        (
            r#"shared struct Slot[T] { mut items: Vec[T] }
fn put[T](s: Slot[T], x: own T) { s.items.push(x); }
fn main() {
    let s: Slot[i64] = Slot { items: Vec.new() };
    put(s, 5);
    println(f"{s.items.len()}");
}
"#,
            "1\n",
        ),
        (
            r#"shared struct Slot[T] { mut items: Vec[T], name: String }
impl[T] Slot[T] {
    fn add(self, x: own T) { self.items.push(x); }
    fn count(self) -> i64 { self.items.len() }
}
fn put[T](s: Slot[T], x: own T) { s.items.push(x); }
fn fill(s: Slot[String]) {
    s.add("b".to_string());
}
fn main() {
    let s: Slot[i64] = Slot { items: Vec.new(), name: "n".to_string() };
    put(s, 5);
    s.add(6);
    let t = s;
    t.add(7);
    println(f"{s.items.len()} {t.count()} {s.name}");
    let w: Slot[String] = Slot { items: Vec.new(), name: "w".to_string() };
    w.add("a".to_string());
    fill(w);
    let v: Vec[Slot[String]] = [w];
    println(f"{v[0].count()} {w.items[1]}");
}
"#,
            "3 3 n\n2 b\n",
        ),
    ];
    for (src, want) in cases {
        let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
        assert_eq!(interp.output, want, "__mir-run");
        let dir = Scratch::new();
        let Some(exe) = cli_build(&dir, src, &[]) else {
            continue;
        };
        let run = Command::new(&exe).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&run.stdout), want, "compiled");
    }
}

/// B-2026-10-20-19: a function item or closure stored in a generic `Fn(V) -> K` field at a `Copy` `V` is erased once, to the field's type, and the interpreter reads the lent argument out for a body that takes it by value.
#[test]
fn mir_function_into_a_lent_fn_field_erases_once() {
    let cases = [
        (
            r#"struct Keyed[V, K] { key: Fn(V) -> K }
fn neg(x: i64) -> i64 { -x }
fn main() { let k = Keyed { key: neg }; println(f"{(k.key)(7)}"); }
"#,
            "-7\n",
        ),
        (
            r#"fn bump(out: mut ref Vec[i64], xs: ref Vec[i64]) {
    xs.iter().for_each(|x| out.push(*x + 1));
}
struct Keyed[V, K] { key: Fn(V) -> K }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    bump(mut v, vec![1, 2]);
    println(f"{v.len()} {v[1]}");
    let k = Keyed { key: |x: i64| x * 2 };
    println(f"{(k.key)(21)}");
}
"#,
            "2 3\n42\n",
        ),
        (
            r#"fn apply_g[V, K](f: Fn(V) -> K, x: V) -> K { f(x) }
fn neg(x: i64) -> i64 { -x }
fn main() {
    println(f"{apply_g(neg, 4)}");
    println(f"{apply_g(|x: i64| x * 2, 5)}");
    let g: Fn(ref i64) -> i64 = neg;
    println(f"{g(3)}");
}
"#,
            "-4\n10\n-3\n",
        ),
    ];
    for (src, want) in cases {
        let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
        assert_eq!(interp.output, want, "__mir-run");
        let dir = Scratch::new();
        let Some(exe) = cli_build(&dir, src, &[]) else {
            continue;
        };
        let run = Command::new(&exe).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&run.stdout), want, "compiled");
    }
}

/// B-2026-10-20-21: a closure capturing a `ref` binding (a parameter, a `let .. = ref ..`, a `for` item) holds that reference, so `*key` in its body reads through it once.
#[test]
fn mir_closure_derefs_a_captured_reference() {
    let cases = [
        (
            r#"fn has(v: ref Vec[(String, i64)], key: ref String) -> bool {
    v.iter().any(|t| t.0 == *key)
}
fn main() {
    let v = vec![("a".to_string(), 1), ("b".to_string(), 2)];
    println(f"{has(v, "b".to_string())} {has(v, "c".to_string())}");
}
"#,
            "true false\n",
        ),
        (
            r#"fn main() {
    let k = "b".to_string();
    let key = ref k;
    let v = vec!["a".to_string(), "b".to_string()];
    println(f"{v.iter().any(|t| *t == *key)}");
    let pending = vec![("a".to_string(), 1), ("c".to_string(), 3)];
    let names = vec!["c".to_string(), "z".to_string()];
    for s in names.iter() {
        println(f"{pending.iter().any(|t| t.0 == *s)}");
    }
}
"#,
            "true\ntrue\nfalse\n",
        ),
    ];
    for (src, want) in cases {
        let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
        assert_eq!(interp.output, want, "__mir-run");
        let dir = Scratch::new();
        let Some(exe) = cli_build(&dir, src, &[]) else {
            continue;
        };
        let run = Command::new(&exe).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&run.stdout), want, "compiled");
    }
}

/// B-2026-10-19-57: an error `karac check` reports fails `karac build --mir`
/// too (core semantics §0), here a generic call that moves its argument
/// twice and a removed `own ||` capture prefix, which the MIR builder alone
/// accepted.
#[cfg(feature = "llvm")]
#[test]
fn karac_build_mir_refuses_what_check_refuses() {
    for (tag, src) in [
        (
            "generic-moves",
            r#"trait Show { fn show(own self) -> String; }
struct N { s: String }
impl Show for N { fn show(self) -> String { self.s.clone() } }
fn twice[T: Show](x: own T) -> String {
    let a = x.show();
    let b = x.show();
    a + b
}
fn main() {
    println(twice(N { s: "a".to_string() }));
}
"#,
        ),
        (
            "own-capture",
            r#"fn main() {
    let s = "a".to_string();
    let f = own || s.len();
    println(f"{f()}");
}
"#,
        ),
    ] {
        let dir = Scratch::new();
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
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{tag}: {stderr}");
        assert!(stderr.contains("1 error(s) found."), "{tag}: {stderr}");
        assert!(!exe.exists(), "{tag}: no executable is written");
    }
}

/// Twelve integer methods at 128 bits on both MIR backends, and
/// `swap_bytes`, `reverse_bits` and `next_power_of_two` at every width,
/// with a `u128` past `i128::MAX` read as unsigned (B-2026-10-19-27).
/// The values are checked against Rust's own methods.
#[test]
fn mir_integer_methods_run_at_128_bits() {
    let src = r#"
fn main() {
    let a: i128 = -170141183460469231731687303715884105728;
    let b: i128 = -5;
    println(b.clamp(-3, 7));
    println(a.clamp(-10, 10));
    println(b.leading_zeros());
    println(b.trailing_zeros());
    println(a.count_ones());
    println(b.count_zeros());
    println(b.pow(3));
    println(b.to_f64());
    println(b.swap_bytes());
    println(b.reverse_bits());
    let bb = b.to_ne_bytes();
    println(bb[0]);
    println(bb[15]);
    let m: u128 = 340282366920938463463374607431768211455u128;
    println(m.to_f64());
    println(m.count_ones());
    println(m.leading_zeros());
    let h: u128 = 170141183460469231731687303715884105728u128;
    println(h.is_power_of_two());
    println(h.next_power_of_two());
    let k: u128 = 170141183460469231731687303715884105727;
    println(k.next_power_of_two());
    let one: u128 = 1;
    println(one.next_power_of_two());
    let s: i32 = -2;
    println(s.swap_bytes());
    println(s.reverse_bits());
    let c: u8 = 0x81;
    println(c.swap_bytes());
    println(c.reverse_bits());
    let t: u16 = 0x1234;
    println(t.swap_bytes());
    let two: u128 = 2;
    println(two.pow(127));
}
"#;
    let want = "-3
-10
0
0
1
1
-125
-5.0
-5316911983139663491615228241121378305
-42535295865117307932921825928971026433
251
255
3.402823669209385e+38
128
0
true
170141183460469231731687303715884105728
170141183460469231731687303715884105728
1
-16777217
2147483647
129
129
13330
170141183460469231731687303715884105728
";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-20-24: an explicit `ref name` over an owned scrutinee borrows
/// its part on the MIR pipeline as in `karac check`, so `*name` reads
/// through it, in a `match` arm, an `if let` and a `let` alike.
#[test]
fn mir_explicit_ref_pattern_derefs() {
    let src = r#"struct P { name: String, n: i64 }
fn main() {
    let long = "b".to_string();
    let opt: Option[String] = Some("a".to_string());
    match opt {
        Some(ref g) => {
            if *g != long { println("differ"); }
        }
        None => {}
    }
    if let Some(ref g) = opt {
        println(f"{*g == "a"}");
    }
    let t = (1, 2);
    let (ref a, ref b) = t;
    println(f"{*a + *b}");
    let p = P { name: "pp".to_string(), n: 4 };
    let P { ref name, ref n } = p;
    println(f"{name} {*n * 2} {*name == "pp"}");
    let mut o: Option[i64] = Some(1);
    match o {
        Some(mut ref x) => { *x = *x + 10; }
        None => {}
    }
    println(f"{o.unwrap()} {p.name}");
}
"#;
    let want = "differ\ntrue\n3\npp 8 true\n11 pp\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want, "__mir-run");
    let dir = Scratch::new();
    let Some(exe) = cli_build(&dir, src, &[]) else {
        return;
    };
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), want, "compiled");
}

/// `#[panic_handler]` (core semantics §10.2, B-2026-10-19-59): after the
/// panic record, the handler runs once with the panic's `PanicInfo`, then
/// the process exits with code 101, on both backends. A panic inside the
/// handler ends the process at once, and a deadlock is a panic too.
#[test]
fn mir_panic_handler_runs_once_before_the_exit() {
    let cases = [
        (
            "#[panic_handler]\nfn on_panic(info: PanicInfo) {\n    \
             println(f\"handler {info.message}\");\n}\n\
             fn main() {\n    println(\"start\");\n    panic(\"boom\");\n}\n",
            "start\nhandler boom\n",
        ),
        (
            "#[panic_handler]\nfn on_panic(info: ref PanicInfo) {\n    \
             println(f\"{info.message} at {info.location}\");\n}\n\
             fn main() {\n    let v: Vec[i64] = Vec.new();\n    println(v[3]);\n}\n",
            "index out of bounds at {f}:7:13\n",
        ),
        (
            "#[panic_handler]\nfn on_panic(info: PanicInfo) {\n    println(\"once\");\n    \
             let o: Option[i64] = None;\n    println(o.unwrap());\n    println(\"never\");\n}\n\
             fn main() {\n    let x: i64 = 9223372036854775807;\n    println(x + 1);\n}\n",
            "once\n",
        ),
        (
            "#[panic_handler]\nfn on_panic(info: PanicInfo) {\n    \
             println(f\"[{info.message}] [{info.location}]\");\n}\n\
             fn main() {\n    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();\n    \
             let v = rx.recv();\n    println(v.is_some());\n    let _ = tx.send(1);\n}\n",
            "[deadlock: every task waits on a channel that no running task sends to or receives \
             from] []\n",
        ),
    ];
    for (src, want) in cases {
        let dir = Scratch::new();
        let file = dir.path().join("main.kara");
        std::fs::write(&file, src).unwrap();
        let want = want.replace("{f}", &file.to_string_lossy());
        let interp = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["__mir-run", file.to_str().unwrap()])
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&interp.stderr);
        assert_eq!(interp.status.code(), Some(101), "__mir-run: {err}");
        assert_eq!(
            String::from_utf8_lossy(&interp.stdout),
            want,
            "__mir-run: {err}"
        );
        for opt in ["0", "2"] {
            let Some(exe) = cli_build(&dir, src, &[("KARAC_MIR_OPT", opt)]) else {
                return;
            };
            let run = Command::new(&exe).output().unwrap();
            assert_eq!(run.status.code(), Some(101), "at -O{opt}");
            assert_eq!(String::from_utf8_lossy(&run.stdout), want, "at -O{opt}");
            // The panic record is written once, before the handler runs.
            assert_eq!(
                String::from_utf8_lossy(&run.stderr),
                String::from_utf8_lossy(&interp.stderr),
                "at -O{opt}"
            );
        }
    }
}

/// `karac check` takes one well-formed `#[panic_handler]` and refuses a
/// second, a wrong signature and a method, each with `E_PANIC_HANDLER`.
#[test]
fn panic_handler_placement_and_signature_are_checked() {
    let ok = "#[panic_handler]\nfn on_panic(info: PanicInfo) {\n    eprintln(info.message);\n}\n\
              fn main() {}\n";
    let bad = [
        "#[panic_handler]\nfn a(info: PanicInfo) {}\n#[panic_handler]\nfn b(info: PanicInfo) {}\n\
         fn main() {}\n",
        "#[panic_handler]\nfn a(code: i64) {}\nfn main() {}\n",
        "#[panic_handler]\nfn a(info: PanicInfo) -> i64 {\n    1\n}\nfn main() {}\n",
        "struct S {}\nimpl S {\n    #[panic_handler]\n    fn h(info: PanicInfo) {}\n}\n\
         fn main() {}\n",
    ];
    let check = |src: &str| {
        let dir = Scratch::new();
        let file = dir.path().join("main.kara");
        std::fs::write(&file, src).unwrap();
        Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["check", file.to_str().unwrap()])
            .output()
            .unwrap()
    };
    let out = check(ok);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for src in bad {
        let out = check(src);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "accepted:\n{src}");
        assert!(err.contains("E_PANIC_HANDLER"), "{src}\n{err}");
    }
}

/// A `let` copy of an explicit `ref` binding is a copy of the reference on
/// the MIR pipeline, as in `karac check`: the part's `Drop` body runs once,
/// with its owner (the follow-up to B-2026-10-20-24).
#[test]
fn mir_let_copy_of_a_ref_binding_is_a_ref() {
    let src = r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.id}"); } }
fn first(o: own Option[R]) -> i64 {
    match o {
        Some(ref r) => { let m = r; m.id + m.tag.len() }
        None => 0,
    }
}
fn main() {
    println(f"{first(Some(R { id: 4, tag: "ab".to_string() }))}");
    let t = (R { id: 7, tag: "x".to_string() }, 1);
    let (ref a, _) = t;
    let b = a;
    println(f"{b.id}");
}
"#;
    let want = "drop 4\n6\n7\ndrop 7\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want, "__mir-run");
    let dir = Scratch::new();
    let Some(exe) = cli_build(&dir, src, &[]) else {
        return;
    };
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), want, "compiled");
}

/// Float classification (B-2026-10-20-20), as Rust's `f64`/`f32`:
/// `is_nan`, `is_infinite`, `is_finite`, `is_sign_negative` and
/// `is_sign_positive` over a NaN, both infinities, both zeros and ordinary
/// values, on the MIR interpreter and compiled at -O0 and -O2.
#[test]
fn mir_float_classification_matches_rust() {
    const SRC: &str = r#"fn show(x: f64) -> String {
    f"{x.is_nan()} {x.is_infinite()} {x.is_finite()} {x.is_sign_negative()} {x.is_sign_positive()}"
}
fn show32(x: f32) -> String {
    f"{x.is_nan()} {x.is_infinite()} {x.is_finite()} {x.is_sign_negative()} {x.is_sign_positive()}"
}
fn main() {
    let zero = 0.0;
    let xs: Vec[f64] = vec![2.0, -3.5, f64.NAN, f64.INFINITY, -1.0 / zero, -0.0, 0.0];
    for x in xs.iter() {
        println(show(*x));
    }
    let ys: Vec[f32] = vec![1.5f32, f32.NAN, -f32.INFINITY, -0.0f32];
    for y in ys.iter() {
        println(show32(*y));
    }
    let bad = zero / zero;
    if bad.is_nan() {
        println("nan");
    }
}
"#;
    let want = "false false true false true\n\
         false false true true false\n\
         true false false false true\n\
         false true false false true\n\
         false true false true false\n\
         false false true true false\n\
         false false true false true\n\
         false false true false true\n\
         true false false false true\n\
         false true false true false\n\
         false false true true false\n\
         nan\n";
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SRC, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-20-9: providers (core semantics §14). A call on a resource
/// reaches the provider its `with_provider` or `providers` block binds,
/// through every function on the way, each copied for the provider type it
/// runs under; the provider drops when the binding ends.
#[test]
fn mir_providers_reach_the_bound_provider() {
    let cases = [
        (
            r#"trait Counter { fn get(ref self) -> i64; fn bump(mut ref self); }
effect resource Ctr: Counter;
struct Mem { n: i64 }
impl Counter for Mem {
    fn get(ref self) -> i64 { self.n }
    fn bump(mut ref self) { self.n = self.n + 1; }
}
impl Drop for Mem { fn drop(mut ref self) { println(f"drop {self.n}"); } }
fn read() -> i64 with reads(Ctr) { Ctr.get() }
fn twice() with writes(Ctr) { Ctr.bump(); Ctr.bump(); }
fn main() {
    let v = with_provider[Ctr](Mem { n: 5 }, || {
        twice();
        read()
    });
    println(f"got {v}");
}
"#,
            "drop 7\ngot 7\n",
        ),
        (
            r#"trait Name { fn name(ref self) -> String; }
effect resource N: Name;
struct A {}
struct B {}
impl Name for A { fn name(ref self) -> String { "a".to_string() } }
impl Name for B { fn name(ref self) -> String { "b".to_string() } }
fn show() with reads(N) { println(N.name()); }
fn main() {
    with_provider[N](A {}, || show());
    with_provider[N](B {}, || show());
}
"#,
            "a\nb\n",
        ),
        (
            r#"trait Recorder { fn record(ref self, value: i64) -> i64; }
struct ProvA {}
impl Recorder for ProvA { fn record(ref self, value: i64) -> i64 { 100 + value } }
struct ProvB {}
impl Recorder for ProvB { fn record(ref self, value: i64) -> i64 { 200 + value } }
effect resource Ra: Recorder;
effect resource Rb: Recorder;
fn both() -> i64 with reads(Ra) reads(Rb) { Ra.record(1) + Rb.record(2) }
fn main() {
    with_provider[Ra](ProvA {}, || {
        with_provider[Rb](ProvB {}, || {
            println(Ra.record(0));
            println(both());
        });
    });
}
"#,
            "100\n303\n",
        ),
        (
            r#"trait Counter { fn get(ref self) -> i64; }
effect resource Ctr: Counter;
struct Mem { n: i64 }
impl Counter for Mem { fn get(ref self) -> i64 { self.n } }
fn read() -> i64 with reads(Ctr) { Ctr.get() }
fn pick(early: bool) -> i64 {
    providers { Ctr => Mem { n: 6 } } in {
        if early { return 1; }
        read()
    }
}
fn main() { println(f"{pick(true)} {pick(false)}"); }
"#,
            "1 6\n",
        ),
        (
            r#"struct FakeEnv {}
impl EnvProvider for FakeEnv { fn args(ref self) -> Vec[String] { ["p", "x", "y"] } }
fn count() -> i64 with reads(Env) { env.args().len() }
fn main() {
    println(count() >= 0);
    with_provider[Env](FakeEnv {}, || println(count()));
}
"#,
            "true\n3\n",
        ),
    ];
    for (src, want) in cases {
        let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
        assert_eq!(interp.output, want, "__mir-run");
        let dir = Scratch::new();
        let Some(exe) = cli_build(&dir, src, &[]) else {
            continue;
        };
        let run = Command::new(&exe).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&run.stdout), want, "compiled");
    }
}

/// B-2026-10-20-31: a variant the checker typed at the `ref` parameter it is
/// lent to (a bare `None` arm of an `if` or `match` passed straight to a
/// call) is built as the value its destination holds, not as a borrow of a
/// temporary written into a value slot.
#[test]
fn mir_variant_arm_lent_to_a_call_is_built_in_place() {
    let src = r#"fn g(v: Option[String]) -> i64 { match v { None => 0, Some(s) => s.len() } }
fn h(v: ref Option[String]) -> i64 { match v { None => 0, Some(s) => s.len() } }
fn k(v: own Option[String]) -> i64 { match v { None => 0, Some(s) => s.len() } }
fn opt(a: Str) -> Option[String] { Some(a.to_string()) }
fn main() {
    for a in ["NONE", "abc"] {
        println(f"{g(if a == "NONE" { None } else { Some(a.to_string()) })}");
        println(f"{h(if a == "NONE" { None } else { opt(a) })}");
        println(f"{k(if a == "NONE" { None } else { opt(a) })}");
        println(f"{g(match a { "NONE" => None, _ => opt(a) })}");
    }
}
"#;
    let want = "0\n0\n0\n0\n3\n3\n3\n3\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want, "__mir-run");
    let dir = Scratch::new();
    let Some(exe) = cli_build(&dir, src, &[]) else {
        return;
    };
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), want, "compiled");
}

#[test]
fn mir_collect_in_one_branch_takes_the_sibling_type() {
    // B-2026-10-19-25: a bare `collect()` as one branch's value of an
    // un-annotated `if` or `match`, whose target only the OTHER branch names,
    // left `collect`'s `C` unsolved, whichever branch it was in.
    let src = r#"
fn pick(word: own String, flip: bool) -> String {
    let out = if flip {
        let chars: Vec[char] = word.chars().collect();
        chars.iter().rev().collect()
    } else {
        word
    };
    out
}
fn pick2(word: own String, flip: bool) -> String {
    let out = if flip {
        word
    } else {
        let chars: Vec[char] = word.chars().collect();
        chars.iter().rev().collect()
    };
    out
}
fn upto(n: i64, k: i64) -> Vec[i64] {
    let v = match k {
        0 => (0..n).collect(),
        _ => {
            let mut w: Vec[i64] = Vec.new();
            w.push(k);
            w
        }
    };
    v
}
fn main() {
    println(f"{pick("abc".to_string(), true)} {pick("abc".to_string(), false)}");
    println(f"{pick2("xyz".to_string(), true)} {pick2("xyz".to_string(), false)}");
    println(f"{upto(3, 0)} {upto(3, 7)}");
}
"#;
    let want = "cba abc\nxyz zyx\n[0, 1, 2] [7]\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_empty_list_in_one_branch_takes_the_sibling_type() {
    // B-2026-10-21-82: an empty `[]` as one branch's value of an un-annotated
    // `if` or `match`, whose element type only a SIBLING branch names, kept
    // `Vec[<error>]` and both MIR lanes refused it, whichever branch it was in.
    let src = r#"
fn pick(n: i64) -> Vec[String] {
    if n == 0 { [] } else if n == 1 { ["one".to_string()] } else { let s = f"{n}"; [s.clone(), s] }
}
fn twice(n: i64) -> Vec[i64] {
    let r = match n % 3 { 0 => { let k = n * 2; [k] }, 1 => [], _ => [n, n] };
    r
}
fn z() -> u8 { 9 }
fn main() {
    let mut v: Vec[Vec[i64]] = Vec.new();
    for i in 0..4 { v.push(if i < 2 { [i] } else { [] }); }
    println(f"{v}");
    let x = if true { [5] } else { [] };
    let y = match v.len() { 0 => [], _ => [7, 8] };
    let a = if x.len() > 1 { [z()] } else { [] };
    println(f"{x} {y} {a.len()}");
    for n in 0..3 { println(f"{pick(n).len()} {twice(n)}"); }
    let mut w: Vec[Vec[f64]] = Vec.new();
    for i in 0..3 { w.push(match i { 0 => [], 1 => [1.5], _ => [] }); }
    println(f"{w}");
}
"#;
    let want = "[[0], [1], [], []]\n[5] [7, 8] 0\n0 [0]\n1 []\n2 [2, 2]\n[[], [1.5], []]\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_from_fn_closure_borrows_its_captures() {
    // B-2026-10-22-1: the closure `Vec.from_fn` / `Array.from_fn` call was
    // lowered as a stored function value, so it moved what it captured and
    // a later read of the capture failed the move check.
    let src = r#"
struct P { name: String, k: i64 }
fn main() {
    let pre = "n".to_string();
    let v: Vec[String] = Vec.from_fn(3, |i| f"{pre}{i}");
    let a: Array[String, 2] = Array.from_fn(2, |i| f"{pre}{i}");
    println(f"{v} {a} {pre}");
    let p = P { name: "p".to_string(), k: 10 };
    let mut calls = 0;
    let w: Vec[i64] = Vec.from_fn(4, |i| { calls += 1; i * p.k + p.name.len() });
    println(f"{w} {calls} {p.name}");
    let owned = "own".to_string();
    let f = |i: i64| f"{owned}{i}";
    let u: Vec[String] = Vec.from_fn(2, f);
    println(f"{u} {owned}");
}
"#;
    let want = "[n0, n1, n2] [n0, n1] n\n[1, 11, 21, 31] 4 p\n[own0, own1] own\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_shared_pattern_parts_read_through_deref() {
    // B-2026-10-18-30: a `shared` scrutinee counts as a `ref` scrutinee, so a
    // handle part reads through `*rest` (core semantics §4.6), as `karac
    // check` accepts; the MIR surfaces bound the part by value and refused it.
    let src = r#"
shared enum List {
    Nil,
    Cons(i64, List),
}
fn tail(list: own List) -> List {
    match list {
        List.Nil => List.Nil,
        List.Cons(_, rest) => *rest,
    }
}
fn len(list: List) -> i64 {
    if let List.Cons(_, rest) = list {
        1 + len(*rest)
    } else {
        0
    }
}
fn nth(list: List, index: i64) -> i64 {
    match list {
        List.Nil => -1,
        List.Cons(v, rest) => if index == 0 { v } else { nth(*rest, index - 1) },
    }
}
fn sum(list: List) -> i64 {
    match list {
        List.Nil => 0,
        List.Cons(v, rest) => match *rest {
            List.Nil => v,
            List.Cons(w, _) => v + w + sum(*rest),
        },
    }
}
fn main() {
    let l = List.Cons(4, List.Cons(5, List.Cons(6, List.Nil)));
    println(f"{len(tail(l))} {len(l)} {nth(l, 1)} {nth(l, 5)} {sum(l)}");
}
"#;
    let want = "2 3 5 -1 26\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_user_struct_named_err_leaves_result_err() {
    // B-2026-10-21-18: a bare `Err(..)` in a call typed `Result[_, Err]`
    // resolved to a user `struct Err` and was lowered as a call of an
    // unknown function; the call's enum names the variant.
    let src = r#"
struct Err { msg: String, }
struct Pair { a: i64, b: i64 }
fn f(x: i64) -> Result[i64, Err] {
    if x > 0 { return Err(Err { msg: "no".to_string() }); }
    Ok(x)
}
fn show(r: Result[i64, Err]) -> String {
    match r { Ok(v) => f"ok {v}", Err(e) => e.msg.clone(), }
}
fn main() {
    match f(1) { Ok(v) => println(f"{v}"), Err(e) => println(e.msg), }
    println(show(f(0)));
    println(show(Err(Err { msg: "lent".to_string() })));
    let p = Pair { a: 3, b: 4 };
    println(f"{p.a + p.b}");
}
"#;
    let want = "no\nok 0\nlent\n7\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_range_assignment_copies_into_the_range() {
    // B-2026-10-21-24: `c[a..b] = other` copies `other`'s elements into the
    // range and panics when the lengths differ (design.md § Range indexing);
    // the builder assigned `other` to a temporary view, so it did nothing.
    let src = r#"
#[derive(Copy, Clone)]
struct P { x: i64, y: f64 }
fn fill(s: mut Slice[i64], src: Slice[i64]) {
    s[1..=2] = src;
}
fn head(v: mut ref Vec[f64], w: ref Vec[f64]) {
    v[..2] = w[1..];
}
fn tail_set(v: mut ref Vec[i64]) {
    v[3..] = [40, 50];
}
fn main() {
    let mut v: Vec[i64] = [1, 2, 3, 4, 5];
    let w: Vec[i64] = [8, 9, 7];
    v[1..3] = w[0..2];
    println(f"{v}");
    tail_set(mut v);
    let src: Vec[i64] = [7, 6];
    fill(mut v, src);
    println(f"{v}");
    let mut a: Array[i64, 4] = [1, 2, 3, 4];
    let u: Vec[i64] = [8, 9];
    a[0..2] = u;
    println(f"{a}");
    let mut f: Vec[f64] = [0.5, 1.5, 2.5];
    let g: Vec[f64] = [9.0, 8.0, 7.0];
    head(mut f, g);
    println(f"{f}");
    let mut ps: Vec[P] = [P { x: 1, y: 1.0 }, P { x: 2, y: 2.0 }];
    let qs: Vec[P] = [P { x: 3, y: 3.0 }];
    ps[1..2] = qs;
    println(f"{ps[0].x} {ps[1].x} {ps[1].y}");
}
"#;
    let want = "[1, 8, 9, 4, 5]\n[1, 7, 6, 40, 50]\n[8, 9, 3, 4]\n[8.0, 7.0, 2.5]\n1 3 3.0\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
    let short = r#"
fn main() {
    let mut v: Vec[i64] = [1, 2, 3, 4];
    let w: Vec[i64] = [8, 9, 7];
    println("before");
    v[1..3] = w[0..3];
    println(f"{v}");
}
"#;
    let r = karac::mir::lower::run_source(short).expect("MIR interpreter");
    assert_eq!(r.output, "before\n");
    assert!(
        matches!(r.outcome, karac::mir::interp::Outcome::Aborted(_)),
        "{:?}",
        r.outcome
    );
}

#[test]
fn mir_default_method_generic_named_like_the_impls() {
    // B-2026-10-21-34: a trait default method's own `K`, copied into an
    // `impl[K] .. for W[K]`, merged with the impl's `K` (types are matched
    // by name), so the instance took the method's argument for both: a use
    // after free on `__mir-run`, a double free compiled.
    let src = r#"
struct W[K] { x: K }
trait Tr {
    type Item;
    fn get(self) -> Self.Item;
    fn pair[K](self, k: own K) -> (Self.Item, K) { (self.get(), k) }
    fn both[K: Clone](self, k: ref K) -> Vec[K] { [k.clone(), k.clone()] }
}
impl[K: Clone] Tr for W[K] {
    type Item = K;
    fn get(self) -> K { self.x.clone() }
}
fn main() {
    let w: W[String] = W { x: "hello".to_string() };
    let p = w.pair(42);
    println(f"{p.0} {p.1}");
    let q = w.pair("there".to_string());
    println(f"{q.0} {q.1}");
    let v = w.both(7);
    println(f"{v}");
}
"#;
    let want = "hello 42\nhello there\n[7, 7]\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_iter_mut_loops_over_deques_and_maps() {
    // B-2026-10-21-25: `for x in c.iter_mut()` over a `VecDeque`, `Map` or
    // `SortedMap` was refused ("a `for` loop over a collection"); it lends
    // each element (a map's value) in place, as over a `Vec`.
    let src = r#"
fn bump(m: mut ref Map[String, Vec[i64]]) {
    for (k, v) in m.iter_mut() {
        v.push(k.len() as i64);
    }
}

fn main() {
    let mut m: Map[String, Vec[i64]] = Map.new();
    m.insert("ab".to_string(), [1]);
    m.insert("xyz".to_string(), Vec.new());
    bump(mut m);
    println(f"{m["ab"]} {m["xyz"]}");
    let mut d: VecDeque[String] = VecDeque.new();
    d.push_back("a".to_string());
    d.push_front("b".to_string());
    for (i, s) in d.iter_mut().enumerate() {
        if i == 1 { break; }
        s.push_str("!");
    }
    for s in d.iter() { println(s); }
    let mut t: SortedMap[i64, String] = SortedMap.new();
    t.insert(2, "two".to_string());
    t.insert(1, "one".to_string());
    for (_, v) in t.iter_mut() { *v = v.to_uppercase(); }
    for (k, v) in t.iter() { println(f"{k} {v}"); }
}
"#;
    let want = "[1, 2] [3]
b!
a
1 ONE
2 TWO
";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_for_over_an_open_range_runs_until_break() {
    // B-2026-10-21-48: `for i in (1..)` was refused as "a `for` loop over a
    // collection"; it counts up until a `break`, `return` or labelled jump.
    let src = r#"
fn first_square_over(n: i64) -> i64 {
    for i in (1..) {
        if i * i > n { return i; }
    }
    -1
}
fn main() {
    for i in (1..) { if i > 3 { break; } println(f"{i}"); }
    let mut total = 0;
    'outer: for a in (10..) {
        for b in (a..) {
            if b > a + 2 { continue 'outer; }
            if a > 11 { break 'outer; }
            total += b;
        }
    }
    println(f"{total} {first_square_over(50)}");
}
"#;
    let want = "1
2
3
69 8
";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-21-49: a program's own `Env` or `Clock` is its own type, and
/// the library's `reads(Env)` and `env.args()` keep the resource.
#[test]
fn mir_a_user_type_named_like_a_resource_is_its_own() {
    let src = r#"
// The Parser keeps cli.kara, whose `parse` reads(Env).
struct Env {
    n: i64,
}

struct Clock {
    t: i64,
}

fn show(e: ref Env) -> i64 {
    e.n
}

fn main() {
    let e = Env { n: 3 };
    let c = Clock { t: 4 };
    println(f"{e.n} {show(e)} {c.t}");
    let a = env.args();
    println(f"{a.len() > 0}");
}
"#;
    let want = "3 3 4\ntrue\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
    // An error names the program's type by its own name.
    let err = karac::mir::lower::build_source(&src.replace(
        "let a = env.args();",
        "let bad: i64 = e;\n    let a = env.args();",
    ))
    .err()
    .expect("refused");
    assert!(err.contains("found 'Env'"), "{err}");
}

/// B-2026-10-21-58: arithmetic over unsuffixed float literals takes a
/// narrow slot's width, operands and all, so compiled code stores an `f32`
/// where it had stored the folded `f64` (a wrong value, and the neighbour
/// clobbered).
#[test]
fn mir_literal_float_arithmetic_takes_the_slots_width() {
    let src = r#"
fn third() -> f32 {
    1.0 / 3.0
}

fn half(x: f32) -> f32 {
    x * 0.5
}

fn main() {
    let c: f32 = 0.1;
    let d: f32 = 1.0 / 3.0;
    let e: f32 = 2.0 * 3.0 - 0.25;
    let n: f32 = -(1.0 / 8.0);
    let h: f64 = 1.0 / 4.0;
    println(c);
    println(d);
    println(e);
    println(n);
    println(h);
    println(third());
    println(half(1.0 / 3.0));
    let v: Vec[f32] = [1.0 / 3.0, 0.5];
    println(v[0] + v[1]);
    let o: Option[f32] = Some(1.0 / 4.0);
    println(o.unwrap());
}
"#;
    let want = "0.1\n0.33333334\n5.75\n-0.125\n0.25\n0.33333334\n0.16666667\n0.8333334\n0.25\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-21-55: a generic fixed only by its return type learns it from
/// the binding through `.unwrap()` / `.expect(..)`, for an `Option` and a
/// `Result` return.
#[test]
fn mir_a_return_only_generic_learns_its_type_through_unwrap() {
    let src = r#"
fn make[T: Default]() -> Option[T] {
    Some(T.default())
}

fn parse_or[T: Default](ok: bool) -> Result[T, String] {
    if ok { Ok(T.default()) } else { Err("no".to_string()) }
}

fn main() {
    let x: i64 = make().unwrap();
    let s: String = make().expect("a value");
    let b: bool = parse_or(true).unwrap();
    let f: f64 = parse_or(true).expect("ok");
    println(f"{x} [{s}] {b} {f}");
}
"#;
    let want = "0 [] false 0.0\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-21-50: a closure's inner `collect()` takes its target from the
/// function's return type, at the tail and at a `return`, as it does from
/// an annotated `let`.
#[test]
fn mir_a_mapped_inner_collect_aims_at_the_return_type() {
    let src = r#"
fn by_return(image: Vec[Vec[i64]]) -> Vec[Vec[i64]] {
    image.iter().map(|row| row.iter().rev().map(|x| 1 - x).collect()).collect()
}

fn by_early(image: Vec[Vec[i64]], early: bool) -> Vec[Vec[i64]] {
    if early {
        return image.iter().map(|row| row.iter().map(|x| x * 2).collect()).collect();
    }
    Vec.new()
}

fn main() {
    let image: Vec[Vec[i64]] = [[1, 1, 0], [0, 0, 1]];
    println(f"{by_return(image.clone())}");
    println(f"{by_early(image, true)}");
}
"#;
    let want = "[[1, 0, 0], [0, 1, 1]]\n[[2, 2, 0], [0, 0, 2]]\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-21-64: `Result.or_else` whose closure changes the error type
/// fixes the new error type from the binding or from the closure's `Err`.
#[test]
fn mir_or_else_may_change_the_error_type() {
    let src = r#"
fn chk(x: i64) -> Result[i64, String] { if x > 0 { Ok(x) } else { Err("neg".to_string()) } }

fn main() {
    let a: Result[i64, String] = Ok(-1);
    let r = a.and_then(chk);
    match r { Ok(x) => println(f"ok {x}"), Err(e) => println(f"err {e}") }
    let b: Result[i64, String] = Err("x".to_string());
    let s: Result[i64, i64] = b.or_else(|e| if e == "x" { Ok(0) } else { Err(1) });
    match s { Ok(x) => println(f"ok {x}"), Err(e) => println(f"err {e}") }
    let c: Result[i64, String] = Ok(5);
    let t: Result[i64, i64] = c.or_else(|e| Err(e.len()));
    match t { Ok(x) => println(f"ok {x}"), Err(e) => println(f"err {e}") }
    let d: Result[i64, String] = Err("abc".to_string());
    let u = d.or_else(|e| Err(e.len()));
    match u { Ok(x) => println(f"ok {x}"), Err(e) => println(f"err {e}") }
}
"#;
    let want = "err neg\nok 0\nok 5\nerr 3\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// B-2026-10-21-51: `Vec.iter_mut()` is a library iterator (`VecIterMut`),
/// so it takes every adaptor, inside a `for` or driven by hand.
#[test]
fn mir_vec_iter_mut_takes_adaptors() {
    let src = r#"
fn main() {
    let mut w: Vec[i64] = [1, 2, 3, 4];
    for x in w.iter_mut().skip(1) {
        *x += 10;
    }
    println(f"{w}");
    for x in w.iter_mut().rev().take(2) {
        *x *= 2;
    }
    println(f"{w}");
    let mut it = w.iter_mut().step_by(2);
    while let Some(x) = it.next() {
        *x = 0;
    }
    println(f"{w}");
    let mut k = 0;
    let mut e = w.iter_mut().enumerate();
    while let Some((i, x)) = e.next() {
        *x += i;
        k += 1;
    }
    println(f"{w} {k}");
    let mut s: Vec[String] = ["a".to_string(), "b".to_string()];
    for t in s.iter_mut().skip(1) {
        t.push_str("!");
    }
    println(f"{s}");
    let mut v: Vec[i64] = [1, 2, 3];
    let mut j = v.iter_mut();
    let a = j.next().unwrap();
    let b = j.next().unwrap();
    *a += 100;
    *b += 200;
    let big = v.iter_mut().filter(|x| x > 150).count();
    println(f"{v} {big}");
}
"#;
    let want = "[1, 12, 13, 14]\n[1, 12, 26, 28]\n[0, 12, 0, 28]\n[0, 13, 2, 31] 4\n[a, b!]\n[101, 202, 3] 1\n";
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}
