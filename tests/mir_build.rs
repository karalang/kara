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
