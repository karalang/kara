//! iterators, ranges, collect -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer iter_range::
//!
//! New fixtures about iterators, ranges, collect belong in this file.

use super::*;

#[test]
fn asan_iter_chain_over_temporary_vec_source() {
    // B-2026-07-18-39 — an iterator chain over a TEMPORARY Vec source
    // (`vec![…].iter()…`) materializes the literal into a fresh temp Vec that
    // codegen must free after the loop drains it. A scalar-element source has
    // only the buffer to free; a STRING-element source must also free each
    // per-element String buffer. Both must be LSan-clean.
    assert_clean_asan_run(
        r#"
fn main() {
    let n: i64 = vec![1, 2, 3, 4].iter().sum();
    println(n);
    let mut c = 0;
    for s in vec!["alpha", "beta", "gamma"].iter() {
        c = c + 1;
    }
    println(c);
}
"#,
        &["10", "3"],
        "asan_iter_chain_over_temporary_vec_source",
    );
}

#[test]
fn asan_iter_rev_bound_vec_reverse_iterate() {
    // B-2026-07-18-41 codegen leg — the reverse-iterate `.rev()` lowering
    // walks the SAME Vec storage at mirrored indices (`len-1-i`); no new
    // allocation, so it is leak-free by construction. The String-element case
    // is the accounting stress: each element is borrowed/cloned in reverse
    // order and the reversed `collect` produces an independent Vec whose
    // buffers must each free exactly once, with the source Vec still owning
    // its own. Must be LSan/UAF-clean.
    assert_clean_asan_run(
        r#"
fn main() {
    let v: Vec[String] = ["alpha", "beta", "gamma", "delta"];
    for s in v.iter().rev() {
        println(s);
    }
    let r: Vec[String] = v.iter().rev().collect();
    println(r.get(0));
    println(v.get(0));
    println(v.iter().rev().count());
}
"#,
        &[
            "delta",
            "gamma",
            "beta",
            "alpha",
            "Some(delta)",
            "Some(alpha)",
            "4",
        ],
        "asan_iter_rev_bound_vec_reverse_iterate",
    );
}

#[test]
fn asan_iter_rev_range_reverse_iterate() {
    // B-2026-07-18-41 range leg — `(a..b).rev()` descends over the same
    // index set. The descending loop allocates nothing itself, but the
    // accounting stress is a reverse-order heap-index MOVE: reading
    // `v[i]` (Vec[String]) in reverse and pushing it into an owning sink
    // must clone the source element (else the container's element-drop and
    // the sink's owner double-free it). The reversed `collect` produces an
    // independent Vec whose buffers each free once. Looped so any
    // per-iteration imbalance accumulates for LSan.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut iter: i64 = 0i64;
    while iter < 3i64 {
        let v: Vec[String] = [f"a{iter}", f"b{iter}", f"c{iter}"];
        let mut d: Vec[String] = Vec.new();
        for i in (0..3).rev() {
            d.push(v[i]);
        }
        println(d.get(0));
        // Source still owns its buffers.
        println(v.get(0));
        let r: Vec[String] = (0..2).rev().map(|x: i64| f"r{x}").collect();
        println(r.get(0));
        iter = iter + 1i64;
    }
}
"#,
        &[
            "Some(c0)", "Some(a0)", "Some(r1)", "Some(c1)", "Some(a1)", "Some(r1)", "Some(c2)",
            "Some(a2)", "Some(r1)",
        ],
        "asan_iter_rev_range_reverse_iterate",
    );
}

#[test]
fn asan_iter_flatten_for_loop_heap_no_leak() {
    // B-2026-07-19-12 slice 2 — the `for w in xs.iter().flatten()`
    // nested-loop codegen over heap String elements. The outer loop borrows
    // each inner Vec, the inner loop borrows each String; the accounting
    // stress is a flatten-order heap MOVE: cloning each borrowed element
    // into an owning sink (`d.push(w.clone())`) must free exactly once, and
    // the source Vec-of-Vecs must still own all its buffers at the end.
    // Looped so any per-iteration imbalance accumulates for LSan.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut iter: i64 = 0i64;
    while iter < 3i64 {
        let nested: Vec[Vec[String]] = [[f"a{iter}", f"b{iter}"], [], [f"c{iter}"]];
        let mut d: Vec[String] = Vec.new();
        for w in nested.iter().flatten() {
            d.push(w.clone());
        }
        println(d.get(0));
        println(d.get(2));
        // Source still owns its buffers (outer Vec length + a re-flatten).
        println(nested.len());
        let mut n2: i64 = 0i64;
        for w2 in nested.iter().flatten() {
            n2 = n2 + 1i64;
        }
        println(n2);
        // Slice 3 — `.flatten().collect()` over heap elements ALLOCATES a fresh
        // Vec whose String buffers (cloned from the borrowed source) must each
        // free exactly once; the source survives.
        let c: Vec[String] = nested.iter().flatten().collect();
        println(c.get(0));
        iter = iter + 1i64;
    }
}
"#,
        &[
            "Some(a0)", "Some(c0)", "3", "3", "Some(a0)", "Some(a1)", "Some(c1)", "3", "3",
            "Some(a1)", "Some(a2)", "Some(c2)", "3", "3", "Some(a2)",
        ],
        "asan_iter_flatten_for_loop_heap_no_leak",
    );
}

