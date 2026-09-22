//! Map, Set, SortedMap, SortedSet -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter map_set::
//!
//! New fixtures about Map, Set, SortedMap, SortedSet belong in this file.

use super::*;

#[test]
fn a_user_impl_drop_on_a_map_key_fires_like_one_on_a_value() {
    // B-2026-08-26-41. `emit_map_val_user_drop_bodies_fn` had no key twin, so
    // an `impl Drop` on a KEY type never ran at map teardown while the same
    // impl on the VALUE type did. design.md § Drop owes it: "the compiler
    // invokes `drop` at the end of each value's live range", and a key stored
    // in a map is a value whose live range ends when the map is destroyed.
    //
    // Four destruction sites in one program, because the walk had to be added
    // at each: binding death, `clear()`, a map held in a struct FIELD, and a
    // map reached as a container element. Order within an entry is KEY then
    // VALUE — an entry's halves drop in the order `Map[K, V]` declares them,
    // the rule a struct's fields already follow. Codegen twin:
    // `test_e2e_user_impl_drop_on_a_map_key_fires`.
    let src = "#[derive(Hash, Eq, PartialEq)]
        struct K { id: i64 }
        struct V { id: i64 }
        impl Drop for K { fn drop(mut ref self) { println(f\"dropK {self.id}\"); } }
        impl Drop for V { fn drop(mut ref self) { println(f\"dropV {self.id}\"); } }
        struct Holder { m: Map[K, V] }
        fn main() {
            let mut a: Map[K, V] = Map.new();
            a.insert(K { id: 1 }, V { id: 1 });
            println(\"--before-clear--\");
            a.clear();
            println(\"--after-clear--\");
            { let mut b: Map[K, V] = Map.new();
              b.insert(K { id: 2 }, V { id: 2 });
              let h = Holder { m: b };
              println(\"--field-in-scope--\"); }
            println(\"--field-gone--\");
        }";
    assert_eq!(
        run_no_errors(src),
        "--before-clear--\ndropK 1\ndropV 1\n--after-clear--\n\
         dropK 2\ndropV 2\n--field-in-scope--\n--field-gone--\n"
    );
}

#[test]
fn a_sorted_container_destroys_its_elements_in_key_order() {
    // B-2026-08-27-7's other half — the escape hatch, pinned. design.md § Map
    // sends code that needs a defined order to `SortedMap`/`SortedSet`, and
    // that advice has to hold for DESTRUCTION as well as iteration or it is
    // useless to anyone with an RAII element type.
    //
    // Unlike the sibling test above this one CAN assert literals: key order is
    // seed-independent, so it is identical on every run and on every backend
    // (measured against `karac build` and the JIT at seeds 1/3/7). Values are
    // inserted at `9 - i` so key order and insertion order DISAGREE — inserting
    // them in ascending order would let a walk that ignored the ordering pass.
    let src = "struct V { id: i64 }
        impl Drop for V { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }
        fn main() {
            let mut a: SortedMap[i64, V] = SortedMap.new();
            let mut i = 1;
            while i <= 8 { a.insert(9 - i, V { id: i }); i = i + 1; }
            println(\"--\");
        }";
    // The `--` lands LAST, not first: the map's last use is the final insert,
    // so NLL ends its live range there and the whole walk fires before the
    // println — design.md's "drop at the end of each value's live range".
    assert_eq!(
        run_no_errors(src),
        "drop 8\ndrop 7\ndrop 6\ndrop 5\ndrop 4\ndrop 3\ndrop 2\ndrop 1\n--\n"
    );
}

#[test]
fn removing_an_entry_runs_the_key_or_element_drop_body() {
    // B-2026-08-27-2 — the site B-2026-08-26-41 deliberately left out.
    // `remove` destroys an entry's KEY in place while its VALUE moves out into
    // the returned `Some(old)`, so the value's body already ran at the caller
    // and only the key's was missing. All four containers are covered because
    // all four were affected: a Set's ELEMENT is the key half, so `Set.remove`
    // ran no body at all.
    //
    // B-2026-09-15-5 UPDATED THE EXPECTATION: each `remove` now prints TWO
    // key bodies, not one. The argument here is a struct LITERAL -- a fresh
    // temporary that is built, hashed, and discarded at the call -- so it is a
    // second value with its own body debt, distinct from the STORED key this
    // row was about. The old single-`dropK` expectation was this defect
    // written down: when B-2026-08-27-2 fixed the stored key's body, the
    // argument temp's was still being dropped on the floor at every lookup
    // entry point, so one body looked complete. Passing a BOUND key instead
    // (`let p = K { n: 1 }; m.remove(p)`) still yields two, because the
    // binding owns one of them -- which is why the count is the same and only
    // the OWNER differs.
    // Key before value falls out of the semantics rather than being imposed:
    // the key dies at the call, the value dies wherever the returned `Option`
    // does. Codegen twin: `test_e2e_remove_runs_the_key_drop_body`.
    let src = "#[derive(Hash, Eq, PartialEq, Ord)]
        struct K { n: i64 }
        struct V { n: i64 }
        impl Drop for K { fn drop(mut ref self) { println(f\"dropK {self.n}\"); } }
        impl Drop for V { fn drop(mut ref self) { println(f\"dropV {self.n}\"); } }
        fn main() {
            let mut m: Map[K, V] = Map.new();
            m.insert(K { n: 1 }, V { n: 1 });
            m.remove(K { n: 1 });
            println(f\"map len={m.len()}\");
            let mut sm: SortedMap[K, V] = SortedMap.new();
            sm.insert(K { n: 2 }, V { n: 2 });
            sm.remove(K { n: 2 });
            println(f\"smap len={sm.len()}\");
            let mut st: Set[K] = Set.new();
            st.insert(K { n: 3 });
            st.remove(K { n: 3 });
            println(f\"set len={st.len()}\");
            let mut ss: SortedSet[K] = SortedSet.new();
            ss.insert(K { n: 4 });
            ss.remove(K { n: 4 });
            println(f\"sset len={ss.len()}\");
        }";
    assert_eq!(
        run_no_errors(src),
        "dropK 1\ndropK 1\ndropV 1\nmap len=0\n\
         dropK 2\ndropK 2\ndropV 2\nsmap len=0\n\
         dropK 3\ndropK 3\nset len=0\n\
         dropK 4\ndropK 4\nsset len=0\n"
    );
}

#[test]
fn clearing_a_set_runs_its_elements_drop_body() {
    // B-2026-08-27-3 (found while fixing the leak). `Map.clear` got the key
    // and value body walks in B-2026-08-26-41; `Set.clear` is a different
    // codegen arm and got neither, so the compiled backends dropped nothing
    // while the interpreter — which runs the same element walk for every
    // container — printed the body. A run-vs-build divergence, and the
    // interpreter is the side that was already right; this pins it as the
    // oracle. Codegen twin: `test_e2e_set_clear_runs_the_element_drop_body`.
    let src = "#[derive(Hash, Eq, PartialEq, Ord)]
        struct E { n: i64 }
        impl Drop for E { fn drop(mut ref self) { println(f\"dropE {self.n}\"); } }
        fn main() {
            let mut s: Set[E] = Set.new();
            s.insert(E { n: 1 });
            println(\"--before--\");
            s.clear();
            println(f\"--after len={s.len()}--\");
        }";
    assert_eq!(run_no_errors(src), "--before--\ndropE 1\n--after len=0--\n");
}

#[test]
fn a_moved_map_key_runs_its_drop_body_exactly_once() {
    // The half of B-2026-08-26-41 that had to move WITH the walk. Before it,
    // a BOUND-LOCAL key moved into a map ran its body at the MOVE SITE — the
    // row's repro used an inline temporary and so never saw this — and the
    // compiled backend ran it over a moved-from slot, printing an empty field
    // where `--interp` printed the value.
    //
    // `disarm_moved_value_arg_user_drops` documented the pairing: keys stayed
    // on the container-only disarm precisely because no walk covered them. So
    // the walk alone would double-fire this, and the disarm alone would stop
    // an RAII key releasing at all. Both assertions below are needed: the
    // COUNT (exactly one) and the CONTENT (the field survives, so the body did
    // not run over a moved-from slot).
    let src = "#[derive(Hash, Eq, PartialEq)]
        struct K { s: String }
        impl Drop for K { fn drop(mut ref self) { println(f\"dropK {self.s}\"); } }
        fn main() {
            let mut m: Map[K, i64] = Map.new();
            let k = K { s: \"kk\" };
            m.insert(k, 1);
            println(\"--after-insert--\");
            println(f\"len={m.len()}\");
        }";
    let out = run_no_errors(src);
    assert_eq!(out.matches("dropK").count(), 1, "exactly one body: {out}");
    assert!(
        out.contains("dropK kk\n"),
        "field must survive the move: {out}"
    );
    assert!(
        out.find("dropK").unwrap() > out.find("len=1").unwrap(),
        "the body belongs at teardown, not at the move site: {out}"
    );
}

/// B-2026-08-30-46 — `dbg()` OF A `Set` / `SortedSet` / `SortedMap` RENDERED A
/// `u64` ELEMENT AT OR ABOVE 2^63 AS ITS NEGATIVE, while `f"{s}"` on the SAME
/// container in the SAME program rendered it unsigned.
///
/// The sibling of B-2026-08-30-9 one arm over: those three arms of
/// `render_typed_mode` were guarded `if !debug`, so under `dbg` they fell past
/// the typed walker to the untyped `debug_fmt` catch-all, which sees a bare
/// signed `Value::Int` carrier and cannot know the element was unsigned.
///
/// The guard's stated reason was that routing them through the typed walker
/// "would also start quoting their leaves". MEASURED: it does not — the
/// catch-all it fell to is `debug_fmt`, which already quotes, so both paths
/// print `Set{"a"}`. That is why the string rows below are here: they pin the
/// property the deferral was protecting, which was never actually at risk.
///
/// `Map` is the CONTROL and the reason this is a defect rather than a
/// convention: its arm never carried the guard, so it printed the unsigned
/// value under `dbg` all along while its three siblings did not. BOTH compiled
/// backends also print unsigned in both modes, so this was an
/// interpreter-only run-vs-build divergence.
///
/// One element per container deliberately: `Set` iteration order is per-process
/// random (design.md § Map), so a multi-element row would assert an order the
/// language does not define.
#[test]
fn dbg_of_a_set_or_sorted_container_renders_wide_unsigned_elements_as_unsigned() {
    let src = "fn main() {
            let big: u64 = 18446744073709551615u64;
            let mut s: Set[u64] = Set.new(); s.insert(big);
            let mut ss: SortedSet[u64] = SortedSet.new(); ss.insert(big);
            let mut sm: SortedMap[u64, u64] = SortedMap.new(); sm.insert(big, big);
            let mut m: Map[u64, u64] = Map.new(); m.insert(big, big);
            dbg(s); dbg(ss); dbg(sm); dbg(m);
            let neg: i64 = -1;
            let mut si: Set[i64] = Set.new(); si.insert(neg);
            dbg(si);
            let mut st: Set[String] = Set.new(); st.insert(f\"a\");
            let mut smt: SortedMap[String, String] = SortedMap.new(); smt.insert(f\"k\", f\"v\");
            dbg(st); dbg(smt);
        }";
    let (_out, dbg) = run_program_with_dbg(src, DbgOutputMode::Terminal);
    let hay = dbg.join("\n");
    for needle in [
        "Set{18446744073709551615}",
        "SortedSet{18446744073709551615}",
        "SortedMap{18446744073709551615: 18446744073709551615}",
        // The `Map` control, correct before this row and after it.
        "{18446744073709551615: 18446744073709551615}",
        // A genuinely signed element still reads as signed: a fix that simply
        // reinterpreted every carrier as unsigned would fail here.
        "Set{-1}",
        // Leaf quoting is UNCHANGED — the property the `!debug` guard was
        // deferring to protect, and which it was not in fact protecting.
        "Set{\"a\"}",
        "SortedMap{\"k\": \"v\"}",
    ] {
        assert!(
            hay.contains(needle),
            "missing {needle:?} in dbg output:\n{hay}"
        );
    }
}

#[test]
fn interp_map_and_set_honour_a_hand_written_hash_and_eq() {
    // The interpreter half of B-2026-08-26-10's `Map` hashing, pinned separately
    // because it reaches the user's impl by a different route: codegen calls a
    // synthesized `karac_hash_bytes_of_T`, while the interpreter runs the impl in
    // a sub-interpreter from inside `MapData`, which has no interpreter of its own.
    //
    // Both impls key on `id` alone while `tag` varies, so every line fails under
    // structural hashing or structural equality.
    assert_eq!(
        run("struct Item { id: i64, tag: i64 }
             impl PartialEq for Item { fn eq(ref self, other: ref Item) -> bool { self.id == other.id } }
             impl Eq for Item {}
             impl Hash for Item { fn hash[H: Hasher](ref self, hasher: mut ref H) { hasher.write_i64(self.id) } }
             fn main() {
                 let mut m: Map[Item, i64] = Map.new();
                 m.insert(Item { id: 1, tag: 100 }, 10);
                 m.insert(Item { id: 2, tag: 200 }, 20);
                 m.insert(Item { id: 1, tag: 999 }, 11);
                 println(m.len());
                 match m.get(Item { id: 1, tag: 0 }) { Some(v) => println(v), None => println(-1) }
                 println(m.contains_key(Item { id: 2, tag: 7 }));
                 m.remove(Item { id: 2, tag: 12345 });
                 println(m.len());
                 let mut s: Set[Item] = Set.new();
                 s.insert(Item { id: 5, tag: 1 });
                 s.insert(Item { id: 5, tag: 2 });
                 println(s.len());
                 println(s.contains(Item { id: 5, tag: 3 }));
             }"),
        "2\n11\ntrue\n1\n1\ntrue\n"
    );
    // The derive path must be undisturbed — every answer structural.
    assert_eq!(
        run("#[derive(Hash, Eq, PartialEq)]
             struct Item { id: i64, tag: i64 }
             fn main() {
                 let mut m: Map[Item, i64] = Map.new();
                 m.insert(Item { id: 1, tag: 100 }, 10);
                 m.insert(Item { id: 1, tag: 999 }, 11);
                 println(m.len());
                 match m.get(Item { id: 1, tag: 0 }) { Some(v) => println(v), None => println(-1) }
             }"),
        "2\n-1\n"
    );
}

#[test]
fn test_weak_field_immutable_form_set_at_construction() {
    // `weak parent: Parent` (no `mut`) is set at construction and
    // never reassigned. While the strong parent lives, upgrade yields
    // Some; after the strong parent's frame exits, upgrade yields None.
    assert_eq!(
        run("shared struct Parent { id: i64 }\n\
             shared struct Child { id: i64, weak parent: Parent }\n\
             fn build() -> Child {\n\
                 let p = Parent { id: 42 };\n\
                 Child { id: 1, parent: p }\n\
             }\n\
             fn main() {\n\
                 let c = build();\n\
                 match c.parent {\n\
                     Some(parent_ref) => println(parent_ref.id),\n\
                     None => println(\"dangling\"),\n\
                 }\n\
             }"),
        "dangling\n"
    );
}

#[test]
fn test_total_order_wrapper_compare_map_sort_interp_parity() {
    // B-2026-07-22-11 — the interpreter must match codegen's TOTAL order for
    // the `F32`/`F64` wrappers. Before the fix, `F32 { value: x }` built a
    // plain `Value::Struct` compared with the IEEE partial order (diverging
    // from codegen on -0/NaN), F32 ordering ops were missing from `eval_ops`,
    // and `value_compare`/the lowered `F32.gt` dispatch had no wrapper arm.
    // Now the struct literal builds a `TotalFloat32/64` (like `F32.from`), so
    // comparison, sort, and Map keys all use `total_cmp`. Same program +
    // expected output as the codegen E2E test
    // (`test_e2e_total_order_float_wrappers`).
    let output = run("fn main() {\n\
             let a: F32 = F32 { value: 2.5 };\n\
             let b: F32 = F32 { value: 1.5 };\n\
             println(a > b);\n\
             println(a < b);\n\
             println(a == a);\n\
             println(a == b);\n\
             println(a.value + b.value);\n\
             let mut m: Map[F32, i64] = Map.new();\n\
             let _ = m.insert(F32 { value: 2.0 }, 20);\n\
             let _ = m.insert(F32 { value: 3.0 }, 30);\n\
             match m.get(F32 { value: 2.0 }) { Some(v) => println(v), None => println(0 - 1) }\n\
             let mut v: Vec[F32] = Vec.new();\n\
             v.push(F32 { value: 3.0 });\n\
             v.push(F32 { value: 1.0 });\n\
             v.push(F32 { value: 2.0 });\n\
             v.sort();\n\
             println(v[0].value);\n\
             println(v[2].value);\n\
             let neg0: F64 = F64 { value: 0.0 * (0.0 - 1.0) };\n\
             let pos0: F64 = F64 { value: 0.0 };\n\
             println(neg0 < pos0);\n\
             println(neg0 == pos0);\n\
         }");
    assert_eq!(
        output,
        "true\nfalse\ntrue\nfalse\n4\n20\n1\n3\ntrue\nfalse\n"
    );
}

#[test]
fn test_map_set_from_enum_payload_interp_parity() {
    // B-2026-07-23-3 — a `Map`/`Set`-family collection bound out of a user-enum
    // payload, method-dispatched (`m.len()` / `s.contains(x)`). The interpreter
    // already handled this; pins run==build for the codegen dispatch fix. Same
    // program + output as `test_e2e_map_set_method_dispatch_from_enum_payload`.
    let output = run("enum MapBox { Table(Map[String, i64]) }\n\
             enum SetBox { Items(Set[i64]) }\n\
             enum SmapBox { T(SortedMap[i64, i64]) }\n\
             enum SsetBox { S(SortedSet[i64]) }\n\
             fn map_len_ref(v: ref MapBox) -> i64 { match v { Table(m) => m.len() as i64 } }\n\
             fn map_len_val(v: MapBox) -> i64 { match v { Table(m) => m.len() as i64 } }\n\
             fn set_len_ref(v: ref SetBox) -> i64 { match v { Items(s) => s.len() as i64 } }\n\
             fn set_has_ref(v: ref SetBox, x: i64) -> bool { match v { Items(s) => s.contains(x) } }\n\
             fn smap_len_ref(v: ref SmapBox) -> i64 { match v { T(m) => m.len() as i64 } }\n\
             fn sset_len_ref(v: ref SsetBox) -> i64 { match v { S(s) => s.len() as i64 } }\n\
             fn main() {\n\
                 let mut mp: Map[String, i64] = Map.new();\n\
                 let _ = mp.insert(\"a\", 1);\n\
                 let _ = mp.insert(\"b\", 2);\n\
                 let t = MapBox.Table(mp);\n\
                 println(map_len_ref(t));\n\
                 let mut mp2: Map[String, i64] = Map.new();\n\
                 let _ = mp2.insert(\"x\", 9);\n\
                 println(map_len_val(MapBox.Table(mp2)));\n\
                 let mut st: Set[i64] = Set.new();\n\
                 st.insert(1); st.insert(2); st.insert(2);\n\
                 let sv = SetBox.Items(st);\n\
                 println(set_len_ref(sv));\n\
                 println(set_has_ref(sv, 2));\n\
                 println(set_has_ref(sv, 5));\n\
                 let mut sm: SortedMap[i64, i64] = SortedMap.new();\n\
                 let _ = sm.insert(3, 1);\n\
                 let _ = sm.insert(1, 2);\n\
                 println(smap_len_ref(SmapBox.T(sm)));\n\
                 let mut ss: SortedSet[i64] = SortedSet.new();\n\
                 ss.insert(7); ss.insert(4); ss.insert(7);\n\
                 println(sset_len_ref(SsetBox.S(ss)));\n\
             }");
    assert_eq!(output, "2\n1\n2\ntrue\nfalse\n2\n2\n");
}

#[test]
fn test_f16_bf16_total_order_compare_map_sort_interp_parity() {
    // The 16-bit siblings — same total-order contract as F32/F64, run==build.
    // Mirrors `test_e2e_total_order_f16_bf16_wrappers`.
    let output = run("fn main() {\n\
             let a: F16 = F16 { value: 2.5 };\n\
             let b: F16 = F16 { value: 1.5 };\n\
             println(a > b);\n\
             println(a < b);\n\
             println(a == a);\n\
             println(a == b);\n\
             let mut m: Map[F16, i64] = Map.new();\n\
             let _ = m.insert(F16 { value: 2.0 }, 20);\n\
             let _ = m.insert(F16 { value: 3.0 }, 30);\n\
             match m.get(F16 { value: 2.0 }) { Some(v) => println(v), None => println(0 - 1) }\n\
             let mut v: Vec[F16] = Vec.new();\n\
             v.push(F16 { value: 3.0 });\n\
             v.push(F16 { value: 1.0 });\n\
             v.push(F16 { value: 2.0 });\n\
             v.sort();\n\
             println(v[0].value);\n\
             println(v[2].value);\n\
             let c: Bf16 = Bf16 { value: 3.25 };\n\
             let d: Bf16 = Bf16 { value: 1.25 };\n\
             println(c > d);\n\
             println(c == c);\n\
             let neg0: Bf16 = Bf16 { value: 0.0 * (0.0 - 1.0) };\n\
             let pos0: Bf16 = Bf16 { value: 0.0 };\n\
             println(neg0 < pos0);\n\
             println(neg0 == pos0);\n\
         }");
    assert_eq!(
        output,
        "true\nfalse\ntrue\nfalse\n20\n1\n3\ntrue\ntrue\ntrue\nfalse\n"
    );
}

// ── SortedSet[T] stdlib methods ────────────────────────────────────────────

#[test]
fn test_sorted_set_new_and_len() {
    let output = run("fn main() {\n\
             let s: SortedSet[i64] = SortedSet.new();\n\
             println(f\"{s.len()}\");\n\
         }");
    assert_eq!(output, "0\n");
}

#[test]
fn test_sorted_set_insert_and_contains() {
    let output = run("fn main() {\n\
             let s: SortedSet[i64] = SortedSet.new();\n\
             let inserted = s.insert(3_i64);\n\
             println(f\"{inserted}\");\n\
             let again = s.insert(3_i64);\n\
             println(f\"{again}\");\n\
             println(f\"{s.contains(3_i64)}\");\n\
             println(f\"{s.contains(99_i64)}\");\n\
         }");
    assert_eq!(output, "true\nfalse\ntrue\nfalse\n");
}

#[test]
fn test_sorted_set_remove() {
    let output = run("fn main() {\n\
             let s: SortedSet[i64] = SortedSet.new();\n\
             s.insert(5_i64);\n\
             let removed = s.remove(5_i64);\n\
             println(f\"{removed}\");\n\
             let again = s.remove(5_i64);\n\
             println(f\"{again}\");\n\
             println(f\"{s.is_empty()}\");\n\
         }");
    assert_eq!(output, "true\nfalse\ntrue\n");
}

#[test]
fn test_sorted_set_min_max() {
    let output = run("fn main() {\n\
             let s: SortedSet[i64] = SortedSet.new();\n\
             s.insert(5_i64);\n\
             s.insert(1_i64);\n\
             s.insert(9_i64);\n\
             match s.min() {\n\
                 Some(v) => println(f\"{v}\"),\n\
                 None    => println(\"empty\"),\n\
             }\n\
             match s.max() {\n\
                 Some(v) => println(f\"{v}\"),\n\
                 None    => println(\"empty\"),\n\
             }\n\
         }");
    assert_eq!(output, "1\n9\n");
}

#[test]
fn test_sorted_set_min_max_empty() {
    let output = run("fn main() {\n\
             let s: SortedSet[i64] = SortedSet.new();\n\
             match s.min() {\n\
                 Some(v) => println(f\"{v}\"),\n\
                 None    => println(\"empty\"),\n\
             }\n\
         }");
    assert_eq!(output, "empty\n");
}

#[test]
fn test_sorted_set_ordered_iteration() {
    let output = run("fn main() {\n\
             let s: SortedSet[i64] = SortedSet.new();\n\
             s.insert(7_i64);\n\
             s.insert(2_i64);\n\
             s.insert(5_i64);\n\
             for x in s {\n\
                 println(f\"{x}\");\n\
             }\n\
         }");
    assert_eq!(output, "2\n5\n7\n");
}

#[test]
fn test_sorted_set_union() {
    let output = run("fn main() {\n\
             let a: SortedSet[i64] = SortedSet.new();\n\
             a.insert(1_i64);\n\
             a.insert(2_i64);\n\
             let b: SortedSet[i64] = SortedSet.new();\n\
             b.insert(2_i64);\n\
             b.insert(3_i64);\n\
             let u = a.union(b);\n\
             println(f\"{u.len()}\");\n\
             for x in u {\n\
                 println(f\"{x}\");\n\
             }\n\
         }");
    assert_eq!(output, "3\n1\n2\n3\n");
}

#[test]
fn test_sorted_set_intersection() {
    let output = run("fn main() {\n\
             let a: SortedSet[i64] = SortedSet.new();\n\
             a.insert(1_i64);\n\
             a.insert(2_i64);\n\
             a.insert(3_i64);\n\
             let b: SortedSet[i64] = SortedSet.new();\n\
             b.insert(2_i64);\n\
             b.insert(3_i64);\n\
             b.insert(4_i64);\n\
             let i = a.intersection(b);\n\
             for x in i {\n\
                 println(f\"{x}\");\n\
             }\n\
         }");
    assert_eq!(output, "2\n3\n");
}

#[test]
fn test_sorted_set_difference() {
    let output = run("fn main() {\n\
             let a: SortedSet[i64] = SortedSet.new();\n\
             a.insert(1_i64);\n\
             a.insert(2_i64);\n\
             a.insert(3_i64);\n\
             let b: SortedSet[i64] = SortedSet.new();\n\
             b.insert(2_i64);\n\
             let d = a.difference(b);\n\
             for x in d {\n\
                 println(f\"{x}\");\n\
             }\n\
         }");
    assert_eq!(output, "1\n3\n");
}

#[test]
fn test_sorted_set_dedup_on_insert() {
    let output = run("fn main() {\n\
             let s: SortedSet[i64] = SortedSet.new();\n\
             s.insert(4_i64);\n\
             s.insert(4_i64);\n\
             s.insert(4_i64);\n\
             println(f\"{s.len()}\");\n\
         }");
    assert_eq!(output, "1\n");
}

// ── SortedMap[K, V] stdlib methods (B3) ─────────────────────────────────────

#[test]
fn test_sorted_map_new_and_len() {
    let output = run("fn main() {\n\
             let m: SortedMap[i64, String] = SortedMap.new();\n\
             println(f\"{m.len()}\");\n\
             println(f\"{m.is_empty()}\");\n\
         }");
    assert_eq!(output, "0\ntrue\n");
}

#[test]
fn test_sorted_map_insert_get_contains() {
    // insert returns Option[V] (old value); duplicate-key insert overwrites
    // and returns the previous value wrapped in Some.
    let output = run("fn main() {\n\
             let m: SortedMap[i64, String] = SortedMap.new();\n\
             match m.insert(1_i64, \"one\") { Some(p) => println(p), None => println(\"none\") }\n\
             match m.insert(1_i64, \"ONE\") { Some(p) => println(p), None => println(\"none\") }\n\
             match m.get(1_i64) { Some(v) => println(v), None => println(\"missing\") }\n\
             println(f\"{m.contains_key(1_i64)}\");\n\
             println(f\"{m.contains_key(2_i64)}\");\n\
             println(f\"{m.len()}\");\n\
         }");
    assert_eq!(output, "none\none\nONE\ntrue\nfalse\n1\n");
}

#[test]
fn test_sorted_map_remove() {
    let output = run("fn main() {\n\
             let m: SortedMap[i64, String] = SortedMap.new();\n\
             m.insert(5_i64, \"five\");\n\
             match m.remove(5_i64) { Some(v) => println(v), None => println(\"none\") }\n\
             match m.remove(5_i64) { Some(v) => println(v), None => println(\"none\") }\n\
             println(f\"{m.is_empty()}\");\n\
         }");
    assert_eq!(output, "five\nnone\ntrue\n");
}

#[test]
fn test_sorted_map_ordered_iteration() {
    // Insertion out of order; keys/values/entries and `for` all yield
    // ascending key order.
    let output = run("fn main() {\n\
             let m: SortedMap[i64, String] = SortedMap.new();\n\
             m.insert(3_i64, \"c\");\n\
             m.insert(1_i64, \"a\");\n\
             m.insert(2_i64, \"b\");\n\
             for k in m.keys() { print(f\"{k}\"); }\n\
             println(\"\");\n\
             for v in m.values() { print(v); }\n\
             println(\"\");\n\
             for pair in m { print(f\"{pair.0}={pair.1};\"); }\n\
             println(\"\");\n\
         }");
    assert_eq!(output, "123\nabc\n1=a;2=b;3=c;\n");
}

#[test]
fn test_sorted_map_min_max() {
    let output = run("fn main() {\n\
             let m: SortedMap[i64, String] = SortedMap.new();\n\
             m.insert(5_i64, \"five\");\n\
             m.insert(1_i64, \"one\");\n\
             m.insert(9_i64, \"nine\");\n\
             match m.min() { Some(p) => println(f\"{p.0}:{p.1}\"), None => println(\"empty\") }\n\
             match m.max() { Some(p) => println(f\"{p.0}:{p.1}\"), None => println(\"empty\") }\n\
         }");
    assert_eq!(output, "1:one\n9:nine\n");
}

#[test]
fn test_sorted_map_min_max_empty() {
    let output = run("fn main() {\n\
             let m: SortedMap[i64, String] = SortedMap.new();\n\
             match m.min() { Some(p) => println(f\"{p.0}\"), None => println(\"empty\") }\n\
             match m.max() { Some(p) => println(f\"{p.0}\"), None => println(\"empty\") }\n\
         }");
    assert_eq!(output, "empty\nempty\n");
}

#[test]
fn test_sorted_map_floor_ceiling() {
    // floor: largest key <= k; ceiling: smallest key >= k. Probe an
    // absent key between entries, the exact key, and out-of-range bounds.
    let output = run("fn main() {\n\
             let m: SortedMap[i64, String] = SortedMap.new();\n\
             m.insert(10_i64, \"ten\");\n\
             m.insert(20_i64, \"twenty\");\n\
             m.insert(30_i64, \"thirty\");\n\
             match m.floor(15_i64)   { Some(p) => println(f\"{p.0}\"), None => println(\"none\") }\n\
             match m.ceiling(15_i64) { Some(p) => println(f\"{p.0}\"), None => println(\"none\") }\n\
             match m.floor(20_i64)   { Some(p) => println(f\"{p.0}\"), None => println(\"none\") }\n\
             match m.ceiling(20_i64) { Some(p) => println(f\"{p.0}\"), None => println(\"none\") }\n\
             match m.floor(5_i64)    { Some(p) => println(f\"{p.0}\"), None => println(\"none\") }\n\
             match m.ceiling(35_i64) { Some(p) => println(f\"{p.0}\"), None => println(\"none\") }\n\
         }");
    assert_eq!(output, "10\n20\n20\n20\nnone\nnone\n");
}

#[test]
fn test_sorted_map_range() {
    // Inclusive [lo, hi]; inverted bounds yield empty.
    let output = run("fn main() {\n\
             let m: SortedMap[i64, String] = SortedMap.new();\n\
             m.insert(10_i64, \"a\");\n\
             m.insert(20_i64, \"b\");\n\
             m.insert(30_i64, \"c\");\n\
             println(f\"{m.range(10_i64, 25_i64).len()}\");\n\
             println(f\"{m.range(10_i64, 30_i64).len()}\");\n\
             println(f\"{m.range(25_i64, 15_i64).len()}\");\n\
             for pair in m.range(15_i64, 30_i64) { print(f\"{pair.0};\"); }\n\
             println(\"\");\n\
         }");
    assert_eq!(output, "2\n3\n0\n20;30;\n");
}

#[test]
fn test_sorted_map_get_or() {
    let output = run("fn main() {\n\
             let m: SortedMap[String, i64] = SortedMap.new();\n\
             m.insert(\"a\", 1_i64);\n\
             let hit = m.get_or(\"a\", 0_i64);\n\
             let miss = m.get_or(\"z\", 99_i64);\n\
             println(f\"{hit}\");\n\
             println(f\"{miss}\");\n\
         }");
    assert_eq!(output, "1\n99\n");
}

#[test]
fn test_sorted_map_clone_independence_and_clear() {
    let output = run("fn main() {\n\
             let m: SortedMap[i64, i64] = SortedMap.new();\n\
             m.insert(1_i64, 10_i64);\n\
             m.insert(2_i64, 20_i64);\n\
             let c = m.clone();\n\
             m.clear();\n\
             println(f\"{m.len()}\");\n\
             println(f\"{c.len()}\");\n\
         }");
    assert_eq!(output, "0\n2\n");
}

#[test]
fn test_sorted_map_merge_last_writer_wins() {
    let output = run("fn main() {\n\
             let a: SortedMap[i64, i64] = SortedMap.new();\n\
             a.insert(1_i64, 100_i64);\n\
             a.insert(2_i64, 200_i64);\n\
             let b: SortedMap[i64, i64] = SortedMap.new();\n\
             b.insert(2_i64, 999_i64);\n\
             b.insert(3_i64, 300_i64);\n\
             let merged = a.merge(b);\n\
             println(f\"{merged.get_or(2_i64, 0_i64)}\");\n\
             println(f\"{merged.len()}\");\n\
         }");
    assert_eq!(output, "999\n3\n");
}

// ── Map[K, V] interpreter tests ────────────────────────────────────────────

#[test]
fn test_map_new_and_len() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             println(m.len());\n\
         }");
    assert_eq!(output, "0\n");
}

#[test]
fn test_map_insert_and_get() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"a\", 1_i64);\n\
             m.insert(\"b\", 2_i64);\n\
             let v = m.get(\"a\");\n\
             match v {\n\
                 Some(x) => println(x),\n\
                 None => println(\"missing\"),\n\
             }\n\
         }");
    assert_eq!(output, "1\n");
}

#[test]
fn test_map_index_read_missing_key_panics() {
    // `m[k]` on a missing key is a runtime panic (design.md § Subscript Trait),
    // distinct from `m.get(k)` which returns `None` (B-2026-07-16-13).
    let errors = runtime_errors(
        "fn main() {\n\
             let mut m: Map[String, i64] = Map.new();\n\
             m[\"x\"] = 1_i64;\n\
             println(m[\"nope\"]);\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("key not found in map")),
        "missing map key must raise a clear runtime error, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_map_get_missing_returns_none() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             let v = m.get(\"z\");\n\
             match v {\n\
                 Some(x) => println(x),\n\
                 None => println(\"none\"),\n\
             }\n\
         }");
    assert_eq!(output, "none\n");
}

#[test]
fn test_map_contains_key() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"x\", 10_i64);\n\
             println(m.contains_key(\"x\"));\n\
             println(m.contains_key(\"y\"));\n\
         }");
    assert_eq!(output, "true\nfalse\n");
}

#[test]
fn test_map_remove() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"a\", 42_i64);\n\
             let old = m.remove(\"a\");\n\
             match old {\n\
                 Some(v) => println(v),\n\
                 None => println(\"none\"),\n\
             }\n\
             println(m.len());\n\
         }");
    assert_eq!(output, "42\n0\n");
}

#[test]
fn test_map_get_or() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"k\", 7_i64);\n\
             println(m.get_or(\"k\", 0_i64));\n\
             println(m.get_or(\"missing\", 99_i64));\n\
         }");
    assert_eq!(output, "7\n99\n");
}

#[test]
fn test_map_is_empty() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             println(m.is_empty());\n\
             m.insert(\"a\", 1_i64);\n\
             println(m.is_empty());\n\
         }");
    assert_eq!(output, "true\nfalse\n");
}

#[test]
fn test_map_keys_and_values() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"a\", 1_i64);\n\
             m.insert(\"b\", 2_i64);\n\
             let ks = m.keys();\n\
             let vs = m.values();\n\
             println(ks.len());\n\
             println(vs.len());\n\
         }");
    assert_eq!(output, "2\n2\n");
}

#[test]
fn test_map_entries_iteration() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"x\", 10_i64);\n\
             let es = m.entries();\n\
             println(es.len());\n\
         }");
    assert_eq!(output, "1\n");
}

#[test]
fn test_map_merge() {
    let output = run("fn main() {\n\
             let a: Map[String, i64] = Map.new();\n\
             a.insert(\"p\", 1_i64);\n\
             let b: Map[String, i64] = Map.new();\n\
             b.insert(\"q\", 2_i64);\n\
             let c = a.merge(b);\n\
             println(c.len());\n\
             println(c.contains_key(\"p\"));\n\
             println(c.contains_key(\"q\"));\n\
         }");
    assert_eq!(output, "2\ntrue\ntrue\n");
}

#[test]
fn test_map_clear() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"a\", 1_i64);\n\
             m.insert(\"b\", 2_i64);\n\
             println(m.len());\n\
             m.clear();\n\
             println(m.len());\n\
             println(m.is_empty());\n\
             m.insert(\"c\", 3_i64);\n\
             println(m.contains_key(\"a\"));\n\
             println(m.contains_key(\"c\"));\n\
         }");
    assert_eq!(output, "2\n0\ntrue\nfalse\ntrue\n");
}

#[test]
fn test_map_merge_overwrite() {
    let output = run("fn main() {\n\
             let a: Map[String, i64] = Map.new();\n\
             a.insert(\"k\", 1_i64);\n\
             let b: Map[String, i64] = Map.new();\n\
             b.insert(\"k\", 99_i64);\n\
             let c = a.merge(b);\n\
             println(c.len());\n\
             match c.get(\"k\") {\n\
                 Some(v) => println(v),\n\
                 None => println(\"none\"),\n\
             }\n\
         }");
    assert_eq!(output, "1\n99\n");
}

#[test]
fn test_map_insert_update_returns_old() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"k\", 5_i64);\n\
             let old = m.insert(\"k\", 10_i64);\n\
             match old {\n\
                 Some(v) => println(v),\n\
                 None => println(\"none\"),\n\
             }\n\
         }");
    assert_eq!(output, "5\n");
}

// ── Map.entry / Entry[K, V] (canonical: phase-8-stdlib-floor.md
//    "Map.entry(k) + Entry[K, V] enum") ────────────────────────────

#[test]
fn test_map_entry_or_insert_vacant_inserts_default() {
    // Vacant key — or_insert pushes (key, default) and returns the default.
    // Verify the map state by re-fetching.
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             let v = m.entry(\"a\").or_insert(7_i64);\n\
             println(v);\n\
             match m.get(\"a\") {\n\
                 Some(x) => println(x),\n\
                 None => println(\"missing\"),\n\
             }\n\
         }");
    assert_eq!(output, "7\n7\n");
}

#[test]
fn test_map_entry_or_insert_occupied_returns_existing() {
    // Occupied key — or_insert is a no-op write; returns the existing value.
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"a\", 42_i64);\n\
             let v = m.entry(\"a\").or_insert(0_i64);\n\
             println(v);\n\
             match m.get(\"a\") {\n\
                 Some(x) => println(x),\n\
                 None => println(\"missing\"),\n\
             }\n\
         }");
    assert_eq!(output, "42\n42\n");
}

#[test]
fn test_map_entry_and_modify_runs_when_occupied() {
    // and_modify's closure fires only on Occupied; it receives the slot
    // value as a mut ref and can mutate through (the interpreter aliases
    // the slot via SharedCell for the duration of the call).
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"k\", 5_i64);\n\
             m.entry(\"k\").and_modify(|v| { v += 1; });\n\
             match m.get(\"k\") {\n\
                 Some(x) => println(x),\n\
                 None => println(\"missing\"),\n\
             }\n\
         }");
    assert_eq!(output, "6\n");
}

#[test]
fn test_sorted_map_entry_chain_counts_in_key_order() {
    // SortedMap shares Map's entry chain (entry/and_modify/or_insert +
    // or_insert_with). Counting words yields the same per-key totals as Map
    // would; `keys()` iterates ascending, so the report is key-sorted.
    let output = run("fn main() {\n\
             let mut counts: SortedMap[String, i64] = SortedMap.new();\n\
             let words = [\"banana\", \"apple\", \"banana\", \"cherry\", \"apple\", \"banana\"];\n\
             let mut i = 0i64;\n\
             while i < words.len() {\n\
                 counts.entry(words[i].to_string()).and_modify(|c| { c += 1; }).or_insert(1);\n\
                 i = i + 1;\n\
             }\n\
             for k in counts.keys() {\n\
                 println(f\"{k}={counts.get_or(k.clone(), 0)}\");\n\
             }\n\
         }");
    assert_eq!(output, "apple=2\nbanana=3\ncherry=1\n");
}

#[test]
fn test_sorted_map_entry_or_insert_with_and_mut_ref_push() {
    // or_insert / or_insert_with return a `mut ref V` slot; `.push` through it
    // writes back into the SortedMap's BTreeMap slot (the read/write_map_slot
    // choke points resolve a SortedMap slot by key).
    let output = run("fn main() {\n\
             let mut groups: SortedMap[String, Vec[i64]] = SortedMap.new();\n\
             groups.entry(\"even\".to_string()).or_insert(Vec.new()).push(2);\n\
             groups.entry(\"odd\".to_string()).or_insert_with(|| Vec.new()).push(1);\n\
             groups.entry(\"even\".to_string()).or_insert(Vec.new()).push(4);\n\
             for k in groups.keys() {\n\
                 println(f\"{k}:{groups.get_or(k.clone(), Vec.new()).len()}\");\n\
             }\n\
         }");
    assert_eq!(output, "even:2\nodd:1\n");
}

#[test]
fn test_map_entry_and_modify_skips_when_vacant() {
    // Vacant — closure does not fire; map state is unchanged.
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.entry(\"k\").and_modify(|v| { v += 1; });\n\
             println(m.is_empty());\n\
         }");
    assert_eq!(output, "true\n");
}

#[test]
fn test_map_entry_and_modify_chain_with_or_insert() {
    // The canonical chain: and_modify on Occupied increments; on Vacant
    // the trailing or_insert provides the seed value.
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.entry(\"a\").and_modify(|v| { v += 1; }).or_insert(1_i64);\n\
             m.entry(\"a\").and_modify(|v| { v += 1; }).or_insert(1_i64);\n\
             m.entry(\"a\").and_modify(|v| { v += 1; }).or_insert(1_i64);\n\
             match m.get(\"a\") {\n\
                 Some(x) => println(x),\n\
                 None => println(\"missing\"),\n\
             }\n\
         }");
    // First call: vacant → or_insert(1) sets a=1.
    // Second:    occupied → and_modify(+1) → a=2; or_insert no-op.
    // Third:     occupied → and_modify(+1) → a=3; or_insert no-op.
    assert_eq!(output, "3\n");
}

#[test]
fn test_map_entry_or_insert_deref_compound_assign_writes_through() {
    // Flagship counter idiom — `*m.entry(k).or_insert(0) += 1` must write
    // through the returned `mut ref V` into the live slot, NOT into a detached
    // clone. Three increments on a vacant-then-occupied key → 3. (Before the
    // MapSlotRef fix the interpreter dropped the write and this read 0.)
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             *m.entry(\"a\").or_insert(0_i64) += 1;\n\
             *m.entry(\"a\").or_insert(0_i64) += 1;\n\
             *m.entry(\"a\").or_insert(0_i64) += 1;\n\
             match m.get(\"a\") {\n\
                 Some(x) => println(x),\n\
                 None => println(\"missing\"),\n\
             }\n\
         }");
    assert_eq!(output, "3\n");
}

#[test]
fn test_map_entry_or_insert_two_step_mut_ref_writes_through() {
    // `let r = m.entry(k).or_insert(seed)` binds a `mut ref V`; both the
    // explicit `*r += 1` and the deref-elided `r += 1` write through to the
    // map slot. 10 → 11 → 12.
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             let r = m.entry(\"a\").or_insert(10_i64);\n\
             *r += 1;\n\
             r += 1;\n\
             match m.get(\"a\") {\n\
                 Some(x) => println(x),\n\
                 None => println(\"missing\"),\n\
             }\n\
         }");
    assert_eq!(output, "12\n");
}

#[test]
fn test_map_entry_or_insert_vec_push_writes_through() {
    // The per-key-Vec append idiom (design.md): `m.entry(k).or_insert(Vec.new())
    // .push(x)`. Pushed elements land in the map slot because the resolved
    // ref shares the slot's Arc-backed storage. Distinct keys stay separate.
    let output = run("fn main() {\n\
             let m: Map[String, Vec[i64]] = Map.new();\n\
             m.entry(\"x\").or_insert(Vec.new()).push(1_i64);\n\
             m.entry(\"x\").or_insert(Vec.new()).push(2_i64);\n\
             m.entry(\"y\").or_insert(Vec.new()).push(3_i64);\n\
             match m.get(\"x\") {\n\
                 Some(v) => println(v),\n\
                 None => println(\"none\"),\n\
             }\n\
             match m.get(\"y\") {\n\
                 Some(v) => println(v),\n\
                 None => println(\"none\"),\n\
             }\n\
         }");
    assert_eq!(output, "[1, 2]\n[3]\n");
}

#[test]
fn test_map_entry_or_insert_get_returns_snapshot_not_ref() {
    // A `MapSlotRef` must never escape the map into a user value: reading the
    // slot back via `get` yields a plain snapshot. Mutating the map after the
    // read does not retroactively change the captured value.
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             *m.entry(\"a\").or_insert(0_i64) += 5;\n\
             let snap = m.get_or(\"a\", -1);\n\
             *m.entry(\"a\").or_insert(0_i64) += 100;\n\
             println(snap);\n\
             println(m.get_or(\"a\", -1));\n\
         }");
    assert_eq!(output, "5\n105\n");
}

/// The same defect reached every container whose VALUES can be collections,
/// not just `Vec` — `Map` / `Set` / `SortedMap` each cloned one level deep.
/// The row only named `Vec[Vec[T]]`; these were found by reading the arm.
#[test]
fn test_clone_of_a_map_with_collection_values_is_deep() {
    let output = run("fn main() {\n\
             let mut m: Map[i64, Vec[i64]] = Map.new();\n\
             m.insert(1_i64, [7_i64]);\n\
             let cm = m.clone();\n\
             let mut got = cm[1_i64];\n\
             got[0] = 55_i64;\n\
             println(m[1_i64][0]);\n\
             println(got[0]);\n\
         }");
    assert_eq!(
        output, "7\n55\n",
        "a Map's Vec value was shared with the clone"
    );
}

#[test]
fn test_map_clone_preserves_entries() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"k\", 7_i64);\n\
             let n: Map[String, i64] = m.clone();\n\
             match n.get(\"k\") {\n\
                 Some(v) => println(v),\n\
                 None => println(\"missing\"),\n\
             }\n\
         }");
    assert_eq!(output, "7\n");
}

#[test]
fn test_map_clone_independent_after_source_insert() {
    // Inserting into the source after cloning leaves the clone unchanged.
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"k\", 1_i64);\n\
             let n: Map[String, i64] = m.clone();\n\
             m.insert(\"k\", 99_i64);\n\
             match n.get(\"k\") {\n\
                 Some(v) => println(v),\n\
                 None => println(\"missing\"),\n\
             }\n\
         }");
    assert_eq!(output, "1\n");
}

#[test]
fn test_try_insert_map_wraps_ok() {
    // First insert returns `Ok(None)` (no prior value); the entry is present.
    let output = run("fn main() {\n\
             let mut m: Map[String, i64] = Map.new();\n\
             match m.try_insert(\"k\", 1_i64) {\n\
                 Ok(prev) => match prev {\n\
                     Some(p) => println(p),\n\
                     None => println(\"no-prev\"),\n\
                 },\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
             println(m.len());\n\
         }");
    assert_eq!(output, "no-prev\n1\n");
}

#[test]
fn test_try_insert_set_wraps_ok() {
    let output = run("fn main() {\n\
             let mut s: Set[i64] = Set.new();\n\
             match s.try_insert(7_i64) {\n\
                 Ok(added) => println(added),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
             println(s.len());\n\
         }");
    assert_eq!(output, "true\n1\n");
}

#[test]
fn test_set_clone_preserves_membership() {
    let output = run("fn main() {\n\
             let s: Set[i64] = Set.new();\n\
             s.insert(5_i64);\n\
             let t: Set[i64] = s.clone();\n\
             println(t.contains(5_i64));\n\
             println(t.contains(99_i64));\n\
         }");
    assert_eq!(output, "true\nfalse\n");
}

#[test]
fn test_set_clone_independent_after_source_insert() {
    let output = run("fn main() {\n\
             let s: Set[i64] = Set.new();\n\
             s.insert(1_i64);\n\
             let t: Set[i64] = s.clone();\n\
             s.insert(2_i64);\n\
             println(t.len());\n\
             println(s.len());\n\
         }");
    assert_eq!(output, "1\n2\n");
}

#[test]
fn test_cli_parse_flag_set_returns_true() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "--verbose"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let p = Parser.new("greet")
                     .flag("--verbose", short: 'v', help: "");
                 match p.parse() {
                     Ok(args) => println(args.get_flag("--verbose")),
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "true\n");
}

#[test]
fn test_cli_parse_flag_unset_returns_false() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let p = Parser.new("greet")
                     .flag("--verbose", short: 'v', help: "");
                 match p.parse() {
                     Ok(args) => println(args.get_flag("--verbose")),
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "false\n");
}

#[test]
fn test_tracing_log_set_exporter_noop_silences() {
    // Registering `NoOpExporter` as the ambient sink silences `Log.*` —
    // the events route to NoOp's empty `export_event` instead of stdout.
    let output = run(r#"fn main() {
         Log.set_exporter(NoOpExporter {});
         Log.info("i");
         Log.error("e");
     }"#);
    assert_eq!(output, "");
}

#[test]
fn test_tracing_log_reset_restores_default() {
    // `Log.reset()` clears both the min-level and the registered sink, so
    // a previously-dropped level emits to stdout again afterward.
    let output = run(r#"fn main() {
         Log.set_min_level("error");
         Log.info("dropped");
         Log.reset();
         Log.info("kept");
     }"#);
    assert_eq!(output, "[info] kept\n");
}

// ── Set[T] ────────────────────────────────────────────────────────

#[test]
fn test_set_new_and_len() {
    let output = run("fn main() {\n\
         let s: Set[i64] = Set.new();\n\
         println(s.len());\n\
     }");
    assert_eq!(output, "0\n");
}

#[test]
fn test_set_insert_and_contains() {
    let output = run("fn main() {\n\
         let s: Set[i64] = Set.new();\n\
         s.insert(1_i64);\n\
         s.insert(2_i64);\n\
         println(s.contains(1_i64));\n\
         println(s.contains(3_i64));\n\
     }");
    assert_eq!(output, "true\nfalse\n");
}

#[test]
fn test_set_insert_dedup() {
    let output = run("fn main() {\n\
         let s: Set[i64] = Set.new();\n\
         s.insert(5_i64);\n\
         s.insert(5_i64);\n\
         println(s.len());\n\
     }");
    assert_eq!(output, "1\n");
}

#[test]
fn test_set_remove() {
    let output = run("fn main() {\n\
         let s: Set[i64] = Set.new();\n\
         s.insert(10_i64);\n\
         let was_present = s.remove(10_i64);\n\
         println(was_present);\n\
         println(s.len());\n\
     }");
    assert_eq!(output, "true\n0\n");
}

#[test]
fn test_set_is_empty() {
    let output = run("fn main() {\n\
         let s: Set[i64] = Set.new();\n\
         println(s.is_empty());\n\
         s.insert(1_i64);\n\
         println(s.is_empty());\n\
     }");
    assert_eq!(output, "true\nfalse\n");
}

#[test]
fn test_set_union() {
    let output = run("fn main() {\n\
         let a: Set[i64] = Set.new();\n\
         a.insert(1_i64);\n\
         a.insert(2_i64);\n\
         let b: Set[i64] = Set.new();\n\
         b.insert(2_i64);\n\
         b.insert(3_i64);\n\
         let c = a.union(b);\n\
         println(c.len());\n\
         println(c.contains(1_i64));\n\
         println(c.contains(3_i64));\n\
     }");
    assert_eq!(output, "3\ntrue\ntrue\n");
}

#[test]
fn test_set_intersection() {
    let output = run("fn main() {\n\
         let a: Set[i64] = Set.new();\n\
         a.insert(1_i64);\n\
         a.insert(2_i64);\n\
         let b: Set[i64] = Set.new();\n\
         b.insert(2_i64);\n\
         b.insert(3_i64);\n\
         let c = a.intersection(b);\n\
         println(c.len());\n\
         println(c.contains(2_i64));\n\
     }");
    assert_eq!(output, "1\ntrue\n");
}

#[test]
fn test_set_difference() {
    let output = run("fn main() {\n\
         let a: Set[i64] = Set.new();\n\
         a.insert(1_i64);\n\
         a.insert(2_i64);\n\
         let b: Set[i64] = Set.new();\n\
         b.insert(2_i64);\n\
         let c = a.difference(b);\n\
         println(c.len());\n\
         println(c.contains(1_i64));\n\
     }");
    assert_eq!(output, "1\ntrue\n");
}

#[test]
fn test_set_for_loop() {
    let output = run("fn main() {\n\
         let s: Set[i64] = Set.new();\n\
         s.insert(42_i64);\n\
         for x in s {\n\
             println(x);\n\
         }\n\
     }");
    assert_eq!(output, "42\n");
}

#[test]
fn test_vec_sorted_by_key_returns_new() {
    // sorted_by_key returns a new Vec; original retains insertion order.
    let output = run("fn main() {
            let mut xs: Vec[i64] = Vec.new();
            xs.push(3i64); xs.push(1i64); xs.push(2i64);
            let ys = xs.sorted_by_key(|x| x);
            for y in ys.iter() { println(y); }
            for x in xs.iter() { println(x); }
        }");
    assert_eq!(output, "1\n2\n3\n3\n1\n2\n");
}

// ── Match write-through on a `mut ref` enum payload (B-2026-07-23-12) ──
// The interpreter binds a match payload BY VALUE, so mutating a bound payload
// (`match v { Table(m) => m.insert(..) }`) updated only the arm-local copy and
// the change was lost — codegen writes through correctly. `eval_match` now
// reconstructs the scrutinee from the (mutated) arm bindings and stores it back
// to a bare-identifier / `self` place, so the `mut ref` mutation propagates.
#[test]
fn test_mut_ref_enum_payload_map_write_through() {
    // The ledger repro: an insert into a `Map` enum payload through `mut ref V`.
    let output = run("enum V { Table(Map[String, i64]) }\n\
         fn add(v: mut ref V) { match v { Table(m) => { m.insert(\"z\", 99); } } }\n\
         fn size(v: ref V) -> i64 { match v { Table(m) => m.len() as i64 } }\n\
         fn main() {\n\
             let mut mp: Map[String, i64] = Map.new();\n\
             mp.insert(\"a\", 1);\n\
             let mut t = V.Table(mp);\n\
             add(mut t);\n\
             println(size(t));\n\
         }");
    assert_eq!(output, "2\n");
}

#[test]
fn test_mut_ref_enum_struct_variant_payload_map_write_through() {
    // Struct-variant payload form of the same write-through.
    let output = run("enum V { Table { m: Map[String, i64], tag: i64 } }\n\
         fn add(v: mut ref V) { match v { Table { m, tag } => { m.insert(\"z\", 99); } } }\n\
         fn size(v: ref V) -> i64 { match v { Table { m, tag } => m.len() as i64 } }\n\
         fn main() {\n\
             let mut mp: Map[String, i64] = Map.new();\n\
             mp.insert(\"a\", 1);\n\
             let mut t = V.Table { m: mp, tag: 7 };\n\
             add(mut t);\n\
             println(size(t));\n\
         }");
    assert_eq!(output, "2\n");
}

#[test]
fn test_iter_on_set_yields_each_element_once() {
    // Set iterates in insertion order at the interpreter (storage backed
    // by Vec<Value>); next() yields each element exactly once before None.
    let output = run_no_errors(
        r#"
fn main() {
    let mut s: Set[i64] = Set.new();
    s.insert(7);
    s.insert(3);
    s.insert(11);
    let mut it = s.iter();
    let mut sum = 0;
    let mut count = 0;
    let mut more = true;
    while more {
        match it.next() {
            Some(n) => { sum = sum + n; count = count + 1; },
            None    => { more = false; },
        }
    }
    println(sum);
    println(count);
}
"#,
    );
    assert_eq!(output, "21\n3\n");
}

#[test]
fn test_iter_on_sorted_set_yields_ascending() {
    // SortedSet iterates ascending — verify next() honors that order.
    let output = run_no_errors(
        r#"
fn main() {
    let mut s: SortedSet[i64] = SortedSet.new();
    s.insert(11);
    s.insert(3);
    s.insert(7);
    let mut it = s.iter();
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
}
"#,
    );
    assert_eq!(output, "3\n7\n11\n");
}

#[test]
fn test_iter_on_map_yields_kv_tuples() {
    // Map.iter() yields (K, V) tuples. The ORDER they come out in is the
    // per-process hash order (B-2026-08-21-6), so this pins that `next()`
    // yields each PAIR intact and destructures — the two pairs are compared as
    // a set.
    let output = run_no_errors(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("a", 1);
    m.insert("b", 2);
    let mut it = m.iter();
    let (k1, v1) = it.next().unwrap();
    let (k2, v2) = it.next().unwrap();
    println(f"{k1}{v1}");
    println(f"{k2}{v2}");
}
"#,
    );
    let mut pairs: Vec<&str> = output.lines().collect();
    pairs.sort_unstable();
    assert_eq!(pairs, vec!["a1", "b2"]);
}

#[test]
fn test_for_loop_on_map_iter_destructures_kv() {
    // Map.iter() yields (K, V) tuples; for-loop binds via tuple pattern. The
    // ORDER is the per-process hash order (B-2026-08-21-6), so the pairs are
    // collected and sorted rather than pinned — what this test is about is the
    // destructuring, not the walk.
    let output = run_no_errors(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("x", 1);
    m.insert("y", 2);
    for (k, v) in m.iter() {
        println(f"{k}{v}");
    }
}
"#,
    );
    let mut pairs: Vec<&str> = output.lines().collect();
    pairs.sort_unstable();
    assert_eq!(pairs, vec!["x1", "y2"]);
}

// ── map / filter (wip-list2 subtask 3) ───────────────────────────

#[test]
fn test_iter_map_transforms_each_element() {
    // `.map(|x| x * 10)` rewrites every element through the closure.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    for n in v.iter().map(|x| x * 10) {
        println(n);
    }
}
"#,
    );
    assert_eq!(output, "10\n20\n30\n");
}

#[test]
fn test_iter_map_then_filter_chain() {
    // Adaptors chain: map first, then filter on the mapped values.
    // 1,2,3,4 → *3 → 3,6,9,12 → > 5 → 6,9,12.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    for n in v.iter().map(|x| x * 3).filter(|y| y > 5) {
        println(n);
    }
}
"#,
    );
    assert_eq!(output, "6\n9\n12\n");
}

#[test]
fn test_iter_filter_map_scalar() {
    // `filter_map(f: Fn(T) -> Option[U])` — map+filter fusion: keep each
    // `Some` payload, drop each `None` (B-2026-07-19-14). 1..5 → even*10 →
    // 20,40.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    for n in v.iter().filter_map(|x| if x % 2 == 0 { Some(x * 10) } else { None }) {
        println(n);
    }
}
"#,
    );
    assert_eq!(output, "20\n40\n");
}

