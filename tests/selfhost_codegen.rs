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
    // Slice 16: Vec[String] — kind from the LET ANNOTATION (the single-pass
    // emitter cannot infer element kinds from later pushes); the vec OWNS its
    // elements (borrowed pushes materialize, temps transfer; reads borrow);
    // scope exit frees each element then the buffer. Valgrind-gated.
    "fn main() { let v: Vec[String] = Vec.new(); println(v.len().to_string()) }",
    "fn main() { let mut v: Vec[String] = Vec.new(); v.push(\"alpha\"); v.push(\"beta\"); println(v.len().to_string()); println(v[0]); println(v[1]) }",
    "fn main() { let mut v: Vec[String] = Vec.new(); v.push(\"x\" + \"1\"); v.push(\"y\" + \"2\"); let mut i = 0; while i < v.len() { println(v[i]); i = i + 1; } }",
    "fn main() { let mut v: Vec[String] = Vec.new(); let mut i = 0; while i < 4 { v.push(\"n\" + i.to_string()); i = i + 1; } println(v[3]); println(v.len().to_string()) }",
    "fn main() { let mut v: Vec[String] = Vec.new(); v.push(\"keep\"); let s = v[0]; println(s + \"-copy\"); println(v[0]) }",
    // Slice 17: for-over-bytes, string ==/!=, substring, bool-returning fns,
    // entry-block let slots (a let in a loop body frees its previous value —
    // the Slice-9 loop-let leak deferral, closed). All valgrind-gated.
    "fn main() { let s = \"abcabc\"; let mut n = 0; for b in s.bytes() { if b == 97 { n = n + 1; } } println(n.to_string()) }",
    "fn main() { println((\"hi\" == \"hi\").to_string()); println((\"hi\" == \"ho\").to_string()); println((\"a\" != \"b\").to_string()); println((\"ab\" == \"abc\").to_string()) }",
    "fn main() { let s = \"hello world\"; println(s.substring(0, 5)); println(s.substring(6, 11)) }",
    "fn is_vowel(b: u8) -> bool { b == 97 or b == 101 or b == 105 or b == 111 or b == 117 }\nfn main() { let s = \"banana\"; let mut n = 0; for b in s.bytes() { if is_vowel(b) { n = n + 1; } } println(n.to_string()) }",
    "fn main() { let mut v: Vec[String] = Vec.new(); let w = \"split me up\"; let mut i = 0; let mut st = 0; for b in w.bytes() { if b == 32 { v.push(w.substring(st, i)); st = i + 1; } i = i + 1; } v.push(w.substring(st, i)); let mut j = 0; while j < v.len() { println(v[j]); j = j + 1; } }",
    // THE TOKENIZER CAPSTONE: the first parser-shaped program compiled by the
    // Kara-authored backend — byte classification via a bool-returning helper,
    // token-text extraction via substring, accumulation into Vec[String],
    // keyword recognition via string equality. Composes eight slices.
    "fn is_alnum(b: u8) -> bool { (b >= 48 and b <= 57) or (b >= 97 and b <= 122) or (b >= 65 and b <= 90) or b == 95 }\nfn main() { let src = \"let x1 = 42 + foo * 7\"; let mut toks: Vec[String] = Vec.new(); let mut i = 0; let mut start = 0 - 1; for b in src.bytes() { if is_alnum(b) { if start < 0 { start = i; } } else { if start >= 0 { toks.push(src.substring(start, i)); start = 0 - 1; } if b != 32 { toks.push(src.substring(i, i + 1)); } } i = i + 1; } if start >= 0 { toks.push(src.substring(start, i)); } let mut j = 0; while j < toks.len() { let t = toks[j]; if t == \"let\" { println(\"kw \" + t) } else { println(\"tok \" + t) } j = j + 1; } }",
    // Slice 18: Vec[<struct>] — vectors of heap-bearing structs (kind
    // 2000+si; stride from field kinds; per-element String-field frees in the
    // drop loop). The tokenizer upgrades to real Token structs {kind, text}.
    "struct Token { kind: i64, text: String }\nfn main() { let mut toks: Vec[Token] = Vec.new(); toks.push(Token { kind: 1, text: \"let\" }); toks.push(Token { kind: 2, text: \"x\" + \"1\" }); let mut i = 0; while i < toks.len() { let t = toks[i]; println(t.kind.to_string() + \":\" + t.text); i = i + 1; } }",
    "struct P { x: i64, y: i64 }\nfn main() { let mut ps: Vec[P] = Vec.new(); let mut i = 0; while i < 4 { ps.push(P { x: i, y: i * i }); i = i + 1; } let mut s = 0; let mut j = 0; while j < ps.len() { s = s + ps[j].y; j = j + 1; } println(s.to_string()) }",
    // STRUCT TOKENIZER: kinds classified (1 word, 2 number, 3 symbol), text
    // extracted via substring, accumulated into Vec[Token] — the tokenizer
    // capstone upgraded to parser-shaped DATA, not just strings.
    "struct Token { kind: i64, text: String }\nfn is_digit(b: u8) -> bool { b >= 48 and b <= 57 }\nfn is_alpha(b: u8) -> bool { (b >= 97 and b <= 122) or (b >= 65 and b <= 90) or b == 95 }\nfn main() { let src = \"x1 = 42 + foo7\"; let mut toks: Vec[Token] = Vec.new(); let mut i = 0; let mut start = 0 - 1; let mut num = false; for b in src.bytes() { if is_alpha(b) or is_digit(b) { if start < 0 { start = i; num = is_digit(b); } } else { if start >= 0 { if num { toks.push(Token { kind: 2, text: src.substring(start, i) }); } else { toks.push(Token { kind: 1, text: src.substring(start, i) }); } start = 0 - 1; } if b != 32 { toks.push(Token { kind: 3, text: src.substring(i, i + 1) }); } } i = i + 1; } if start >= 0 { if num { toks.push(Token { kind: 2, text: src.substring(start, i) }); } else { toks.push(Token { kind: 1, text: src.substring(start, i) }); } } let mut j = 0; while j < toks.len() { let t = toks[j]; println(t.kind.to_string() + \" \" + t.text); j = j + 1; } }",
    // Slice 19: `ref Vec` params (borrow ABI — bit-copy pass, no caller
    // materialization, no callee free) + SHORT-CIRCUIT and/or (the old
    // non-short-circuit lowering read past the end in `p < len and toks[p]`
    // guards — an OOB read valgrind caught in the parser capstone).
    "fn total(xs: ref Vec[i64]) -> i64 { let mut s = 0; let mut i = 0; while i < xs.len() { s = s + xs[i]; i = i + 1; } s }\nfn main() { let mut v: Vec[i64] = Vec.new(); v.push(3); v.push(4); v.push(5); println(total(v).to_string()); println(v.len().to_string()) }",
    "fn first_word(ws: ref Vec[String]) -> String { ws[0] }\nfn main() { let mut v: Vec[String] = Vec.new(); v.push(\"lead\"); v.push(\"tail\"); println(first_word(v)); println(v[1]) }",
    "fn main() { let mut v: Vec[i64] = Vec.new(); v.push(9); let mut p = 0; while p < v.len() and v[p] > 0 { p = p + 1; } println(p.to_string()) }",
    // THE PARSER CAPSTONE: recursive-descent expression evaluation with
    // precedence over a token stream — parse fns take (ref Vec[i64], pos) and
    // return R{v,p} structs; ops encoded negative. "2+3*4-6/2" = 11.
    "struct R { v: i64, p: i64 }\nfn parse_primary(toks: ref Vec[i64], pos: i64) -> R { R { v: toks[pos], p: pos + 1 } }\nfn parse_term(toks: ref Vec[i64], pos: i64) -> R { let r0 = parse_primary(toks, pos); let mut v = r0.v; let mut p = r0.p; while p < toks.len() and (toks[p] == 0 - 42 or toks[p] == 0 - 47) { let op = toks[p]; let rhs = parse_primary(toks, p + 1); if op == 0 - 42 { v = v * rhs.v; } else { v = v / rhs.v; } p = rhs.p; } R { v: v, p: p } }\nfn parse_expr(toks: ref Vec[i64], pos: i64) -> R { let r0 = parse_term(toks, pos); let mut v = r0.v; let mut p = r0.p; while p < toks.len() and (toks[p] == 0 - 43 or toks[p] == 0 - 45) { let op = toks[p]; let rhs = parse_term(toks, p + 1); if op == 0 - 43 { v = v + rhs.v; } else { v = v - rhs.v; } p = rhs.p; } R { v: v, p: p } }\nfn main() { let mut toks: Vec[i64] = Vec.new(); toks.push(2); toks.push(0 - 43); toks.push(3); toks.push(0 - 42); toks.push(4); toks.push(0 - 45); toks.push(6); toks.push(0 - 47); toks.push(2); let r = parse_expr(toks, 0); println(r.v.to_string()) }",
    // Slice 20: `mut ref` params — true by-reference (the callee receives a
    // pointer to the caller's slot, aliased to the canonical %v name via a
    // no-op GEP); call sites pass the slot pointer for `mut`-marked args.
    "fn bump(n: mut ref i64) { n = n + 7; }\nfn main() { let mut c = 10; bump(mut c); bump(mut c); println(c.to_string()) }",
    "fn add_tok(v: mut ref Vec[i64], x: i64) { v.push(x * 2) }\nfn main() { let mut v: Vec[i64] = Vec.new(); add_tok(mut v, 3); add_tok(mut v, 5); println(v.len().to_string()); println((v[0] + v[1]).to_string()) }",
    "fn add_word(v: mut ref Vec[String], w: String) { v.push(w + \"!\") }\nfn main() { let mut v: Vec[String] = Vec.new(); add_word(mut v, \"hey\"); add_word(mut v, \"ho\"); println(v[0]); println(v[1]) }",
    // Tokenizer helper shape: mut-ref token sink + ref source string.
    "fn emit_tok(toks: mut ref Vec[String], src: ref String, a: i64, b: i64) { toks.push(src.substring(a, b)) }\nfn main() { let src = \"ab cd\"; let mut toks: Vec[String] = Vec.new(); emit_tok(mut toks, src, 0, 2); emit_tok(mut toks, src, 3, 5); println(toks[0]); println(toks[1]); println(src) }",
    // Slice 21: FIELD ASSIGNMENT — GEP the struct slot's field and store;
    // a String field frees its old buffer first; composes with mut-ref
    // struct params (the aliased slot pointer). Valgrind-gated.
    "struct User { name: String, age: i64 }\nfn main() { let mut u = User { name: \"ada\", age: 36 }; u.age = 40; println(u.age.to_string()); println(u.name) }",
    "struct User { name: String, age: i64 }\nfn main() { let mut u = User { name: \"ada\", age: 36 }; u.name = \"grace\"; println(u.name); println(u.age.to_string()) }",
    "struct User { name: String, age: i64 }\nfn birthday(u: mut ref User) { u.age = u.age + 1; }\nfn main() { let mut u = User { name: \"bo\", age: 9 }; birthday(mut u); birthday(mut u); println(u.age.to_string()) }",
    "struct User { name: String, age: i64 }\nfn rename(u: mut ref User, n: String) { u.name = n + \"!\"; }\nfn main() { let mut u = User { name: \"ada\", age: 1 }; rename(mut u, \"kay\"); println(u.name) }",
    // A counter-struct threaded through helpers — compiler-shaped state.
    "struct Cnt { words: i64, syms: i64 }\nfn tally(c: mut ref Cnt, b: u8) { if (b >= 97 and b <= 122) { c.words = c.words + 1; } else { if b != 32 { c.syms = c.syms + 1; } } }\nfn main() { let src = \"ab + cd\"; let mut c = Cnt { words: 0, syms: 0 }; for b in src.bytes() { tally(mut c, b); } println(c.words.to_string() + \"/\" + c.syms.to_string()) }",
    // Slice 23: MID-FN RETURN runs the scope-exit frees — every early-return
    // path releases owned heap slots before its ret (was a documented leak).
    // Each program holds owned String/struct locals alive across an early
    // return; the leak_audit leg is the real assertion here.
    "fn find(v: ref Vec[i64], t: i64) -> i64 { let tag = \"probe\".to_string(); let mut i = 0; while i < v.len() { if v[i] == t { return i; } i = i + 1; } return 0 - 1; }\nfn main() { let mut v: Vec[i64] = Vec.new(); v.push(4); v.push(9); v.push(7); println(find(v, 9).to_string()); println(find(v, 5).to_string()) }",
    "fn shout(w: String, early: bool) -> String { let pad = \"..\".to_string(); if early { return w + \"!\"; } let out = pad + w; return out; }\nfn main() { println(shout(\"hey\".to_string(), true)); println(shout(\"ho\".to_string(), false)) }",
    // Early-returned BORROWS (a local on one path, an owned param on the
    // other): materialized into an owned copy, then the originals freed.
    "fn pick(a: String, big: bool) -> String { let b = \"zed\".to_string(); if big { return b; } return a; }\nfn main() { println(pick(\"aa\".to_string(), true)); println(pick(\"bb\".to_string(), false)) }",
    // Early-returned heap-struct borrows on both paths.
    "struct Pair { tag: String, n: i64 }\nfn stamp(n: i64) -> Pair { let p = Pair { tag: \"id-\".to_string() + n.to_string(), n: n }; if n > 9 { return p; } let q = Pair { tag: \"small\".to_string(), n: n }; return q; }\nfn main() { let p1 = stamp(12); println(p1.tag); let p2 = stamp(3); println(p2.tag) }",
    // Return from inside an enum-match arm with an owned local alive.
    "enum Verdict { Pass, Fail(i64) }\nfn judge(v: Verdict) -> String { let label = \"case \".to_string(); match v { Fail(code) => { return label + \"failed-\" + code.to_string(); } Pass => {} } return label + \"ok\"; }\nfn main() { println(judge(Verdict.Fail(7))); println(judge(Verdict.Pass)) }",
    // Slice 24: STRING-PAYLOAD ENUM VARIANTS (the Token.Identifier shape).
    // A str-enum widens to { tag, i64, { ptr, i64 } } and OWNS its String
    // payload: tag-conditional copy on borrow-bind, tag-conditional free on
    // drop; a match binds the payload as a borrow. Valgrind-gated throughout.
    "enum Tok { Plus, Num(i64), Ident(String) }\nfn show(t: Tok) -> String { match t { Ident(name) => { return \"id:\".to_string() + name; } Num(n) => { return \"num:\".to_string() + n.to_string(); } Plus => {} } return \"plus\".to_string(); }\nfn main() { let a = Tok.Ident(\"foo\".to_string()); println(show(a)); println(show(Tok.Num(42))); println(show(Tok.Plus)) }",
    // Enum-returning fns (borrow + temp returns), ref-enum params, rebind
    // (deep copy), reassignment (old payload freed), loop construction.
    "enum Tok { Plus, Num(i64), Ident(String) }\nfn classify(w: String) -> Tok { if w == \"+\" { return Tok.Plus; } if w == \"42\" { return Tok.Num(42); } return Tok.Ident(w); }\nfn name_of(t: ref Tok) -> String { match t { Ident(name) => { return \"<\".to_string() + name + \">\"; } Num(n) => { return \"#\".to_string() + n.to_string(); } Plus => {} } return \"+\".to_string(); }\nfn main() { let mut t = classify(\"foo\".to_string()); println(name_of(t)); let u = t; println(name_of(u)); t = classify(\"42\".to_string()); println(name_of(t)); t = classify(\"+\".to_string()); println(name_of(t)); let mut i = 0; while i < 3 { let w = classify(\"loop\".to_string()); println(name_of(w)); i = i + 1; } }",
    // Statement-position match printing a borrowed String payload.
    "enum Msg { Quit, Say(String) }\nfn main() { let m = Msg.Say(\"hello\".to_string() + \" there\"); match m { Say(text) => { println(text); } Quit => { println(\"quit\"); } } match Msg.Quit { Say(text) => { println(text); } Quit => { println(\"bye\"); } } }",
    // Slice 25: VEC RETURNS + OWNED-VEC PARAMS — Vec[i64]/Vec[String]/
    // Vec[<struct>] cross fn boundaries. A returned borrow deep-copies
    // (elements included); owned vec args move in (caller deep-copies a
    // borrow, callee frees at epilogue); a discarded vec result is freed.
    "fn nums(n: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); let mut i = 0; while i < n { v.push(i * 10); i = i + 1; } return v; }\nfn main() { let ns = nums(4); let mut t = 0; let mut i = 0; while i < ns.len() { t = t + ns[i]; i = i + 1; } println(t.to_string()) }",
    "fn words() -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push(\"alpha\".to_string()); v.push(\"beta\".to_string()); return v; }\nfn join(v: ref Vec[String]) -> String { let mut s = \"\".to_string(); let mut i = 0; while i < v.len() { s = s + v[i] + \"|\"; i = i + 1; } return s; }\nfn main() { let ws = words(); println(ws[0]); println(join(ws)); println(ws[1]) }",
    // An owned Vec[String] param: the arg moves in, the callee frees it.
    "fn words() -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push(\"alpha\".to_string()); v.push(\"beta\".to_string()); return v; }\nfn total_len(v: Vec[String]) -> i64 { let mut n = 0; let mut i = 0; while i < v.len() { let w = v[i]; n = n + w.len(); i = i + 1; } return n; }\nfn main() { println(total_len(words()).to_string()); let ws = words(); println(total_len(ws).to_string()) }",
    // Vec[<struct with String>] return + rebind (deep copy) + discard.
    "struct Tk { kind: i64, text: String }\nfn toks() -> Vec[Tk] { let mut v: Vec[Tk] = Vec.new(); v.push(Tk { kind: 1, text: \"x1\".to_string() }); v.push(Tk { kind: 3, text: \"=\".to_string() }); return v; }\nfn main() { let ts = toks(); println(ts[0].text); println(ts[1].kind.to_string()); toks(); let ts2 = ts; println(ts2[1].text) }",
    // A mid-fn vec return path (borrow materialized before the scope frees).
    "fn firstn(cap: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); let mut i = 0; while i < 10 { if i == cap { return v; } v.push(i); i = i + 1; } return v; }\nfn main() { let v = firstn(3); println(v.len().to_string()); println((v[0] + v[1] + v[2]).to_string()) }",
    // Slice 25 CAPSTONE: a MINI-LEXER MODULE compiled whole — flat
    // Span-carrying tokens (kind/text/offset/length), byte-classifier
    // helpers, keyword promotion via a ref-String helper, and
    // lex(src: String) -> Vec[Token] returned across the fn boundary.
    "struct Token { kind: i64, text: String, offset: i64, length: i64 }\nfn is_word(b: u8) -> bool {\n    if b >= 97 and b <= 122 {\n        return true;\n    }\n    if b >= 65 and b <= 90 {\n        return true;\n    }\n    return b == 95;\n}\nfn is_digit(b: u8) -> bool {\n    return b >= 48 and b <= 57;\n}\nfn is_kw(w: ref String) -> bool {\n    if w == \"let\" {\n        return true;\n    }\n    if w == \"fn\" {\n        return true;\n    }\n    return w == \"if\";\n}\nfn lex(src: String) -> Vec[Token] {\n    let mut toks: Vec[Token] = Vec.new();\n    let mut start = 0 - 1;\n    let mut kind = 0;\n    let mut i = 0;\n    for b in src.bytes() {\n        if start >= 0 {\n            let mut cont = false;\n            if kind == 1 and (is_word(b) or is_digit(b)) {\n                cont = true;\n            }\n            if kind == 2 and is_digit(b) {\n                cont = true;\n            }\n            if not cont {\n                let w = src.substring(start, i);\n                let mut k = kind;\n                if kind == 1 and is_kw(w) {\n                    k = 4;\n                }\n                toks.push(Token { kind: k, text: w, offset: start, length: i - start });\n                start = 0 - 1;\n            }\n        }\n        if start < 0 {\n            if is_word(b) {\n                start = i;\n                kind = 1;\n            } else if is_digit(b) {\n                start = i;\n                kind = 2;\n            } else if b != 32 {\n                toks.push(Token { kind: 3, text: src.substring(i, i + 1), offset: i, length: 1 });\n            }\n        }\n        i = i + 1;\n    }\n    if start >= 0 {\n        let w = src.substring(start, src.len());\n        let mut k = kind;\n        if kind == 1 and is_kw(w) {\n            k = 4;\n        }\n        toks.push(Token { kind: k, text: w, offset: start, length: src.len() - start });\n    }\n    return toks;\n}\nfn main() {\n    let toks = lex(\"let x1 = 42 + foo * 7\".to_string());\n    let mut i = 0;\n    while i < toks.len() {\n        let t = toks[i];\n        println(t.kind.to_string() + \"@\" + t.offset.to_string() + \"+\" + t.length.to_string() + \":\" + t.text);\n        i = i + 1;\n    }\n}",
    // Slice 26: the two SILENT mis-lowerings made real. (1) int-literal
    // match — icmp/branch chain per IntPat arm, binding/wildcard catch-all,
    // value position via the i64 result slot; (2) string ordering < <= > >=
    // — memcmp three-way with length tie-break.
    "fn grade(s: i64) -> String { let label = \"grade \".to_string(); match s { 0 => { return label + \"zero\"; } 1 => { return label + \"one\"; } n => { return label + \"n\" + n.to_string(); } } }\nfn main() { println(grade(0)); println(grade(1)); println(grade(7)) }",
    "fn main() { let v = match 2 { 1 => 10, 2 => 20, _ => 30 }; println(v.to_string()); let w = match 9 { 1 => 10, 2 => 20, _ => 30 }; println(w.to_string()) }",
    "fn ord(a: String, b: String) -> String { if a < b { return \"lt\".to_string(); } if a > b { return \"gt\".to_string(); } return \"eq\".to_string(); }\nfn main() { println(ord(\"ab\".to_string(), \"abc\".to_string())); println(ord(\"b\".to_string(), \"abc\".to_string())); println(ord(\"same\".to_string(), \"same\".to_string())); println((\"0\" <= \"9\").to_string()); println((\"z\" < \"a\").to_string()) }",
    // Digit classification via string ordering — the lexer idiom that
    // exposed the silent gap in the first place.
    "fn is_digit_s(f: String) -> bool { return f >= \"0\" and f <= \"9\"; }\nfn main() { let s = \"a7\"; println(is_digit_s(s.substring(0, 1)).to_string()); println(is_digit_s(s.substring(1, 2)).to_string()) }",
    // Slice 27: INHERENT IMPL METHODS on structs — all three receiver
    // modes (owned / ref / mut ref self as arg 0 through the existing
    // param machinery), self.field reads/writes, user-method dispatch,
    // and Type.assoc() calls (sig key "Type.name", symbol u_Type_m_name).
    "struct Counter { n: i64, tag: String }\nimpl Counter {\n    fn new(tag: String) -> Counter {\n        return Counter { n: 0, tag: tag };\n    }\n    fn bump(mut ref self, by: i64) {\n        self.n = self.n + by;\n    }\n    fn label(ref self) -> String {\n        return self.tag + \"=\" + self.n.to_string();\n    }\n    fn consume(self) -> i64 {\n        return self.n * 10;\n    }\n}\nfn main() {\n    let mut c = Counter.new(\"hits\".to_string());\n    c.bump(3);\n    c.bump(4);\n    println(c.label());\n    println(c.consume().to_string());\n}",
    // The compiler-shaped capstone shape: a Scanner with mutable scan
    // state and substring extraction through self fields — the real
    // lexer.kara architecture.
    "struct Scanner { src: String, pos: i64 }\nimpl Scanner {\n    fn new(src: String) -> Scanner {\n        return Scanner { src: src, pos: 0 };\n    }\n    fn done(ref self) -> bool {\n        return self.pos >= self.src.len();\n    }\n    fn advance(mut ref self, by: i64) {\n        self.pos = self.pos + by;\n    }\n    fn take(mut ref self, n: i64) -> String {\n        let w = self.src.substring(self.pos, self.pos + n);\n        self.pos = self.pos + n;\n        return w;\n    }\n    fn rest(ref self) -> String {\n        return self.src.substring(self.pos, self.src.len());\n    }\n}\nfn main() {\n    let mut s = Scanner.new(\"let x = 7\".to_string());\n    println(s.take(3));\n    s.advance(1);\n    println(s.take(1));\n    s.advance(1);\n    println(s.rest());\n    println(s.done().to_string());\n    s.advance(3);\n    println(s.done().to_string());\n}",
    // Slice 28 CAPSTONE: the METHOD-BASED LEXER — a Lexer struct whose
    // next_token() drives INTRA-METHOD self calls (self.skip_ws() from a
    // mut-ref method, self.peek() from both modes), classification via
    // string ordering on 1-byte peeks, and Token construction per scan.
    // The true lexer.kara shape; needed ZERO new emitter code.
    "struct Token { kind: i64, text: String }\nstruct Lexer { src: String, pos: i64 }\nfn is_wordch(c: String) -> bool {\n    if c >= \"a\" and c <= \"z\" {\n        return true;\n    }\n    if c >= \"A\" and c <= \"Z\" {\n        return true;\n    }\n    return c == \"_\";\n}\nfn is_digitch(c: String) -> bool {\n    return c >= \"0\" and c <= \"9\";\n}\nimpl Lexer {\n    fn new(src: String) -> Lexer {\n        return Lexer { src: src, pos: 0 };\n    }\n    fn done(ref self) -> bool {\n        return self.pos >= self.src.len();\n    }\n    fn peek(ref self) -> String {\n        return self.src.substring(self.pos, self.pos + 1);\n    }\n    fn skip_ws(mut ref self) {\n        while self.pos < self.src.len() and self.peek() == \" \" {\n            self.pos = self.pos + 1;\n        }\n    }\n    fn next_token(mut ref self) -> Token {\n        self.skip_ws();\n        let start = self.pos;\n        if is_wordch(self.peek()) {\n            while self.pos < self.src.len() and (is_wordch(self.peek()) or is_digitch(self.peek())) {\n                self.pos = self.pos + 1;\n            }\n            let w = self.src.substring(start, self.pos);\n            if w == \"let\" or w == \"fn\" or w == \"if\" {\n                return Token { kind: 4, text: w };\n            }\n            return Token { kind: 1, text: w };\n        }\n        if is_digitch(self.peek()) {\n            while self.pos < self.src.len() and is_digitch(self.peek()) {\n                self.pos = self.pos + 1;\n            }\n            return Token { kind: 2, text: self.src.substring(start, self.pos) };\n        }\n        self.pos = self.pos + 1;\n        return Token { kind: 3, text: self.src.substring(start, self.pos) };\n    }\n}\nfn main() {\n    let mut lx = Lexer.new(\"let x1 = 42 + foo\".to_string());\n    while not lx.done() {\n        let t = lx.next_token();\n        println(t.kind.to_string() + \":\" + t.text);\n        lx.skip_ws();\n    }\n}",
    // Slice 29: Option[i64]/Option[String] as SYNTHETIC ENUMS (Option$i /
    // Option$s registered first in the en_ tables, None=0/Some=1) — the
    // entire tag-conditional ownership + match + ABI machinery inherited
    // verbatim. Some(x) constructs by payload kind; None is a negative
    // sentinel kind resolved by context (annotation/return/param).
    "fn find(v: ref Vec[i64], t: i64) -> Option[i64] {\n    let mut i = 0;\n    while i < v.len() {\n        if v[i] == t {\n            return Some(i);\n        }\n        i = i + 1;\n    }\n    return None;\n}\nfn label(o: Option[String]) -> String {\n    match o {\n        Some(s) => {\n            return \"got:\".to_string() + s;\n        }\n        None => {}\n    }\n    return \"nothing\".to_string();\n}\nfn pick(flag: bool) -> Option[String] {\n    if flag {\n        return Some(\"payload\".to_string() + \"!\");\n    }\n    return None;\n}\nfn main() {\n    let mut v: Vec[i64] = Vec.new();\n    v.push(4);\n    v.push(9);\n    match find(v, 9) {\n        Some(i) => {\n            println(\"at \" + i.to_string());\n        }\n        None => {\n            println(\"missing\");\n        }\n    }\n    match find(v, 5) {\n        Some(i) => {\n            println(\"at \" + i.to_string());\n        }\n        None => {\n            println(\"missing\");\n        }\n    }\n    println(label(pick(true)));\n    println(label(pick(false)));\n    let held = pick(true);\n    println(label(held));\n    let empty: Option[String] = None;\n    println(label(empty));\n}",
    // Slice 30: Result[T, E] over {i64,String}^2 — four synthetic enums
    // (Result$ii..$ss, fixed kinds 1002..1005) sharing ONE widened
    // layout, so Ok/Err construction is context-free under the -10
    // sentinel; annotation/return/param/assign contexts resolve it.
    "fn parse_num(s: String) -> Result[i64, String] {\n    if s == \"42\" {\n        return Ok(42);\n    }\n    return Err(\"bad: \".to_string() + s);\n}\nfn render(r: Result[i64, String]) -> String {\n    match r {\n        Ok(n) => {\n            return \"ok \".to_string() + n.to_string();\n        }\n        Err(e) => {\n            return \"err \".to_string() + e;\n        }\n    }\n    return \"\".to_string();\n}\nfn flip(r: Result[String, i64]) -> String {\n    match r {\n        Ok(s) => {\n            return s;\n        }\n        Err(c) => {\n            return \"code\".to_string() + c.to_string();\n        }\n    }\n    return \"\".to_string();\n}\nfn main() {\n    println(render(parse_num(\"42\".to_string())));\n    println(render(parse_num(\"xx\".to_string())));\n    println(flip(Ok(\"fine\".to_string())));\n    println(flip(Err(7)));\n    let held: Result[i64, String] = Err(\"kept\".to_string());\n    println(render(held));\n    let mut sw: Result[i64, String] = Ok(1);\n    sw = Err(\"swapped\".to_string());\n    println(render(sw));\n}",
    // Slice 31: ENUM METHOD RECEIVERS — impl on an enum (the token.kara
    // idiom): match self in ref-self methods binding String/i64 payloads,
    // and an owned-self consumer (tag-conditional receiver deep copy).
    "enum Tok { Plus, Num(i64), Ident(String) }\nimpl Tok {\n    fn describe(ref self) -> String {\n        match self {\n            Ident(name) => {\n                return \"ident:\".to_string() + name;\n            }\n            Num(n) => {\n                return \"num:\".to_string() + n.to_string();\n            }\n            Plus => {}\n        }\n        return \"plus\".to_string();\n    }\n    fn weight(ref self) -> i64 {\n        match self {\n            Ident(name) => {\n                return name.len();\n            }\n            Num(n) => {\n                return n;\n            }\n            Plus => {}\n        }\n        return 0;\n    }\n    fn into_code(self) -> i64 {\n        match self {\n            Ident(name) => {\n                return 100 + name.len();\n            }\n            Num(n) => {\n                return n;\n            }\n            Plus => {}\n        }\n        return 0 - 1;\n    }\n}\nfn main() {\n    let a = Tok.Ident(\"foo\".to_string());\n    let b = Tok.Num(42);\n    let c = Tok.Plus;\n    println(a.describe());\n    println(b.describe());\n    println(c.describe());\n    println(a.weight().to_string());\n    println(b.weight().to_string());\n    println(a.into_code().to_string());\n    println(c.into_code().to_string());\n}",
    // Slice 32: Vec[<enum>] (kind 4000+ei) — THE real token-stream shape:
    // a lexer returning Vec[Tok] of unit/int/String-payload enum tokens;
    // element pushes materialize borrows tag-conditionally, the returned
    // vec deep-copies, index reads borrow into enum-method dispatch, and
    // the drop loop frees each element's payload by tag.
    "enum Tok { Plus, Num(i64), Ident(String) }\nimpl Tok {\n    fn describe(ref self) -> String {\n        match self {\n            Ident(name) => {\n                return \"id:\".to_string() + name;\n            }\n            Num(n) => {\n                return \"n:\".to_string() + n.to_string();\n            }\n            Plus => {}\n        }\n        return \"+\".to_string();\n    }\n}\nfn lex(src: String) -> Vec[Tok] {\n    let mut toks: Vec[Tok] = Vec.new();\n    let mut start = 0 - 1;\n    let mut i = 0;\n    for b in src.bytes() {\n        if start >= 0 {\n            if not ((b >= 97 and b <= 122) or (b >= 48 and b <= 57)) {\n                let w = src.substring(start, i);\n                let f = src.substring(start, start + 1);\n                if f >= \"0\" and f <= \"9\" {\n                    toks.push(Tok.Num(w.len()));\n                } else {\n                    toks.push(Tok.Ident(w));\n                }\n                start = 0 - 1;\n            }\n        }\n        if start < 0 {\n            if (b >= 97 and b <= 122) or (b >= 48 and b <= 57) {\n                start = i;\n            } else if b == 43 {\n                toks.push(Tok.Plus);\n            }\n        }\n        i = i + 1;\n    }\n    if start >= 0 {\n        let w = src.substring(start, src.len());\n        let f = src.substring(start, start + 1);\n        if f >= \"0\" and f <= \"9\" {\n            toks.push(Tok.Num(w.len()));\n        } else {\n            toks.push(Tok.Ident(w));\n        }\n    }\n    return toks;\n}\nfn main() {\n    let toks = lex(\"abc + 4711 + x\".to_string());\n    let mut i = 0;\n    while i < toks.len() {\n        let t = toks[i];\n        println(t.describe());\n        i = i + 1;\n    }\n    let held = toks[0];\n    println(held.describe());\n}",
    // Slice 33: NESTED AGGREGATE STRUCT FIELDS — the real SpannedToken
    // shape { tok: <enum>, span: <struct> }: recursive ty_text/heap/
    // stride, recursive materialize/free field legs, borrowed aggregate
    // field values deep-copying into literals, Vec[SpTok] with element
    // binds, match on the enum field, chained POD reads bound to temps,
    // and a whole-value rebind. Shaped to stay seed-green: the ref-param
    // consuming-match idiom is B-2026-07-21-3/-4 territory (open seed
    // bugs found by this slice — the Kara emitter compiles those shapes
    // correctly; entries land when the seed is fixed).
    "struct Span { line: i64, col: i64 }\nenum Tok { Plus, Num(i64), Ident(String) }\nstruct SpTok { tok: Tok, span: Span }\nfn mk(t: Tok, line: i64, col: i64) -> SpTok {\n    return SpTok { tok: t, span: Span { line: line, col: col } };\n}\nfn main() {\n    let mut v: Vec[SpTok] = Vec.new();\n    v.push(mk(Tok.Num(42), 2, 1));\n    v.push(mk(Tok.Ident(\"zed\".to_string()), 3, 1));\n    v.push(mk(Tok.Plus, 3, 5));\n    let mut i = 0;\n    while i < v.len() {\n        let t = v[i];\n        match t.tok {\n            Ident(name) => {\n                println(\"id \".to_string() + name);\n            }\n            Num(n) => {\n                println(\"n\".to_string() + n.to_string());\n            }\n            Plus => {\n                println(\"+\");\n            }\n        }\n        let ln = t.span.line;\n        let cl = t.span.col;\n        println(ln.to_string() + \":\" + cl.to_string());\n        i = i + 1;\n    }\n    let a = mk(Tok.Ident(\"foo\".to_string()), 9, 9);\n    let b = a;\n    match b.tok {\n        Ident(name) => {\n            println(\"held \".to_string() + name);\n        }\n        Num(n) => {\n            println(\"hn\");\n        }\n        Plus => {\n            println(\"h+\");\n        }\n    }\n}",
    // Slice 34: Option/Result METHOD CONVENIENCES — is_some/is_none/
    // is_ok/is_err (tag predicates vs 0) and unwrap (payload extract by
    // recorded kind; String payloads are borrows; unwrap-only-verified
    // by corpus discipline — seed panic parity deferred).
    "fn find(v: ref Vec[i64], t: i64) -> Option[i64] {\n    let mut i = 0;\n    while i < v.len() {\n        if v[i] == t {\n            return Some(i);\n        }\n        i = i + 1;\n    }\n    return None;\n}\nfn pick(flag: bool) -> Option[String] {\n    if flag {\n        return Some(\"payload\".to_string());\n    }\n    return None;\n}\nfn parse_num(s: String) -> Result[i64, String] {\n    if s == \"9\" {\n        return Ok(9);\n    }\n    return Err(\"nope\".to_string());\n}\nfn main() {\n    let mut v: Vec[i64] = Vec.new();\n    v.push(4);\n    v.push(9);\n    let hit = find(v, 9);\n    println(hit.is_some().to_string());\n    println(hit.is_none().to_string());\n    if hit.is_some() {\n        println(hit.unwrap().to_string());\n    }\n    let miss = find(v, 5);\n    println(miss.is_none().to_string());\n    let s = pick(true);\n    if s.is_some() {\n        println(s.unwrap());\n    }\n    let r = parse_num(\"9\".to_string());\n    println(r.is_ok().to_string());\n    if r.is_ok() {\n        println(r.unwrap().to_string());\n    }\n    let e = parse_num(\"x\".to_string());\n    println(e.is_err().to_string());\n}",
    // Slice 35: TWO-PAYLOAD ENUM VARIANTS — Integer(i64, String), the
    // real Token number shape. Payloads route by KIND into the widened
    // layout (i64 -> slot 1, String -> slot 2); match binds positionally
    // by each payload's recorded kind; Vec[Tok] elements compose.
    "enum Tok { Eof, Integer(i64, String), Ident(String) }\nfn describe(t: ref Tok) -> String {\n    match t {\n        Integer(v, lex) => {\n            return \"int \".to_string() + v.to_string() + \" '\" + lex + \"'\";\n        }\n        Ident(name) => {\n            return \"ident \".to_string() + name;\n        }\n        Eof => {}\n    }\n    return \"eof\".to_string();\n}\nfn main() {\n    let a = Tok.Integer(42, \"42\".to_string());\n    println(describe(a));\n    let b = Tok.Ident(\"foo\".to_string());\n    println(describe(b));\n    println(describe(Tok.Eof));\n    let mut v: Vec[Tok] = Vec.new();\n    v.push(Tok.Integer(7, \"0x7\".to_string()));\n    v.push(Tok.Eof);\n    let mut i = 0;\n    while i < v.len() {\n        let t = v[i];\n        match t {\n            Integer(val, lx) => {\n                println(val.to_string() + \"/\" + lx);\n            }\n            Ident(nm) => {\n                println(nm);\n            }\n            Eof => {\n                println(\"eof\");\n            }\n        }\n        i = i + 1;\n    }\n}",
    // Slice 36: f64 (kind 5) — literals as EXACT hex bit patterns (LLVM
    // rejects inexact decimal FP), fadd/fsub/fmul/fdiv + ordered fcmp,
    // params/returns/locals, and to_string via karac_runtime_f64_to_str
    // (Rust-Display shortest-roundtrip — 0.1+0.2 and 42.0 print exactly
    // as the seed does).
    "fn area(w: f64, h: f64) -> f64 {\n    return w * h;\n}\nfn main() {\n    let x = 1.5;\n    let y = 2.25;\n    println((x + y).to_string());\n    println((x * y).to_string());\n    println((y - x).to_string());\n    println((y / x).to_string());\n    println((x < y).to_string());\n    println((x == 1.5).to_string());\n    println(area(3.5, 2.0).to_string());\n    let z = 0.1;\n    println((z + 0.2).to_string());\n    let mut acc = 0.0;\n    let mut i = 0;\n    while i < 4 {\n        acc = acc + 0.25;\n        i = i + 1;\n    }\n    println(acc.to_string());\n    println(42.0.to_string());\n}",
    // Slice 37: char/u8 LITERALS + ENUM PAYLOADS — int-backed (the
    // Unicode scalar / byte value in an i64), so comparisons, params,
    // and payload slots ride the existing int machinery. CharTok(char) /
    // ByteTok(u8) — the Token.CharLiteral/ByteLiteral shape. Char
    // PRINTING (UTF-8 encode parity) is deferred; compare-only corpus.
    "enum Tok { Eof, CharTok(char), ByteTok(u8) }\nfn code(t: ref Tok) -> i64 {\n    match t {\n        CharTok(c) => {\n            if c == 'z' {\n                return 1000;\n            }\n            return 1;\n        }\n        ByteTok(b) => {\n            if b == 43 {\n                return 2000;\n            }\n            return 2;\n        }\n        Eof => {}\n    }\n    return 0;\n}\nfn is_lower(c: char) -> bool {\n    return c >= 'a' and c <= 'z';\n}\nfn main() {\n    println(code(Tok.CharTok('z')).to_string());\n    println(code(Tok.CharTok('A')).to_string());\n    println(code(Tok.ByteTok(43)).to_string());\n    println(code(Tok.Eof).to_string());\n    println(is_lower('m').to_string());\n    println(is_lower('M').to_string());\n    let c = 'q';\n    if c == 'q' {\n        println(\"q!\");\n    }\n}",
    // Slice 38 CAPSTONE — THE REAL-FILE COMPILE: the verbatim span.kara
    // Span struct, a 22-variant Token enum (unit + String + two-payload
    // variants), SpannedToken { token: <enum>, span: <struct> }, a
    // 4-field Lexer with 6 methods (assoc ctor, ref + mut-ref receivers,
    // intra-method self calls), keyword/symbol tables, a decimal-
    // accumulating number scanner, and lex_all -> Vec[SpannedToken]
    // (64-byte elements with nested heap) matched in main with span
    // rendering. ZERO new emitter surface — 20 slices composing.
    "struct Span {\n    line: i64,\n    column: i64,\n    offset: i64,\n    length: i64,\n}\nenum Token {\n    Fn,\n    Struct,\n    Enum,\n    Let,\n    Mut,\n    If,\n    Else,\n    Return,\n    LeftParen,\n    RightParen,\n    LeftBrace,\n    RightBrace,\n    Colon,\n    Comma,\n    Equal,\n    Plus,\n    Minus,\n    Star,\n    Slash,\n    Identifier(String),\n    Integer(i64, String),\n    Eof,\n}\nstruct SpannedToken {\n    token: Token,\n    span: Span,\n}\nstruct Lexer {\n    src: String,\n    pos: i64,\n    line: i64,\n    col: i64,\n}\nfn is_word_start(c: String) -> bool {\n    if c >= \"a\" and c <= \"z\" {\n        return true;\n    }\n    if c >= \"A\" and c <= \"Z\" {\n        return true;\n    }\n    return c == \"_\";\n}\nfn is_word_cont(c: String) -> bool {\n    if is_word_start(c) {\n        return true;\n    }\n    return c >= \"0\" and c <= \"9\";\n}\nfn is_digit(c: String) -> bool {\n    return c >= \"0\" and c <= \"9\";\n}\nfn keyword_of(w: ref String) -> i64 {\n    if w == \"fn\" {\n        return 1;\n    }\n    if w == \"struct\" {\n        return 2;\n    }\n    if w == \"enum\" {\n        return 3;\n    }\n    if w == \"let\" {\n        return 4;\n    }\n    if w == \"mut\" {\n        return 5;\n    }\n    if w == \"if\" {\n        return 6;\n    }\n    if w == \"else\" {\n        return 7;\n    }\n    if w == \"return\" {\n        return 8;\n    }\n    return 0;\n}\nfn kw_token(k: i64) -> Token {\n    match k {\n        1 => {\n            return Token.Fn;\n        }\n        2 => {\n            return Token.Struct;\n        }\n        3 => {\n            return Token.Enum;\n        }\n        4 => {\n            return Token.Let;\n        }\n        5 => {\n            return Token.Mut;\n        }\n        6 => {\n            return Token.If;\n        }\n        7 => {\n            return Token.Else;\n        }\n        _ => {\n            return Token.Return;\n        }\n    }\n    return Token.Eof;\n}\nfn sym_token(c: ref String) -> Token {\n    if c == \"(\" {\n        return Token.LeftParen;\n    }\n    if c == \")\" {\n        return Token.RightParen;\n    }\n    if c == \"{\" {\n        return Token.LeftBrace;\n    }\n    if c == \"}\" {\n        return Token.RightBrace;\n    }\n    if c == \":\" {\n        return Token.Colon;\n    }\n    if c == \",\" {\n        return Token.Comma;\n    }\n    if c == \"=\" {\n        return Token.Equal;\n    }\n    if c == \"+\" {\n        return Token.Plus;\n    }\n    if c == \"-\" {\n        return Token.Minus;\n    }\n    if c == \"*\" {\n        return Token.Star;\n    }\n    return Token.Slash;\n}\nimpl Lexer {\n    fn new(src: String) -> Lexer {\n        return Lexer { src: src, pos: 0, line: 1, col: 1 };\n    }\n    fn done(ref self) -> bool {\n        return self.pos >= self.src.len();\n    }\n    fn peek(ref self) -> String {\n        return self.src.substring(self.pos, self.pos + 1);\n    }\n    fn advance(mut ref self) {\n        self.pos = self.pos + 1;\n        self.col = self.col + 1;\n    }\n    fn skip_ws(mut ref self) {\n        while self.pos < self.src.len() and self.peek() == \" \" {\n            self.advance();\n        }\n    }\n    fn next_token(mut ref self) -> SpannedToken {\n        self.skip_ws();\n        let start = self.pos;\n        let start_col = self.col;\n        if self.pos >= self.src.len() {\n            return SpannedToken {\n                token: Token.Eof,\n                span: Span { line: self.line, column: start_col, offset: start, length: 0 },\n            };\n        }\n        if is_word_start(self.peek()) {\n            while self.pos < self.src.len() and is_word_cont(self.peek()) {\n                self.advance();\n            }\n            let w = self.src.substring(start, self.pos);\n            let k = keyword_of(w);\n            let ln = self.pos - start;\n            if k > 0 {\n                return SpannedToken {\n                    token: kw_token(k),\n                    span: Span { line: self.line, column: start_col, offset: start, length: ln },\n                };\n            }\n            return SpannedToken {\n                token: Token.Identifier(w),\n                span: Span { line: self.line, column: start_col, offset: start, length: ln },\n            };\n        }\n        if is_digit(self.peek()) {\n            let mut val = 0;\n            while self.pos < self.src.len() and is_digit(self.peek()) {\n                let d = self.src.substring(self.pos, self.pos + 1);\n                let mut dv = 0;\n                if d == \"1\" {\n                    dv = 1;\n                }\n                if d == \"2\" {\n                    dv = 2;\n                }\n                if d == \"3\" {\n                    dv = 3;\n                }\n                if d == \"4\" {\n                    dv = 4;\n                }\n                if d == \"5\" {\n                    dv = 5;\n                }\n                if d == \"6\" {\n                    dv = 6;\n                }\n                if d == \"7\" {\n                    dv = 7;\n                }\n                if d == \"8\" {\n                    dv = 8;\n                }\n                if d == \"9\" {\n                    dv = 9;\n                }\n                val = val * 10 + dv;\n                self.advance();\n            }\n            let lex = self.src.substring(start, self.pos);\n            let ln = self.pos - start;\n            return SpannedToken {\n                token: Token.Integer(val, lex),\n                span: Span { line: self.line, column: start_col, offset: start, length: ln },\n            };\n        }\n        self.advance();\n        let c = self.src.substring(start, self.pos);\n        return SpannedToken {\n            token: sym_token(c),\n            span: Span { line: self.line, column: start_col, offset: start, length: 1 },\n        };\n    }\n}\nfn lex_all(src: String) -> Vec[SpannedToken] {\n    let mut lx = Lexer.new(src);\n    let mut out: Vec[SpannedToken] = Vec.new();\n    while not lx.done() {\n        out.push(lx.next_token());\n        lx.skip_ws();\n    }\n    return out;\n}\nfn main() {\n    let toks = lex_all(\"fn add(a: i64, b) { return a + b * 42 }\".to_string());\n    let mut i = 0;\n    while i < toks.len() {\n        let t = toks[i];\n        match t.token {\n            Identifier(name) => {\n                println(\"ident \".to_string() + name);\n            }\n            Integer(v, lex) => {\n                println(\"int \".to_string() + v.to_string() + \" '\" + lex + \"'\");\n            }\n            Fn => {\n                println(\"kw fn\");\n            }\n            Struct => {\n                println(\"kw struct\");\n            }\n            Enum => {\n                println(\"kw enum\");\n            }\n            Let => {\n                println(\"kw let\");\n            }\n            Mut => {\n                println(\"kw mut\");\n            }\n            If => {\n                println(\"kw if\");\n            }\n            Else => {\n                println(\"kw else\");\n            }\n            Return => {\n                println(\"kw return\");\n            }\n            Eof => {\n                println(\"eof\");\n            }\n            other => {\n                println(\"sym\");\n            }\n        }\n        let cl = t.span.column;\n        let ln = t.span.length;\n        println(cl.to_string() + \"+\" + ln.to_string());\n        i = i + 1;\n    }\n}",
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
    // Link the runtime archive when present (Slice 36): runtime-backed
    // surface (f64.to_string -> karac_runtime_f64_to_str) needs it; earlier
    // corpus IR referenced only libc symbols. The emitted spawn-site stub
    // globals satisfy the runtime's metadata externs.
    let runtime =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/release/libkarac_runtime.a");
    let mut cc_cmd = Command::new("clang");
    cc_cmd.arg(&ll);
    if runtime.exists() {
        cc_cmd.arg(&runtime).arg("-lm");
    }
    let cc = cc_cmd.arg("-o").arg(&bin).output().unwrap();
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