#[test]
fn asan_generic_forward_owned_collection_param_no_leak_or_double_free() {
    // B-2026-07-13-2: a bare generic param bound to a String/Vec forwarded
    // through a nested generic call (leg A) and a `Vec[i64]` bound to a
    // generic param (leg B) must deep-copy on return. 200 iterations: each
    // call frees the arg buffer + an independent returned copy; a missed
    // copy double-frees (ASAN) and a leaked copy accumulates (LSan). Was:
    // aborted with double-free before the fix.
    assert_clean_asan_run(
        r#"
fn id[T](x: T) -> T { x }
fn twice[T](x: T) -> T { id(x) }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 200i64 {
        let s = twice(f"str{i}");
        total = total + s.len();
        let mut v: Vec[i64] = Vec.new();
        v.push(i);
        v.push(i);
        let w = twice(v);
        total = total + w.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // len("str{i}")=3+digits: i 0..9 -> 4 (*10=40), 10..99 -> 5 (*90=450),
        // 100..199 -> 6 (*100=600). String total = 40+450+600 = 1090.
        // Each Vec has len 2 -> 200*2 = 400. Grand total = 1090 + 400 = 1490.
        &["1490"],
        "generic_forward_owned_collection_param_no_leak_or_double_free",
    );
}

#[test]
fn asan_for_over_iter_chain_heap_elems_no_leak() {
    // B-2026-07-11-18 — `for <p> in <src>.iter().{map|filter}+ { .. }` over
    // HEAP elements. The desugar peels the adaptors into a `for` over the base
    // source and binds the user pattern (`let p = <adapted element>`) before
    // the body; over a `Vec[String]`, that must not leak the source Vec, a
    // per-element String, or a mapped String. Exercises a String-yielding map
    // (`|w| w.clone()`), a filter that drops elements, and a two-stage
    // filter+map, each consumed by a body that prints the bound String.
    assert_clean_asan_run(
        r#"
fn main() {
    let words: Vec[String] = ["alpha", "bb", "gamma", "dd", "epsilon"];
    for w in words.iter().filter(|w| w.len() > 2).map(|w| w.clone()) {
        println(w);
    }
    let nums: Vec[i64] = [1, 2, 3, 4, 5, 6];
    for s in nums.iter().filter(|n| n % 2 == 0).map(|n| f"n={n}") {
        println(s);
    }
}
"#,
        &["alpha", "gamma", "epsilon", "n=2", "n=4", "n=6"],
        "asan_for_over_iter_chain_heap_elems_no_leak",
    );
}

#[test]
fn asan_iter_over_tuple_element_vec_no_leak() {
    // Regression companion to codegen::e2e_iter_over_tuple_element_vec.
    // Iterating a HEAP-element `Vec` that lives in a TUPLE element
    // (`t.0.iter()`) drives a read-only alias of the tuple's inline
    // storage — no per-element move, so no element is freed by the loop
    // and the whole tuple's Vec is freed exactly once at scope exit.
    // Rebuild the tuple every iteration to amplify any leak, iterating the
    // `Vec[String]` element via both a for-loop and a fold-shaped chain.
    assert_clean_asan_run(
        r#"
fn make() -> (Vec[String], i64) {
    let mut a: Vec[String] = Vec.new();
    a.push("alpha");
    a.push("beta");
    a.push("gamma");
    (a, 0)
}
fn main() {
    let mut total: i64 = 0;
    for _ in 0..50 {
        let t = make();
        for w in t.0.iter() {
            total = total + w.len();
        }
        let n = t.0.iter().fold(0, |acc, w| acc + 1);
        total = total + n;
    }
    println(total);
}
"#,
        // per iter: 5+4+5 (chars) + 3 (count) = 17; × 50 = 850
        &["850"],
        "asan_iter_over_tuple_element_vec_no_leak",
    );
}

#[test]
#[ignore = "B-2026-08-05-35 sweep: does not compile — no codegen handler for `len` on an iter-chain element binding. Silently SKIPPED until the harness learned to fail on a codegen error."]
fn asan_iter_chain_any_all_short_circuit_no_leak() {
    // B-2026-07-11-19 — `any`/`all` short-circuit terminals over a chain whose
    // `map` produces HEAP Strings. When the predicate decides early the loop
    // `break`s mid-iteration; each per-element mapped String (the deciding one
    // included) must be dropped, and the source `Vec[String]` freed once. `any`
    // stops at the first match, `all` at the first failure — both mid-stream.
    assert_clean_asan_run(
        r#"
fn main() {
    let words: Vec[String] = ["alpha", "beta", "gamma", "delta", "epsilon"];
    let hit = words.iter().map(|w| f"[{w}]").any(|s| s.len() > 6);
    let allshort = words.iter().map(|w| f"[{w}]").all(|s| s.len() < 5);
    if hit { println("hit"); } else { println("miss"); }
    if allshort { println("all-short"); } else { println("not-all"); }
}
"#,
        &["hit", "not-all"],
        "asan_iter_chain_any_all_short_circuit_no_leak",
    );
}

#[test]
fn asan_b04_4_heap_enumerate_conditional_tuple_collect_no_double_free() {
    // B-2026-07-04-4: a HEAP `enumerate` whose `(i64, String)` tuple flows
    // into a CONDITIONAL whole-tuple push — `filter`/`take_while`/
    // `skip_while`/`inspect` after enumerate, plus a `filter().take()` two-
    // stage chain. Each binds the tuple DIRECTLY to the (first) downstream
    // param, so the heap tuple keeps a SINGLE owning binding and its
    // conditional `if pred { push(p) }` is a clean move (no aliasing copy).
    // Previously gated to the loud dispatch-fail; now lowered. Reads an
    // element from every result each round to expose any double-free/UAF.
    assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let w: Vec[String] = Vec[
            "alpha-b044-payload-aaaaaaaaaaaaaaaaaaaa".to_string(),
            "bravo-b044-payload-bbbbbbbbbbbbbbbbbbbb".to_string(),
            "charlie-b044-payload-cccccccccccccccc".to_string(),
            "delta-b044-payload-dddddddddddddddddddd".to_string()
        ];
        let f: Vec[(i64, String)] = w.iter().enumerate().filter(|p| p.0 > 0i64).collect();
        let tw: Vec[(i64, String)] = w.iter().enumerate().take_while(|p| p.0 < 2i64).collect();
        let sw: Vec[(i64, String)] = w.iter().enumerate().skip_while(|p| p.0 < 1i64).collect();
        let ft: Vec[(i64, String)] = w.iter().enumerate().filter(|p| p.0 > 0i64).take(1i64).collect();
        let ins: Vec[(i64, String)] = w.iter().enumerate().inspect(|p| p.0).collect();
        let f0: (i64, String) = f[0].clone();
        let tw0: (i64, String) = tw[0].clone();
        let sw2: (i64, String) = sw[2].clone();
        let ft0: (i64, String) = ft[0].clone();
        let ins3: (i64, String) = ins[3].clone();
        println(f"{f.len()}:{f0.1} {tw.len()}:{tw0.1} {sw.len()}:{sw2.1} {ft.len()}:{ft0.1} {ins.len()}:{ins3.1}");
        round = round + 1i64;
    }
}
"#,
            [
                "3:bravo-b044-payload-bbbbbbbbbbbbbbbbbbbb 2:alpha-b044-payload-aaaaaaaaaaaaaaaaaaaa 3:delta-b044-payload-dddddddddddddddddddd 1:bravo-b044-payload-bbbbbbbbbbbbbbbbbbbb 4:delta-b044-payload-dddddddddddddddddddd",
            ]
            .repeat(40)
            .as_slice(),
            "asan_b04_4_heap_enumerate_conditional_tuple_collect_no_double_free",
        );
}