#[test]
fn test_iter_filter_map_heap_collect() {
    // filter_map with a HEAP `U` (`String`) over an enum receiver, collected —
    // the `Some` payloads are the uppercased words, the `Num` variants drop.
    let output = run_no_errors(
        r#"
enum Tok { Word(String), Num(i64) }
fn main() {
    let v = [Tok.Word("hi".to_string()), Tok.Num(3), Tok.Word("yo".to_string())];
    let words: Vec[String] = v.iter().filter_map(|t| match t { Tok.Word(s) => Some(s.to_uppercase()), Tok.Num(_) => None }).collect();
    println(words.join(","));
}
"#,
    );
    assert_eq!(output, "HI,YO\n");
}

#[test]
fn test_iter_find_map_scalar_and_heap() {
    // `find_map(f: Fn(T) -> Option[U]) -> Option[U]` — first `Some(u)` the
    // closure produces, or `None` (B-2026-07-19-14). Covers a scalar payload,
    // an empty (`None`) result, and a HEAP `String` payload produced by the
    // closure (which the codegen backend defers to interp).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let a: Option[i64] = v.iter().find_map(|x| if x > 2 { Some(x * 2) } else { None });
    match a { Some(n) => println(n), None => println(-1) }
    let b: Option[i64] = v.iter().find_map(|x| if x > 100 { Some(x) } else { None });
    match b { Some(n) => println(n), None => println(-1) }
    let w = ["hi".to_string(), "there".to_string(), "yo".to_string()];
    let c: Option[String] = w.iter().find_map(|s| if s.len() > 2 { Some(s.to_uppercase()) } else { None });
    match c { Some(s) => println(s), None => println("none") }
}
"#,
    );
    assert_eq!(output, "6\n-1\nTHERE\n");
}

