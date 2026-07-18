//! Differential BEHAVIORAL oracle for the self-hosted codegen emitter
//! (`selfhost/src/codegen.kara`, Phase 12 Codegen port).
//!
//! Unlike the front-end oracles (which diff a canonical render byte-for-byte),
//! LLVM IR text is not reproducible character-for-character — SSA value
//! numbering and block labels are construction-order artifacts. So the gate
//! here is PROGRAM-OUTPUT parity: for each source program, emit IR with the
//! Kāra emitter, run that IR via `karac_jit_runner`, and assert its stdout +
//! exit status match the seed's `karac run` on the same source.
//!
//! Slice 1 surface: a `main` of `println("literal")` statements.
//!
//! Requires `--features llvm` (the JIT runner + codegen). Skips benignly if the
//! selfhost driver can't link (no runtime archive), never on a compiler panic.
#![cfg(feature = "llvm")]

use std::path::PathBuf;
use std::process::Command;

/// Programs whose emitted IR must run identically to `karac run`.
const CORPUS: &[&str] = &[
    "fn main() { println(\"hi\") }",
    "fn main() { println(\"hello\"); println(\"world\") }",
    "fn main() { println(\"\") }",
    "fn main() { println(\"a b c\") }",
    "fn main() { println(\"tab\tand\tspaces\") }",
    "fn main() { println(\"unicode: \u{e9}\u{2192}\") }",
    "fn main() { }",
    "fn main() { println(\"one\"); println(\"two\"); println(\"three\") }",
    // Slice 2: integer literals + arithmetic, formatted via `.to_string()`.
    "fn main() { println((2 + 3).to_string()) }",
    "fn main() { println(42.to_string()) }",
    "fn main() { println((10 - 4).to_string()) }",
    "fn main() { println((6 * 7).to_string()) }",
    "fn main() { println((2 + 3 * 4).to_string()) }",
    "fn main() { println((0 - 5).to_string()) }",
    "fn main() { println(\"n = \"); println((1 + 1).to_string()) }",
    // Slice 3: let bindings, local reads, assignment, shadowing.
    "fn main() { let x = 5; println(x.to_string()) }",
    "fn main() { let x = 2; let y = 3; println((x + y).to_string()) }",
    "fn main() { let x = 2; let y = x * 10; println((y + x).to_string()) }",
    "fn main() { let mut x = 1; x = x + 41; println(x.to_string()) }",
    "fn main() { let x = 1; let x = x + 1; println(x.to_string()) }",
    "fn main() { let mut a = 10; a = a - 3; a = a * 2; println(a.to_string()) }",
    // Slice 4: bools, comparisons, logical ops, if/else (incl. else-if), div/mod.
    "fn main() { let x = 5; if x > 3 { println(\"big\") } else { println(\"small\") } }",
    "fn main() { let x = 2; if x > 3 { println(\"big\") } else { println(\"small\") } }",
    "fn main() { println((3 < 4).to_string()); println((4 < 3).to_string()) }",
    "fn main() { println(true.to_string()); println(false.to_string()) }",
    "fn main() { let a = 1; let b = 2; println((a < b and b < 3).to_string()) }",
    "fn main() { println((not (1 == 2)).to_string()) }",
    "fn main() { let n = 17; if n % 2 == 0 { println(\"even\") } else { println(\"odd\") } }",
    "fn main() { let n = 9; if n < 5 { println(\"lo\") } else { if n < 20 { println(\"mid\") } else { println(\"hi\") } } }",
    "fn main() { println((84 / 2).to_string()); println((17 % 5).to_string()) }",
    "fn main() { let mut x = 1; if true { x = x + 1; } println(x.to_string()) }",
    // Slice 5: while loops.
    "fn main() { let mut i = 0; while i < 5 { println(i.to_string()); i = i + 1; } }",
    "fn main() { let mut s = 0; let mut i = 1; while i <= 10 { s = s + i; i = i + 1; } println(s.to_string()) }",
    "fn main() { let mut n = 1; while n < 100 { n = n * 2; } println(n.to_string()) }",
    "fn main() { let mut i = 0; while i < 0 { println(\"never\"); i = i + 1; } println(\"done\") }",
    // Nested: FizzBuzz-lite (loop + if/else-if inside).
    "fn main() { let mut i = 1; while i <= 15 { if i % 15 == 0 { println(\"fizzbuzz\") } else { if i % 3 == 0 { println(\"fizz\") } else { if i % 5 == 0 { println(\"buzz\") } else { println(i.to_string()) } } } i = i + 1; } }",
    // Slice 6: user-defined functions — params, calls, tails, return, recursion.
    "fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() { println(add(2, 3).to_string()) }",
    "fn dbl(n: i64) -> i64 { n * 2 }\nfn main() { println(dbl(dbl(10) + 1).to_string()) }",
    "fn greet() { println(\"hello\") }\nfn main() { greet(); greet() }",
    "fn max(a: i64, b: i64) -> i64 { if a > b { a } else { b } }\nfn main() { println(max(3, 9).to_string()); println(max(9, 3).to_string()) }",
    "fn fib(n: i64) -> i64 { if n < 2 { return n; } fib(n - 1) + fib(n - 2) }\nfn main() { println(fib(10).to_string()) }",
    "fn fact(n: i64) -> i64 { if n <= 1 { 1 } else { n * fact(n - 1) } }\nfn main() { println(fact(6).to_string()) }",
    "fn sign(n: i64) -> i64 { if n > 0 { return 1; } if n < 0 { return 0 - 1; } 0 }\nfn main() { println(sign(42).to_string()); println(sign(0 - 7).to_string()); println(sign(0).to_string()) }",
    // A helper called for effect inside a loop.
    "fn shout(n: i64) { println(n.to_string()); println(\"!\") }\nfn main() { let mut i = 0; while i < 3 { shout(i); i = i + 1; } }",
    // Slice 7: string locals ({ptr,i64} aggregates over interned globals),
    // typed slots (also fixes bool locals), moves, reassignment, shadowing.
    "fn main() { let s = \"hello\"; println(s) }",
    "fn main() { let mut t = \"a\"; t = \"b\"; println(t) }",
    "fn main() { let s = \"x\"; let t = s; println(t) }",
    "fn main() { let s = \"one\"; let s = \"two\"; println(s) }",
    "fn main() { let b = true; println(b.to_string()) }",
    "fn main() { let name = \"kara\"; let n = 5; println(name); println(n.to_string()) }",
    // Slice 8: string concatenation (malloc+memcpy; frees deferred to the
    // drop slice — concat results leak until exit, oracle checks stdout+exit).
    "fn main() { let s = \"foo\" + \"bar\"; println(s) }",
    "fn main() { let a = \"x\"; let b = \"y\"; let c = a + b; println(c) }",
    "fn main() { println(\"a\" + \"b\" + \"c\") }",
    "fn main() { let name = \"kara\"; println(\"hi \" + name) }",
    "fn main() { let mut s = \"\"; let mut i = 0; while i < 3 { s = s + \"ab\"; i = i + 1; } println(s) }",
    // Slice 10: string params & returns — heap values cross fn boundaries.
    // Contract: args MOVE IN (caller materializes a borrowed arg into an owned
    // copy; a heap temp transfers directly); callee owns+frees its params; a
    // returned borrow is materialized; a discarded owned result is freed.
    "fn greet(name: String) -> String { \"hi \" + name }\nfn main() { println(greet(\"kara\")) }",
    "fn id(s: String) -> String { s }\nfn main() { println(id(\"echo\")) }",
    "fn make() -> String { \"a\" + \"b\" }\nfn main() { let s = make(); println(s) }",
    "fn make() -> String { \"z\" + \"z\" }\nfn main() { make(); println(\"done\") }",
    "fn wrap(s: String) -> String { \"[\" + s + \"]\" }\nfn main() { println(wrap(wrap(\"x\"))) }",
    "fn shout(s: String) { println(s + \"!\") }\nfn main() { shout(\"hey\"); shout(\"ho\") }",
    "fn pad(a: String, b: String) -> String { a + \" \" + b }\nfn main() { println(pad(\"left\", \"right\")) }",
    // Slice 11: to_string() in VALUE position — i64 formats into a fresh heap
    // buffer (snprintf), bool borrows the true/false globals, string passes
    // through; composes with concat, bindings, params, and loops.
    "fn label(n: i64, tag: String) -> String { tag + n.to_string() }\nfn main() { println(label(7, \"v\")) }",
    "fn main() { let s = 42.to_string(); println(s) }",
    "fn main() { let n = 3; println(\"n=\" + n.to_string()) }",
    "fn main() { println(true.to_string() + \"!\") }",
    "fn main() { let mut i = 0; let mut acc = \"\"; while i < 3 { acc = acc + i.to_string(); i = i + 1; } println(acc) }",
    "fn main() { println((0 - 99).to_string() + \"/\" + (7 * 6).to_string()) }",
    // Slice 12: POD structs — construction (reordered literals), field reads,
    // struct params/returns/calls, bool fields. (Unblocked by the
    // B-2026-07-18-2 seed fix: the AOT-built generator previously double-freed
    // on any struct-bearing input.)
    "struct P { x: i64, y: i64 }\nfn main() { let p = P { x: 3, y: 4 }; println(p.x.to_string()); println(p.y.to_string()) }",
    "struct P { x: i64, y: i64 }\nfn main() { let p = P { y: 9, x: 1 }; println((p.x + p.y).to_string()) }",
    "struct P { x: i64, y: i64 }\nfn dist2(p: P) -> i64 { p.x * p.x + p.y * p.y }\nfn main() { println(dist2(P { x: 3, y: 4 }).to_string()) }",
    "struct P { x: i64, y: i64 }\nfn mk(a: i64, b: i64) -> P { P { x: a, y: b } }\nfn main() { let p = mk(2, 5); println((p.y - p.x).to_string()) }",
    "struct F { on: bool, n: i64 }\nfn main() { let f = F { on: true, n: 8 }; if f.on { println(f.n.to_string()) } }",
    "struct P { x: i64, y: i64 }\nfn shift(p: P, d: i64) -> P { P { x: p.x + d, y: p.y + d } }\nfn main() { let p = shift(P { x: 1, y: 2 }, 10); println((p.x + p.y).to_string()) }",
    // Struct-var reassignment — deferred while B-2026-07-18-7 was open (the
    // SEED emitted a gpu_free_soa reference on a plain reassign, breaking
    // run+build while the Kara emitter was already correct); re-landed on the
    // 13f9c2a seed fix.
    "struct P { x: i64, y: i64 }\nfn main() { let mut p = P { x: 1, y: 2 }; p = P { x: 10, y: 20 }; println((p.x + p.y).to_string()) }",
    // Slice 13: Vec[i64] — new/push/len/index/iteration; grow-by-one realloc
    // (observationally identical to the seed's amortized doubling), buffer
    // freed at scope exit (free(null)-safe for the empty vec).
    "fn main() { let v = Vec.new(); println(v.len().to_string()) }",
    "fn main() { let mut v = Vec.new(); v.push(10); v.push(20); v.push(30); println(v.len().to_string()); println(v[0].to_string()) }",
    "fn main() { let mut v = Vec.new(); v.push(7); v.push(8); println((v[0] * v[1]).to_string()) }",
    "fn main() { let mut v = Vec.new(); let mut i = 0; while i < 6 { v.push(i * i); i = i + 1; } let mut s = 0; let mut j = 0; while j < v.len() { s = s + v[j]; j = j + 1; } println(s.to_string()) }",
    "fn main() { let mut a = Vec.new(); let mut b = Vec.new(); a.push(1); b.push(2); b.push(3); println((a.len() + b.len()).to_string()); println((a[0] + b[1]).to_string()) }",
    // Slice 14: enums + match — {tag,payload} aggregates (0/1 i64 payload),
    // qualified construction (bare path + call-with-path), value- and
    // statement-position match, payload bindings, bare-variant arms
    // (BindingPat whose name IS a variant), wildcard, enum params/returns.
    "enum Op { Add(i64), Neg(i64), Zero }\nfn eval(o: Op) -> i64 { match o { Add(n) => n, Neg(n) => 0 - n, Zero => 0 } }\nfn main() { println(eval(Op.Add(5)).to_string()); println(eval(Op.Neg(3)).to_string()); println(eval(Op.Zero).to_string()) }",
    "enum Color { Red, Green, Blue }\nfn main() { let c = Color.Green; match c { Red => { println(\"r\") } Green => { println(\"g\") } Blue => { println(\"b\") } } }",
    "enum Op { Add(i64), Zero }\nfn main() { let e = Op.Add(7); match e { Add(n) => { println(n.to_string()) } _ => { println(\"other\") } } }",
    "enum Op { Add(i64), Zero }\nfn main() { let e = Op.Zero; match e { Add(n) => { println(n.to_string()) } _ => { println(\"other\") } } }",
    "enum Op { Add(i64), Zero }\nfn main() { let x = match Op.Add(20) { Add(n) => n * 2, Zero => 0 }; println(x.to_string()) }",
    "enum Op { Add(i64), Zero }\nfn mk(n: i64) -> Op { if n > 0 { return Op.Add(n); } Op.Zero }\nfn main() { println(match mk(4) { Add(n) => n + 100, Zero => 0 }.to_string()); println(match mk(0 - 1) { Add(n) => n, Zero => 99 }.to_string()) }",
    // Slice 15: String fields in structs — the literal owns its fields
    // (borrows materialize in), whole-struct binding deep-copies, scope exit
    // frees each String field, params/returns ride the materialize-on-borrow
    // contract. All valgrind-gated.
    "struct User { name: String, age: i64 }\nfn main() { let u = User { name: \"ada\", age: 36 }; println(u.name); println(u.age.to_string()) }",
    "struct User { name: String, age: i64 }\nfn main() { let u = User { name: \"ada\", age: 36 }; let v = u; println(v.name) }",
    "struct User { name: String, age: i64 }\nfn describe(u: User) -> String { u.name + \"/\" + u.age.to_string() }\nfn main() { println(describe(User { name: \"bo\", age: 7 })) }",
    "struct User { name: String, age: i64 }\nfn mk(n: String, a: i64) -> User { User { name: n, age: a } }\nfn main() { let u = mk(\"kara\", 1); println(u.name); println(u.age.to_string()) }",
    "struct Pair { a: String, b: String }\nfn main() { let p = Pair { a: \"x\" + \"1\", b: \"y\" }; println(p.a + p.b) }",
    "struct User { name: String, age: i64 }\nfn main() { let mut u = User { name: \"one\", age: 1 }; u = User { name: \"two\", age: 2 }; println(u.name); println(u.age.to_string()) }",
];