#[test]
fn asan_b04_2_identity_collect_no_leak() {
    // B-2026-07-04-2 sub-part 4: a PLAIN `<src>.iter().collect()` identity
    // collect (no map/filter/... adaptor). The fix injects a synthetic
    // identity `map(|x| x)`, cloning each element into a fresh Vec. A
    // named-local source is BORROWED (survives, freed once at its own scope),
    // and a FRESH-TEMP source (`mk().iter().collect()`) must free its heap
    // after cloning. Loops 40× with >=36-byte payloads for LSan reachability;
    // reads an element each round to expose any double-free/UAF.
    assert_clean_asan_run(
            r#"
fn mk() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("identity-collect-freshtemp-alpha-aaaaaaaaaaaa".to_string());
    v.push("identity-collect-freshtemp-bravo-bbbbbbbbbbbb".to_string());
    v
}
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let w: Vec[String] = Vec[
            "identity-collect-local-alpha-aaaaaaaaaaaaaaaa".to_string(),
            "identity-collect-local-bravo-bbbbbbbbbbbbbbbb".to_string(),
            "identity-collect-local-charlie-cccccccccccccc".to_string()
        ];
        let a: Vec[String] = w.iter().collect();
        let ft: Vec[String] = mk().iter().collect();
        let a1: String = a[1i64].clone();
        let ft0: String = ft[0i64].clone();
        println(f"{a.len()} {w.len()} {a1} {ft.len()} {ft0}");
        round = round + 1i64;
    }
}
"#,
            [
                "3 3 identity-collect-local-bravo-bbbbbbbbbbbbbbbb 2 identity-collect-freshtemp-alpha-aaaaaaaaaaaa",
            ]
            .repeat(40)
            .as_slice(),
            "asan_b04_2_identity_collect_no_leak",
        );
}

#[test]
fn asan_b04_2_into_iter_identity_collect_no_leak() {
    // B-2026-07-04-2 sub-part 4 (into_iter half): `<local>.into_iter()
    // .collect()` lowers identically to `.iter().collect()` — the ownership
    // checker treats it as NON-consuming (`w.len()` stays valid after), so
    // it clones each element into a fresh Vec and the source survives. Same
    // leak/double-free surface as the `.iter()` identity collect; asserts a
    // heap source over 40× ≥44-byte payloads (LSan reachability).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let w: Vec[String] = Vec[
            "into-iter-collect-alpha-aaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "into-iter-collect-bravo-bbbbbbbbbbbbbbbbbbbbbb".to_string()
        ];
        let r: Vec[String] = w.into_iter().collect();
        let r1: String = r[1i64].clone();
        println(f"{r.len()} {w.len()} {r1}");
        round = round + 1i64;
    }
}
"#,
        ["2 2 into-iter-collect-bravo-bbbbbbbbbbbbbbbbbbbbbb"]
            .repeat(40)
            .as_slice(),
        "asan_b04_2_into_iter_identity_collect_no_leak",
    );
}

#[test]
fn asan_b04_2_zip_heap_collect_no_leak() {
    // B-2026-07-04-2 (heap-zip leg): `a.iter().zip(b.iter()).collect()` over
    // two `Vec[String]` sources. The pushed tuple `(a[i], b[i])` deep-clones
    // each named-Vec heap index-read, so the borrowed sources SURVIVE (freed
    // once at their own scope) and the collect result owns independent
    // buffers (freed once). Before the fix the index-read aliased the source
    // buffer — both the result's element-drop and the source's scope-exit
    // free released it (double-free). 40× ≥40-byte payloads for LSan
    // reachability; re-reads a source and a result element each round.
    assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let mut a: Vec[String] = Vec[
            "zip-heap-collect-left-alpha-aaaaaaaaaaaaaaaa".to_string(),
            "zip-heap-collect-left-bravo-bbbbbbbbbbbbbbbb".to_string()
        ];
        let mut b: Vec[String] = Vec[
            "zip-heap-collect-right-charlie-cccccccccccc".to_string(),
            "zip-heap-collect-right-delta-dddddddddddddd".to_string()
        ];
        let z: Vec[(String, String)] = a.iter().zip(b.iter()).collect();
        let p: (String, String) = z[0i64].clone();
        println(f"{z.len()} {a.len()} {b.len()} {p.0} {p.1}");
        round = round + 1i64;
    }
}
"#,
            [
                "2 2 2 zip-heap-collect-left-alpha-aaaaaaaaaaaaaaaa zip-heap-collect-right-charlie-cccccccccccc",
            ]
            .repeat(40)
            .as_slice(),
            "asan_b04_2_zip_heap_collect_no_leak",
        );
}