#[test]
fn test_iter_filter_then_map_chain() {
    // Order matters — filter first, then map on filtered elements.
    // 1,2,3,4,5 → > 2 → 3,4,5 → +100 → 103,104,105.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    for n in v.iter().filter(|x| x > 2).map(|x| x + 100) {
        println(n);
    }
}
"#,
    );
    assert_eq!(output, "103\n104\n105\n");
}

#[test]
fn test_iter_map_via_next_step_by_step() {
    // map is lazy — each next() pull invokes the closure exactly once.
    // No more, no fewer (verified by counting println side effects).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let mut it = v.iter().map(|x| x + 100);
    println(it.next().unwrap());
    println(it.next().unwrap());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    assert_eq!(output, "101\n102\ndone\n");
}

#[test]
fn test_iter_map_on_map_kv_tuples() {
    // Map.iter() yields (K, V) tuples; .map(|pair| ...) gets the tuple.
    let output = run_no_errors(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("a", 1);
    m.insert("b", 2);
    for s in m.iter().map(|(k, v)| f"{k}={v}") {
        println(s);
    }
}
"#,
    );
    // Hash order, not insertion order (B-2026-08-21-6) — what this pins is
    // the tuple reaching the closure destructured, not the sequence.
    let mut lines: Vec<&str> = output.lines().collect();
    lines.sort_unstable();
    assert_eq!(lines, vec!["a=1", "b=2"]);
}

