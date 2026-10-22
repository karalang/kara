//! Compile-time evaluation on the v2 pipeline (B-2026-10-22-20).
//!
//! `comptime { .. }` blocks, `#[derive(X)]` backed by a `comptime fn
//! derive_x` and `#[proto_schema]` constants run on the MIR interpreter, not
//! on the legacy tree-walk interpreter: every program here goes through
//! `karac::mir::lower::run_source` (what `karac __mir-run` and `karac run`
//! build) or the `karac` binary. The legacy evaluator asserts, in a debug
//! build, that it never sees a program carrying the library, which is what
//! every v2 pipeline compiles.

use std::process::Command;

/// The program's stdout on the v2 pipeline, or what refused it.
fn run(src: &str) -> Result<String, String> {
    let r = karac::mir::lower::run_source(src)?;
    match r.outcome {
        karac::mir::interp::Outcome::Returned(_) => Ok(r.output),
        other => Err(format!("{other:?}: {}", r.stderr)),
    }
}

/// `karac <cmd>` on `src`: its exit code and stderr.
fn karac(cmd: &str, name: &str, src: &str) -> (i32, String) {
    let dir =
        std::env::temp_dir().join(format!("karac-comptime-mir-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.kara"));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&path)
        .output()
        .expect("karac runs");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn comptime_blocks_fold_at_the_type_their_context_gives() {
    let src = r#"
comptime fn sq(n: u8) -> u8 { n * n }
comptime fn half(x: f64) -> f64 { x / 2.0 }
comptime fn fib(n: i64) -> i64 { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }
const SQ: u8 = comptime { sq(5) };
const FIB_TEN: i64 = comptime { fib(10) };
fn main() {
    let h = comptime { half(3.0) };
    let t = comptime { (1, true, 'c') };
    let narrow: i32 = comptime { 4 };
    let v: Vec[i32] = comptime { [4, 5] };
    let s = comptime { "hi" + " there" };
    println(f"{SQ} {FIB_TEN} {h} {t.0} {t.1} {t.2} {narrow} {v[1]} {s}");
}
"#;
    assert_eq!(run(src).unwrap(), "25 55 1.5 1 true c 4 5 hi there\n");
}

#[test]
fn a_quoted_expression_is_spliced_where_the_block_was() {
    let src = r#"
fn main() {
    let x = 10;
    let y = comptime {
        let n = 3;
        ast.expr(f"x * {n + 1}")
    };
    println(y);
}
"#;
    assert_eq!(run(src).unwrap(), "40\n");
}

#[test]
fn a_derive_reads_the_type_through_reflection() {
    // `comptime T: Type` is a borrow under D5, and `T.name()` is the path
    // call form; both reach the MIR interpreter's reflection natives.
    let src = r#"
comptime fn derive_describe(comptime T: Type) -> Vec[Item] {
    let mut text = "impl " + T.name() + " { fn describe(self) -> String { \"";
    if T.is_struct() {
        for f in T.fields() {
            text = text + f.name.clone() + ":" + f.ty.name() + " ";
        }
    } else {
        for v in T.variants() {
            text = text + v.name.clone() + "/" + f"{v.field_count}" + " ";
        }
    }
    text = text + "\" } }";
    [ast.item(text)]
}

#[derive(Describe)]
struct Point { x: i64, label: String }

#[derive(Describe)]
enum Shape { Dot, Circle(f64), Rect(f64, f64) }

fn main() {
    println(Point { x: 1, label: "a" }.describe());
    println(Shape.Dot.describe());
}
"#;
    assert_eq!(
        run(src).unwrap(),
        "x:i64 label:String \nDot/0 Circle/1 Rect/2 \n"
    );
}

#[test]
fn the_library_derives_run_on_types_named_like_compiler_or_prelude_types() {
    // `Item` is also what a derive returns (`Vec[Item]`) and `Stats` is a
    // prelude name: a program declaring its own type by either still derives.
    let src = r#"
import std.json.{Json};

#[derive(Message)]
struct Item { v: i64 }

#[derive(Message)]
struct Bag { items: Vec[Item] }

#[derive(Serialize)]
struct Stats { hits: i64 }

fn main() {
    let b = Bag { items: [Item { v: 7 }, Item { v: -8 }] };
    let back = Bag.decode(b.encode());
    println(f"{back.items.len()} {back.items[0].v} {back.items[1].v}");
    println(Json.serialize(Stats { hits: 3 }));
}
"#;
    assert_eq!(run(src).unwrap(), "2 7 -8\n{\"hits\":3}\n");
}

#[test]
fn a_proto_schema_expands_into_message_types() {
    let src = r#"
#[proto_schema]
const SCHEMA: String = "
    syntax = \"proto3\";
    message Person {
        string name = 1;
        int64 age = 2;
    }
";

fn main() {
    let p = Person { name: "Ada", age: 36 };
    let q = Person.decode(p.encode());
    println(f"{q.name} {q.age}");
}
"#;
    assert_eq!(run(src).unwrap(), "Ada 36\n");
}

#[test]
fn compiler_error_refuses_the_program_at_its_call() {
    let src = r#"comptime fn small(n: i64) -> i64 {
    if n > 10 {
        compiler.error("value too large");
    }
    n
}
const BIG: i64 = comptime { small(42) };
fn main() {
    println(f"{BIG}");
}
"#;
    for cmd in ["check", "run"] {
        let (code, stderr) = karac(cmd, "error", src);
        assert_ne!(code, 0, "{cmd}: {stderr}");
        assert!(
            stderr.contains(":3:9: error[E_COMPTIME_ERROR]: value too large"),
            "{cmd}: {stderr}"
        );
    }
}

#[test]
fn a_comptime_panic_and_a_runaway_loop_are_compile_errors() {
    let panics = r#"comptime fn boom(n: i64) -> i64 {
    let v: Vec[i64] = Vec.new();
    v[n]
}
fn main() {
    let x = comptime { boom(3) };
    println(f"{x}");
}
"#;
    let (code, stderr) = karac("check", "panic", panics);
    assert_ne!(code, 0, "{stderr}");
    assert!(
        stderr.contains(":6:13: error[E_COMPTIME_PANIC]: `comptime` evaluation panicked"),
        "{stderr}"
    );
    let spins = r#"comptime fn spin() -> i64 {
    let mut i = 0;
    loop { i = i + 1; }
    i
}
fn main() {
    let x = comptime { spin() };
    println(f"{x}");
}
"#;
    let (code, stderr) = karac("check", "spin", spins);
    assert_ne!(code, 0, "{stderr}");
    assert!(
        stderr.contains(":7:13: error[E_COMPTIME_ITER_LIMIT_EXCEEDED]"),
        "{stderr}"
    );
}

#[test]
fn a_library_derive_reports_its_error_at_the_derive_site() {
    let src = r#"#[derive(Message)]
enum Color { Red, Green }
fn main() { println("x"); }
"#;
    let (code, stderr) = karac("check", "site", src);
    assert_ne!(code, 0, "{stderr}");
    assert!(
        stderr.contains(":2:1: error[E_COMPTIME_ERROR]: derive(Message): `Color` is an enum"),
        "{stderr}"
    );
}