#[test]
fn asan_b04_2_chunks_heap_collect_no_leak() {
    // B-2026-07-04-2 sub-part 1 (chunks heap leg): `v.iter().chunks(2)
    // .collect()` over a `Vec[String]` -> `Vec[Vec[String]]`. Each chunk is
    // built as a FRESH temp via an inline block tail-return
    // (`acc.push({ let mut c = Vec.new(); ...; c })`), the `mk()`-fresh-temp
    // pattern inlined — not a consume-then-reuse loop binding (which needs
    // the ownership RC fallback the synthetic AST can't emit) nor an
    // in-place fill of a growing accumulator (which double-freed on
    // realloc). Each `base[j]` deep-clones (the heap-index-read fix), so
    // `base` survives and every clone is owned once by the result. 40x
    // >=40-byte payloads; re-reads `v` and reads nested chunk elements
    // INLINE via the f-string (a `let x = cs[i][j]` double-index bind hits
    // the separate documented `matrix[i][j]` clone gap, unrelated to
    // chunks).
    assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let mut v: Vec[String] = Vec[
            "chunks-heap-collect-alpha-aaaaaaaaaaaaaaaaaa".to_string(),
            "chunks-heap-collect-bravo-bbbbbbbbbbbbbbbbbb".to_string(),
            "chunks-heap-collect-charlie-cccccccccccccccc".to_string(),
            "chunks-heap-collect-delta-dddddddddddddddddd".to_string(),
            "chunks-heap-collect-echo-eeeeeeeeeeeeeeeeeeee".to_string()
        ];
        let cs: Vec[Vec[String]] = v.iter().chunks(2i64).collect();
        println(f"{cs.len()} {cs[0i64].len()} {cs[2i64].len()} {cs[0i64][0i64]} {cs[2i64][0i64]} {v.len()}");
        round = round + 1i64;
    }
}
"#,
            [
                "3 2 1 chunks-heap-collect-alpha-aaaaaaaaaaaaaaaaaa chunks-heap-collect-echo-eeeeeeeeeeeeeeeeeeee 5",
            ]
            .repeat(40)
            .as_slice(),
            "asan_b04_2_chunks_heap_collect_no_leak",
        );
}

#[test]
fn asan_b04_2_windows_heap_collect_no_leak() {
    // B-2026-07-04-2 sub-part 1 (windows heap leg): `v.iter().windows(2)
    // .collect()` over a `Vec[String]` -> overlapping length-2 slices. Each
    // element is cloned into MULTIPLE windows (`base[j]` deep-clones per
    // read), so each window owns independent buffers and the borrowed `v`
    // survives -- the overlap must not alias. Same fresh-temp block-return
    // lowering as chunks (step=1, full-window cutoff). 40x >=40-byte
    // payloads; inline nested reads.
    assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let mut v: Vec[String] = Vec[
            "windows-heap-collect-alpha-aaaaaaaaaaaaaaaa".to_string(),
            "windows-heap-collect-bravo-bbbbbbbbbbbbbbbb".to_string(),
            "windows-heap-collect-charlie-cccccccccccccc".to_string(),
            "windows-heap-collect-delta-dddddddddddddddd".to_string()
        ];
        let ws: Vec[Vec[String]] = v.iter().windows(2i64).collect();
        println(f"{ws.len()} {ws[0i64].len()} {ws[0i64][0i64]} {ws[2i64][1i64]} {v.len()}");
        round = round + 1i64;
    }
}
"#,
            [
                "3 2 windows-heap-collect-alpha-aaaaaaaaaaaaaaaa windows-heap-collect-delta-dddddddddddddddd 4",
            ]
            .repeat(40)
            .as_slice(),
            "asan_b04_2_windows_heap_collect_no_leak",
        );
}

#[test]
fn asan_b04_2_chain_identity_heap_collect_no_leak() {
    // B-2026-07-04-2 sub-part 1 (chain half): `A.chain(B).collect()` over two
    // heap `Vec[String]` sources. The fix emits `for x in A { acc.push x };
    // for y in B { acc.push y }` — each `push` over a borrowed source CLONES,
    // so both sources survive and the accumulator owns independent copies.
    // Neither a leak (a source's buffer / an un-cloned element) nor a
    // double-free (a shared heap element) may result. 40× with ≥45-byte
    // payloads for LSan reachability; reads a collected element each round.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let a: Vec[String] = Vec[
            "chain-collect-payload-alpha-aaaaaaaaaaaaaaaaaa".to_string(),
            "chain-collect-payload-bravo-bbbbbbbbbbbbbbbbbb".to_string()
        ];
        let b: Vec[String] = Vec[
            "chain-collect-payload-charlie-cccccccccccccccc".to_string()
        ];
        let r: Vec[String] = a.iter().chain(b.iter()).collect();
        let r2: String = r[2i64].clone();
        println(f"{r.len()} {a.len()} {b.len()} {r2}");
        round = round + 1i64;
    }
}
"#,
        ["3 2 1 chain-collect-payload-charlie-cccccccccccccccc"]
            .repeat(40)
            .as_slice(),
        "asan_b04_2_chain_identity_heap_collect_no_leak",
    );
}