#[test]
fn test_iter_collect_after_map_collects_mapped_values() {
    // map then collect — closure runs once per element during collect's drain.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let xs: Vec[i64] = v.iter().map(|x| x * 10).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "10\n20\n30\n");
}

#[test]
fn test_iter_any_after_map_predicate_sees_mapped_values() {
    // Composes with map — the predicate sees mapped i64 (x * 10), so
    // it returns true once the running x*10 exceeds 25 (i.e. on x=3).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let b: bool = v.iter().map(|x| x * 10).any(|y| y > 25);
    println(b);
}
"#,
    );
    assert_eq!(output, "true\n");
}

#[test]
fn test_iter_enumerate_after_map_indexes_mapped_values() {
    // Map first, then enumerate — index counts mapped output positions
    // (same as source positions here, but enumerate sees the mapped item).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    for (i, y) in v.iter().map(|x| x * 100).enumerate() {
        println(f"{i}={y}");
    }
}
"#,
    );
    assert_eq!(output, "0=100\n1=200\n2=300\n");
}

#[test]
fn test_iter_flat_map_yields_concatenated_inner_iters() {
    // For each outer item, closure yields a 2-element inner. Result
    // concatenates them in outer-then-inner order.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    for x in v.iter().flat_map(|n| { let inner = [n * 10, n * 100]; inner.iter() }) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "10\n100\n20\n200\n30\n300\n");
}