const ENTRY: &str = ";;;KARA_ENTRY;;;";

fn kara_str_lit(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Build the selfhost modules + a driver that emits IR (separated by `ENTRY`)
/// for every corpus program, run it, and return the raw stdout — or `None` on a
/// benign link skip.
fn build_and_emit_all() -> Option<String> {
    let tmp = std::env::temp_dir().join(format!("karac-selfhost-codegen-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        "[package]\nname = \"cg\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let selfhost_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("selfhost/src");
    for f in [
        "span.kara",
        "token.kara",
        "lexer.kara",
        "ast.kara",
        "parser.kara",
        "codegen.kara",
    ] {
        std::fs::copy(selfhost_src.join(f), tmp.join("src").join(f))
            .unwrap_or_else(|e| panic!("copy selfhost module {f}: {e}"));
    }

    let mut driver = String::from(
        "import parser.parse_program;\n\
         import codegen.emit_program;\n\
         \n\
         fn dump(src: String) with panics {\n\
         \x20   println(\";;;KARA_ENTRY;;;\");\n\
         \x20   print(emit_program(parse_program(src)));\n\
         }\n\
         fn main() {\n",
    );
    for input in CORPUS {
        driver.push_str(&format!("    dump(\"{}\");\n", kara_str_lit(input)));
    }
    driver.push_str("}\n");
    std::fs::write(tmp.join("src").join("main.kara"), &driver).unwrap();

    let build = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&tmp)
        .args(["build"])
        .env_remove("KARAC_RUNTIME")
        .output()
        .expect("spawn karac build");
    let berr = String::from_utf8_lossy(&build.stderr);
    let bin = tmp.join("cg");
    if !bin.exists() {
        let crashed = berr.contains("panicked at") || build.status.code().is_none();
        let compile_err = crashed
            || berr.contains("error[")
            || berr.contains("codegen failed")
            || berr.contains("parse error")
            || berr.contains("Module verification failed");
        assert!(
            !compile_err,
            "self-hosted emitter FAILED TO COMPILE (port regression):\n{berr}\n\
             --- driver ---\n{driver}"
        );
        eprintln!("skip: selfhost codegen oracle — driver did not link:\n{berr}");
        let _ = std::fs::remove_dir_all(&tmp);
        return None;
    }
    let run = Command::new(&bin).output().expect("run emitter driver");
    assert!(
        run.status.success(),
        "emitter driver exited nonzero:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let out = String::from_utf8_lossy(&run.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&tmp);
    Some(out)
}

/// Run LLVM IR text through `karac_jit_runner`, returning (stdout, exit code).
fn run_ir(ir: &str) -> (String, i32) {
    let tmp = std::env::temp_dir().join(format!("karac-cg-ir-{}.ll", std::process::id()));
    std::fs::write(&tmp, ir).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac_jit_runner"))
        .arg(&tmp)
        .output()
        .expect("spawn karac_jit_runner");
    let _ = std::fs::remove_file(&tmp);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// Run a source program through the seed's `karac run`, returning (stdout, code).
fn seed_run(src: &str) -> (String, i32) {
    let tmp = std::env::temp_dir().join(format!("karac-cg-seed-{}.kara", std::process::id()));
    std::fs::write(&tmp, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["run", tmp.to_str().unwrap()])
        .output()
        .expect("spawn karac run");
    let _ = std::fs::remove_file(&tmp);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn selfhost_codegen_matches_seed_run() {
    let Some(all) = build_and_emit_all() else {
        return;
    };
    // Split the driver's stdout into per-program IR blocks.
    let blocks: Vec<&str> = all.split(ENTRY).skip(1).collect();
    assert_eq!(
        blocks.len(),
        CORPUS.len(),
        "expected {} IR blocks, got {}",
        CORPUS.len(),
        blocks.len()
    );
    for (i, (src, ir)) in CORPUS.iter().zip(blocks.iter()).enumerate() {
        let ir = ir.trim_start_matches('\n');
        let (kara_out, kara_code) = run_ir(ir);
        let (seed_out, seed_code) = seed_run(src);
        assert_eq!(
            kara_out, seed_out,
            "stdout mismatch at corpus[{i}] ({src:?}):\n  Kāra-emitted: {kara_out:?}\n  \
             seed run:     {seed_out:?}\n--- emitted IR ---\n{ir}"
        );
        assert_eq!(
            kara_code, seed_code,
            "exit-code mismatch at corpus[{i}] ({src:?}): Kāra {kara_code} vs seed {seed_code}"
        );
        leak_audit(i, src, ir);
    }
}

/// Memory audit for the emitted IR (Slice 9 — drop insertion): compile the
/// block with clang and run it under valgrind, failing on any leak or invalid
/// free. Skips silently when clang or valgrind is unavailable (macOS local
/// runs); the Linux CI leg is the authoritative gate, matching the
/// memory-sanitizer convention. The audit exists because the first drop
/// implementation leaked in loops while passing every stdout check — output
/// parity alone cannot see a leak.
fn leak_audit(i: usize, src: &str, ir: &str) {
    use std::sync::OnceLock;
    static TOOLS: OnceLock<bool> = OnceLock::new();
    let have = *TOOLS.get_or_init(|| {
        let ok = |c: &str| {
            Command::new(c)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        };
        let both = ok("clang") && ok("valgrind");
        if !both {
            eprintln!("selfhost_codegen: clang/valgrind unavailable — leak audit skipped");
        }
        both
    });
    if !have {
        return;
    }
    let dir = std::env::temp_dir().join(format!("selfhost_cg_leak_{i}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ll = dir.join("prog.ll");
    let bin = dir.join("prog");
    std::fs::write(&ll, ir).unwrap();
    let cc = Command::new("clang")
        .arg(&ll)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap();
    assert!(
        cc.status.success(),
        "clang failed on corpus[{i}] ({src:?}):\n{}",
        String::from_utf8_lossy(&cc.stderr)
    );
    let vg = Command::new("valgrind")
        .args(["--leak-check=full", "--error-exitcode=99", "--quiet"])
        .arg(&bin)
        .output()
        .unwrap();
    let vg_err = String::from_utf8_lossy(&vg.stderr);
    assert!(
        vg.status.code() != Some(99) && !vg_err.contains("definitely lost"),
        "valgrind flagged corpus[{i}] ({src:?}):\n{vg_err}\n--- emitted IR ---\n{ir}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