#[test]
fn asan_fresh_temp_source_enumerate_collect_no_double_free() {
    // B-2026-07-04-5: a collect-adaptor chain whose SOURCE is a fresh-temp
    // call result (`mk().iter()…`) rather than a named local. The for-loop
    // over the materialized `mk()` temp resolved its element type from the
    // span-colliding `owned_temp_drops` — which held the OUTERMOST result
    // `Vec[(i64, String)]` instead of the source `Vec[String]` — and so read
    // and dropped the `Vec[String]` buffer at the wider `(i64, String)`
    // element stride, freeing garbage (`pointer being freed was not
    // allocated`). The terminal heap enumerate (whole `(i64, String)` tuple
    // pushed and later dropped alongside the freed `mk()` temp) is the
    // double-free shape; `enumerate().map(|p| p.1)` and a plain `.map` over a
    // fresh-temp source ride the same corrected element-type resolution.
    // Reads `.0`/`.1` and an element each round to expose the free.
    assert_clean_asan_run(
            r#"
fn mk() -> Vec[String] {
    return Vec[
        "fresh-temp-enum-payload-alpha-aaaaaaaaaaaaaaaa".to_string(),
        "fresh-temp-enum-payload-bravo-bbbbbbbbbbbbbbbb".to_string()
    ];
}
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let he: Vec[(i64, String)] = mk().iter().enumerate().collect();
        let e0: (i64, String) = he[0].clone();
        let e1: (i64, String) = he[1].clone();
        let hm: Vec[String] = mk().iter().enumerate().map(|p| p.1).collect();
        let m1: String = hm[1].clone();
        let hl: Vec[i64] = mk().iter().map(|s| s.len()).collect();
        println(f"{he.len()} {e0.0} {e0.1} {e1.0} {e1.1} {m1} {hl[0]}");
        round = round + 1i64;
    }
}
"#,
            [
                "2 0 fresh-temp-enum-payload-alpha-aaaaaaaaaaaaaaaa 1 fresh-temp-enum-payload-bravo-bbbbbbbbbbbbbbbb fresh-temp-enum-payload-bravo-bbbbbbbbbbbbbbbb 46",
            ]
            .repeat(40)
            .as_slice(),
            "asan_fresh_temp_source_enumerate_collect_no_double_free",
        );
}

#[test]
fn asan_for_over_collection_body_local_no_leak() {
    // B-2026-06-14-21: a body-local owned heap `let` inside a
    // for-over-COLLECTION loop (Vec/Slice/Map/Set/String/array — NOT
    // for-over-range, which already had per-iteration cleanup) leaked
    // every iteration but the last. The binding's `FreeVecBuffer` was
    // registered in the enclosing FUNCTION frame (the collection
    // for-variants called `compile_block(body)` with no per-iteration
    // cleanup frame), so only the final iteration's value was freed at
    // the function tail — N-1 iterations leaked (surfaced as a browser
    // OOM in the Fathom dogfood: `for handle in handles { let chunk =
    // handle.join(); … }` leaked the joined Vec every frame). The fix
    // wraps each collection for-variant's body in
    // `compile_loop_body_with_cleanup`. Looping over a Vec-of-keys with
    // a per-iteration `let row = build(…)` makes the leak visible to the
    // Linux-CI LSan gate (mac checks no double-free / UAF).
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < n { v.push(i); i = i + 1; }
    v
}
fn main() {
    let mut keys: Vec[i64] = Vec.new();
    let mut k = 0;
    while k < 200 { keys.push(k); k = k + 1; }
    let mut total: i64 = 0;
    for key in keys {
        let row: Vec[i64] = build(64);
        total = total + (row.len() as i64) + key;
    }
    println(total);
}
"#,
        &["32700"],
        "for_over_collection_body_local_no_leak",
    );
}

// ── `collect_all_vec` gather (phase-6 slice 1b) ───────────────
//
// Lowers a runtime Vec of closures into parallel `karac_par_run`
// branches, each writing a `Result` into a malloc'd slot array; the
// slots become the output Vec's buffer while the temp branch/ctx
// arrays are freed. The `Err` payloads are heap `String`s
// (`f"neg:{n}"`) that flow closure → slot → output Vec → print →
// drop; a double-free across the par boundary or in the slot/Vec
// hand-off (or a use-after-free of a freed temp array) would trip
// ASAN here. (LeakSanitizer is unsupported on Darwin, so a pure
// leak is caught only in Linux CI; this guards the UAF/double-free
// class, which is the codegen-ownership risk.)

#[test]
fn asan_collect_all_vec_gather_no_double_free() {
    assert_clean_asan_run(
        r#"
fn work(n: i64) -> Result[i64, String] {
    if n > 0 { Result.Ok(n * 10) } else { Result.Err(f"neg:{n}") }
}
fn main() {
    let fs: Vec[Fn() -> Result[i64, String]] = Vec[|| work(1), || work(-2), || work(3)];
    let results: Vec[Result[i64, String]] = collect_all_vec(fs);
    for r in results {
        match r {
            Result.Ok(v) => { println(f"ok {v}"); }
            Result.Err(e) => { println(f"err {e}"); }
        }
    }
}
"#,
        &["ok 10", "err neg:-2", "ok 30"],
        "collect_all_vec_gather",
    );
}