#[test]
fn test_iter_flat_map_empty_outer_yields_nothing() {
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let mut it = v.iter().flat_map(|n| { let inner = [n, n]; inner.iter() });
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

#[test]
fn test_iter_flat_map_inner_empty_skipped() {
    // Some outer items produce empty inner iterators — those are
    // skipped without yielding anything.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    for x in v.iter().flat_map(|n| {
        if n % 2 == 0 {
            let inner = [n * 10];
            inner.iter()
        } else {
            let inner: Vec[i64] = Vec[];
            inner.iter()
        }
    }) {
        println(x);
    }
}
"#,
    );
    // Outer 1 → empty; 2 → [20]; 3 → empty; 4 → [40].
    assert_eq!(output, "20\n40\n");
}

#[test]
fn test_iter_flat_map_state_persists_across_next_calls() {
    // next() pulls one item at a time. When the in-flight inner is
    // exhausted, the next pull must transparently switch to the
    // next outer's inner.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let mut it = v.iter().flat_map(|n| { let inner = [n * 10, n * 100]; inner.iter() });
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    assert_eq!(output, "10\n100\n20\n200\ndone\n");
}

#[test]
fn test_iter_flat_map_inner_can_have_adaptors() {
    // Closure returns an inner iterator with its own adaptor chain
    // (filter here). Those filter-rejected items don't surface as
    // flat_map yields.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let xs: Vec[i64] = v.iter().flat_map(|n| {
        let inner = [n - 1, n, n + 1];
        inner.iter().filter(|x| x > 1)
    }).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // Outer 1 → [0,1,2] filtered to [2]
    // Outer 2 → [1,2,3] filtered to [2,3]
    // Outer 3 → [2,3,4] filtered to [2,3,4]
    assert_eq!(output, "2\n2\n3\n2\n3\n4\n");
}

#[test]
fn test_iter_flat_map_composes_with_filter_after() {
    // Downstream filter applies to the flattened stream.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let xs: Vec[i64] = v.iter()
        .flat_map(|n| { let inner = [n, n * 10]; inner.iter() })
        .filter(|x| x > 5)
        .collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // Flattened: 1, 10, 2, 20, 3, 30. Filter >5: 10, 20, 30.
    assert_eq!(output, "10\n20\n30\n");
}

#[test]
fn test_iter_flat_map_with_take_short_circuits_outer() {
    // Downstream take(2) means we should only need to drain enough
    // outer items to produce 2 yields. Side-effect prefixes prove
    // that outer 3 is never visited.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    for x in v.iter()
        .flat_map(|n| { println(f"outer:{n}"); let inner = [n * 10, n * 100]; inner.iter() })
        .take(2)
    {
        println(f"y:{x}");
    }
}
"#,
    );
    // For-loop drains: outer:1 → inner [10, 100], take pulls both →
    // remaining=0. Source still pulled but step rejects. Outer 2 is
    // pulled (because take exhaustion happens AFTER outer 1's inner
    // is fully drained — the take step counts post-flat_map). The
    // test of importance: outer 3 must NEVER be pulled.
    // Expected: outer:1, y:10, y:100 (or similar drain order),
    // then nothing more — definitely no "outer:3".
    let lines: Vec<&str> = output.lines().collect();
    assert!(
        lines.contains(&"outer:1"),
        "outer:1 must fire, got: {:?}",
        lines
    );
    assert!(
        !lines.contains(&"outer:3"),
        "outer:3 must NOT fire (take(2) short-circuits), got: {:?}",
        lines
    );
    assert!(
        lines.iter().filter(|l| l.starts_with("y:")).count() == 2,
        "exactly 2 yields expected, got: {:?}",
        lines
    );
}

#[test]
fn test_iter_flat_map_after_filter() {
    // Filter the OUTER stream first — the closure only runs on
    // kept-outer items.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let xs: Vec[i64] = v.iter()
        .filter(|n| n % 2 == 0)
        .flat_map(|n| { let inner = [n, n * 10]; inner.iter() })
        .collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // Filtered outer: 2, 4. flat_map: [2, 20], [4, 40] → 2, 20, 4, 40.
    assert_eq!(output, "2\n20\n4\n40\n");
}

#[test]
fn test_iter_flat_map_with_count_terminal() {
    // Terminal count() drains and counts the flattened stream.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let n = v.iter().flat_map(|n| { let inner = [n, n, n]; inner.iter() }).count();
    println(n);
}
"#,
    );
    // 3 outer × 3 inner each = 9.
    assert_eq!(output, "9\n");
}

#[test]
fn test_iter_chunk_by_after_map_uses_mapped_item() {
    // Map runs before chunk_by — the groups carry mapped values.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let groups: Vec[Vec[i64]] = v.iter()
        .map(|x| x * 10)
        .chunk_by(|x| x > 25)
        .collect();
    for g in groups {
        let first = g[0];
        println(first);
    }
}
"#,
    );
    // Mapped: [10, 20, 30, 40]. Keys: false, false, true, true.
    // Groups: [10, 20], [30, 40]. First-elements: 10, 30.
    assert_eq!(output, "10\n30\n");
}

#[test]
fn test_iter_windows_after_map_yields_mapped_values() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let wins: Vec[Vec[i64]] = v.iter().map(|x| x * 10).windows(2).collect();
    for w in wins {
        let a = w[0];
        let b = w[1];
        println(f"{a},{b}");
    }
}
"#,
    );
    // Mapped: [10, 20, 30, 40]. Windows(2): [10,20], [20,30], [30,40].
    assert_eq!(output, "10,20\n20,30\n30,40\n");
}

#[test]
fn test_entry_chain_on_a_map_field_mutates_the_real_map() {
    // ORACLE for B-2026-08-18-34. `map.entry(k).or_insert(d).push(v)` is the
    // idiomatic append into a `Map[K, Vec[V]]`, and it worked only when the map
    // was a LOCAL. Rooted at a struct FIELD -- where a multimap normally lives
    // -- the interpreter reported "method 'push' not found on type 'unknown'"
    // because `Value::Entry` names its map by BINDING NAME and a field has none.
    //
    // The local leg is kept beside the field one so the two spellings are
    // asserted to agree, which is the property that actually matters: the row
    // was a check/run divergence, and `karac check` accepted both all along.
    //
    // `and_modify` is included because it is the arm that diverged AFTER the
    // rest was fixed -- codegen applied the modification on a field while the
    // interpreter silently skipped it, leaving the `or_insert` default.
    let output = run_no_errors(
        r#"
struct Holder { buckets: Map[i64, Vec[i64]], counts: Map[i64, i64] }

fn main() {
    let mut local: Map[i64, Vec[i64]] = Map.new();
    local.entry(1).or_insert(Vec.new()).push(7);
    local.entry(1).or_insert(Vec.new()).push(8);
    println(local[1].len());

    let mut h = Holder { buckets: Map.new(), counts: Map.new() };
    h.buckets.entry(1).or_insert(Vec.new()).push(7);
    h.buckets.entry(1).or_insert(Vec.new()).push(8);
    println(h.buckets[1].len());

    h.counts.entry(2).and_modify(|v| { *v = *v + 100; }).or_insert(7);
    println(h.counts[2]);
    h.counts.entry(2).and_modify(|v| { *v = *v + 100; }).or_insert(7);
    println(h.counts[2]);

    h.buckets.entry(3).or_insert_with(|| Vec.new()).push(42);
    println(h.buckets[3].len());
}
"#,
    );
    assert_eq!(output, "2\n2\n7\n107\n1\n");
}

#[test]
fn test_entry_chain_on_a_self_map_field() {
    // The `self.buckets…` spelling of B-2026-08-18-34, which is how the shape
    // actually appears: this is LeetCode 895 (Maximum Frequency Stack), the
    // kata that found the bug. `self` is a `mut ref self` receiver, and the row
    // established that the field root -- not the `self` context -- was the
    // variable, so this pins the combination that has to keep working.
    let output = run_no_errors(
        r#"
struct FreqStack { freq: Map[i64, i64], buckets: Map[i64, Vec[i64]], maxfreq: i64 }

impl FreqStack {
    fn push_val(mut ref self, v: i64) {
        let f = self.freq.get_or(v, 0) + 1;
        self.freq.insert(v, f);
        if f > self.maxfreq { self.maxfreq = f; }
        self.buckets.entry(f).or_insert(Vec.new()).push(v);
    }

    fn pop_val(mut ref self) -> i64 {
        let mut b = self.buckets.get_or(self.maxfreq, Vec.new());
        let top = b[b.len() - 1];
        b.pop();
        let blen = b.len() as i64;
        self.buckets.insert(self.maxfreq, b);
        self.freq.insert(top, self.freq.get_or(top, 0) - 1);
        if blen == 0 { self.maxfreq = self.maxfreq - 1; }
        return top;
    }
}

fn main() {
    let mut s = FreqStack { freq: Map.new(), buckets: Map.new(), maxfreq: 0 };
    s.push_val(5); s.push_val(7); s.push_val(5); s.push_val(7); s.push_val(4); s.push_val(5);
    println(s.pop_val());
    println(s.pop_val());
    println(s.pop_val());
    println(s.pop_val());
}
"#,
    );
    // The LeetCode 895 answer for this push sequence.
    assert_eq!(output, "5\n7\n5\n4\n");
}