/// B-2026-08-05-7 (collect_all_vec leg): two ownership holes in the
/// homogeneous gather, both on the input/output Vec buffers rather than the
/// element payloads — a scalar `Result[i64, i64]` reproduced the first.
///
/// 1. DOUBLE-FREE of the input Vec. Step 5b frees `fs`'s buffer, justified
///    in its own comment by "the caller's scope-exit drop is suppressed".
///    Nothing suppressed it: `collect_all_vec` is intercepted in
///    `compile_call` BEFORE the ordinary argument-lowering path, which is
///    where `suppress_source_vec_cleanup_for_arg` runs for a moved owned Vec
///    argument. A let-bound `fs` kept its drop and both freed one buffer.
/// 2. LEAK on an EMPTY input. The returned Vec is `{ slots, n, n }`, and
///    every Vec free is cap-guarded, so with `n == 0` the `malloc(0)` slots
///    stub was unfreeable by anyone.
///
/// Covers both in one program, with runtime-derived closures so the work is
/// not folded away.
#[test]
fn asan_collect_all_vec_input_and_empty_output_owned_once() {
    assert_clean_asan_run_min_allocs(
        r#"
fn work(n: i64) -> Result[i64, String] {
    if n > 0 { Result.Ok(n * 10) } else { Result.Err(f"neg:{n}") }
}
fn main() {
    let base: i64 = env.args().len();
    let fs: Vec[Fn() -> Result[i64, String]] = Vec[|| work(base), || work(0 - base), || work(base + 2)];
    let results: Vec[Result[i64, String]] = collect_all_vec(fs);
    let mut acc = 0;
    for r in results {
        match r {
            Result.Ok(v) => { acc = acc + v; }
            Result.Err(e) => { if e.starts_with("neg") { acc = acc + 1; } }
        }
    }
    let empty: Vec[Fn() -> Result[i64, String]] = Vec.new();
    let none: Vec[Result[i64, String]] = collect_all_vec(empty);
    acc = acc + none.len();
    println(acc);
}
"#,
        &["41"],
        "collect_all_vec_input_and_empty_output_owned_once",
        // The par runtime's own startup dominates; the floor only has to sit
        // above the 3 an allocation-free run reports.
        20,
    );
}

// ── bound `s.chars()` iterator materialized as Vec[char] — clone ownership ──
//
// B-2026-06-18-5: `let it = s.chars()` materializes an eager `Vec[char]`
// snapshot, and `it.collect()` returns a CLONE of it. Collecting the same
// bound iterator twice yields two independent buffers, and the snapshot `it`
// is itself freed at scope exit — three buffers per iteration that must each
// be freed exactly once. A `for c in it` also drains a materialized snapshot.
// Looped 1000× so LSan catches a per-iter leak (e.g. the snapshot never
// freed) and local ASAN catches a double-free (e.g. a clone aliasing the
// snapshot's buffer). A ≥36-byte payload keeps the snapshot heap-allocated
// past any short-buffer fast path.
#[test]
fn asan_chars_bound_iterator_collect_clone_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let s = "the quick brown fox jumps over the lazy dog";
        let it = s.chars();
        let a: Vec[char] = it.collect();
        let b: Vec[char] = it.collect();
        total = total + a.len() + b.len();
        let it2 = s.chars();
        for c in it2 { if c == 'o' { total = total + 1; } }
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        // len 43 each (×2 = 86) + 4 'o's, ×1000 = 90000.
        &["90000"],
        "chars_bound_iterator_collect_clone_loop",
    );
}

// ── `<iter>.map/filter(...).collect()` adaptor chain → Vec (B-2026-07-03-25) ──
//
// The desugar builds a fresh Vec and pushes each transformed/surviving
// element. Two heap-ownership hazards it must get right, looped 1000× so
// Linux LSan catches a per-iter leak and local ASAN a double-free/UAF:
//   1. Heap OUTPUT: `.map(|n| n.to_string())` collects a `Vec[String]`; each
//      produced String is owned by the Vec and freed exactly once at scope
//      exit (payloads ≥36 bytes so LSan's short-String reachability blind
//      spot doesn't mask a leak — see the user's LSan memory).
//   2. Heap SOURCE, borrowed: `words.iter().map(|w| w.len())` reads each
//      `String` element of the source Vec without consuming it — the source
//      must remain fully owned and be freed once (a stray move would
//      double-free or leak). The source is re-read after the collect to
//      prove it survived. A `filter().map()` chain over the same heap source
//      exercises the multi-stage path.
#[test]
fn asan_iter_adaptor_collect_to_vec_heap_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let src: Vec[i64] = Vec[1i64, 2i64, 3i64, 4i64];
        // Heap OUTPUT — Vec[String], long payloads.
        let strs: Vec[String] = src.iter().map(|n| f"iteration-payload-number-{n}-xyzzy").collect();
        for s in strs { total = total + s.len(); }
        // Heap SOURCE, borrowed — must survive the collect.
        let words: Vec[String] = Vec[
            "alpha-alpha-alpha-alpha-alpha-alpha".to_string(),
            "beta-beta-beta-beta-beta-beta-beta-beta".to_string(),
            "gamma-gamma-gamma-gamma-gamma-gamma-gamma".to_string()
        ];
        let lens: Vec[i64] = words.iter().map(|w| w.len()).collect();
        total = total + lens[0] + lens[1] + lens[2];
        // Multi-stage over the heap source.
        let longs: Vec[i64] = words.iter().filter(|w| w.len() > 36i64).map(|w| w.len()).collect();
        total = total + longs.len();
        // Source still owned/usable after the collects.
        total = total + words.len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        // strs: 4 payloads/iter, each "iteration-payload-number-N-xyzzy" (32).
        // words lens: 35 + 39 + 41 = 115. longs: 2 (39,41 > 36). words.len()=3.
        // per iter = 128 + 115 + 2 + 3 = 248; ×1000 = 248000 (matches `karac run`).
        &["248000"],
        "iter_adaptor_collect_to_vec_heap",
    );
}

// ── `collect_all` heterogeneous tuple gather (phase-6) ────────
//
// Static-N sibling of collect_all_vec: each inline closure runs via
// karac_par_run into a stack Result slot, then the slots are assembled
// into a tuple. Captured args (`base`) live in stack env allocas read
// by worker threads across the synchronous join, and the f-string
// `Err` payloads (`f"a{n}"`) flow closure → slot → tuple → match →
// print → drop. A use-after-free of an env / slot, or a double-free of
// an Err String, would trip ASAN here.

#[test]
fn asan_collect_all_heterogeneous_tuple_no_uaf() {
    assert_clean_asan_run(
        r#"
fn fa(n: i64) -> Result[i64, String] {
    if n > 0 { Result.Ok(n * 10) } else { Result.Err(f"a{n}") }
}
fn fb(s: String) -> Result[String, i64] { Result.Err(7) }
fn main() {
    let base: i64 = 3;
    let t: (Result[i64, String], Result[String, i64], Result[i64, String]) =
        collect_all(|| fa(-5), || fb("x"), || fa(base));
    match t.0 { Result.Ok(v) => { println(f"0 ok {v}"); } Result.Err(e) => { println(f"0 err {e}"); } }
    match t.1 { Result.Ok(v) => { println(f"1 ok {v}"); } Result.Err(e) => { println(f"1 err {e}"); } }
    match t.2 { Result.Ok(v) => { println(f"2 ok {v}"); } Result.Err(e) => { println(f"2 err {e}"); } }
}
"#,
        &["0 err a-5", "1 err 7", "2 ok 30"],
        "collect_all_heterogeneous_tuple",
    );
}

#[test]
fn asan_for_self_field_vec_iter_no_double_free() {
    // `for s in self.items.iter()` inside an impl method (`ref self`) —
    // the silent-0-iteration miscompile fix. The loop binds each field
    // String element as a BORROW: the enclosing `Counter` owns the field
    // Vec and frees each element String exactly once at the struct's own
    // scope-exit drop, so the loop body must NOT independently free `s`
    // (else double-free on macOS ASAN), and every element String must be
    // freed once by the struct drop (else leak on Linux LSan). Two
    // counters are built and totalled so the field buffers are freed on a
    // real path. ≥36-byte payloads defeat LSan short-string reachability.
    assert_clean_asan_run(
        r#"
struct Counter { items: Vec[String], base: i64 }
impl Counter {
    fn total(ref self) -> i64 {
        let mut t = self.base;
        for s in self.items.iter() { t = t + s.len(); };
        return t;
    }
}
fn make_counter(tag: i64) -> Counter {
    let mut c = Counter { items: Vec.new(), base: tag };
    c.items.push("first field string padded well beyond thirty-six bytes ok");
    c.items.push("second field string padded well beyond thirty-six byte");
    return c;
}
fn main() {
    let a = make_counter(100_i64);
    println(a.total());
    let b = make_counter(200_i64);
    println(b.total());
}
"#,
        &["211", "311"],
        "for_self_field_vec_iter_no_double_free",
    );
}

#[test]
fn asan_for_shared_self_field_vec_iter_no_double_free() {
    // Shared-struct sibling of `asan_for_self_field_vec_iter_no_double_free`.
    // `for s in self.items.iter()` on a `shared struct` `ref self` receiver:
    // the field Vec[String] is owned by the RC struct and freed once when
    // the last handle drops, so the loop's `s` bindings must be borrows (no
    // per-iteration free → no double-free on macOS ASAN; every element freed
    // once by the struct drop → no leak on Linux LSan). ≥36-byte payloads
    // defeat LSan short-string reachability.
    assert_clean_asan_run(
        r#"
shared struct SBag { mut items: Vec[String], base: i64 }
impl SBag {
    fn total(ref self) -> i64 {
        let mut t = self.base;
        for s in self.items.iter() { t = t + s.len(); };
        return t;
    }
}
fn make_bag(tag: i64) -> SBag {
    let b = SBag { items: Vec.new(), base: tag };
    b.items.push("shared field string padded well beyond thirty-six bytes ok");
    b.items.push("another shared field string well beyond thirty-six byte");
    return b;
}
fn main() {
    let a = make_bag(10_i64);
    println(a.total());
    let b = make_bag(20_i64);
    println(b.total());
}
"#,
        &["123", "133"],
        "for_shared_self_field_vec_iter_no_double_free",
    );
}

#[test]
fn asan_freshtemp_vec_into_iter_struct_no_double_free() {
    // Slice 3h companion: `for r in make_recs().into_iter()` on a fresh-temp
    // `Vec[Rec]` (Rec has a String field), reading the heap field through the
    // bound element (`r.name.len()`). Exercises the agg-drop threading
    // (`track_vec_of_aggs_var` → `__karac_drop_struct_Rec`) on the
    // materialized iter temp: each element's String field must be freed once
    // by the per-element drop before the buffer. `.into_iter()` rides the same
    // materialize path as `.iter()` here. Loops to accumulate any leak.
    assert_clean_asan_run(
        r#"
struct Rec { name: String, n: i64 }

fn make_recs() -> Vec[Rec] {
    let mut v: Vec[Rec] = Vec.new();
    v.push(Rec { name: "alpha rec name padded out beyond thirty-six bytes ok", n: 1_i64 });
    v.push(Rec { name: "beta rec name padded out beyond thirty-six bytes okk", n: 2_i64 });
    return v;
}

fn main() {
    let mut pass = 0;
    while pass < 3 {
        let mut total = 0_i64;
        for r in make_recs().into_iter() {
            total = total + r.n + r.name.len();
        };
        println(total);
        pass = pass + 1;
    };
}
"#,
        &["107", "107", "107"],
        "freshtemp_vec_into_iter_struct_no_double_free",
    );
}

#[test]
fn asan_soa_drop_empty_collection() {
    // Empty SoA — never pushed, so cap stays 0 and the cleanup
    // should short-circuit at the `is_heap` guard without freeing
    // anything. Catches a regression where the cap check reads the
    // wrong slot and accidentally calls free on undef group ptrs.
    assert_clean_asan_run(
        r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn main() {
    let entities: Vec[Entity] = Vec.new();
    println(entities.len());
}
"#,
        &["0"],
        "soa_drop_empty_collection",
    );
}