#[test]
fn test_interpreter_slice_iter_chain_with_map_collect() {
    // `s.iter().map(|x| x * 2).collect()` returns [2, 4, 6].
    let output = run_no_errors(
        r#"
fn main() {
    let v = Vec[1, 2, 3];
    let s: Slice[i64] = v.as_slice();
    let xs: Vec[i64] = s.iter().map(|x| x * 2).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "2\n4\n6\n");
}

// ── offset_of[T](field.path) — interpreter parity with codegen ──
//
// The tree-walk interpreter had no `ExprKind::OffsetOf` arm at all
// (`karac run` panicked "unhandled expr" on any offset_of while
// `karac build` printed the offset). These mirror
// tests/codegen.rs::test_e2e_offset_of_* so run/build stay A/B-equal
// on the layout model (natural alignment, LLVM lowering shapes).

#[test]
fn offset_of_first_field_is_0() {
    let out = run_no_errors(
        "struct Point { x: i64, y: i64 }\n\
         fn main() { println(offset_of[Point](x)); }",
    );
    assert_eq!(out, "0\n");
}

#[test]
fn offset_of_second_field() {
    // `y` follows `x: i64` → offset 8.
    let out = run_no_errors(
        "struct Point { x: i64, y: i64 }\n\
         fn main() { println(offset_of[Point](y)); }",
    );
    assert_eq!(out, "8\n");
}

#[test]
fn offset_of_nested_path() {
    // offset(inner in Outer) + offset(y in Inner) = 4 + 4 = 8.
    let out = run_no_errors(
        "struct Inner { x: i32, y: i32 }\n\
         struct Outer { a: i32, inner: Inner, c: i32 }\n\
         fn main() { println(offset_of[Outer](inner.y)); }",
    );
    assert_eq!(out, "8\n");
}

#[test]
fn offset_of_mixed_alignment_padding() {
    // {bool, i32, i8, i64, i16} → 0, 4, 8, 16, 24 under natural
    // alignment (verified byte-identical against karac build).
    let out = run_no_errors(
        "struct Mixed { a: bool, b: i32, c: i8, d: i64, e: i16 }\n\
         fn main() {\n\
         \x20   println(offset_of[Mixed](a));\n\
         \x20   println(offset_of[Mixed](b));\n\
         \x20   println(offset_of[Mixed](c));\n\
         \x20   println(offset_of[Mixed](d));\n\
         \x20   println(offset_of[Mixed](e));\n\
         }",
    );
    assert_eq!(out, "0\n4\n8\n16\n24\n");
}

#[test]
fn offset_of_heap_fields_use_abi_shapes() {
    // String/Vec are 24-byte {ptr,len,cap} aggregates in the compiled
    // ABI; the interpreter's layout model must agree even though its
    // runtime values are boxed differently. tag:i8 pads to 8, name
    // spans 8..32, items 32..56, tail lands at 56 (build-verified).
    let out = run_no_errors(
        "struct Heapy { tag: i8, name: String, items: Vec[i64], tail: bool }\n\
         fn main() {\n\
         \x20   println(offset_of[Heapy](name));\n\
         \x20   println(offset_of[Heapy](items));\n\
         \x20   println(offset_of[Heapy](tail));\n\
         }",
    );
    assert_eq!(out, "8\n32\n56\n");
}

#[test]
fn offset_of_enum_field_tagged_word_layout() {
    // Shape = {i64 tag, i64 × 3 payload words} (Label(String) is the
    // widest variant at 3 words) = 32 bytes, 8-aligned → sh at 8,
    // post at 40 (build-verified).
    let out = run_no_errors(
        "enum Shape { Dot, Line(i64, i64), Label(String) }\n\
         struct HasEnum { pre: i8, sh: Shape, post: i32 }\n\
         fn main() {\n\
         \x20   println(offset_of[HasEnum](sh));\n\
         \x20   println(offset_of[HasEnum](post));\n\
         }",
    );
    assert_eq!(out, "8\n40\n");
}

#[test]
fn offset_of_three_level_nested_path() {
    // Outer is all-i32 → align 4, size 16; `o` lands at 4 (past
    // pad:i8), inner at o+4, x at +0 → 8. `z` follows o's end (20)
    // aligned up to 8 → 24. Both build-verified.
    let out = run_no_errors(
        "struct Inner { x: i32, y: i32 }\n\
         struct Outer { a: i32, inner: Inner, c: i32 }\n\
         struct Nest2 { pad: i8, o: Outer, z: i64 }\n\
         fn main() {\n\
         \x20   println(offset_of[Nest2](o.inner.x));\n\
         \x20   println(offset_of[Nest2](z));\n\
         }",
    );
    assert_eq!(out, "8\n24\n");
}

// B-2026-07-08-14: mutating a by-value collection (`Map`/`Set`/`SortedMap`/
// `SortedSet`) or a `String` through a NON-identifier place (a struct field, an
// index slot) must persist under the interpreter, matching codegen. Before the
// fix only a bare-identifier receiver was written back, so `c.index.insert(..)`
// mutated a copy and the insert was lost (interp printed the un-mutated length).
#[test]
fn test_map_mutation_through_struct_field_persists() {
    let src = "struct Cache { index: Map[i64, u64] }\n\
               fn main() {\n\
               \x20 let mut c = Cache { index: Map.new() };\n\
               \x20 let _ = c.index.insert(5, 9);\n\
               \x20 let _ = c.index.insert(6, 10);\n\
               \x20 println(c.index.len());\n\
               }";
    assert_eq!(run(src), "2\n");
}

#[test]
fn test_map_mutation_through_index_place_persists() {
    // `v[0].insert(..)` — mutation through an index slot (not just a bare name).
    let src = "fn main() {\n\
               \x20 let m0: Map[i64, i64] = Map.new();\n\
               \x20 let mut v: Vec[Map[i64, i64]] = Vec.new();\n\
               \x20 v.push(m0);\n\
               \x20 let _ = v[0].insert(5, 50);\n\
               \x20 let _ = v[0].insert(6, 60);\n\
               \x20 println(v[0].len());\n\
               }";
    assert_eq!(run(src), "2\n");
}

/// B-2026-07-30-11 (Map-values leg) — interpreter twin of `tests/codegen.rs`'s
/// `e2e_map_values_run_user_drop_bodies`, same source and expected string.
/// One Drop-bearing value per map because iteration order is per-backend
/// (unordered-map semantics); the walk keys on the te the Let arm records
/// through codegen's exact chain.
#[test]
fn test_map_values_run_user_drop_bodies() {
    assert_eq!(
        run("struct Res { id: i64 }\n\
             impl Drop for Res { fn drop(mut ref self) { println(90 + self.id); } }\n\
             struct HeapRes { name: String, id: i64 }\n\
             impl Drop for HeapRes { fn drop(mut ref self) { println(80 + self.id); } }\n\
             fn main() {\n\
                 let mut m: Map[i64, Res] = Map.new();\n\
                 m.insert(1, Res { id: 1 });\n\
                 println(1);\n\
                 let mut m2: Map[i64, Res] = Map.new();\n\
                 m2.insert(5, Res { id: 5 });\n\
                 let out = m2.remove(5);\n\
                 println(2);\n\
                 let mut m3: Map[i64, Res] = Map.new();\n\
                 m3.insert(7, Res { id: 7 });\n\
                 let m4 = m3;\n\
                 println(3);\n\
                 let mut m5: Map[i64, Res] = Map.new();\n\
                 let r = Res { id: 4 };\n\
                 m5.insert(4, r);\n\
                 println(4);\n\
                 let mut m6: Map[i64, HeapRes] = Map.new();\n\
                 m6.insert(9, HeapRes { name: \"value-payload-string\", id: 9 });\n\
                 println(5);\n\
                 let mut m7: Map[i64, i64] = Map.new();\n\
                 m7.insert(2, 2);\n\
                 println(6);\n\
             }\n"),
        "91\n1\n95\n2\n97\n3\n94\n4\n89\n5\n6\n"
    );
}

/// B-2026-07-31-27 — a whole-map/set rebind (`let m2 = m;`) keeps the value
/// READABLE through the destination: rebind-then-return, chained rebinds with
/// a mutation through the final binding, and a Set rebind. The interpreter's
/// Rc semantics were always correct here; this is the oracle half of the pair
/// with `tests/codegen.rs`'s `e2e_map_whole_rebind_transfers_ownership`, where
/// the move nulls the source slot and `transfer_map_handle_on_rebind` hands
/// the scope-exit free to the destination (before the fix codegen leaked the
/// entire map; a wrong fix that instead dropped the source-null would UAF the
/// rebind-then-return shape — both drifts show up as output divergence here).
#[test]
fn test_map_whole_rebind_transfers_ownership() {
    assert_eq!(
        run("fn make() -> Map[i64, i64] {\n\
                 let mut m: Map[i64, i64] = Map.new();\n\
                 m.insert(1, 11);\n\
                 let m2 = m;\n\
                 m2\n\
             }\n\
             fn main() {\n\
                 let got = make();\n\
                 println(got.get(1).unwrap());\n\
                 let mut a: Map[i64, i64] = Map.new();\n\
                 a.insert(3, 30);\n\
                 let b = a;\n\
                 let mut c = b;\n\
                 c.insert(4, 40);\n\
                 println(c.len());\n\
                 println(c.get(4).unwrap());\n\
                 let mut s: Set[i64] = Set.new();\n\
                 s.insert(9);\n\
                 let s2 = s;\n\
                 println(s2.contains(9));\n\
             }\n"),
        "11\n2\n40\ntrue\n"
    );
}

/// B-2026-07-30-11 (Set-elements leg) — interpreter twin of
/// `tests/codegen.rs`'s `e2e_set_element_drop_bodies_fire`, same source and
/// expected string. A `Set[DropStruct]` element's body never fired at the
/// binding's death — Set lowers to the KEY half of the map table, and the
/// values walk never looked there. Single element keeps the expected
/// string deterministic (a multi-element Set walk is storage-ordered under
/// codegen vs insertion-ordered here — the same unordered-container
/// difference `for x in s` has; the two-element parity + valgrind evidence
/// lives in the session probes).
#[test]
fn test_set_element_drop_bodies_fire() {
    assert_eq!(
        run("#[derive(Hash, Eq)]\n\
             struct Res { id: i64 }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id}\")\n\
                 }\n\
             }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let mut s: Set[Res] = Set.new();\n\
                 s.insert(Res { id: 5 });\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 5\nend\n"
    );
}

/// B-2026-08-01-28 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_for_loop_elem_map_set_field_consumes` (the interpreter was already
/// correct; the twin pins parity for the Map/Set insert + field move-out
/// consume arms over for-loop element bindings).
#[test]
fn test_for_loop_elem_map_set_field_consumes() {
    assert_eq!(
        run("#[derive(Hash, Eq, Ord)]\n\
             struct P { a: i64, s: String }\n\
             struct Header { name: String, value: String }\n\
             fn main() {\n\
                 let mut hs: Vec[Header] = Vec.new();\n\
                 hs.push(Header { name: f\"n{1}\", value: f\"v{1}\" });\n\
                 hs.push(Header { name: f\"n{2}\", value: f\"v{2}\" });\n\
                 let mut m: Map[i64, Header] = Map.new();\n\
                 let mut i = 0;\n\
                 for h in hs {\n\
                     let _ = m.insert(i, h);\n\
                     i = i + 1;\n\
                 }\n\
                 match m.get(1) {\n\
                     Some(h) => println(h.value),\n\
                     None => println(\"none\"),\n\
                 }\n\
                 let mut ps: Vec[P] = Vec.new();\n\
                 ps.push(P { a: 1, s: f\"x{1}\" });\n\
                 ps.push(P { a: 2, s: f\"y{2}\" });\n\
                 let mut set: Set[P] = Set.new();\n\
                 for p in ps {\n\
                     set.insert(p);\n\
                 }\n\
                 println(set.len());\n\
                 let mut src: Vec[Header] = Vec.new();\n\
                 src.push(Header { name: f\"a{7}\", value: f\"b{7}\" });\n\
                 let mut names: Vec[String] = Vec.new();\n\
                 for h in src {\n\
                     names.push(h.name);\n\
                 }\n\
                 for n in names { println(n); }\n\
             }\n"),
        "v2\n2\na7\n"
    );
}

/// B-2026-08-13-5 — interpreter twin of `tests/codegen.rs`'s
/// `test_e2e_slice_and_map_elem_field_read_is_a_copy`, same source and expected
/// string.
///
/// The interpreter is the oracle this row's fix was argued from: it has always
/// treated a field read off a slice element or a map value as a COPY, which is
/// what makes cloning in codegen a correction rather than a preference. Pinning
/// the bytes here keeps that argument checkable.
#[test]
fn test_slice_and_map_elem_field_read_is_a_copy() {
    assert_eq!(
        run("struct Pair { word: String, n: i64 }\n\
             fn take(xs: Slice[Pair]) -> String { let w = xs[0].word; w }\n\
             fn main() {\n\
                 let k = 1;\n\
                 let mut ps: Vec[Pair] = Vec.new();\n\
                 ps.push(Pair { word: f\"a{k}\", n: 7 });\n\
                 println(take(ps[0..1]));\n\
                 println(ps[0].word);\n\
                 let mut m: Map[String, Pair] = Map.new();\n\
                 m.insert(f\"k{k}\", Pair { word: f\"c{k}\", n: 8 });\n\
                 let mapped = m[f\"k{k}\"].word;\n\
                 println(mapped);\n\
                 println(m[f\"k{k}\"].word);\n\
                 println(m[f\"k{k}\"].n);\n\
             }"),
        "a1\na1\nc1\nc1\n8\n"
    );
}

/// B-2026-08-02-18 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_struct_field_tuple_and_map_drop_bodies` (STASH-PROVEN pin: the
/// interp's field walk had no tuple or Map/Set arm, so all three drop
/// lines were missing pre-fix). Same source and expected string.
#[test]
fn test_struct_field_tuple_and_map_drop_bodies() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             struct DuoR { pair: (Res, i64), tag: i64 }\n\
             struct Duo[T] { pair: (T, i64), tag: i64 }\n\
             struct MapHold { m: Map[i64, Res], tag: i64 }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 {\n\
                     let d = DuoR { pair: (Res { id: 1, name: f\"aa{1}\" }, 5), tag: 7 };\n\
                     println(d.tag);\n\
                 }\n\
                 println(\"b\");\n\
                 {\n\
                     let g: Duo[Res] = Duo { pair: (Res { id: 2, name: f\"bb{2}\" }, 6), tag: 8 };\n\
                     println(g.tag);\n\
                 }\n\
                 println(\"c\");\n\
                 {\n\
                     let mut h = MapHold { m: Map.new(), tag: 9 };\n\
                     h.m.insert(1i64, Res { id: 3, name: f\"cc{3}\" });\n\
                     println(h.tag);\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "a\n7\ndrop 1 aa1\nb\n8\ndrop 2 bb2\nc\n9\ndrop 3 cc3\nend\n"
    );
}

#[test]
fn test_map_value_struct_vec_field_element_bodies() {
    // B-2026-08-02-24 — a Map VALUE that is a struct carrying its Drop only
    // through a Vec FIELD's elements. The interpreter's type-level gate
    // (`type_name_runs_user_drop`) recursed into struct fields by HEAD NAME
    // only, so `Holder { xs: Vec[Res] }` read as head "Vec" -> drop-free,
    // the Map-value bodies registration never armed, and the element body
    // was silent while AOT printed it — a run-vs-build divergence. The gate
    // now sees one container level, matching codegen's `type_runs_user_drop`.
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             struct Holder { xs: Vec[Res], tag: i64 }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 {\n\
                     let mut m: Map[i64, Holder] = Map.new();\n\
                     let mut xs: Vec[Res] = Vec.new();\n\
                     xs.push(Res { id: 3, name: f\"mm{3}\" });\n\
                     let _ = m.insert(1, Holder { xs: xs, tag: 7 });\n\
                     println(m.len());\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "a\n1\ndrop 3 mm3\nend\n"
    );
}

/// B-2026-08-02-20 (leg 1) — a Set/SortedMap binding moved into a
/// struct-literal FIELD must not fire its element/value bodies at the
/// SOURCE binding's death; the holder owns the one logical value and
/// fires at ITS death (codegen already did, by nulling the source slot).
/// Pre-fix `record_container_move_source_name`'s value-shape match was
/// missing Set/SortedMap/SortedSet, so every body fired early as well —
/// a run-vs-build divergence.
#[test]
fn test_set_and_sortedmap_into_literal_field_source_walk_suppressed() {
    assert_eq!(
        run("#[derive(Hash, Eq)]\n\
             struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             struct SetHold { s: Set[Res], tag: i64 }\n\
             struct SmHold { m: SortedMap[i64, Res], tag: i64 }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 {\n\
                     let mut s: Set[Res] = Set.new();\n\
                     let _ = s.insert(Res { id: 3, name: f\"s{3}\" });\n\
                     let h = SetHold { s: s, tag: 1 };\n\
                     println(h.tag);\n\
                 }\n\
                 println(\"mid\");\n\
                 {\n\
                     let mut m: SortedMap[i64, Res] = SortedMap.new();\n\
                     let _ = m.insert(2, Res { id: 4, name: f\"m{4}\" });\n\
                     let _ = m.insert(1, Res { id: 5, name: f\"n{5}\" });\n\
                     let g = SmHold { m: m, tag: 2 };\n\
                     println(g.tag);\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "a\n1\ndrop 3 s3\nmid\n2\ndrop 5 n5\ndrop 4 m4\nend\n"
    );
}

#[test]
fn test_struct_field_set_and_sortedmap_bodies() {
    assert_eq!(
        run("#[derive(Hash, Eq)]\n\
             struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             struct SetHold { s: Set[Res], tag: i64 }\n\
             struct SmHold { m: SortedMap[i64, Res], tag: i64 }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 {\n\
                     let mut h = SetHold { s: Set.new(), tag: 1 };\n\
                     let _ = h.s.insert(Res { id: 3, name: f\"s{3}\" });\n\
                     println(h.tag);\n\
                 }\n\
                 println(\"mid\");\n\
                 {\n\
                     let mut g = SmHold { m: SortedMap.new(), tag: 2 };\n\
                     let _ = g.m.insert(2, Res { id: 4, name: f\"m{4}\" });\n\
                     let _ = g.m.insert(1, Res { id: 5, name: f\"n{5}\" });\n\
                     println(g.tag);\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "a\n1\ndrop 3 s3\nmid\n2\ndrop 5 n5\ndrop 4 m4\nend\n"
    );
}

/// B-2026-08-01-17 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_sorted_container_drop_bodies_key_order`, same source and expected
/// string. The interpreter always drained sorted-container element/value
/// Drop bodies in ascending key order — this pin holds that side of the
/// parity while the codegen walker learns the same order.
#[test]
fn test_sorted_container_drop_bodies_key_order() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             #[derive(Hash, Eq, Ord)]\n\
             struct Tag { id: i64 }\n\
             impl Drop for Tag {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"tag {self.id}\")\n\
                 }\n\
             }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let mut m: SortedMap[i64, Res] = SortedMap.new();\n\
                 let _ = m.insert(6, Res { id: 6, name: f\"s{6}\" });\n\
                 let _ = m.insert(5, Res { id: 5, name: f\"s{5}\" });\n\
                 let _ = m.insert(7, Res { id: 7, name: f\"s{7}\" });\n\
                 println(\"b\");\n\
                 let mut s: SortedSet[Tag] = SortedSet.new();\n\
                 s.insert(Tag { id: 6 });\n\
                 s.insert(Tag { id: 5 });\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 5 s5\ndrop 6 s6\ndrop 7 s7\nb\ntag 5\ntag 6\nend\n"
    );
}

/// B-2026-07-30-11 (SortedMap-values leg) — interpreter twin of
/// `tests/codegen.rs`'s `e2e_sortedmap_value_drop_bodies_fire`, same source
/// and expected string. Both walks emit in key order for a SortedMap, so
/// unlike the plain-Map twin this expectation is order-exact.
#[test]
fn test_sortedmap_value_drop_bodies_fire() {
    assert_eq!(
        run("struct Res { id: i64 }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id}\")\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let mut sm: SortedMap[i64, Res] = SortedMap.new();\n\
                 sm.insert(2, Res { id: 72 });\n\
                 sm.insert(1, Res { id: 71 });\n\
                 println(\"a\");\n\
                 let _ = sm.insert(1, Res { id: 73 });\n\
                 println(\"b\");\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 71\ndrop 73\ndrop 72\nb\nend\n"
    );
}

#[test]
fn test_struct_field_and_map_value_tuple_run_element_drop() {
    // B-2026-08-03-7 interp twin. Both positions were silent here too: the
    // struct-field tuple walk handled only a DIRECT struct item, and the
    // map-value walk gates on a declared HEAD name, which a tuple TE does not
    // have. Both now route their items through the shared
    // `run_tuple_item_user_drops`, the single walk behind every
    // tuple-as-content position.
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             struct W { p: (Option[Res], i64) }\n\
             fn main() {\n\
                 println(\"struct-field-tuple:\");\n\
                 { let w = W { p: (Option.Some(Res { id: 1, name: f\"a{1}\" }), 10) }; println(w.p.1); }\n\
                 println(\"map-value-tuple:\");\n\
                 {\n\
                     let mut m: Map[i64, (Option[Res], i64)] = Map.new();\n\
                     m.insert(5, (Option.Some(Res { id: 2, name: f\"bb{2}\" }), 20));\n\
                     println(m.len());\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "struct-field-tuple:\n10\ndrop 1 a1\nmap-value-tuple:\n1\ndrop 2 bb2\nend\n"
    );
}

/// B-2026-08-06-6 — the ORACLE half. The interpreter has always transferred a
/// moved-out `Map` field correctly; codegen left the SOURCE handle live, so
/// the owner's struct drop freed storage the destination still owned and the
/// program segfaulted on a use-after-free.
///
/// Passes before and after, which is its job: it is the reference answer the
/// codegen twin `e2e_map_field_move_out_neutralizes_the_source` is measured
/// against. Seed is the literal 1 here rather than `env.args().len()` for the
/// usual reason — an in-process interpreter test would see the TEST binary's
/// argv.
#[test]
fn test_map_field_move_out_transfers_the_handle() {
    assert_eq!(
        run(r#"struct MapH { m: Map[i64, String], tag: String }
struct SortH { s: SortedMap[i64, String] }

fn mk(i: i64, n: i64) -> Map[i64, String] {
    let mut m: Map[i64, String] = Map.new();
    m.insert(i, f"mapv-{i}-padded-out-to-force-a-real-heap-buffer-{n}");
    m.insert(i + 100i64, f"mapw-{i}-padded-out-to-force-a-real-heap-buffer-{n}");
    return m;
}

fn mks(i: i64, n: i64) -> SortedMap[i64, String] {
    let mut m: SortedMap[i64, String] = SortedMap.new();
    m.insert(i, f"sortv-{i}-padded-out-to-force-a-real-heap-buffer-{n}");
    return m;
}

fn take(h: MapH) -> Map[i64, String] { return h.m; }
fn eat(m: Map[i64, String]) -> i64 { return m.len(); }
fn eats(m: SortedMap[i64, String]) -> i64 { return m.len(); }

fn main() {
    let n: i64 = 1i64;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40i64 {
        // (a) field returned out of a by-value param — the SIGSEGV
        let h1: MapH = MapH { m: mk(i, n), tag: f"tag-{i}-payload" };
        let m1 = take(h1);
        acc = acc + m1.len();
        // (b) field moved into a local
        let h2: MapH = MapH { m: mk(i, n), tag: f"tag-{i}-payload" };
        let m2 = h2.m;
        acc = acc + m2.len();
        // (c) field passed to a consuming callee
        let h3: MapH = MapH { m: mk(i, n), tag: f"tag-{i}-payload" };
        acc = acc + eat(h3.m);
        // (d) SortedMap field moved out
        let h4: SortH = SortH { s: mks(i, n) };
        acc = acc + eats(h4.s);
        // CONTROLS, correct before and after: a field only READ, and a
        // WHOLE-struct move (which retracts the source's StructDrop rather
        // than neutralizing one field).
        let h5: MapH = MapH { m: mk(i, n), tag: f"tag-{i}-payload" };
        if h5.tag.contains("payload") { acc = acc + 1i64; }
        acc = acc + h5.m.len();
        let h6: MapH = MapH { m: mk(i, n), tag: f"tag-{i}-payload" };
        let h7 = h6;
        acc = acc + h7.m.len();
        i = i + 1i64;
    }
    println(acc);
}
"#),
        "480\n"
    );
}

/// B-2026-08-06-1 — the ORACLE half. The interpreter has always freed and
/// transferred a generic wrapper's bare-`T` `Map` / `Set` field correctly;
/// codegen classified the erased field as no-heap and freed nothing (a leak),
/// and once the classifier learned the Map head, neither move neutralizer knew
/// what the field was either (a double free).
///
/// Passes before and after, which is its job: it pins the reference answer the
/// codegen twin `e2e_bare_generic_param_map_field_is_freed_and_neutralized` is
/// measured against — memory correctness is not observable from here, only the
/// value is. Seed is the literal 1 rather than `env.args().len()` for the usual
/// reason: an in-process interpreter test would see the TEST binary's argv.
#[test]
fn test_option_map_set_field_transfers_and_frees_the_handle() {
    // B-2026-08-07-12 leg 1 ORACLE. The codegen fix is a memory-ownership
    // change with no observable output difference — stdout is 20440 both
    // before and after — so this exists to pin the VALUE the sanitizer twin
    // (`asan_option_map_set_field_owned_by_transfer_no_leak_no_double_free`)
    // asserts, independently of the backend that leaks it. If a future change
    // to `Option[Map]` field semantics alters what these moves observe, this
    // fails on the interpreter and the ASAN fixture's expectation is wrong
    // rather than merely unmet.
    assert_eq!(
        run(r#"struct Om { s: Option[Map[i64, String]] }
struct Os { s: Option[Set[i64]] }
struct Sib { p: Option[String], m: Option[Map[i64, String]] }
fn ignore_om(x: Om) -> i64 { 1 }
fn consume_opt(o: Option[Map[i64, String]]) -> i64 {
    match o {
        Option.Some(inner) => { inner.len() }
        Option.None => { 0 }
    }
}
fn eat_om(x: Om) -> i64 {
    match x.s {
        Option.Some(inner) => { inner.len() }
        Option.None => { 0 }
    }
}
fn mk_map(k: i64) -> Map[i64, String] {
    let mut m: Map[i64, String] = Map.new();
    m.insert(k, f"mapv-{k}-padded-out-to-force-a-real-heap-buffer");
    m
}
fn main() {
    let n: i64 = 1;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let a: Om = Om { s: Option.Some(mk_map(n + i)) };
        acc = acc + 1;
        let mut st: Set[i64] = Set.new();
        st.insert(n + i);
        let b: Os = Os { s: Option.Some(st) };
        acc = acc + 2;
        acc = acc + ignore_om(Om { s: Option.Some(mk_map(n + i)) }) * 4;
        let named: Om = Om { s: Option.Some(mk_map(n + i)) };
        acc = acc + ignore_om(named) * 8;
        let sib: Sib = Sib { p: Option.Some(f"sib-{n + i}-padded-out-well-past-inline"), m: Option.Some(mk_map(n + i)) };
        acc = acc + 16;
        let mo: Om = Om { s: Option.Some(mk_map(n + i)) };
        match mo.s {
            Option.Some(inner) => { acc = acc + inner.len() * 32; }
            Option.None => { acc = acc + 0; }
        }
        let wm: Om = Om { s: Option.Some(mk_map(n + i)) };
        acc = acc + consume_opt(wm.s) * 64;
        let lm: Om = Om { s: Option.Some(mk_map(n + i)) };
        let taken = lm.s;
        acc = acc + consume_opt(taken) * 128;
        acc = acc + eat_om(Om { s: Option.Some(mk_map(n + i)) }) * 256;
        i = i + 1;
    }
    println(acc);
}
"#),
        "20440\n"
    );
}

#[test]
fn test_bare_generic_param_map_field_transfers_the_handle() {
    assert_eq!(
        run(r#"struct Box[T] { v: T }
struct Trip[T] { a: String, v: T, n: i64 }

fn mk(i: i64, n: i64) -> Map[i64, String] {
    let mut m: Map[i64, String] = Map.new();
    m.insert(i, f"mapv-{i}-padded-out-to-force-a-real-heap-buffer-{n}");
    m.insert(i + 100i64, f"mapw-{i}-padded-out-to-force-a-real-heap-buffer-{n}");
    return m;
}

fn mkset(i: i64, n: i64) -> Set[String] {
    let mut s: Set[String] = Set.new();
    s.insert(f"setv-{i}-padded-out-to-force-a-real-heap-buffer-{n}");
    return s;
}

fn sink(b: Box[Map[i64, String]]) -> i64 { return b.v.len(); }
fn take(b: Box[Map[i64, String]]) -> Map[i64, String] { return b.v; }
fn eat(m: Map[i64, String]) -> i64 { return m.len(); }
fn peek(b: ref Box[Map[i64, String]]) -> i64 { return b.v.len(); }
fn sinkset(b: Box[Set[String]]) -> i64 { return b.v.len(); }
fn sinkmid(t: Trip[Map[i64, String]]) -> i64 {
    let mut r: i64 = t.v.len() + t.n;
    if t.a.contains("padding") { r = r + 1i64; }
    return r;
}

fn main() {
    let n: i64 = 1i64;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40i64 {
        // (a) read through a by-value param — the reported leak
        let b1 = Box { v: mk(i, n) };
        acc = acc + sink(b1);
        // (b) read through a plain LOCAL — not param-specific
        let b2 = Box { v: mk(i, n) };
        acc = acc + b2.v.len();
        // (c) the MID field of a multi-field wrapper (offset-sensitive)
        let t1 = Trip { a: f"lead-{i}-padding-{n}", v: mk(i, n), n: 3i64 };
        acc = acc + sinkmid(t1);
        // (d) the field RETURNED out of a by-value param
        let b3 = Box { v: mk(i, n) };
        let m1 = take(b3);
        acc = acc + m1.len();
        // (e) the field moved into a local
        let b4 = Box { v: mk(i, n) };
        let m2 = b4.v;
        acc = acc + m2.len();
        // (f) the field passed to a consuming callee
        let b5 = Box { v: mk(i, n) };
        acc = acc + eat(b5.v);
        // (g) a WHOLE-struct move
        let b6 = Box { v: mk(i, n) };
        let b7 = b6;
        acc = acc + sink(b7);
        // (h) a `ref` param read — the source keeps ownership
        let b8 = Box { v: mk(i, n) };
        acc = acc + peek(b8);
        // (i) the Set instantiation
        let b9 = Box { v: mkset(i, n) };
        acc = acc + sinkset(b9);
        // (j) a struct-pattern destructure
        let ba = Box { v: mk(i, n) };
        let Box { v } = ba;
        acc = acc + eat(v);
        // CONTROL, correct before and after: a struct that is never moved and
        // whose fields are only READ, so it must still free everything itself.
        let t2 = Trip { a: f"lead-{i}-padding-{n}", v: mk(i, n), n: 3i64 };
        if t2.a.contains("padding") { acc = acc + 1i64; }
        acc = acc + t2.v.len();
        i = i + 1i64;
    }
    println(acc);
}
"#),
        "1040\n"
    );
}

/// B-2026-09-04-16 — the interpreter oracle for the sortedness-marker leak.
///
/// This backend carries sortedness IN THE VALUE (`Value::SortedMap` /
/// `Value::SortedSet`), so it has no name-keyed marker to leak and was already
/// right on every line. That is exactly what makes it the oracle: codegen
/// tracks sortedness in a `HashSet<String>` of BINDING NAMES that outlived the
/// function it was registered in, so a plain `Map`/`Set` reusing a
/// `SortedMap`/`SortedSet` binding's name in a later function iterated sorted
/// and rendered with the sorted prefix. Compiled output was
/// `01 DIVERGED` / `SortedMap{7: 7}` / `03 SortedMap{7: 7}` / `SortedSet{5}`.
///
/// The program is byte-identical to `SORTED_MARKER_LEAK_SRC` in
/// `tests/codegen.rs`, whose twin asserts this exact output under
/// `karac build`. Line 01 compares two identically-filled plain `Map`s to each
/// other rather than to a fixed sequence — hash order is per-process random, so
/// naming a permutation would be the wrong assertion in either backend.
#[test]
fn test_sorted_marker_does_not_leak_across_functions() {
    assert_eq!(
        run(r#"
fn twin() -> i64 {
    let mut cols: SortedMap[i64, i64] = SortedMap.new();
    cols.insert(1, 1);
    let mut only: SortedSet[i64] = SortedSet.new();
    only.insert(4);
    return cols.len() + only.len();
}
fn colliding() -> String {
    let mut cols: Map[i64, i64] = Map.new();
    for i in 0..12 { cols.insert((i * 37) % 97 - 50, i); }
    let k = cols.keys();
    let mut s = "".to_string();
    for i in 0..k.len() { s = s + k[i].to_string() + ","; }
    return s;
}
fn clean() -> String {
    let mut zzz: Map[i64, i64] = Map.new();
    for i in 0..12 { zzz.insert((i * 37) % 97 - 50, i); }
    let k = zzz.keys();
    let mut s = "".to_string();
    for i in 0..k.len() { s = s + k[i].to_string() + ","; }
    return s;
}
fn main() {
    println(if colliding() == clean() { "01 same" } else { "01 DIVERGED" });
    let mut cols: Map[i64, i64] = Map.new();
    cols.insert(7, 7);
    println(cols);
    println(f"03 {cols}");
    let mut only: Set[i64] = Set.new();
    only.insert(5);
    println(only);
    let mut keep: SortedMap[i64, i64] = SortedMap.new();
    keep.insert(9, 9);
    keep.insert(3, 3);
    println(keep);
    println(twin());
}
"#),
        "01 same\n{7: 7}\n03 {7: 7}\nSet{5}\nSortedMap{3: 3, 9: 9}\n2\n"
    );
}

/// B-2026-08-18-26 — the interpreter oracle for READ METHODS on a freshly
/// returned `Set`/`Map`. `mk_set().len()` printed 0 for a three-element set
/// under `karac build`, `mk_map().len()` printed 152, `is_empty()` printed raw
/// bytes, and six such calls in one program segfaulted — while this side was
/// correct throughout, which is what made it the oracle.
///
/// The BUILD counter is the other half and the more important one: the AOT
/// lowering evaluated the receiver TWICE, so a producer with side effects ran
/// twice and the first handle was stranded. One line of output per call is the
/// assertion.
#[test]
fn test_freshtemp_mapset_read_methods_oracle() {
    let out = run_no_errors(
        "fn mk_set(n: i64) -> Set[i64] {\n\
             println(\"B\");\n\
             let mut s: Set[i64] = Set.new();\n\
             let mut i = 0;\n\
             while i < n { s.insert(i); i = i + 1; }\n\
             return s;\n\
         }\n\
         fn mk_map(n: i64) -> Map[String, i64] {\n\
             let mut m: Map[String, i64] = Map.new();\n\
             let mut i = 0;\n\
             while i < n { m.insert(f\"k{i}\", i); i = i + 1; }\n\
             return m;\n\
         }\n\
         fn main() {\n\
             println(mk_set(3).len().to_string());\n\
             println(mk_set(3).is_empty().to_string());\n\
             println(mk_set(3).contains(2).to_string());\n\
             println(mk_map(2).len().to_string());\n\
             println(mk_map(2).contains_key(\"k0\").to_string());\n\
         }\n",
    );
    assert_eq!(out, "B\n3\nB\nfalse\nB\ntrue\n2\ntrue\n");
}

#[test]
fn map_iteration_visits_every_entry_exactly_once() {
    // Order is unspecified and varies per process (B-2026-08-21-6), so what is
    // pinned is the multiset: four distinct pairs, none dropped, none doubled.
    // A walk that skipped or repeated an entry still fails here.
    let out = run("fn main() {\n\
        let mut m: Map[i64, i64] = Map.new();\n\
        m.insert(30, 3);\n\
        m.insert(10, 1);\n\
        m.insert(20, 2);\n\
        m.insert(5, 0);\n\
        for (k, v) in m { println(f\"{k}={v}\"); }\n\
    }");
    assert_eq!(sorted_prefix(&out, 4), "10=1\n20=2\n30=3\n5=0\n");
}

#[test]
fn map_overwrite_replaces_rather_than_duplicates() {
    // An overwrite updates the existing entry rather than appending a second
    // one for the same key. Position is no longer observable (the walk is hash
    // ordered), but "two entries for key 1" would still show up here as three
    // lines instead of two.
    let out = run("fn main() {\n\
        let mut m: Map[i64, i64] = Map.new();\n\
        m.insert(1, 1);\n\
        m.insert(2, 2);\n\
        m.insert(1, 99);\n\
        for (k, v) in m { println(f\"{k}={v}\"); }\n\
    }");
    assert_eq!(sorted_prefix(&out, 2), "1=99\n2=2\n");
}

#[test]
fn map_remove_keeps_the_survivors_findable() {
    // The index stores POSITIONS, and removing an entry shifts every later one
    // down. Without the rebuild that follows the removal, the survivors would
    // still print (the `Vec` is walked directly) but would no longer be found
    // by key — a silent wrong answer that only a lookup after a removal shows.
    let out = run("fn main() {\n\
        let mut m: Map[i64, i64] = Map.new();\n\
        m.insert(10, 1);\n\
        m.insert(20, 2);\n\
        m.insert(30, 3);\n\
        m.remove(10);\n\
        for (k, v) in m { println(f\"{k}={v}\"); }\n\
        match m.get(30) { Some(v) => { println(f\"found30={v}\"); } None => { println(\"LOST30\"); } }\n\
        match m.get(20) { Some(v) => { println(f\"found20={v}\"); } None => { println(\"LOST20\"); } }\n\
        match m.get(10) { Some(v) => { println(f\"found10={v}\"); } None => { println(\"gone10\"); } }\n\
    }");
    assert_eq!(
        sorted_prefix(&out, 2),
        "20=2\n30=3\nfound30=3\nfound20=2\ngone10\n"
    );
}

#[test]
fn map_binding_is_a_value_not_an_alias() {
    // THE regression this change could have introduced. `Map` storage is now
    // shared by the derived `Clone`, so independence rests entirely on
    // `deep_clone_value` firing at the binding. If it did not, inserting into
    // `n` would show up in `m`.
    let out = run("fn main() {\n\
        let mut m: Map[i64, i64] = Map.new();\n\
        m.insert(1, 1);\n\
        let mut n = m;\n\
        n.insert(2, 2);\n\
        println(f\"m={m.len()} n={n.len()}\");\n\
    }");
    assert_eq!(out, "m=1 n=2\n");
}

#[test]
fn set_binding_is_a_value_not_an_alias() {
    let out = run("fn main() {\n\
        let mut s: Set[i64] = Set.new();\n\
        s.insert(1);\n\
        let mut t = s;\n\
        t.insert(2);\n\
        println(f\"s={s.len()} t={t.len()}\");\n\
    }");
    assert_eq!(out, "s=1 t=2\n");
}

#[test]
fn map_in_a_struct_field_is_independent_per_struct() {
    // The same independence one level down: a map reached through a field is
    // shared storage too, so copying the struct must not alias its map.
    let out = run("struct H { m: Map[i64, i64] }\n\
    fn main() {\n\
        let mut a = H { m: Map.new() };\n\
        a.m.insert(1, 1);\n\
        let mut b = a;\n\
        b.m.insert(2, 2);\n\
        println(f\"a={a.m.len()} b={b.m.len()}\");\n\
    }");
    assert_eq!(out, "a=1 b=2\n");
}

#[test]
fn set_iteration_and_membership_survive_a_removal() {
    // `Set.remove` used to `swap_remove`, moving the last element into the
    // hole. What matters — and all that is specified — is that every survivor
    // is still walked and still a member; the order they come out in is the
    // per-process hash order (B-2026-08-21-6), so it is sorted before the
    // comparison rather than pinned.
    let out = run("fn main() {\n\
        let mut s: Set[i64] = Set.new();\n\
        s.insert(9); s.insert(3); s.insert(7); s.insert(1);\n\
        s.remove(3);\n\
        for x in s { println(x); }\n\
        if s.contains(1) { println(\"has1\"); } else { println(\"LOST1\"); }\n\
        if s.contains(7) { println(\"has7\"); } else { println(\"LOST7\"); }\n\
        if s.contains(3) { println(\"BAD3\"); } else { println(\"gone3\"); }\n\
    }");
    assert_eq!(sorted_prefix(&out, 3), "1\n7\n9\nhas1\nhas7\ngone3\n");
}

#[test]
fn map_entry_chain_still_reaches_the_live_slot() {
    // `entry` / `or_insert` resolve a slot POSITIONALLY (`slot_idx`) and then
    // write through it. Both halves moved onto the indexed storage, so this
    // pins that the write lands in the map rather than in a detached copy.
    let out = run("fn main() {\n\
        let mut m: Map[String, i64] = Map.new();\n\
        m.insert(\"a\", 1);\n\
        m.entry(\"a\").or_insert(0);\n\
        m.entry(\"b\").or_insert(7);\n\
        for (k, v) in m { println(f\"{k}={v}\"); }\n\
    }");
    // Sorted: the walk order is the per-process hash order (B-2026-08-21-6).
    // What is pinned is that `a` kept its ORIGINAL value (`or_insert` on a
    // present key must not overwrite) and that `b` was created with 7.
    assert_eq!(sorted_prefix(&out, 2), "a=1\nb=7\n");
}

#[test]
fn map_keeps_distinct_keys_that_share_a_hash_bucket() {
    // Tuple keys whose components are swapped are different keys. They are
    // built to be structurally similar so a hash that ignored component ORDER
    // would collide them — and the `==` recheck is what keeps them apart even
    // then. A conflation here is a lost entry, not a slow one.
    let out = run("fn main() {\n\
        let mut m: Map[(i64, i64), i64] = Map.new();\n\
        m.insert((1, 2), 12);\n\
        m.insert((2, 1), 21);\n\
        println(m.len());\n\
        match m.get((1, 2)) { Some(v) => { println(v); } None => { println(\"MISS\"); } }\n\
        match m.get((2, 1)) { Some(v) => { println(v); } None => { println(\"MISS\"); } }\n\
    }");
    assert_eq!(out, "2\n12\n21\n");
}

#[test]
fn map_with_many_keys_finds_every_one() {
    // Volume, so a bucket that grows past one entry is actually exercised —
    // the single-entry case can pass with an index that never collides. Also
    // the shape that was quadratic: 400 inserts followed by 400 lookups took
    // measurable seconds before this change.
    let out = run("fn main() {\n\
        let mut m: Map[i64, i64] = Map.new();\n\
        let mut i = 0i64;\n\
        while i < 400i64 { m.insert(i, i * 3i64); i = i + 1i64; }\n\
        let mut missing = 0i64;\n\
        let mut sum = 0i64;\n\
        let mut j = 0i64;\n\
        while j < 400i64 {\n\
            match m.get(j) { Some(v) => { sum = sum + v; } None => { missing = missing + 1i64; } }\n\
            j = j + 1i64;\n\
        }\n\
        println(f\"len={m.len()} missing={missing} sum={sum}\");\n\
    }");
    assert_eq!(out, "len=400 missing=0 sum=239400\n");
}

// ── B-2026-08-21-10: `is_sorted()` on the sequence receivers ─────
//
// The interpreter's element surface is wider than codegen's (a user struct
// without `#[derive(Ord)]` compares here and refuses to lower), so these pin
// the answers the tree-walk backend gives; the E2E sweep in tests/codegen.rs
// pins the compiled ones on the shapes both backends run.

#[test]
fn is_sorted_answers_the_boundary_shapes() {
    let out = run("fn main() {\n\
        let e: Vec[i64] = [];\n\
        let one: Vec[i64] = [7];\n\
        let eq: Vec[i64] = [4, 4, 4];\n\
        let asc: Vec[i64] = [1, 2, 3];\n\
        let desc: Vec[i64] = [3, 2, 1];\n\
        let dip: Vec[i64] = [1, 2, 3, 2, 4];\n\
        println(f\"{e.is_sorted()} {one.is_sorted()} {eq.is_sorted()} \
                   {asc.is_sorted()} {desc.is_sorted()} {dip.is_sorted()}\");\n\
    }");
    // Empty and single are vacuously sorted; equal neighbours are sorted
    // (non-strict); a dip anywhere, not just at the end, is not.
    assert_eq!(out, "true true true true false false\n");
}

#[test]
fn is_sorted_reaches_slice_and_array_receivers() {
    let out = run("fn main() {\n\
        let v: Vec[i64] = [1, 2, 3, 0];\n\
        let head: Slice[i64] = v[0..3];\n\
        let tail: Slice[i64] = v[1..4];\n\
        let a: Array[i64, 3] = [5, 6, 7];\n\
        let b: Array[i64, 3] = [7, 6, 5];\n\
        println(f\"{head.is_sorted()} {tail.is_sorted()} {a.is_sorted()} {b.is_sorted()}\");\n\
    }");
    assert_eq!(out, "true false true false\n");
}

#[test]
fn a_user_hashed_map_still_finds_and_removes_its_keys_in_the_interpreter() {
    assert_eq!(
        run_no_errors(&format!(
            "{USER_HASHERS}\
             fn main() {{\n\
                 let mut m: Map[String, i64, FnvBuild] = Map.new();\n\
                 m.insert(\"alpha\", 1);\n\
                 m.insert(\"bravo\", 2);\n\
                 m.insert(\"charlie\", 3);\n\
                 m.remove(\"bravo\");\n\
                 println(m.get(\"alpha\").unwrap());\n\
                 println(m.get(\"charlie\").unwrap());\n\
                 println(m.contains_key(\"bravo\"));\n\
                 println(m.len());\n\
             }}"
        )),
        "1\n3\nfalse\n2\n"
    );
}

#[test]
fn a_user_hasher_serves_a_set_and_a_struct_key_in_the_interpreter() {
    // A struct key is the encoding's sharpest case: `Value::Struct` holds its
    // fields in a `HashMap`, so `encode_value` sorts by field name to keep two
    // equal structs byte-identical. If it did not, a lookup would miss on most
    // runs rather than deterministically, which is exactly the shape of bug a
    // single green run hides.
    assert_eq!(
        run_no_errors(&format!(
            "{USER_HASHERS}\
             struct Point {{ x: i64, y: i64 }}\n\
             fn main() {{\n\
                 let mut s: Set[String, SumBuild] = Set.new();\n\
                 s.insert(\"alpha\");\n\
                 s.insert(\"bravo\");\n\
                 s.insert(\"alpha\");\n\
                 println(s.len());\n\
                 println(s.contains(\"bravo\"));\n\
                 let mut m: Map[Point, String, FnvBuild] = Map.new();\n\
                 m.insert(Point {{ x: 1, y: 2 }}, \"a\");\n\
                 m.insert(Point {{ x: 3, y: 4 }}, \"b\");\n\
                 println(m.get(Point {{ x: 3, y: 4 }}).unwrap());\n\
                 println(m.contains_key(Point {{ x: 9, y: 9 }}));\n\
             }}"
        )),
        "2\ntrue\nb\nfalse\n"
    );
}

/// The stable digest and the `Map` default are DIFFERENT FUNCTIONS, and must
/// stay that way. Routing `siphash24` through the seeded default would pass a
/// single-run equality check and then fail in the field, on the one axis this
/// function exists to guarantee — so assert the inequality directly rather
/// than trusting that nobody will "simplify" two SipHashes into one.
#[test]
fn stable_hash_siphash24_is_not_the_seeded_map_hasher() {
    assert_eq!(
        run("fn main() {\n\
                 let s: String = \"kara\";\n\
                 let mut m: Map[String, i64] = Map.new();\n\
                 m.insert(\"kara\", 1);\n\
                 println(StableHash.siphash24(s.bytes(), 0u64, 0u64) == 0);\n\
                 println(m.len());\n\
             }\n"),
        "false\n1\n"
    );
}

// ── B-2026-08-26-22: Map/Set reserve and Vec.from_iter ──────────

/// `Map.reserve` / `Set.reserve` are hints with no `capacity()` accessor — a
/// Map's bucket count is an implementation detail of the probing scheme (a
/// power of two sized off a 3/4 load factor, counting tombstones), and
/// publishing it would make that scheme observable. What both backends owe is
/// that CONTENTS and `len()` are unchanged.
#[test]
fn map_and_set_reserve_leave_contents_and_len_alone() {
    let out = run(r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.reserve(1000);
    let mut i = 0;
    while i < 200 { m.insert(i, i * 2); i = i + 1; }
    m.reserve(-5);
    m.reserve(0);
    println(f"{m.len()} {m.get(7).unwrap()} {m.get(199).unwrap()}");
    let mut s: Set[i64] = Set.new();
    s.reserve(500);
    s.insert(1); s.insert(2);
    s.reserve(-3);
    println(f"{s.len()} {s.contains(1)} {s.contains(9)}");
}
"#);
    assert_eq!(out, "200 14 398\n2 true false\n");
}

/// B-2026-09-06-60 — the interpreter was the correct column here (it printed
/// the right string while both compiled backends aborted), so this twin holds
/// the compiled string to it.
///
/// Twin of `tests/codegen.rs`'s `e2e_map_field_param_by_transfer`, pinned to the same string.
#[test]
fn test_map_field_param_by_transfer() {
    assert_eq!(
        run(r#"struct Q { id: i64, name: String, tbl: Map[i64, i64] }
impl Drop for Q { fn drop(mut ref self) { println(f"  dQ{self.id}") } }
struct QNoDrop { id: i64, name: String, tbl: Map[i64, i64] }
struct P { id: i64, name: String, xs: Vec[i64] }
impl Drop for P { fn drop(mut ref self) { println(f"  dP{self.id}") } }

fn mkq(i: i64) -> Q { let mut t = Map[i64, i64].new(); t.insert(i, i); return Q { id: i, name: f"h{i}", tbl: t }; }
fn mkqn(i: i64) -> QNoDrop { let mut t = Map[i64, i64].new(); t.insert(i, i); return QNoDrop { id: i, name: f"h{i}", tbl: t }; }
fn mkp(i: i64) -> P { return P { id: i, name: f"p{i}", xs: [i] }; }

fn norebind(q: Q) -> i64 { return q.id; }
fn readmap(q: Q) -> i64 { return q.tbl.len(); }
fn rebind(q: Q) -> i64 { let m = q; return m.id; }
fn nodrop(q: QNoDrop) -> i64 { return q.id; }
fn copyable(p: P) -> i64 { let m = p; return m.id; }

fn main() {
    println("temp_arg"); println(f"  v={norebind(mkq(1))}");
    println("temp_arg_reads_map"); println(f"  v={readmap(mkq(2))}");
    println("temp_arg_rebind"); println(f"  v={rebind(mkq(3))}");
    println("no_drop_struct"); println(f"  v={nodrop(mkqn(4))}");
    println("copy_supported"); println(f"  v={copyable(mkp(5))}");
    println("no_call"); let a = mkq(6); println(f"  v={a.id}");
    println("end");
}
"#),
        r#"temp_arg
  dQ1
  v=1
temp_arg_reads_map
  dQ2
  v=1
temp_arg_rebind
  dQ3
  v=3
no_drop_struct
  v=4
copy_supported
  dP5
  v=5
no_call
  v=6
  dQ6
end
"#
    );
}

// ── prefix collection literals: VecDeque / SortedMap ─────────────

/// B-2026-09-15-25 — the `SortedMap[k: v]` literal has to build an ORDERED
/// map, not a hashed one wearing the name. The assertion is the interesting
/// half: the source writes its keys out of order, so insertion order and key
/// order disagree and only a real `SortedMap` prints them sorted.
///
/// Asserting iteration order is legal HERE and nowhere near a plain `Map`:
/// `SortedMap` destroys and walks in KEY order, seed-independent and identical
/// on every backend (CLAUDE.md § `Map` / `Set` iteration order), which is
/// exactly what makes it the escape hatch from the per-process hash seed.
#[test]
fn sortedmap_prefix_literal_iterates_in_key_order() {
    let out = run("fn main() { \
                   let m = SortedMap[\"c\": 3, \"a\": 1, \"b\": 2]; \
                   for (k, v) in m { print(k); print(v); } }");
    assert_eq!(out, "a1b2c3");
    // Annotated spelling, same answer.
    let out = run("fn main() { \
                   let m: SortedMap[String, i64] = SortedMap[\"c\": 3, \"a\": 1]; \
                   for (k, v) in m { print(k); print(v); } }");
    assert_eq!(out, "a1c3");
    // `Display` names the type, which is the cheapest proof the literal did
    // not quietly produce a `Map` (a `Map` renders with no prefix).
    assert_eq!(
        run("fn main() { let m = SortedMap[\"b\": 2, \"a\": 1]; print(m); }"),
        "SortedMap{a: 1, b: 2}"
    );
    // Duplicate key keeps the last binding, as `Map`'s literal does.
    assert_eq!(
        run("fn main() { let m = SortedMap[\"a\": 1, \"a\": 9]; print(m); }"),
        "SortedMap{a: 9}"
    );
    // Empty annotated form.
    assert_eq!(
        run("fn main() { let m: SortedMap[String, i64] = SortedMap[]; print(m.len()); }"),
        "0"
    );
}

/// The bare `["k": v]` and `Map["k": v]` spellings must still build a plain
/// hashed `Map` — the prefix name now travels on the node, so the risk is
/// that the absent name gets read as something. Order is NOT asserted: a
/// `Map` walks under the per-process hash seed. B-2026-09-15-25.
#[test]
fn map_literal_spellings_still_build_a_plain_map() {
    for src in [
        "fn main() { let m = [\"a\": 1]; print(m); }",
        "fn main() { let m = Map[\"a\": 1]; print(m); }",
        "fn main() { let m: Map[String, i64] = Map[\"a\": 1]; print(m); }",
    ] {
        assert_eq!(run(src), "{a: 1}", "`{src}` did not build a plain Map");
    }
}