/// B-2026-08-26-27 — `Vec.try_from_iter` over HEAP elements must not leak.
///
/// The fallible collect builds its accumulator through `try_push`, so every
/// growth goes through `karac_alloc_fallible` + memcpy + free rather than
/// the panicking `realloc`. That is a different allocation path from the
/// one the infallible `collect` exercises, and it moves per-element
/// `{ptr,len,cap}` headers across each time — exactly the shape where an
/// off-by-one on the copied byte count leaks or double-frees a String
/// buffer. LeakSanitizer is what holds it honest; the E2E twins in
/// tests/codegen.rs only check the values.
#[test]
fn asan_vec_try_from_iter_over_heap_elements_no_leak() {
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> Result[i64, AllocError] {
    let src: Vec[String] = ["alpha", "beta", "gamma", "delta"];
    let mut total = 0i64;
    let mut k = 0i64;
    while k < n {
        let up: Vec[String] = Vec.try_from_iter(src.iter().map(|s| s.to_uppercase()))?;
        total = total + up.len() + up[0].len();
        k = k + 1i64;
    }
    return Ok(total);
}
fn main() {
    match build(64i64) {
        Ok(n) => { println(f"ok {n}"); }
        Err(e) => { println("oom"); }
    }
}
"#,
        &["ok 576"],
        "vec_try_from_iter_heap_elements_no_leak",
    );
}

#[test]
fn asan_iter_axis_row_view_bind_no_double_free() {
    // B-2026-07-13-7: `t.iter_axis(n)` returns a `Vec[Tensor]` of freshly
    // malloc'd sub-tensor blocks, freed per-element by
    // `track_vec_of_tensors_var`. Binding an element out — `let r = rows[i]`
    // — shallow-copied the 8-byte tensor pointer (no Tensor arm in the
    // clone dispatcher), so the binding's `FreeTensor` and the container's
    // per-element free hit the SAME block: `free(): double free detected in
    // tcache 2` under JIT/native (interpreter was correct). The fix
    // deep-clones the whole tensor block so the binding owns an independent
    // copy. 300 iters, two row views bound per iter: a missed clone
    // double-frees (ASAN), a leaked clone accumulates (LSan on Linux).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: f32 = 0.0f32;
    while i < 300 {
        let m: Tensor[f32, [2, 3]] = Tensor.from([[1.0f32, 2.0f32, 3.0f32], [4.0f32, 5.0f32, 6.0f32]]);
        let rows = m.iter_axis(0);
        let r0 = ref rows[0];
        let r1 = ref rows[1];
        total = total + r0.sum() + r1.sum();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // Each iter: r0.sum()=1+2+3=6, r1.sum()=4+5+6=15 → 21. 300*21 = 6300.
        &["6300"],
        "iter_axis_row_view_bind_no_double_free",
    );
}

#[test]
fn asan_getmove_iter_value_push_no_double_free() {
    // Slice 3s adjacency probe: `for (k, v) in m { out.push(v); }` — the
    // iteration value binding pushed into a Vec must not leave the Vec
    // and the map co-owning one buffer.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut out: Vec[String] = Vec.new();
    let mut m: Map[i64, String] = Map.new();
    let mut i = 0;
    while i < 3 {
        m.insert(i, f"map string payload padded beyond thirty-six bytes {i}");
        i = i + 1;
    };
    for (k, v) in m {
        out.push(v);
    }
    println(out.len());
}
"#,
        &["3"],
        "getmove_iter_value_push_no_double_free",
    );
}

/// B-2026-08-15-15 — a SLICE BINDING must not be registered for a
/// value-type struct drop.
///
/// `var_type_names` holds the ELEMENT's name for a slice binding
/// (`let s = es[0..2]` over `Vec[Entry]` records `s -> "Entry"`), which is
/// what element dispatch wants and what the let-site struct-drop gate
/// misread as "s IS an Entry". It registered
/// `__karac_drop_struct_Entry` against `s`'s slot — a two-word
/// `{ptr, len}` view — so the drop read the slice's `ptr`/`len` as
/// `Entry`'s `{String, i64}` and freed the SOURCE VEC's element buffer.
/// `es`'s own cleanup then freed it again.
///
/// The heap field makes it VISIBLE, not wrong: a `Vec[i64]` slice registers
/// no drop at all because `i64` is not in `struct_types`, so the identical
/// misread produced no free. Both are here.
///
/// The row reports the default build as clean and `KARAC_AUTO_PAR=0` as the
/// crashing configuration — an inversion that would send a bisector away
/// from the cause. That polarity is real but narrow: it holds only for the
/// one shape auto-par happened to split (a `Map` local plus a `Vec` param).
/// The plain spelling below double-frees in the DEFAULT build, which is why
/// this fixture is written without the scenery.
#[test]
fn asan_range_slice_binding_of_struct_vec_owns_nothing() {
    assert_clean_asan_run(
        r#"
struct Entry { service: String, weight: i64 }

fn main() {
    let mut es: Vec[Entry] = Vec.new();
    es.push(Entry { service: "alphabetical", weight: 3 });
    es.push(Entry { service: "betamaximum", weight: 5 });

    // The bug's own shape: a range slice of a struct Vec with a heap field.
    let s = es[0..2];
    println(f"{s.len()} {s[0].service} {s[1].service}");

    // The whole-container spelling of the same view.
    let a = es.as_slice();
    println(f"{a.len()} {a[1].service}");

    // The element kind whose misread was silent: no `struct_types` entry, so
    // no drop was registered and nothing was freed twice.
    let mut ns: Vec[i64] = Vec.new();
    ns.push(3); ns.push(5);
    let t = ns[0..2];
    println(f"{t.len()}");

    // The source must still own and free its own elements afterwards.
    println(f"{es.len()} {es[0].service}");
    println("end");
}
"#,
        &[
            "2 alphabetical betamaximum",
            "2 betamaximum",
            "2",
            "2 alphabetical",
            "end",
        ],
        "range_slice_binding_of_struct_vec_owns_nothing",
    );
}
