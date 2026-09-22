//! String, f-strings, chars, formatting, regex, JSON, display -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter strings::
//!
//! New fixtures about String, f-strings, chars, formatting, regex, JSON, display belong in this file.

use super::*;

/// B-2026-09-07-45 (interpreter half) — the specs
/// `needs_runtime_formatter()` diverts, at 128 bits.
///
/// Center align, binary radix and a non-space fill take a different codegen
/// entrypoint than plain width/zero-pad/hex do, and that entrypoint was the
/// one still stuck at 64 bits. The interpreter reaches all of them through the
/// same `FormatSpec`, so this asserts the interpreter half of the agreement
/// the codegen E2E asserts on the compiled side — the expected bytes are the
/// compiled oracle, measured on all three compiled legs.
///
/// The last three holes are the width CONTROLS: `{-1i64:b}` is sixty-four ones
/// where `{-1i128:b}` is a hundred and twenty-eight, and a `u8` hole stays
/// eight bits. A non-decimal radix reinterprets at the hole's OWN width, so
/// widening every hole to 128 bits would break these three while fixing the
/// others.
#[test]
fn test_interp_spec_128_bit_runtime_formatter_holes() {
    let out = run(r#"
fn main() {
    let big: u128 = 170141183460469231731687303715884105727u128;
    let umax: u128 = 340282366920938463463374607431768211455u128;
    let ineg: i128 = -1i128;
    let ism: i128 = -7i128;
    println(f"[{big:^44}]");
    println(f"[{big:*>44}]");
    println(f"[{umax:b}]");
    println(f"[{ineg:b}]");
    println(f"[{ism:^12}]");
    println(f"[{ism:=^12}]");
    let n64: i64 = -1;
    let u8v: u8 = 255;
    println(f"[{n64:b}]");
    println(f"[{u8v:*>12b}]");
    println(f"[{n64:^8}]");
}
"#);
    assert_eq!(
        out,
        "[  170141183460469231731687303715884105727   ]\n\
         [*****170141183460469231731687303715884105727]\n\
         [11111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111]\n\
         [11111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111]\n\
         [     -7     ]\n\
         [=====-7=====]\n\
         [1111111111111111111111111111111111111111111111111111111111111111]\n\
         [****11111111]\n\
         [   -1   ]\n"
    );
}

#[test]
fn test_cstr_as_ptr_rejects_with_runtime_error_not_panic() {
    // B-2026-08-02-3: the interpreter's raw-pointer refusal must be a
    // structured RuntimeError (same delivery as the sibling `CStr.from_ptr`
    // / `volatile_read` rejections), not a Rust panic!. Reaching these
    // assertions at all proves the no-panic half — a panic would abort the
    // test before `errors` could be inspected.
    let errors = runtime_errors(
        "fn main() {\n\
             let p = c\"abc\".as_ptr();\n\
             println(\"unreached\");\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("not supported under `karac run`")
                && e.message.contains("karac build")),
        "as_ptr under the interpreter must produce the structured guidance error, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_struct_display_declaration_order() {
    // `#[derive(Display)]` structs render `Name { field: value, … }` in
    // DECLARATION order (the `Value::Struct` HashMap had lost source order and
    // rendered in random hash order before `display_render`). println,
    // .to_string(), and f-string interpolation all agree.
    let src = "#[derive(Display)]
        struct Wrap { p: Point, name: String, ok: bool }
        #[derive(Display)]
        struct Point { x: i64, y: i64 }
        fn main() {
            let w = Wrap { p: Point { x: 1, y: 2 }, name: \"hi\", ok: true };
            println(w);
            println(w.to_string());
            println(f\"w={w}\");
        }";
    let expected = "Wrap { p: Point { x: 1, y: 2 }, name: hi, ok: true }\n".repeat(2)
        + "w=Wrap { p: Point { x: 1, y: 2 }, name: hi, ok: true }\n";
    assert_eq!(run_no_errors(src), expected);
}

#[test]
fn test_enum_display_unit_variants() {
    // All-unit `#[derive(Display)]` enum renders the bare variant name across
    // println, .to_string(), and f-string — matching codegen.
    let src = "#[derive(Display)]
        enum Color { Red, Green, Blue }
        fn main() {
            let a = Color.Green;
            println(a.to_string());
            println(f\"c={a}\");
            println(a);
        }";
    assert_eq!(run_no_errors(src), "Green\nc=Green\nGreen\n");
}

#[test]
fn test_enum_display_payload_variants_to_string() {
    // Explicit `.to_string()` on a `#[derive(Display)]` enum with PAYLOAD
    // variants renders identically to `f"{e}"` / `println(e)`, including a
    // struct-field receiver. Guards the codegen sibling
    // (`test_e2e_payload_enum_to_string`) — the all-unit restriction on
    // `.to_string()` was stale, and this keeps interp == build.
    let src = "#[derive(Display)]
        enum IoErr { NotFound, Other(String) }
        struct Wrap { e: IoErr }
        fn main() {
            let a: IoErr = IoErr.NotFound;
            let b: IoErr = IoErr.Other(String.from(\"disk full\"));
            println(a.to_string());
            println(b.to_string());
            let w: Wrap = Wrap { e: IoErr.Other(String.from(\"boom\")) };
            println(w.e.to_string());
        }";
    assert_eq!(
        run_no_errors(src),
        "NotFound\nOther(disk full)\nOther(boom)\n"
    );
}

#[test]
fn test_enum_self_to_string_in_impl_method() {
    // `self.to_string()` inside an impl method (a `ref self` receiver) renders
    // a `#[derive(Display)]` enum — both all-unit and payload variants — the
    // `impl Error { fn message(ref self) -> String { self.to_string() } }`
    // pattern. Guards the codegen sibling (`test_e2e_enum_self_to_string`):
    // codegen recognizes the `SelfValue` receiver in the Display helpers so
    // build == run. (The struct receiver is a separate, still-open case —
    // B-2026-07-12-17 — so this covers enums only.)
    let src = "#[derive(Display)]
        enum IoErr { NotFound, Other(String) }
        trait Error { fn message(ref self) -> String; }
        impl Error for IoErr { fn message(ref self) -> String { self.to_string() } }
        fn report[E: Error](e: ref E) -> String { e.message() }
        fn main() {
            let a: IoErr = IoErr.NotFound;
            let b: IoErr = IoErr.Other(String.from(\"disk full\"));
            println(a.message());
            println(b.message());
            println(report(a));
        }";
    assert_eq!(run_no_errors(src), "NotFound\nOther(disk full)\nNotFound\n");
}

#[test]
fn a_match_bound_string_slice_keeps_its_methods() {
    // Oracle twin of `test_e2e_match_bound_string_slice_keeps_method_dispatch`
    // in `tests/codegen.rs` — the two assert the same bytes. The interpreter
    // always ran this; codegen refused every method but `to_string` on a
    // match-bound view, so the pair is what pins the two backends together.
    //
    // The `SplitIter` is the zero-copy tokenizing spelling the language can
    // express today, and the reason the codegen gap mattered: it is the
    // non-allocating alternative to `s.split(",")` (B-2026-08-26-13).
    let src = "fn head(s: ref String) -> Option[StringSlice] { return Some(s.slice(0, 3)); }
        struct SplitIter { rest: StringSlice, done: bool }
        impl SplitIter {
            fn next(mut ref self) -> Option[StringSlice] {
                if self.done { return None; }
                match self.rest.find(\",\") {
                    Some(k) => {
                        let head = self.rest.slice(0, k);
                        self.rest = self.rest.slice(k + 1, self.rest.len());
                        return Some(head);
                    }
                    None => { self.done = true; return Some(self.rest); }
                }
            }
        }
        fn main() {
            let s = \"hello\";
            match head(s) {
                Some(v) => println(f\"len={v.len()}\"),
                None => println(\"none\")
            }
            match head(s) {
                Some(v) => println(f\"sub={v.slice(0, 2).to_string()}\"),
                None => println(\"none\")
            }
            let text = \"aa,bb,cc\";
            let mut it = SplitIter { rest: text.slice(0, text.len()), done: false };
            let mut n = 0;
            let mut c = 0;
            while true {
                match it.next() { None => break, Some(f) => { n = n + f.len(); c = c + 1; } }
            }
            println(f\"{c} fields, {n} bytes\");
        }";
    assert_eq!(run_no_errors(src), "len=3\nsub=he\n3 fields, 6 bytes\n");
}

#[test]
fn user_impl_display_wins_at_every_depth_not_just_the_top_level() {
    // B-2026-08-26-29. A hand-written `impl Display` took effect at depth 0
    // (`f"{e}"` -> `aye 7`) and was IGNORED one level down, where the DERIVED
    // shape rendered instead (`f"{[e]}"` -> `[A { n: 7 }, B]`). That made the
    // one mechanism the language offers for overriding a rendering stop
    // applying inside a container, so a type whose `Display` exists to hide its
    // internals leaked them from inside any `Vec` / `Option` / struct field.
    //
    // Every container contributes only its punctuation; each element renders
    // through its own `Display` (design.md § derive(Display) on enums, "The
    // override applies at every depth").
    let src = "enum Ue { A { n: i64 }, B }
        impl Display for Ue {
            fn to_string(ref self) -> String {
                match self { A { n } => f\"aye {n}\", B => \"bee\" }
            }
        }
        struct Wrap { u: Ue }
        impl Display for Wrap {
            fn to_string(ref self) -> String { f\"<{self.u}>\" }
        }
        struct Holder { u: Ue }
        fn main() {
            let e = Ue.A { n: 7 };
            println(f\"top={e}\");
            let v = [Ue.A { n: 7 }, Ue.B];
            println(f\"vec={v}\");
            println(f\"nest={[[Ue.B]]}\");
            let o = Some(Ue.B);
            println(f\"opt={o}\");
            let r: Result[Ue, i64] = Ok(Ue.B);
            println(f\"res={r}\");
            let t = (Ue.B, 1);
            println(f\"tup={t}\");
            let h = Holder { u: Ue.B };
            println(f\"fld={h.u}\");
            let w = Wrap { u: Ue.A { n: 3 } };
            println(f\"wrap={w}\");
            println(f\"vw={[Wrap { u: Ue.B }]}\");
            let mut m: Map[String, Ue] = Map.new();
            m.insert(\"k\", Ue.B);
            println(f\"map={m}\");
            println(f\"str={v.to_string()}\");
        }";
    let out = run_no_errors(src);
    for want in [
        "top=aye 7\n",
        "vec=[aye 7, bee]\n",
        "nest=[[bee]]\n",
        "opt=Some(bee)\n",
        "res=Ok(bee)\n",
        "tup=(bee, 1)\n",
        "fld=bee\n",
        "wrap=<aye 3>\n",
        "vw=[<bee>]\n",
        "map={k: bee}\n",
        "str=[aye 7, bee]\n",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    // The derived shape must not leak from ANY position — that is the bug.
    assert!(!out.contains("A {"), "derived shape leaked: {out}");
    assert!(!out.contains("Some(B)"), "derived shape leaked: {out}");
}

#[test]
fn user_impl_display_reaches_set_and_sorted_collection_elements() {
    // B-2026-08-26-29, second half. `Set` / `SortedSet` / `SortedMap` were the
    // three shapes the typed walker never destructured — they fell to a
    // catch-all that formats through `Value`'s RUST `Display`, which cannot
    // reach a user impl. Codegen's Set renderer DOES recurse through the shared
    // dispatcher, so honoring the impl there and not here would have turned a
    // consistent-but-wrong rendering into a run-vs-build divergence.
    //
    // `SortedSet`/`SortedMap` with a user-enum key are refused by codegen today
    // for an unrelated reason, so only the `Set` line has a compiled twin; the
    // other two are asserted here because the rule is the interpreter's to keep
    // either way.
    let src = "#[derive(Hash, Eq, PartialEq, Ord)]
        enum Ue { A { n: i64 }, B }
        impl Display for Ue {
            fn to_string(ref self) -> String {
                match self { A { n } => f\"aye {n}\", B => \"bee\" }
            }
        }
        fn main() {
            let mut s: Set[Ue] = Set.new();
            s.insert(Ue.B);
            println(f\"set={s}\");
            let mut ss: SortedSet[Ue] = SortedSet.new();
            ss.insert(Ue.B);
            println(f\"sset={ss}\");
            let mut sm: SortedMap[Ue, i64] = SortedMap.new();
            sm.insert(Ue.B, 1);
            println(f\"smap={sm}\");
        }";
    assert_eq!(
        run_no_errors(src),
        "set=Set{bee}\nsset=SortedSet{bee}\nsmap=SortedMap{bee: 1}\n"
    );
}

#[test]
fn user_impl_display_on_a_shared_struct_is_honored_like_any_other() {
    // B-2026-08-26-29, third leg — and the only one that was ALREADY a
    // run-vs-build divergence at DEPTH 0 before this row touched anything.
    //
    // `user_display_impl_to_string_key` matched `Struct` and `EnumVariant` and
    // not `SharedStruct`, so the interpreter ignored a `shared struct`'s
    // `impl Display` everywhere — even for a bare `println(a)` and an explicit
    // `a.to_string()`. Codegen resolves a shared struct through
    // `expr_user_struct_name` (shared types are registered in
    // `struct_field_names`), so it honored the impl and printed `sh(1)` where
    // `--interp` printed `Sh { v: 1 }`.
    //
    // A shared ENUM was never affected: it rides as a `Value::EnumVariant`,
    // which the helper already matched. It is asserted here as the control.
    let src = "shared struct Sh { v: i64 }
        impl Display for Sh {
            fn to_string(ref self) -> String { f\"sh({self.v})\" }
        }
        shared enum Se { X { n: i64 }, Y }
        impl Display for Se {
            fn to_string(ref self) -> String { \"se\" }
        }
        fn main() {
            let a = Sh { v: 1 };
            println(f\"top={a}\");
            println(a.to_string());
            let v = [Sh { v: 2 }];
            println(f\"vec={v}\");
            let e = Se.Y;
            println(f\"enum={e}\");
        }";
    assert_eq!(
        run_no_errors(src),
        "top=sh(1)\nsh(1)\nvec=[sh(2)]\nenum=se\n"
    );
}

#[test]
fn debug_keeps_the_field_shape_when_display_is_overridden() {
    // The companion invariant to the test above: `Debug` is a DIFFERENT trait
    // and a user `impl Display` must not reach it. `dbg()` reports the `{:?}`
    // form, which design.md pins as the field-name shape — so the same vector
    // that prints `[aye 7, bee]` through `Display` must still report
    // `[A { n: 7 }, B]` through `Debug`.
    let src = "enum Ue { A { n: i64 }, B }
        impl Display for Ue {
            fn to_string(ref self) -> String {
                match self { A { n } => f\"aye {n}\", B => \"bee\" }
            }
        }
        fn main() {
            let v = [Ue.A { n: 7 }, Ue.B];
            println(f\"disp={v}\");
            dbg(v);
        }";
    let (out, dbg) = run_program_with_dbg(src, DbgOutputMode::Terminal);
    let out = out.join("");
    assert!(out.contains("disp=[aye 7, bee]\n"), "Display: {out}");
    assert_eq!(dbg.len(), 1, "expected one dbg line, got {dbg:?}");
    assert!(
        dbg[0].contains("[A { n: 7 }, B]"),
        "Debug must keep the field shape: {:?}",
        dbg[0]
    );
    assert!(
        !dbg[0].contains("aye 7"),
        "the user Display must not reach Debug: {:?}",
        dbg[0]
    );
}

#[test]
fn a_compound_option_payload_renders_through_its_own_display() {
    // design.md § Strings names this exact example: `Some(p)` for a `p: Point`
    // with an `impl Display` prints `Some((3, 4))`, NOT the field-name form.
    // Both backends rendered `Some(Point { x: 3, y: 4 })` before B-2026-08-26-29
    // — the `Debug`-in-`Display` bug that paragraph was written against.
    let src = "struct Point { x: i64, y: i64 }
        impl Display for Point {
            fn to_string(ref self) -> String { f\"({self.x}, {self.y})\" }
        }
        fn main() {
            let p = Point { x: 3, y: 4 };
            let o = Some(p);
            println(f\"{o}\");
        }";
    assert_eq!(run_no_errors(src), "Some((3, 4))\n");
}

#[test]
fn test_struct_display_nested_in_container() {
    // A struct nested in a Vec still renders in declaration order (the
    // renderer recurses through containers).
    let src = "#[derive(Display)]
        struct Point { x: i64, y: i64 }
        fn main() {
            let v: Vec[Point] = [Point { x: 9, y: 8 }, Point { x: 7, y: 6 }];
            println(f\"list={v}\");
        }";
    assert_eq!(
        run_no_errors(src),
        "list=[Point { x: 9, y: 8 }, Point { x: 7, y: 6 }]\n"
    );
}

#[test]
fn test_char_to_digit_some_none_and_radix() {
    // `c.to_digit(radix) -> Option[u32]`: digit value in the given radix, None
    // when not a digit. Radix is u32 (suffix-free literal promotes); 'a'/'z'
    // count in hex/base-36.
    let out = run("fn main() {\n\
             match '7'.to_digit(10) { Some(d) => println(d), None => println(99u32) }\n\
             match 'a'.to_digit(16) { Some(d) => println(d), None => println(99u32) }\n\
             match 'z'.to_digit(36) { Some(d) => println(d), None => println(99u32) }\n\
             match 'x'.to_digit(10) { Some(d) => println(d), None => println(99u32) }\n\
             let r: u32 = 2;\n\
             match '1'.to_digit(r) { Some(d) => println(d), None => println(99u32) }\n\
         }");
    assert_eq!(out, "7\n10\n35\n99\n1\n");
}

#[test]
fn test_char_to_digit_radix_out_of_range_traps() {
    // An out-of-range radix (> 36) traps, matching Rust's `char::to_digit` panic.
    // The trap fires inside the `match` scrutinee — exercises the scrutinee-fault
    // short-circuit (otherwise the poison value matches no Some/None arm and the
    // tree-walker would hit a non-exhaustive `unreachable!`).
    let errors = runtime_errors(
        "fn main() { match '5'.to_digit(40) { Some(d) => println(d), None => println(0u32) } }",
    );
    assert!(
        errors.iter().any(|e| e.message.contains("radix")),
        "expected a to_digit radix-range trap, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_to_string_on_primitives() {
    assert_eq!(run("fn main() { println((-42i64).to_string()); }"), "-42\n");
    assert_eq!(run("fn main() { println((3.5f64).to_string()); }"), "3.5\n");
    assert_eq!(run("fn main() { println(true.to_string()); }"), "true\n");
    assert_eq!(run("fn main() { println('Z'.to_string()); }"), "Z\n");
}

#[test]
fn test_vec_pop_string_elements() {
    // Non-Copy element type flows through the same dispatch — pins the
    // generic shape against future regressions where the arm might
    // accidentally specialize to numerics.
    let out = run(r#"
        fn main() {
            let mut v: Vec[String] = Vec.new();
            v.push("a");
            v.push("b");
            println(v.pop());
            println(v.pop());
            println(v.pop());
        }
    "#);
    assert_eq!(out, "Some(b)\nSome(a)\nNone\n");
}

#[test]
fn test_vec_deque_string_elements() {
    // Non-Copy element type flows through unchanged.
    let out = run(r#"
        fn main() {
            let mut q: VecDeque[String] = VecDeque.new();
            q.push_back("first");
            q.push_back("second");
            q.push_front("zero");
            let f = q.pop_front();
            let b = q.pop_back();
            println(f);
            println(b);
        }
    "#);
    assert_eq!(out, "Some(zero)\nSome(second)\n");
}

#[test]
fn test_vec_filled_string() {
    // Non-`Copy` element type — the per-slot clone in the dispatch
    // arm satisfies the spec's `T: Clone` requirement.
    let out = run(r#"
        fn main() {
            let v: Vec[String] = Vec.filled(2, "hi");
            println(v.len());
            println(v[0]);
            println(v[1]);
        }
    "#);
    assert_eq!(out, "2\nhi\nhi\n");
}

#[test]
fn test_string_literal() {
    assert_eq!(
        run(r#"fn main() { println("hello world"); }"#),
        "hello world\n"
    );
}

#[test]
fn test_compound_assignment_string_concat() {
    // `s += other` desugars to `s = s + other`. Confirms the desugar still
    // works for non-integer Add impls — guard for when Step 6 lowering lands.
    assert_eq!(
        run("fn main() { let mut s = \"hello \"; s += \"world\"; println(s); }"),
        "hello world\n"
    );
}

// ── String Interpolation ───────────────────────────────────────

#[test]
fn test_string_interpolation_basic() {
    assert_eq!(
        run("fn main() {\n\
                 let x = 42;\n\
                 println(f\"the answer is {x}\");\n\
             }"),
        "the answer is 42\n"
    );
}

#[test]
fn test_fstring_format_specifiers() {
    // Phase 8 format specifiers — same output codegen asserts in
    // tests/codegen.rs::test_e2e_fstring_format_specifiers (build==run parity).
    assert_eq!(
        run("fn main() {\n\
                 let n = 7;\n\
                 let big = 255;\n\
                 let neg = 0 - 42;\n\
                 let pi = 1.23456;\n\
                 let s = \"hi\";\n\
                 println(f\"{n:04}|{n:x}|{big:08X}|{big:o}\");\n\
                 println(f\"{neg:6}|{neg:06}|{neg:<6}\");\n\
                 println(f\"{pi:.2}|{pi:8.2}|{pi:08.2}|{pi:<8.2}\");\n\
                 println(f\"{s:6}|{s:<6}|{s:>6}|{s}\");\n\
             }"),
        "0007|7|000000FF|377\n\
         \x20  -42|-00042|-42   \n\
         1.23|    1.23|00001.23|1.23    \n\
         \x20   hi|hi    |    hi|hi\n"
    );
}

#[test]
fn test_fstring_binary_center_fill_specifiers() {
    // Binary radix, center align, and custom fill — the runtime-formatter
    // specs. Same output codegen asserts in
    // tests/codegen.rs::e2e_fstring_binary_center_and_fill_specs (run==build).
    assert_eq!(
        run("fn main() {\n\
                 let n = 5;\n\
                 let big = 255;\n\
                 let neg = 42;\n\
                 let pi = 3.14159;\n\
                 let s = \"kara\";\n\
                 println(f\"{n:b}|{big:b}|{n:08b}\");\n\
                 println(f\"{neg:^6}|{s:^10}|{pi:^10.2}\");\n\
                 println(f\"{s:*^10}|{s:*<10}|{neg:*>8}|{pi:*^10.2}\");\n\
                 println(f\"{big:^8x}|{s:.^12}|{s:^2}\");\n\
             }"),
        "101|11111111|00000101\n\
         \x20 42  |   kara   |   3.14   \n\
         ***kara***|kara******|******42|***3.14***\n\
         \x20  ff   |....kara....|kara\n"
    );
}

#[test]
fn test_fstring_format_specifier_errors() {
    // Malformed / type-incompatible specifiers are COMPILE errors (parse or
    // typecheck) — never silently dropped, which is the whole point.
    fn compile_errors(src: &str) -> Vec<String> {
        let parsed = karac::parse(src);
        if !parsed.errors.is_empty() {
            return parsed.errors.iter().map(|e| e.message.clone()).collect();
        }
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        typed.errors.iter().map(|e| e.message.clone()).collect()
    }
    for (src, needle) in [
        ("fn main() { let f = 1.0; println(f\"{f:x}\"); }", "radix"),
        (
            "fn main() { let b = true; println(f\"{b:04}\"); }",
            "int, float, and string",
        ),
        (
            "fn main() { let n = 1; println(f\"{n:0q}\"); }",
            "unsupported type",
        ),
        (
            "fn main() { let f = 1.0; println(f\"{f:8}\"); }",
            "needs a precision",
        ),
        (
            // Binary radix on a float is still rejected (radix is int-only).
            "fn main() { let f = 1.0; println(f\"{f:b}\"); }",
            "radix",
        ),
    ] {
        let errs = compile_errors(src);
        assert!(
            errs.iter().any(|e| e.contains(needle)),
            "expected an error containing {needle:?} for {src:?}, got: {errs:?}"
        );
    }
}

#[test]
fn test_println_multiple() {
    assert_eq!(
        run("fn main() {\n\
                 println(1);\n\
                 println(2);\n\
                 println(3);\n\
             }"),
        "1\n2\n3\n"
    );
}

#[test]
fn test_var_error_not_unicode_maps_to_io_error_invalid_utf8() {
    // VarError.NotUnicode → IoError.InvalidUtf8 via the baked stdlib impl.
    let output = run("fn main() {
         let io: IoError = IoError.from(VarError.NotUnicode);
         match io {
             IoError.NotFound => println(\"not_found\"),
             IoError.PermissionDenied => println(\"perm_denied\"),
             IoError.AlreadyExists => println(\"already_exists\"),
             IoError.UnexpectedEof => println(\"eof\"),
             IoError.InvalidUtf8 => println(\"invalid_utf8\"),
             IoError.Interrupted => println(\"interrupted\"),
             IoError.Other(_) => println(\"other\"),
         }
     }");
    assert_eq!(output, "invalid_utf8\n");
}

#[test]
fn test_ambient_stdout_println_resource_method_writes() {
    // Direct `Stdout.println(s)` dispatches to the BuiltinDefault arm,
    // which routes through `write_stdout` and lands in the harness's
    // `captured_output` buffer. Same path the routed free `println`
    // takes — this just exercises the user-visible Stdout surface.
    let output = run("fn main() {\n\
                          Stdout.println(\"hello\");\n\
                          Stdout.println(\"world\");\n\
                      }");
    assert_eq!(output, "hello\nworld\n");
}

#[test]
fn test_free_println_routes_through_stdout_provider() {
    // The free `println(x)` is routed through the `Stdout` provider
    // stack — installing a `with_provider[Stdout]` fake intercepts the
    // call. The Mute fake swallows everything; the inner `println` has
    // no observable effect, while the outer `println` (after the scope
    // pops back to the BuiltinDefault) still writes normally.
    let output = run("struct Mute {}\n\
                      impl Mute {\n\
                          fn println(self, s: String) { }\n\
                          fn print(self, s: String) { }\n\
                      }\n\
                      fn main() {\n\
                          with_provider[Stdout](Mute {}, || {\n\
                              println(\"hidden\");\n\
                              println(\"also hidden\");\n\
                          });\n\
                          println(\"visible\");\n\
                      }");
    assert_eq!(output, "visible\n");
}

#[test]
fn test_eprintln_routes_through_stderr_provider() {
    // `eprintln(x)` previously panicked with "variable 'eprintln' not
    // found" — it was in PRELUDE_FUNCTIONS but had no interpreter arm.
    // Now it routes through `Stderr.println` like `println` routes
    // through `Stdout.println`. We can't assert the stderr contents
    // (the test harness only captures stdout) but we can prove the
    // call succeeds and a subsequent `println` still writes stdout.
    let output = run("fn main() {\n\
                          eprintln(\"to stderr\");\n\
                          println(\"to stdout\");\n\
                      }");
    assert_eq!(output, "to stdout\n");
}

#[test]
fn test_bufreader_read_to_string_slurps_whole_file() {
    let tmp = std::env::temp_dir().join("karac_test_bufreader_slurp.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"alpha\nbeta\n").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.new(f);
                     let mut all = String.new();
                     match br.read_to_string(all) {{
                         Ok(n) => println(\"n=\" + n.to_string() + \" [\" + all + \"]\"),
                         Err(_) => println(\"read err\"),
                     }}
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "n=11 [alpha\nbeta\n]\n");
    let _ = std::fs::remove_file(&tmp);
}

// ── Debug trait format (item 161) ─────────────────────────────────────────────

#[test]
fn test_assert_eq_failure_shows_debug_format_for_strings() {
    // assert_eq failure left/right fields show strings with quotes.
    let errors = runtime_errors("fn main() { assert_eq(\"hello\", \"world\"); }");
    assert!(!errors.is_empty(), "expected a runtime error");
    let e = &errors[0];
    assert_eq!(e.left.as_deref(), Some("\"hello\""));
    assert_eq!(e.right.as_deref(), Some("\"world\""));
}

#[test]
fn test_assert_ne_failure_shows_debug_format() {
    // assert_ne failure for equal strings shows debug format.
    let errors = runtime_errors("fn main() { assert_ne(\"same\", \"same\"); }");
    assert!(!errors.is_empty(), "expected a runtime error");
    let e = &errors[0];
    assert_eq!(e.left.as_deref(), Some("\"same\""));
    assert_eq!(e.right.as_deref(), Some("\"same\""));
}

#[test]
fn test_assert_two_arg_uses_string_literal_message() {
    // B-2026-07-18-26: `assert(cond, "msg")` — the optional 2-arg form the
    // typechecker accepts and the compiler emits for tensor shape-checks — now
    // reports the string-literal message on failure (was: silently ignored,
    // always "assertion failed"). Kept symmetric with codegen's compile_assert.
    let errors = runtime_errors("fn main() { assert(1 == 2, \"shape mismatch\"); }");
    assert!(!errors.is_empty(), "expected a runtime error");
    assert_eq!(errors[0].message, "shape mismatch");
}

// ── #[derive(Display)] on unit enums ────────────────────────────

#[test]
fn test_derive_display_to_string_returns_variant_name() {
    // `#[derive(Display)]` — `.to_string()` returns the PascalCase variant name.
    let output = run("#[derive(Display)]\n\
         enum Direction { Up, Down, Left, Right }\n\
         fn main() {\n\
             let d = Direction.Up;\n\
             println(d.to_string());\n\
         }");
    assert_eq!(output, "Up\n");
}

#[test]
fn test_derive_display_snake_case_lowercases_variant() {
    // `#[derive(Display(snake_case))]` — `.to_string()` returns the lower_snake_case name.
    let output = run("#[derive(Display(snake_case))]\n\
         enum Status { Active, InProgress, Done }\n\
         fn main() {\n\
             let s = Status.InProgress;\n\
             println(s.to_string());\n\
         }");
    assert_eq!(output, "in_progress\n");
}

#[test]
fn test_derive_display_fstring_interpolation() {
    // Enum with derived Display works inside an f-string.
    let output = run("#[derive(Display)]\n\
         enum Color { Red, Green, Blue }\n\
         fn main() {\n\
             let c = Color.Green;\n\
             println(f\"color={c.to_string()}\");\n\
         }");
    assert_eq!(output, "color=Green\n");
}

#[test]
fn test_sorted_set_string_elements() {
    let output = run("fn main() {\n\
             let s: SortedSet[String] = SortedSet.new();\n\
             s.insert(\"banana\");\n\
             s.insert(\"apple\");\n\
             s.insert(\"cherry\");\n\
             for x in s {\n\
                 println(x);\n\
             }\n\
         }");
    assert_eq!(output, "apple\nbanana\ncherry\n");
}

#[test]
fn test_sorted_map_string_keys_sorted() {
    let output = run("fn main() {\n\
             let m: SortedMap[String, i64] = SortedMap.new();\n\
             m.insert(\"banana\", 2_i64);\n\
             m.insert(\"apple\", 1_i64);\n\
             m.insert(\"cherry\", 3_i64);\n\
             for k in m.keys() { println(k); }\n\
         }");
    assert_eq!(output, "apple\nbanana\ncherry\n");
}

#[test]
fn test_map_prefix_literal_string_keys() {
    // Map["a": 1, "b": 2] prefix-literal form — parses + type-checks +
    // produces a Map with the entries.
    let output = run("fn main() {\n\
             let m = Map[\"a\": 1_i64, \"b\": 2_i64, \"c\": 3_i64];\n\
             println(m.len());\n\
             match m.get(\"b\") {\n\
                 Some(v) => println(v),\n\
                 None => println(0_i64),\n\
             }\n\
         }");
    assert_eq!(output, "3\n2\n");
}

#[test]
fn test_string_clone_preserves_value() {
    let output = run("fn main() {\n\
             let s = \"hello\";\n\
             let t = s.clone();\n\
             println(t);\n\
         }");
    assert_eq!(output, "hello\n");
}

#[test]
fn test_string_push_char_ascii() {
    // Interpreter mirror of the codegen String.push(char) arm.
    // `karac run` was the panic surface that surfaced the
    // method_call_seq.rs dispatch gap; this regression test makes the
    // arm load-bearing for the kata 71 follow-up.
    let output = run("fn main() {\n\
             let mut s: String = \"\";\n\
             s.push('h');\n\
             s.push('i');\n\
             println(s);\n\
             println(s.len());\n\
         }");
    assert_eq!(output, "hi\n2\n");
}

#[test]
fn test_try_push_str_and_try_push_char_string() {
    let output = run("fn main() {\n\
             let mut s: String = \"\";\n\
             s.try_push('a');\n\
             match s.try_push_str(\"bc\") {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
             println(s);\n\
         }");
    assert_eq!(output, "ok\nabc\n");
}

#[test]
fn test_try_clone_string_wraps_ok() {
    let output = run("fn main() {\n\
             let s = \"hello\";\n\
             match s.try_clone() {\n\
                 Ok(t) => println(t),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "hello\n");
}

// ── Regex ─────────────────────────────────────────────────────────

#[test]
fn test_regex_compile_ok_result() {
    let output = run(r#"fn main() {
         match Regex.compile("[0-9]+") {
             Ok(r) => println("ok"),
             Err(e) => println("err"),
         }
     }"#);
    assert_eq!(output, "ok\n");
}

#[test]
fn test_regex_compile_invalid_err() {
    let output = run(r#"fn main() {
         match Regex.compile("[invalid") {
             Ok(r) => println("ok"),
             Err(e) => println("err"),
         }
     }"#);
    assert_eq!(output, "err\n");
}

#[test]
fn test_regex_is_match_true() {
    let output = run(r#"fn main() {
         let r = Regex.compile("[0-9]+").unwrap();
         println(r.is_match("abc123"));
     }"#);
    assert_eq!(output, "true\n");
}

#[test]
fn test_regex_is_match_false() {
    let output = run(r#"fn main() {
         let r = Regex.compile("[0-9]+").unwrap();
         println(r.is_match("abc"));
     }"#);
    assert_eq!(output, "false\n");
}

#[test]
fn test_regex_find_some() {
    let output = run(r#"fn main() {
         let r = Regex.compile("[0-9]+").unwrap();
         match r.find("abc123def") {
             Some(m) => println(m.text),
             None => println("none"),
         }
     }"#);
    assert_eq!(output, "123\n");
}

#[test]
fn test_regex_find_none() {
    let output = run(r#"fn main() {
         let r = Regex.compile("[0-9]+").unwrap();
         match r.find("abcdef") {
             Some(m) => println(m.text),
             None => println("none"),
         }
     }"#);
    assert_eq!(output, "none\n");
}

#[test]
fn test_regex_find_all() {
    let output = run(r#"fn main() {
         let r = Regex.compile("[0-9]+").unwrap();
         let ms = r.find_all("abc 123 def 456");
         println(ms.len());
     }"#);
    assert_eq!(output, "2\n");
}

#[test]
fn test_regex_replace_all() {
    let output = run(r#"fn main() {
         let r = Regex.compile("[0-9]+").unwrap();
         println(r.replace_all("abc 123 def 456", "NUM"));
     }"#);
    assert_eq!(output, "abc NUM def NUM\n");
}

#[test]
fn test_arena_string_elements() {
    // Heap-payload element type: String values round-trip through the
    // arena (the backing slot is just a `Value`, so T erases cleanly).
    let output = run(r#"fn main() {
             let a: Arena[String] = Arena.new();
             let r0 = a.push("hello");
             let r1 = a.push("world");
             println(a.get(r0));
             println(a.get(r1));
         }"#);
    assert_eq!(output, "hello\nworld\n");
}

#[test]
fn test_interner_dedups_equal_strings() {
    // Interning equal strings returns the SAME handle (the whole point);
    // `Symbol` equality is integer comparison.
    let output = run(r#"fn main() {
             let mut tab: Interner = Interner.new();
             let a = tab.intern("hello");
             let b = tab.intern("hello");
             println(a == b);
             println(tab.len());
         }"#);
    assert_eq!(output, "true\n1\n");
}

#[test]
fn test_encoding_on_slice() {
    // B-2026-07-18-20: `Base64.encode`/`Hex.encode` on a `Slice[u8]` value
    // (`v.as_slice()`, the declared `Slice[u8]` param's canonical form) read
    // ZERO bytes in the interpreter — the byte extraction matched only
    // `Value::Array`, so a `Value::Slice` produced "" while a Vec arg encoded
    // the real bytes. Now the interpreter views `storage[start..start+len]` and
    // a slice encodes identically to the underlying Vec.
    let output = run("fn main() {\n\
             let v: Vec[u8] = vec![72u8, 105u8];\n\
             let s: Slice[u8] = v.as_slice();\n\
             println(Base64.encode(v));\n\
             println(Base64.encode(s));\n\
             println(Hex.encode(s));\n\
         }");
    assert_eq!(output, "SGk=\nSGk=\n4869\n");
}

// ── Encoding namespace (Base64 / Hex / Url) ───────────────────────

#[test]
fn test_base64_encode_basic() {
    // RFC 4648 vector: "foobar" → "Zm9vYmFy"
    let output = run("fn main() {\n\
             let bs = [102u8, 111u8, 111u8, 98u8, 97u8, 114u8];\n\
             println(Base64.encode(bs));\n\
         }");
    assert_eq!(output, "Zm9vYmFy\n");
}

#[test]
fn test_base64_encode_padding_one_byte() {
    // RFC 4648 vector: "f" → "Zg=="
    let output = run("fn main() {\n\
             let bs = [102u8];\n\
             println(Base64.encode(bs));\n\
         }");
    assert_eq!(output, "Zg==\n");
}

#[test]
fn test_base64_encode_padding_two_bytes() {
    // RFC 4648 vector: "fo" → "Zm8="
    let output = run("fn main() {\n\
             let bs = [102u8, 111u8];\n\
             println(Base64.encode(bs));\n\
         }");
    assert_eq!(output, "Zm8=\n");
}

#[test]
fn test_base64_encode_url_safe_no_padding() {
    // Bytes 0xfb 0xff 0xbf encode to "+/+/" in standard alphabet,
    // "-_-_" in URL-safe alphabet. URL-safe omits padding.
    let output = run("fn main() {\n\
             let bs = [251u8, 255u8, 191u8];\n\
             println(Base64.encode_url_safe(bs));\n\
         }");
    assert_eq!(output, "-_-_\n");
}

#[test]
fn test_base64_decode_invalid_char() {
    let output = run("fn main() {\n\
             match Base64.decode(\"!!!!\") {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(_) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "err\n");
}

#[test]
fn test_string_from_utf8_valid_returns_ok() {
    let output = run("fn main() {\n\
             let mut bs: Vec[u8] = Vec.new();\n\
             bs.push(72u8);\n\
             bs.push(101u8);\n\
             bs.push(108u8);\n\
             bs.push(108u8);\n\
             bs.push(111u8);\n\
             match String.from_utf8(bs) {\n\
                 Ok(s) => println(s),\n\
                 Err(_) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "Hello\n");
}

#[test]
fn test_string_from_utf8_invalid_byte_returns_err_invalid_byte() {
    // 0xff is never a valid UTF-8 lead byte; Rust's `Utf8Error::error_len`
    // returns `Some(1)` here, so the variant must be `InvalidByte`.
    let output = run("fn main() {\n\
             let mut bs: Vec[u8] = Vec.new();\n\
             bs.push(255u8);\n\
             match String.from_utf8(bs) {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(Utf8Error.InvalidByte) => println(\"invalid_byte\"),\n\
                 Err(Utf8Error.IncompleteSequence) => println(\"incomplete\"),\n\
                 Err(Utf8Error.Other(_)) => println(\"other\"),\n\
             }\n\
         }");
    assert_eq!(output, "invalid_byte\n");
}

#[test]
fn test_string_from_utf8_incomplete_returns_err_incomplete_sequence() {
    // 0xe2 starts a 3-byte sequence; on its own the stream is truncated.
    // Rust's `Utf8Error::error_len` returns `None`, so the variant must
    // be `IncompleteSequence`.
    let output = run("fn main() {\n\
             let mut bs: Vec[u8] = Vec.new();\n\
             bs.push(226u8);\n\
             match String.from_utf8(bs) {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(Utf8Error.InvalidByte) => println(\"invalid_byte\"),\n\
                 Err(Utf8Error.IncompleteSequence) => println(\"incomplete\"),\n\
                 Err(Utf8Error.Other(_)) => println(\"other\"),\n\
             }\n\
         }");
    assert_eq!(output, "incomplete\n");
}

#[test]
fn test_string_from_utf8_empty_returns_ok_empty() {
    let output = run("fn main() {\n\
             let bs: Vec[u8] = Vec.new();\n\
             match String.from_utf8(bs) {\n\
                 Ok(s) => println(s.len()),\n\
                 Err(_) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "0\n");
}

// ── String slicing — `s[a..b]` (phase-8 line 737) ─────────────────
//
// `s[a..b]` returns a fresh substring `String` (not a Slice), with all
// range forms (`a..b` / `a..` / `..b` / `..` / `a..=b`). Byte offsets
// with UTF-8 char-boundary validation: a non-boundary index is a runtime
// panic carrying `E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY`.

#[test]
fn test_string_slice_basic_half_open() {
    let output = run_no_errors(
        "fn main() {
             let s = \"hello world\";
             println(s[0..5]);
             println(s[6..11]);
         }",
    );
    assert_eq!(output, "hello\nworld\n");
}

#[test]
fn test_string_slice_open_ended_forms() {
    // `a..` (to end), `..b` (from start), `..` (full), all fresh Strings.
    let output = run_no_errors(
        "fn main() {
             let s = \"hello world\";
             println(s[6..]);
             println(s[..5]);
             println(s[..]);
         }",
    );
    assert_eq!(output, "world\nhello\nhello world\n");
}

#[test]
fn test_string_slice_inclusive_and_empty() {
    // `a..=b` includes byte b; `a..a` is the empty string.
    let output = run_no_errors(
        "fn main() {
             let s = \"hello\";
             println(s[0..=4]);
             println(\"[\" + s[2..2] + \"]\");
         }",
    );
    assert_eq!(output, "hello\n[]\n");
}

#[test]
fn test_string_slice_result_is_string_and_concatenates() {
    // The slice result is a real String — it concatenates with `+` and
    // exposes String methods (`.len()`).
    let output = run_no_errors(
        "fn main() {
             let s = \"hello world\";
             let mid = s[6..11];
             println(mid + \"!\");
             println(mid.len());
         }",
    );
    assert_eq!(output, "world!\n5\n");
}

#[test]
fn test_string_slice_multibyte_on_boundary_ok() {
    // `é` is two bytes at offsets 1..3, so 0..1 ('h') and 1..3 ('é') both
    // land on char boundaries and slice cleanly.
    let output = run_no_errors(
        "fn main() {
             let s = \"héllo\";
             println(s[0..1]);
             println(s[1..3]);
         }",
    );
    assert_eq!(output, "h\né\n");
}

#[test]
fn test_string_slice_non_char_boundary_panics() {
    // Byte 2 falls in the middle of the 2-byte `é`, so `s[0..2]` panics
    // with E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY (Rust's slicing contract).
    let errs = runtime_errors(
        "fn main() {
             let s = \"héllo\";
             let bad = s[0..2];
             println(bad);
         }",
    );
    assert!(
        errs.iter()
            .any(|e| e.message.contains("E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY")),
        "expected E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY panic, got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_string_slice_out_of_range_is_runtime_error() {
    let errs = runtime_errors(
        "fn main() {
             let s = \"hi\";
             let bad = s[0..9];
             println(bad);
         }",
    );
    assert!(
        errs.iter()
            .any(|e| e.message.contains("string slice bounds 0..9 out of range")),
        "expected out-of-range slice error, got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_to_string_bool() {
    let output = run("fn main() { println(true.to_string()); }");
    assert_eq!(output, "true\n");
}

#[test]
fn test_to_string_str() {
    let output = run(r#"fn main() { let s = "hello"; println(s.to_string()); }"#);
    assert_eq!(output, "hello\n");
}

#[test]
fn test_to_string_vec() {
    let output = run("fn main() {\n\
         let v = Vec[1_i64, 2_i64];\n\
         println(v.to_string());\n\
     }");
    assert_eq!(output, "[1, 2]\n");
}

#[test]
fn test_to_string_option_some() {
    let output = run("fn main() {\n\
         let x: Option[i64] = Some(99_i64);\n\
         println(x.to_string());\n\
     }");
    assert_eq!(output, "Some(99)\n");
}

#[test]
fn test_to_string_option_none() {
    let output = run("fn main() {\n\
         let x: Option[i64] = None;\n\
         println(x.to_string());\n\
     }");
    assert_eq!(output, "None\n");
}

#[test]
fn test_fstring_interpolates_vec() {
    let output = run("fn main() {\n\
         let v = Vec[10_i64];\n\
         println(f\"v={v}\");\n\
     }");
    assert_eq!(output, "v=[10]\n");
}

#[test]
fn test_string_sorted_basic() {
    let output = run(r#"fn main() { let s = "cba"; println(s.sorted()); }"#);
    assert_eq!(output, "abc\n");
}

#[test]
fn test_string_sorted_already_sorted() {
    let output = run(r#"fn main() { let s = "abc"; println(s.sorted()); }"#);
    assert_eq!(output, "abc\n");
}

#[test]
fn test_string_sorted_empty() {
    let output = run(r#"fn main() { let s = ""; println(s.sorted()); }"#);
    assert_eq!(output, "\n");
}

#[test]
fn test_string_chars_for_loop_prints_each() {
    // The canonical `for c in s.chars()` shape; verifies the explicit
    // chars() iterator yields one Value::Char per Unicode scalar.
    let output = run(r#"fn main() { for c in "abc".chars() { println(c); } }"#);
    assert_eq!(output, "a\nb\nc\n");
}

#[test]
fn test_string_for_loop_iterates_chars() {
    // design.md § Character type (line 2299) pins `for c in s` and
    // `s.chars()` as semantic peers. Same output as the chars() variant.
    let output = run(r#"fn main() { for c in "abc" { println(c); } }"#);
    assert_eq!(output, "a\nb\nc\n");
}

#[test]
fn test_string_chars_empty_iterates_zero_times() {
    let output = run(r#"fn main() {
            let mut n = 0i64;
            for _ in "".chars() { n = n + 1; }
            println(n);
        }"#);
    assert_eq!(output, "0\n");
}

#[test]
fn test_string_starts_with_interpreter() {
    // Mirrors the four-case probe at
    // `/tmp/kara-probes/starts_with_probe.kara`: match / mismatch /
    // prefix-longer-than-receiver / empty-prefix.
    let output = run(r#"fn main() {
            let s = "/todos/42";
            if s.starts_with("/todos/") { println("yes"); } else { println("no"); }
            if s.starts_with("/foo") { println("yes2"); } else { println("no2"); }
            if s.starts_with("/todos/42/extra") { println("yes3"); } else { println("no3"); }
            if s.starts_with("") { println("yes4"); } else { println("no4"); }
        }"#);
    assert_eq!(output, "yes\nno2\nno3\nyes4\n");
}

#[test]
fn test_string_strip_prefix_suffix_interpreter() {
    // `String.strip_{prefix,suffix}(p) -> Option[String]`: Some(remainder) when
    // the receiver starts/ends with `p`, else None. Covers match (non-empty
    // remainder), no-match, matched-empty remainder (`Some("")`), and the
    // empty-argument case (matches, remainder is the whole string). Codegen
    // mirrors this (`tests/codegen.rs::e2e_string_strip_prefix_suffix`).
    let output = run(r#"fn main() {
            let s = "hello world";
            match s.strip_prefix("hello ") { Some(r) => println(f"p:{r}"), None => println("pn") }
            match s.strip_prefix("xyz")    { Some(r) => println(f"p:{r}"), None => println("pn") }
            match s.strip_suffix(" world") { Some(r) => println(f"s:{r}"), None => println("sn") }
            match s.strip_suffix("xyz")    { Some(r) => println(f"s:{r}"), None => println("sn") }
            match s.strip_prefix("hello world") { Some(r) => println(f"e:{r}"), None => println("en") }
            match s.strip_prefix("")       { Some(r) => println(f"a:{r}"), None => println("an") }
        }"#);
    assert_eq!(output, "p:world\npn\ns:hello\nsn\ne:\na:hello world\n");
}

#[test]
fn test_string_split_interpreter() {
    // `String.split(sep) -> Vec[String]`. Surfaced by examples/weave (CSV
    // ETL). Covers: char separator, String separator, leading/trailing empty
    // pieces, a separator-free string (single piece), and indexing the result.
    let output = run(r#"fn main() {
            let csv = "a,b,c";
            let parts = csv.split(',');
            let n = parts.len();
            println(f"{n}");
            println(parts[0]);
            println(parts[2]);

            let path = "x::y::z";
            let seg = path.split("::");
            let sn = seg.len();
            println(f"{sn}");

            let edges = ",lead,trail,";
            let en = edges.split(',').len();
            println(f"{en}");

            let whole = "nosep";
            let one = whole.split(',');
            let on = one.len();
            println(f"{on}");
            println(one[0]);
        }"#);
    assert_eq!(output, "3\na\nc\n3\n4\n1\nnosep\n");
}

#[test]
fn test_string_lines_interpreter() {
    // `String.lines() -> Vec[String]` (Rust `str::lines`): split at `\n`, strip
    // a trailing `\r`, and drop a final empty line for a trailing newline.
    // Codegen's `karac_runtime_string_lines` decodes to `&str` and calls the
    // same `str::lines`, so the backends are byte-identical
    // (`tests/codegen.rs::test_e2e_string_lines`; leak-checked in
    // `tests/memory_sanitizer.rs`).
    let output = run(r#"fn main() {
            let a = "one\ntwo\nthree";
            let v = a.lines();
            println(f"{v.len()}");
            println(v[0]);
            println(v[2]);
            // Trailing newline → no final empty line.
            println(f"{"trailing\n".lines().len()}");
            // CRLF endings → `\r` stripped, no trailing empty.
            let c = "crlf\r\nhandling\r\n";
            let w = c.lines();
            println(f"{w.len()}");
            println(w[1]);
            // Empty middle line is preserved.
            let d = "a\n\nb";
            let x = d.lines();
            println(f"{x.len()} {x[1].len()}");
            // Empty string → zero lines.
            println(f"{"".lines().len()}");
        }"#);
    assert_eq!(output, "3\none\nthree\n1\n2\nhandling\n3 0\n0\n");
}

#[test]
fn test_string_split_whitespace_interpreter() {
    // `String.split_whitespace() -> Vec[String]` (Rust `str::split_whitespace`):
    // split on runs of Unicode whitespace, collapsing leading / trailing /
    // repeated whitespace (no empty pieces). Codegen's
    // `karac_runtime_string_split_whitespace` calls the same method, so the
    // backends are byte-identical (`tests/codegen.rs::test_e2e_string_split_whitespace`;
    // leak-checked in `tests/memory_sanitizer.rs`).
    let output = run(r#"fn main() {
            let a = "  the  quick   brown fox  ";
            let v = a.split_whitespace();
            println(f"{v.len()}");
            println(v[0]);
            println(v[3]);
            // Tab and newline are whitespace too.
            let e = "tab\tand\nnewline";
            let w = e.split_whitespace();
            println(f"{w.len()}");
            println(w[1]);
            // Single token, all-whitespace, and empty → 1 / 0 / 0.
            let one = "single".split_whitespace();
            println(f"{one.len()}");
            let ws = "   ".split_whitespace();
            println(f"{ws.len()}");
            let empty = "".split_whitespace();
            println(f"{empty.len()}");
        }"#);
    assert_eq!(output, "4\nthe\nfox\n3\nand\n1\n0\n0\n");
}

#[test]
fn test_string_substring_interpreter() {
    // Mirrors `/tmp/kara-probes/substring_probe.kara`:
    // in-range / start-zero / out-of-range / negative / empty-receiver.
    let output = run(r#"fn main() {
            let s: String = "/todos/42";
            println(s.substring(7));
            println(s.substring(0));
            println(s.substring(100));
            println(s.substring(-1));
            let empty: String = "";
            println(empty.substring(0));
        }"#);
    assert_eq!(output, "42\n/todos/42\n\n\n\n");
}

#[test]
fn test_string_trim_replace_case_interpreter() {
    // Allocating String→String methods (full Unicode, Rust stdlib): trim,
    // replace, to_lowercase, to_uppercase. The case methods can change byte
    // length (`ß` → `SS`); trim is whitespace-only and leaves the receiver
    // untouched (returns a fresh owned copy). Mirrored A/B by
    // tests/codegen.rs::e2e_string_trim_replace_case_codegen.
    let output = run(r#"fn main() {
            let s: String = "  Hello World  ";
            println(s.trim());
            println(s);
            println("HeLLo".to_lowercase());
            println("HeLLo".to_uppercase());
            println("a-b-c".replace("-", "+"));
            println("aaa".replace("a", "bb"));
            println("straße".to_uppercase());
            println("café".to_uppercase());
            println("   ".trim());
            println("Hello World".to_lowercase().replace(" ", "_"));
        }"#);
    assert_eq!(
        output,
        "Hello World\n  Hello World  \nhello\nHELLO\na+b+c\nbbbbbb\nSTRASSE\nCAFÉ\n\nhello_world\n"
    );
}

#[test]
fn test_string_replacen_interpreter() {
    // `String.replacen(from, to, n)` — replace at most the first `n`
    // occurrences (Rust `str::replacen`). A negative count clamps to 0
    // (replace nothing), the documented contract shared with codegen.
    // Mirrored A/B by tests/codegen.rs::e2e_string_replacen_codegen.
    let output = run(r#"fn main() {
            println("a-b-c-d".replacen("-", "_", 2));
            println("x.x.x".replacen(".", "!", 10));
            println("aaaa".replacen("a", "bb", 3));
            println("1,2,3".replacen(",", ";", 0));
            println("1,2,3".replacen(",", ";", -1));
            let s: String = "one two two two";
            println(s.replacen("two", "2", 2));
            // Receiver untouched (fresh owned copy).
            println(s);
        }"#);
    assert_eq!(
        output,
        "a_b_c-d\nx!x!x\nbbbbbba\n1,2,3\n1,2,3\none 2 2 two\none two two two\n"
    );
}

#[test]
fn test_string_trim_start_end_interpreter() {
    // `trim_start` / `trim_end` strip only the leading / trailing Unicode
    // whitespace (Rust `str::trim_{start,end}`), returning a fresh owned copy.
    // Codegen routes through `karac_string_trim_{start,end}` so the backends
    // are byte-identical (tests/codegen.rs::e2e_string_trim_start_end_codegen;
    // leak-checked in tests/memory_sanitizer.rs).
    let output = run(r#"fn main() {
            let s: String = "  Hello  ";
            println(f"[{s.trim_start()}]");
            println(f"[{s.trim_end()}]");
            // Receiver untouched (fresh owned copy).
            println(f"[{s}]");
            // Tabs / newlines are whitespace too.
            println(f"[{"\t x \n".trim_start()}]");
            println(f"[{"\t x \n".trim_end()}]");
            // No whitespace → identity; all-whitespace → empty.
            println(f"[{"none".trim_start()}]");
            println(f"[{"   ".trim_end()}]");
        }"#);
    assert_eq!(
        output,
        "[Hello  ]\n[  Hello]\n[  Hello  ]\n[x \n]\n[\t x]\n[none]\n[]\n"
    );
}

#[test]
fn test_string_substring_two_arg_interpreter() {
    // Two-arg `substring(start, end)` (byte range `[start, end)`): prefix /
    // suffix / empty-when-equal / inverted-bounds (end<start) / end-clamped /
    // negative-start (→ empty, matching the one-arg contract). Drives the
    // self-hosted lexer's `token_text` extraction.
    let output = run(r#"fn main() {
            let s: String = "hello world";
            println(s.substring(0, 5));
            println(s.substring(6, 11));
            println(s.substring(3, 3));
            println(s.substring(8, 2));
            println(s.substring(2, 100));
            println(s.substring(-2, 4));
        }"#);
    assert_eq!(output, "hello\nworld\n\n\nllo world\n\n");
}

#[test]
fn test_char_unicode_predicates_interpreter() {
    // #13 (phase-12 self-hosting) — Unicode `char` classification predicates:
    // is_alphabetic / is_numeric / is_alphanumeric / is_whitespace. The
    // Unicode-aware companions of the `u8.is_ascii_*` byte predicates above.
    // Greek alpha (U+03B1) is_alphabetic and a Devanagari digit (U+096B)
    // is_numeric — the cases a byte-level ASCII check would miss. The interp
    // backend must agree with codegen's `karac_runtime_char_is_*` externs
    // (`test_e2e_char_unicode_predicates`).
    let output = run(r#"fn main() {
            println(f"{'a'.is_alphabetic()} {'5'.is_numeric()} {'5'.is_alphabetic()}");
            println(f"{' '.is_whitespace()} {'_'.is_alphanumeric()} {'z'.is_alphanumeric()}");
            match char.try_from(945) {
                Ok(g) => { println(f"{g.is_alphabetic()} {g.is_numeric()}"); }
                Err(e) => { println("err"); }
            }
            match char.try_from(2411) {
                Ok(d) => { println(f"{d.is_numeric()} {d.is_alphabetic()}"); }
                Err(e) => { println("err"); }
            }
        }"#);
    assert_eq!(
        output,
        "true true false\n\
         true false true\n\
         true false\n\
         true false\n"
    );
}

#[test]
fn test_char_unicode_case_fold_and_is_digit_interpreter() {
    // B-2026-08-12-25 — `char.to_lowercase()` / `to_uppercase()` / `is_digit()`,
    // the spellings a writer reaches for before the `ascii`-qualified ones. The
    // codegen twin is `test_e2e_char_unicode_case_fold_and_is_digit`, asserting
    // the identical bytes.
    //
    // THE EXPANDING CASES ARE THE SPEC, not an edge case: a scalar can fold to
    // several (`ß` → `SS`), which a `char → char` signature cannot express, so
    // the mapping applies only when it yields exactly one scalar and returns
    // `self` otherwise — Go's and Java's rule. `ß`, the `ﬁ` ligature (U+FB01)
    // and `İ` (U+0130, whose full LOWERcase is `i` + a combining dot) are all
    // pinned here as unchanged, and the full-fidelity route
    // (`c.to_string().to_uppercase()` → `SS`) is pinned beside them so the
    // deferral stays visibly a routing choice rather than a missing capability.
    // (`sharp.to_string().to_uppercase()` is bound to a name because the same
    // chain on a bare literal hits a pre-existing, unrelated codegen gap —
    // B-2026-08-13-2; interp takes either form.)
    let output = run(r#"fn main() {
            println(f"{'A'.to_lowercase()} {'a'.to_uppercase()} {'7'.to_uppercase()}");
            println(f"{'é'.to_uppercase()} {'É'.to_lowercase()} {'ß'.to_uppercase()}");
            match char.try_from(64257) {
                Ok(l) => { println(f"{l.to_uppercase()} {l.to_lowercase()}"); }
                Err(e) => { println("err"); }
            }
            match char.try_from(304) {
                Ok(i) => { println(f"{i.to_lowercase()}"); }
                Err(e) => { println("err"); }
            }
            let sharp = 'ß';
            println(sharp.to_string().to_uppercase());
            println(f"{'7'.is_digit(10)} {'f'.is_digit(16)} {'f'.is_digit(10)}");
            println(f"{'z'.is_digit(36)} {' '.is_digit(10)} {'0'.is_digit(2)}");
            let mut out = "".to_string();
            for ch in "HeLLo Wörld".chars() {
                out = out + ch.to_lowercase().to_string();
            }
            println(out);
        }"#);
    assert_eq!(
        output,
        "a A 7\n\
         É é ß\n\
         ﬁ ﬁ\n\
         İ\n\
         SS\n\
         true true false\n\
         true false true\n\
         hello wörld\n"
    );
}

#[test]
fn test_char_case_fold_is_digit_radix_trap_interpreter() {
    // The radix trap is `to_digit`'s, shared verbatim (same arm in interp, same
    // arm in codegen) so the two methods cannot drift apart on it. The message
    // names the method that was called, not the one that owns the arm.
    let errors = runtime_errors("fn main() { println(f\"{'z'.is_digit(37)}\"); }");
    assert!(
        errors.iter().any(|e| e.message.contains("is_digit")
            && e.message.contains("radix must be in 2..=36")
            && e.message.contains("37")),
        "expected the is_digit radix trap, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_char_case_predicates_interpreter() {
    // B-2026-06-18-4 — `char.is_uppercase()` / `is_lowercase()`, the case
    // siblings of the classification predicates above. The interpreter must
    // agree with codegen's `karac_runtime_char_is_upper/lowercase` externs
    // (`test_e2e_char_case_predicates`). Ä (U+00C4) is uppercase, ß (U+00DF) is
    // lowercase — both beyond an ASCII A-Z / a-z check.
    let output = run(r#"fn main() {
            println(f"{'A'.is_uppercase()} {'A'.is_lowercase()}");
            println(f"{'z'.is_uppercase()} {'z'.is_lowercase()}");
            println(f"{'5'.is_uppercase()} {' '.is_lowercase()}");
            match char.try_from(196) {
                Ok(u) => { println(f"{u.is_uppercase()} {u.is_lowercase()}"); }
                Err(e) => { println("err"); }
            }
            match char.try_from(223) {
                Ok(l) => { println(f"{l.is_uppercase()} {l.is_lowercase()}"); }
                Err(e) => { println("err"); }
            }
        }"#);
    assert_eq!(
        output,
        "true false\n\
         false true\n\
         false false\n\
         true false\n\
         false true\n"
    );
}

#[test]
fn test_char_ascii_case_and_is_ascii_interpreter() {
    // `char.to_ascii_uppercase()` / `to_ascii_lowercase()` → char (only the
    // ASCII letters are mapped; digits, punctuation, and non-ASCII pass
    // through), and `char.is_ascii()` → bool. Codegen inlines the same
    // codepoint arithmetic (`test_e2e_char_ascii_case_and_is_ascii`). `é`
    // (U+00E9) is left unchanged by the ASCII fold and is not ASCII.
    let output = run(r#"fn main() {
            println(f"{'a'.to_ascii_uppercase()} {'Z'.to_ascii_lowercase()}");
            println(f"{'5'.to_ascii_uppercase()} {'!'.to_ascii_lowercase()}");
            println(f"{'a'.is_ascii()} {'é'.is_ascii()}");
            println('é'.to_ascii_uppercase());
        }"#);
    assert_eq!(
        output,
        "A z\n\
         5 !\n\
         true false\n\
         é\n"
    );
}

#[test]
fn test_string_char_at_and_count_interpreter() {
    // B-2026-06-18-3 — `s.char_at(i) -> Option[char]` / `s.char_count() -> i64`,
    // the O(n) Unicode-aware access pair. The interpreter must agree with
    // codegen's `karac_runtime_string_char_*` (`test_e2e_string_char_at_and_count`):
    // "héllo" is 6 bytes / 5 scalars, scalar index 1 is `é`; out-of-range and
    // negative indices yield None.
    let output = run(r#"fn nth(s: String, i: i64) -> String {
            match s.char_at(i) {
                Some(c) => f"{c}",
                None => "_",
            }
        }
        fn main() {
            let s: String = "héllo";
            println(f"{s.len()} {s.char_count()}");
            println(f"{nth(s, 0)} {nth(s, 1)} {nth(s, 4)}");
            println(f"{nth(s, 5)} {nth(s, 99)} {nth(s, -1)}");
            let cjk: String = "日本語";
            println(f"{cjk.len()} {cjk.char_count()} {nth(cjk, 1)}");
        }"#);
    assert_eq!(output, "6 5\nh é o\n_ _ _\n9 3 本\n");
}

#[test]
fn test_char_try_from_interpreter() {
    // #10: `char.try_from(n) -> Result[char, i64]`. Must match the codegen E2E
    // (test_e2e_char_try_from) — valid scalars → Ok(char), surrogate / above-
    // max / negative → Err(codepoint).
    let output = run(
        r#"fn show(r: Result[char, i64]) { match r { Ok(ch) => println(ch.to_string()), Err(cp) => println("err:" + cp.to_string()), } }
        fn main() {
            let b: u8 = 65;
            show(char.try_from(b))
            show(char.try_from(97))
            show(char.try_from(0x1F600))
            show(char.try_from(0xD800))
            show(char.try_from(0x110000))
            show(char.try_from(-1))
        }"#,
    );
    assert_eq!(output, "A\na\n😀\nerr:55296\nerr:1114112\nerr:-1\n");
}

#[test]
fn test_char_to_string_from_and_into_interpreter() {
    // `From[char] for String` (design.md § Conversion Traits, "from char
    // literals"). Both `String.from(c)` and `c.into()` produce a one-glyph
    // owned String; multibyte chars encode to full UTF-8; the result is a real
    // heap String (supports `+`). Must match the codegen E2E
    // (`test_e2e_char_to_string_from_and_into`).
    let output = run(r#"fn main() {
            let a: String = String.from('Z');
            println(a);
            let ch: char = 'Q';
            let b: String = ch.into();
            println(b);
            let c: String = '😀'.into();
            println(c);
            let d: String = String.from('A') + "BC";
            println(d);
        }"#);
    assert_eq!(output, "Z\nQ\n😀\nABC\n");
}

#[test]
fn test_string_chars_with_map_char_as_key() {
    // Locks down the LeetCode #3 idiom — chars feeding a Map[char, i64]
    // last-index map. The sliding-window kata is the natural-pull that
    // surfaced this gap; this test guards against regression.
    let output = run(r#"fn main() {
            let mut last_idx: Map[char, i64] = Map.new();
            let mut i = 0i64;
            for c in "abca".chars() {
                last_idx.insert(c, i);
                i = i + 1;
            }
            match last_idx.get('a') { Some(v) => println(v), None => println(-1) }
            match last_idx.get('b') { Some(v) => println(v), None => println(-1) }
            match last_idx.get('c') { Some(v) => println(v), None => println(-1) }
        }"#);
    assert_eq!(output, "3\n1\n2\n");
}

#[test]
fn test_string_bytes_returns_slice_with_byte_values() {
    // `String.bytes() -> Slice[u8]` (design.md § Character type).
    // ASCII input: each byte is the codepoint. Locks down length +
    // positional access + comparison against `u8` literals via the
    // `char as u32 as u8` chain. This is the primitive the kata-8
    // (atoi) rewrite uses to drop the O(n) Vec[char] snapshot.
    let output = run(r#"fn main() {
            let s = "hello";
            let bs = s.bytes();
            println(bs.len());
            println(bs[0]);
            println(bs[4]);
            let h: u8 = 'h' as u32 as u8;
            println(bs[0] == h);
        }"#);
    assert_eq!(output, "5\n104\n111\ntrue\n");
}

#[test]
fn test_string_bytes_empty_string_zero_len() {
    let output = run(r#"fn main() {
            let bs = "".bytes();
            println(bs.len());
        }"#);
    assert_eq!(output, "0\n");
}

#[test]
fn test_string_bytes_multibyte_utf8_yields_byte_count_not_char_count() {
    // UTF-8 encodes a single Unicode scalar in 1..=4 bytes; the
    // distinction matters for the kata's use case (`bytes().len()`
    // is the byte count, NOT the character count — `chars().count()`
    // is the character count). Regression guard against accidentally
    // returning `Slice[char]` or counting characters.
    // "héllo" = h (1B) + é (2B: 0xC3 0xA9) + l (1B) + l (1B) + o (1B) = 6 bytes.
    let output = run(r#"fn main() {
            let bs = "héllo".bytes();
            println(bs.len());
        }"#);
    assert_eq!(output, "6\n");
}

#[test]
fn test_string_from_literal_passthrough() {
    let output = run_no_errors(r#"fn main() { println(String.from("xy")); }"#);
    assert_eq!(output, "xy\n");
}

#[test]
fn test_string_with_capacity_behaves_like_new() {
    let output = run_no_errors(
        r#"fn main() {
            let mut s = String.with_capacity(8);
            s.push_str("ok");
            println(s);
            println(s.len());
        }"#,
    );
    assert_eq!(output, "ok\n2\n");
}

#[test]
fn test_string_sorted_by_cmp_descending() {
    // Char `cmp` via the builtin Ord impl — closes the `b.cmp(a)` idiom
    // that was wedged by the missing primitive `cmp` dispatch.
    let output = run(r#"fn main() { let s = "bdac"; println(s.sorted_by(|a, b| b.cmp(a))); }"#);
    assert_eq!(output, "dcba\n");
}

#[test]
fn test_char_comparison_operators() {
    // Char `<` / `>` / `==` — previously fell through to the binop
    // unreachable. Pinned alongside the primitive Ord dispatch fix.
    let output = run(r#"fn main() {
            let a = 'a';
            let b = 'b';
            println(a < b);
            println(b > a);
            println(a == 'a');
            println(a != b);
        }"#);
    assert_eq!(output, "true\ntrue\ntrue\ntrue\n");
}

#[test]
fn test_string_comparison_operators() {
    let output = run(r#"fn main() {
            let a = "abc";
            let b = "abd";
            println(a < b);
            println(b > a);
            println(a <= "abc");
            println(a >= "abc");
        }"#);
    assert_eq!(output, "true\ntrue\ntrue\ntrue\n");
}

// ── Item 7: broad integration coverage ──────────────────────────

#[test]
fn test_integration_from_str_user_trait() {
    // FromStr-style factory trait taking a String argument. Verifies
    // dispatch when the trait method has a non-Self parameter and the
    // dispatch goes through the bare-call expected-type lowering.
    let output = run(r#"
trait FromStr {
    fn from_str(s: String) -> Self;
}

struct Tag { label: String }

impl FromStr for Tag {
    fn from_str(s: String) -> Tag { Tag { label: s } }
}

fn main() {
    let t: Tag = from_str("hello");
    println(t.label);
}
"#);
    assert_eq!(output, "hello\n");
}

/// HTTP handler ABI trampoline (2026-05-09): F2 owned-String contract.
/// `Request.path()` (and `.method()`) return owned Strings each call —
/// the interpreter side mirrors the codegen contract by returning a
/// fresh `Value::String` per invocation, so two back-to-back calls
/// don't share a buffer or fight over a borrow. The interpreter
/// doesn't run a real HTTP server, so the returned String is empty;
/// what the test pins is the *shape* (owned, not a `ref` borrow) and
/// repeat-callability.
#[test]
fn test_server_serve_handler_request_path_returns_owned_string() {
    let output = run(r#"
fn main() {
    let req = Request { };
    let p1 = req.path();
    let p2 = req.path();
    let m1 = req.method();
    println(p1.len());
    println(p2.len());
    println(m1.len());
}
"#);
    // Empty owned Strings: each `.len()` returns 0, and the chained
    // calls compose without lifetime conflicts.
    assert_eq!(output, "0\n0\n0\n");
}

#[test]
fn test_for_in_vec_string_calls_len_interp() {
    // Interpreter parity for List 2 / item 3 (codegen for-loop element-type
    // propagation). The interpreter already binds runtime-tagged Values, so
    // dispatch on the bound name routes through Value::String automatically.
    let output = run(r#"
fn main() {
    let v = ["alice", "bobby"];
    for s in v {
        println(s.len());
    }
}
"#);
    assert_eq!(output, "5\n5\n");
}

#[test]
fn test_string_receiver_parse_sugar() {
    // String-receiver `s.parse()` resolved against an expected `Option[T]`
    // annotation is sugar for the type-receiver `T.parse(s)` (lowering rewrite).
    let output = run_no_errors(
        r#"
fn main() {
    let a: Option[i64] = "42".parse();
    match a { Some(n) => println(n), None => println(-1) }
    let b: Option[i64] = "bad".parse();
    match b { Some(n) => println(n), None => println(-1) }
    let f: Option[f64] = "2.5".parse();
    match f { Some(x) => println(x), None => println(-1.0) }
}
"#,
    );
    assert_eq!(output, "42\n-1\n2.5\n");
}

#[test]
fn test_iter_scan_with_string_state() {
    // State can be any type — String here.
    let output = run_no_errors(
        r#"
fn main() {
    let v = ["a", "b", "c"];
    for x in v.iter().scan("", |state, item| {
        let new = state + item;
        Some((new, new))
    }) {
        println(x);
    }
}
"#,
    );
    // Concatenating: "" + "a" = "a", "a" + "b" = "ab", "ab" + "c" = "abc".
    assert_eq!(output, "a\nab\nabc\n");
}

#[test]
fn test_range_pattern_char_bounded_inclusive() {
    let output = run_no_errors(
        r#"
fn classify(c: char) -> i32 {
    match c {
        'a'..='z' => 1,
        'A'..='Z' => 2,
        _ => 0,
    }
}
fn main() {
    println(classify('m'));
    println(classify('M'));
    println(classify('1'));
}
"#,
    );
    assert_eq!(output, "1\n2\n0\n");
}

#[test]
fn test_range_pattern_char_bounded_exclusive() {
    let output = run_no_errors(
        r#"
fn main() {
    let c: char = 'g';
    let r = match c {
        'a'..'h' => 1,
        _ => 0,
    };
    println(r);
}
"#,
    );
    // 'g' is in [a, h) — should match.
    assert_eq!(output, "1\n");
}

#[test]
fn test_dbg_json_value_uses_debug_fmt_quoting() {
    // The `value` field must use `Debug` formatting: strings get
    // quoted as `"hello"` not `hello`. The whole field is then JSON-
    // escaped, so the inner quotes show up as `\"`.
    let src = r#"fn main() {
    let _ = dbg("hi");
}
"#;
    let (_stdout, dbg) = run_program_with_dbg(src, DbgOutputMode::Json);
    assert_eq!(dbg.len(), 1);
    // Inner Debug form: `"hi"`. JSON-escaped: `"\"hi\""`.
    assert!(
        dbg[0].contains("\"value\":\"\\\"hi\\\"\""),
        "expected JSON-escaped quoted string, got {:?}",
        dbg[0]
    );
}

#[test]
fn test_interp_refinement_string_try_from_uses_method_predicate() {
    // A refinement over String with a zero-arg method predicate
    // (`self.len() > 0`): `try_from("")` fails, `try_from("hi")` succeeds.
    let output = run_no_errors(
        r#"
type NonEmpty = String where self.len() > 0;
fn main() {
    match NonEmpty.try_from("") {
        Ok(s) => println(s),
        Err(_) => println("rejected-empty"),
    }
    match NonEmpty.try_from("hi") {
        Ok(s) => println(s),
        Err(_) => println("rejected"),
    }
}
"#,
    );
    assert_eq!(output.trim(), "rejected-empty\nhi");
}

// ── CStr borrowed surface (Phase 8 — design.md § C-String Literals) ──
//
// `len` / `is_empty` / `as_bytes` evaluate in tree-walk mode; `as_ptr`
// is deliberately rejected at eval time (no raw-pointer representation
// in the interpreter — see Value::CStr's docstring). Interpreter/codegen
// parity for the value-producing trio is pinned by the codegen E2E
// (`test_e2e_cstr_len_is_empty_as_bytes` asserts identical output).

#[test]
fn test_cstr_len_and_is_empty() {
    let out = run_no_errors(
        r#"
fn main() {
    let msg = c"hello, world";
    println(msg.len());
    let e = c"";
    println(e.len());
    if e.is_empty() { println("empty"); }
    if msg.is_empty() { println("BAD"); } else { println("non-empty"); }
}
"#,
    );
    assert_eq!(out, "12\n0\nempty\nnon-empty\n");
}

#[test]
fn test_cstr_len_excludes_trailing_nul_and_counts_utf8_bytes() {
    // design.md: `c"hello".len()` is 5, not 6 (the NUL is a codegen
    // artifact); `c"café"` is the UTF-8 byte count (5), not the char count.
    let out = run_no_errors(
        r#"
fn main() {
    println(c"hello".len());
    println(c"caf\u{e9}".len());
}
"#,
    );
    assert_eq!(out, "5\n5\n");
}

#[test]
fn test_cstr_as_bytes_yields_source_bytes() {
    let out = run_no_errors(
        r#"
fn main() {
    let bytes = c"abc".as_bytes();
    println(bytes.len());
    println(bytes[0]);
    println(bytes[2]);
}
"#,
    );
    assert_eq!(out, "3\n97\n99\n");
}

#[test]
fn test_cstr_annotated_binding_form() {
    // The design's canonical annotated form (`let msg: ref CStr = ...`).
    let out = run_no_errors(
        r#"
fn main() {
    let msg: ref CStr = c"hi";
    println(msg.len());
}
"#,
    );
    assert_eq!(out, "2\n");
}

#[test]
fn test_cstr_to_string_slice_validates_and_views() {
    // `CStr.to_string_slice() -> Result[StringSlice, Utf8Error]` — parity with
    // codegen (`tests/codegen.rs::test_e2e_cstr_to_string_slice_result`). The
    // tree-walk interpreter has no separate StringSlice value (a borrowed view
    // is just a `Value::String`), so the observable result is the content plus
    // the Ok/Err arm; codegen is where the zero-copy view is real.
    let out = run_no_errors(
        r#"
fn main() {
    match c"hello".to_string_slice() {
        Ok(s) => println(s),
        Err(_) => println("ERR"),
    }
    match c"\xff".to_string_slice() {
        Ok(_) => println("OK?"),
        Err(e) => match e {
            Utf8Error.InvalidByte => println("INVALID"),
            Utf8Error.IncompleteSequence => println("INCOMPLETE"),
            Utf8Error.Other(m) => println(m),
        },
    }
}
"#,
    );
    assert_eq!(out, "hello\nINVALID\n");
}

// ── Owning CString + String.to_cstring (Phase 8 — design.md § C-String
//    Literals, "Owning `CString`") ──
//
// `to_cstring` copies a String into an owning `CString` unless it holds an
// interior NUL (→ `Err(NulError.InteriorNul)`). The introspection surface
// (`len`/`is_empty`/`as_bytes`) matches `CStr`; `as_ptr` is rejected at eval
// time (no raw pointers in the tree-walk). Codegen parity is pinned by
// `tests/codegen.rs::test_e2e_string_to_cstring_*`.

#[test]
fn test_string_to_cstring_ok_len_and_bytes() {
    let out = run_no_errors(
        r#"
fn main() {
    let s = "hello";
    match s.to_cstring() {
        Ok(cs) => {
            println(cs.len());
            let b = cs.as_bytes();
            println(b[0]);
            println(b[4]);
            if cs.is_empty() { println("empty"); } else { println("non-empty"); }
        }
        Err(_) => println("ERR"),
    }
}
"#,
    );
    assert_eq!(out, "5\n104\n111\nnon-empty\n");
}

#[test]
fn test_string_to_cstring_len_excludes_nul_counts_utf8_bytes() {
    // Mirrors the `CStr.len()` rule: byte count, NUL excluded; `café` is 5.
    let out = run_no_errors(
        r#"
fn main() {
    let s = "caf\u{e9}";
    match s.to_cstring() {
        Ok(cs) => println(cs.len()),
        Err(_) => println("ERR"),
    }
}
"#,
    );
    assert_eq!(out, "5\n");
}

#[test]
fn test_string_to_cstring_interior_nul_is_err() {
    // A String carrying an interior NUL cannot become a CString (C truncates).
    let out = run_no_errors(
        r#"
fn main() {
    let s = "ab\u{0}cd";
    match s.to_cstring() {
        Ok(_) => println("OK?"),
        Err(e) => match e {
            NulError.InteriorNul => println("INTERIOR_NUL"),
            NulError.Other(m) => println(m),
        },
    }
}
"#,
    );
    assert_eq!(out, "INTERIOR_NUL\n");
}

#[test]
fn test_string_to_cstring_empty_is_ok_empty() {
    let out = run_no_errors(
        r#"
fn main() {
    let s = "";
    match s.to_cstring() {
        Ok(cs) => {
            println(cs.len());
            if cs.is_empty() { println("empty"); } else { println("non-empty"); }
        }
        Err(_) => println("ERR"),
    }
}
"#,
    );
    assert_eq!(out, "0\nempty\n");
}

#[test]
fn test_set_and_string_mutation_through_field_persists() {
    let set_src = "struct S { seen: Set[i64] }\n\
                   fn main() {\n\
                   \x20 let mut s = S { seen: Set.new() };\n\
                   \x20 let _ = s.seen.insert(3);\n\
                   \x20 let _ = s.seen.insert(3);\n\
                   \x20 let _ = s.seen.insert(9);\n\
                   \x20 println(s.seen.len());\n\
                   }";
    assert_eq!(run(set_src), "2\n");
    let str_src = "struct T { buf: String }\n\
                   fn main() {\n\
                   \x20 let mut t = T { buf: \"\" };\n\
                   \x20 t.buf.push('a');\n\
                   \x20 t.buf.push_str(\"bc\");\n\
                   \x20 println(t.buf);\n\
                   }";
    assert_eq!(run(str_src), "abc\n");
}

#[test]
fn test_secret_field_redacted_in_display() {
    // A struct containing a `Secret[T]` field renders the field as
    // `<redacted>` in the built-in / derived Display across every position
    // (println, .to_string(), f-string) — never leaking the wrapped value.
    // Matches codegen's `test_e2e_secret_field_redacted_in_display`.
    let out = run_no_errors(
        r#"
import std.secret.{Secret};
#[derive(Display)]
struct Config { name: String, token: Secret[String] }
fn main() {
    let c = Config { name: "alice", token: Secret.new("hunter2") };
    println(c);
    println(c.to_string());
    println(f"cfg={c}");
}
"#,
    );
    let expected = "Config { name: alice, token: <redacted> }\n".repeat(2)
        + "cfg=Config { name: alice, token: <redacted> }\n";
    assert_eq!(out, expected);
}

#[test]
fn test_secret_string_zeroize_on_drop_runs() {
    // std.secret Zeroize-on-drop (design.md § Clone/Drop/Zeroize): the compiled
    // path overwrites a `Secret[String]`'s buffer with zeros before freeing it;
    // the tree-walk interpreter has no observable heap after drop, so zeroize is
    // a no-op there (same posture as `ct_eq`'s constant-time). This asserts the
    // interpreter runs construction / use / drop of `Secret[String]` identically
    // to codegen (`test_e2e_secret_string_zeroize_runs_all_backends`).
    let out = run(r#"
import std.secret.{Secret};
fn check(tok: ref Secret[String]) -> bool {
    let ref_val: Secret[String] = Secret.new("hunter2-token-01");
    return tok.ct_eq(ref_val)
}
fn main() {
    let a: Secret[String] = Secret.new("hunter2-token-01");
    let b: Secret[String] = Secret.new("different-secret1");
    println(check(a));
    println(check(b));
}
"#);
    assert_eq!(out, "true\nfalse\n");
}

/// B-2026-08-14-20 — `Slice[T].to_vec()` returns an INDEPENDENT owned
/// container, and `String.from_utf8` accepts a borrowed view.
///
/// Both halves are interpreter-specific traps rather than restatements of the
/// codegen test. `Value::Array` is an `Arc`-shared cell, so the obvious
/// implementation — hand back the receiver, or `.clone()` it — makes the
/// result an ALIAS of its source: lines 03 and 04 write the copy and read the
/// SOURCE, which is the only way that failure shows up (04 is the nested case,
/// where a one-level copy still shares every row). And a `Slice[T]` SLOT is
/// type-erased here: line 05's `Slice` parameter and line 06's `chunks`
/// element both arrive as `Value::Array`, not `Value::Slice`, so a
/// `Value::Slice`-only arm answers `no method 'to_vec' on type 'Vec'` for two
/// shapes the typechecker accepts.
#[test]
fn slice_to_vec_copies_and_from_utf8_takes_a_view() {
    let out = run_no_errors(
        r#"
fn sum_slice(xs: Slice[i64]) -> i64 {
    let v = xs.to_vec();
    let mut t = 0i64;
    let mut i = 0i64;
    while i < v.len() { t = t + v[i]; i = i + 1i64; }
    t
}

fn main() {
    let s = "café";
    match String.from_utf8(s.bytes()) { Ok(t) => println(f"01 {t}"), Err(_) => println("01 bad") }
    match String.from_utf8(s.bytes().to_vec()) { Ok(t) => println(f"02 {t}"), Err(_) => println("02 bad") }

    let nums: Vec[i64] = [10i64, 20i64, 30i64];
    let mut copy = nums.as_slice().to_vec();
    copy[0i64] = 99i64;
    println(f"03 {copy[0i64]} {nums[0i64]}");

    let rows: Vec[Vec[i64]] = [[1i64, 2i64], [3i64, 4i64]];
    let mut rc = rows.as_slice().to_vec();
    rc[0i64][0i64] = 77i64;
    println(f"04 {rc[0i64][0i64]} {rows[0i64][0i64]}");

    println(f"05 {sum_slice(nums)}");

    let cs = nums.as_slice().chunks(2i64);
    let c0 = cs[0i64];
    println(f"06 {c0.to_vec().len()}");

    let words: Vec[String] = ["alpha", "beta"];
    let wc = words[0..2].to_vec();
    println(f"07 {wc.len()} {wc[1i64]}");
}
"#,
    );
    assert_eq!(
        out,
        "01 café\n\
         02 café\n\
         03 99 10\n\
         04 77 1\n\
         05 60\n\
         06 2\n\
         07 2 beta\n"
    );
}

/// B-2026-08-31-19 — `Array[T, N]` renders under codegen, at every depth, the
/// way the interpreter renders it.
///
/// Codegen had NO array Display at all, and the two halves failed differently.
/// Nested — a `Vec` element, a struct field, a tuple field, an enum payload — it
/// fell through `emit_display_fn_for_type_expr` to the by-name catch-all and
/// PANICKED the compiler with `type_name 'Array_i64_3' not yet supported`. At
/// depth 0 it was SILENT, and what it printed depended on the element type:
/// `f"{a}"` on an `Array[i64, 3]` printed `1` (the value-kind fallback reading the
/// aggregate as a scalar), `println(a)` printed NOTHING AT ALL, and an
/// `Array[String, 2]` printed its first element's raw data POINTER. So the
/// compiler-abort half is the one that failed loudly and the plain interpolation
/// was the quiet miscompile, which is the wrong way round for how they read — the
/// row's original title said "nested", and the depth-0 measurement is what widened
/// it.
///
/// `fstr` / `arg` / `str` are those three depth-0 shapes, one per failure mode.
/// `nested`, `struct`, `vecarr`, `field`, `tuple`, `enum` and `mapval` are the
/// positions that panicked; each reaches the renderer through a different
/// dispatcher. `marm` is the match-arm binding — a VALUE, so it asserts the
/// reconstruction and not merely the rendering. `opt` and `res` are the shapes
/// B-2026-08-31-10 had to EXCLUDE from the Option/Result Display gate precisely
/// because of this row and B-2026-08-31-18; they are in here because lifting that
/// exclusion is what closes the loop, and a regression in either row re-declines
/// them.
///
/// `one` is the degenerate extent, which the loop must render without a separator.
/// `flt` and `str` pin element types whose per-element renderer is not the integer
/// one, since the array body's whole job is to delegate.
///
/// Twin of `tests/codegen.rs`'s `e2e_array_display_renders_at_every_depth`,
/// pinned to the same string. The interpreter rendered every row correctly
/// throughout — it is the oracle the compiled side is asserted against.
///
/// The seed is the literal 1 rather than `env.args().len()`: an IN-PROCESS
/// interpreter test sees the TEST binary's argv, which is 1 only when the suite
/// runs unfiltered.
#[test]
fn test_array_display_renders_at_every_depth() {
    assert_eq!(
        run(r#"#[derive(Display)]
struct P { x: i64, s: String }
#[derive(Display)]
struct WithArr { a: Array[i64, 3], n: i64 }
#[derive(Display)]
enum E { A(Array[i64, 3]), S(Array[String, 2]), N }

fn main() {
    let n: i64 = 1;
    let a: Array[i64, 3] = [n, n + 1, n + 2];
    let s: Array[String, 2] = [f"x{n}", f"y{n}"];
    let f: Array[f64, 2] = [n as f64 + 0.5, n as f64 + 1.5];
    let one: Array[i64, 1] = [n];

    println(f"fstr   {a}");
    println(a);
    println(f"str    {s}");
    println(f"flt    {f}");
    println(f"one    {one}");

    let nested: Array[Array[i64, 2], 2] = [[n, n + 1], [n + 2, n + 3]];
    println(f"nested {nested}");
    let structs: Array[P, 2] = [P { x: n, s: f"p1" }, P { x: n + 1, s: f"p2" }];
    println(f"struct {structs}");

    let mut v: Vec[Array[i64, 3]] = Vec.new();
    v.push(a);
    println(f"vecarr {v}");

    let w = WithArr { a: a, n: n };
    println(f"field  {w}");

    let t: (Array[i64, 3], i64) = (a, n);
    println(f"tuple  {t}");

    let e = E.A(a);
    println(f"enum   {e}");
    match e { E.A(b) => { println(f"marm   {b}"); } E.S(b) => {} E.N => {} }

    let oa: Option[Array[i64, 3]] = Some(a);
    println(f"opt    {oa}");
    let ra: Result[Array[i64, 3], i64] = Ok(a);
    println(f"res    {ra}");

    let mut m: Map[String, Array[i64, 3]] = Map.new();
    m.insert(f"k", a);
    println(f"mapval {m}");
}
"#),
        r#"fstr   [1, 2, 3]
[1, 2, 3]
str    [x1, y1]
flt    [1.5, 2.5]
one    [1]
nested [[1, 2], [3, 4]]
struct [P { x: 1, s: p1 }, P { x: 2, s: p2 }]
vecarr [[1, 2, 3]]
field  WithArr { a: [1, 2, 3], n: 1 }
tuple  ([1, 2, 3], 1)
enum   A([1, 2, 3])
marm   [1, 2, 3]
opt    Some([1, 2, 3])
res    Ok([1, 2, 3])
mapval {k: [1, 2, 3]}
"#
    );
}

/// B-2026-08-31-10 — the ORACLE half: what an `Option`/`Result` with a
/// multi-word payload prints. Twin of `tests/codegen.rs`'s
/// `e2e_option_result_display_multiword_payloads`, pinned to the same string.
///
/// The interpreter rendered every one of these all along; codegen's
/// `is_reconstructable_display_payload` gate refused them and the f-string
/// failed to compile. This side is what the compiled side is asserted against,
/// so it belongs in the tree even though it never went RED — a future change
/// that alters, say, how a `Map` payload nests inside `Some(…)` has to move
/// both.
#[test]
fn test_option_result_display_multiword_payloads() {
    assert_eq!(
        run(r#"#[derive(Display)]
struct P { x: i64, y: String }
#[derive(Display)]
struct Q { a: i64, b: i64, c: i64, d: i64 }
#[derive(Display)]
enum Inner { A(i64), B }

fn mk(n: i64) -> Option[Vec[i64]] {
    let mut v: Vec[i64] = Vec.new();
    v.push(n);
    return Some(v)
}

fn main() {
    // The seed is the literal 1 rather than `env.args().len()`: an IN-PROCESS
    // interpreter test sees the TEST binary's argv, which is 1 only when the
    // suite runs unfiltered and 2+ under `cargo test <filter>`. The codegen
    // twin keeps `env.args()` because it needs an opaque seed to survive -O2
    // folding, and 1 is what that yields under its harness.
    let n: i64 = 1;

    let mut v: Vec[i64] = Vec.new();
    v.push(n);
    v.push(n + 1);
    let ov: Option[Vec[i64]] = Some(v);
    println(f"vec    {ov}");
    println(ov);

    let ot: Option[(i64, i64)] = Some((n, n + 1));
    println(f"tuple  {ot}");

    let mut m: Map[String, i64] = Map.new();
    m.insert(f"a", n);
    let om: Option[Map[String, i64]] = Some(m);
    println(f"map    {om}");

    let mut sm: SortedMap[String, i64] = SortedMap.new();
    sm.insert(f"k", n);
    let osm: Option[SortedMap[String, i64]] = Some(sm);
    println(f"smap   {osm}");

    let oo: Option[Option[i64]] = Some(Some(n));
    println(f"nested {oo}");

    let op: Option[P] = Some(P { x: n, y: f"s" });
    println(f"struct {op}");

    let oq: Option[Q] = Some(Q { a: n, b: n, c: n, d: n });
    println(f"four   {oq}");

    let oi: Option[Inner] = Some(Inner.A(n));
    println(f"enum   {oi}");

    let of: Option[f16] = Some(((n as f32) + 1.5f32) as f16);
    println(f"f16    {of}");
    let ob: Option[bf16] = Some(((n as f32) + 2.25f32) as bf16);
    println(f"bf16   {ob}");

    let mut vs: Vec[String] = Vec.new();
    vs.push(f"q");
    let re: Result[i64, Vec[String]] = Err(vs);
    println(f"result {re}");

    println(f"call   {mk(n)}");

    let nn: Option[Vec[i64]] = None;
    println(f"none   {nn}");
}
"#),
        r#"vec    Some([1, 2])
Some([1, 2])
tuple  Some((1, 2))
map    Some({a: 1})
smap   Some(SortedMap{k: 1})
nested Some(Some(1))
struct Some(P { x: 1, y: s })
four   Some(Q { a: 1, b: 1, c: 1, d: 1 })
enum   Some(A(1))
f16    Some(2.5)
bf16   Some(3.25)
result Err([q])
call   Some([1])
none   None
"#
    );
}

/// B-2026-08-14-19's over-reach guard, and the interpreter twin of
/// `test_e2e_substring_aligned_slices_unchanged`. Every slice landing ON a
/// boundary is untouched, including the two established out-of-range contracts
/// (a start past the end yields empty and must NOT fault; an end past the end
/// clamps). These passed before the check and must keep passing.
#[test]
fn test_substring_aligned_slices_unchanged() {
    assert_eq!(
        run("fn main() {\n\
                 let s = \"\u{65e5}\u{672c}\u{8a9e}\";\n\
                 println(s.substring(0i64, 3i64).len());\n\
                 println(s.substring(3i64, 9i64).len());\n\
                 println(s.substring(0i64, 9i64).len());\n\
                 println(s.substring(9i64).len());\n\
                 println(\"abcdef\".substring(0i64, 20i64));\n\
                 println(\"abcdef\".substring(2i64));\n\
                 println(s.substring(20i64).len());\n\
             }"),
        "3\n6\n9\n0\nabcdef\ncdef\n0\n"
    );
}

#[test]
fn unsigned_to_string_matches_fstring_interpolation() {
    // B-2026-08-11-21 leg 2, and it pointed the OPPOSITE way to leg 1: the
    // interpreter rendered `u64.to_string()` signed while its f-string of the
    // same value was correct, and codegen got both right.
    //
    // The unsigned check asked `expr_types[receiver span]`, but the parser
    // gives a MethodCall its receiver's span, so that entry held the call's
    // `Str` result by the time the interpreter ran — measured as
    // `expr_types[SpanKey(66, 2)] = Str`. `f"{hi}"` was correct precisely
    // because its interpolated expression is the bare identifier, whose span
    // nothing aliases. The receiver type is now stashed at the closing paren,
    // the leaf span `pow` and the bit intrinsics already use.
    //
    // The signed binding is the control: a fix that simply rendered every
    // `Value::Int` unsigned would turn -1 into 2^64-1.
    assert_eq!(
        run("fn main() {\n\
                 let hi: u64 = 9223372036854775808u64;\n\
                 let all: u64 = 18446744073709551615u64;\n\
                 let neg: i64 = 0i64 - 1i64;\n\
                 println(f\"{hi}\");\n\
                 println(hi.to_string());\n\
                 println(all.to_string());\n\
                 println(neg.to_string());\n\
             }\n"),
        "9223372036854775808\n9223372036854775808\n18446744073709551615\n-1\n"
    );
}

/// B-2026-08-14-35 — the interpreter oracle for `SortedMap` / `SortedSet`
/// rendering. This backend renders from the value's own type and was right on
/// every line here already; what makes it the oracle is that codegen, which
/// pointed the sorted types at `Map` / `Set`'s Display fns because they share
/// the `KaracMap` storage, disagreed on ALL of them — wrong type name over
/// hash-bucket order — with no diagnostic.
///
/// The program is byte-identical to `SORTED_DISPLAY_SRC` in `tests/codegen.rs`,
/// whose twin asserts this exact output under `karac build`. Insertion order is
/// deliberately not sorted order anywhere in it (zebra/apple/mango, 30/10/20),
/// so a regression to bucket or insertion order fails rather than passing by
/// luck.
#[test]
fn test_sorted_map_and_set_display_prefix_and_order() {
    assert_eq!(
        run(r#"
struct Holder { m: SortedMap[String, i64], s: SortedSet[i64] }

fn mkm() -> SortedMap[String, i64] {
    let mut m: SortedMap[String, i64] = SortedMap.new();
    let _ = m.insert("zebra", 1);
    let _ = m.insert("apple", 2);
    let _ = m.insert("mango", 3);
    return m;
}

fn mks() -> SortedSet[i64] {
    let mut s: SortedSet[i64] = SortedSet.new();
    s.insert(30);
    s.insert(10);
    s.insert(20);
    return s;
}

fn main() {
    let mut bm: SortedMap[String, i64] = SortedMap.new();
    let _ = bm.insert("zebra", 1);
    let _ = bm.insert("apple", 2);
    let _ = bm.insert("mango", 3);
    println(f"{bm}");
    println(bm);
    let mut bs: SortedSet[i64] = SortedSet.new();
    bs.insert(30);
    bs.insert(10);
    bs.insert(20);
    println(f"{bs}");
    println(bs);
    let h = Holder { m: mkm(), s: mks() };
    println(f"{h.m}");
    println(h.s);
    println(f"{mkm()}");
    println(f"{mks()}");
    let vm: Vec[SortedMap[String, i64]] = [mkm()];
    println(f"{vm}");
    let vs: Vec[SortedSet[i64]] = [mks()];
    println(f"{vs}");
    let em: SortedMap[String, i64] = SortedMap.new();
    println(f"{em}");
    let es: SortedSet[i64] = SortedSet.new();
    println(f"{es}");
}
"#),
        "\
SortedMap{apple: 2, mango: 3, zebra: 1}
SortedMap{apple: 2, mango: 3, zebra: 1}
SortedSet{10, 20, 30}
SortedSet{10, 20, 30}
SortedMap{apple: 2, mango: 3, zebra: 1}
SortedSet{10, 20, 30}
SortedMap{apple: 2, mango: 3, zebra: 1}
SortedSet{10, 20, 30}
[SortedMap{apple: 2, mango: 3, zebra: 1}]
[SortedSet{10, 20, 30}]
SortedMap{}
SortedSet{}
"
    );
}

/// B-2026-08-14-23 — the interpreter oracle for the in-place String append.
/// Values were never in doubt on this backend; what makes it the oracle is that
/// codegen now takes a structurally different path for the admitted shapes and
/// must still produce these exact bytes.
#[test]
fn test_string_append_spellings_agree() {
    assert_eq!(
        run("fn mk(n: i64) -> String { let mut t = String.new(); t.push_str(\"v\"); t.push_str(n.to_string()); t }\n\
             fn app_ref(s: mut ref String) { s = s + \"R\"; }\n\
             fn main() {\n\
                 let mut s = \"lit\";\n\
                 s = s + \"abc\"; println(s);\n\
                 let t = \"T\";\n\
                 s = s + t; println(s);\n\
                 s = s + \" \" + t; println(s);\n\
                 s = s + mk(3i64); println(s);\n\
                 s += \"P\"; println(s);\n\
                 let mut d = \"ab\";\n\
                 d = d + d; println(d);\n\
                 let mut e = \"ab\";\n\
                 e = e + e[0..1]; println(e);\n\
                 let mut p = \"P\";\n\
                 p = \"X\" + p; println(p);\n\
                 let mut r = \"r\";\n\
                 app_ref(mut r); println(r);\n\
                 let mut z = String.new();\n\
                 z = z + \"\"; println(z.len());\n\
             }\n"),
        "litabc\nlitabcT\nlitabcT T\nlitabcT Tv3\nlitabcT Tv3P\nabab\naba\nXP\nrR\n0\n"
    );
}

#[test]
fn test_enum_variant_path_display_oracle() {
    // Oracle twin of `tests/codegen.rs`'s `test_e2e_enum_variant_path_display`
    // (B-2026-08-17-34). The interpreter always handled the variant-path
    // operand; the compiled backends refused it, and separately ignored
    // `#[derive(Display(snake_case))]`. This is the reference the two
    // compiled backends were made to match.
    let out = run("\n\
         #[derive(Display)]\n\
         enum Direction { Up, Down }\n\
         #[derive(Display(snake_case))]\n\
         enum Mode { FastPath, SlowPath }\n\
         #[derive(Display)]\n\
         enum Evt { KeyDown(i64), MouseUp }\n\
         fn main() {\n\
             println(f\"{Direction.Up}\");\n\
             println(Direction.Down);\n\
             println(f\"{Mode.FastPath}\");\n\
             println(f\"{Evt.MouseUp}\");\n\
             println(f\"{Direction.Up} then {Direction.Down}\");\n\
         }\n");
    assert_eq!(out, "Up\nDown\nfast_path\nMouseUp\nUp then Down\n");
}

/// B-2026-08-18-22 — the interpreter oracle for SCALAR READERS on a String
/// range subscript. `s[0..5].len()` ran here and under `karac check` all along
/// while the build died on "element TypeExpr unknown", so this is the side the
/// compiled fix was measured against — a run-vs-build divergence.
///
/// The `ref String` parameter is deliberate: that receiver records as
/// `Ref(Str)`, the shape a span-keyed String test filters out, and it is what
/// the first cut of the fix silently declined.
#[test]
fn test_string_range_slice_scalar_readers_oracle() {
    let out = run_no_errors(
        "fn count(s: ref String) -> i64 {\n\
             let mut n = 0;\n\
             n = n + s[0..5].len();\n\
             if s[0..5].starts_with(\"he\") { n = n + 1; }\n\
             if s[6..11].contains(\"or\") { n = n + 1; }\n\
             if s[0..5].is_empty() { n = n + 100; }\n\
             return n;\n\
         }\n\
         fn main() {\n\
             let src: String = \"hello world\";\n\
             println(count(src).to_string());\n\
             println(src[0..5].char_count().to_string());\n\
             println(src.len().to_string());\n\
         }\n",
    );
    assert_eq!(out, "7\n5\n11\n");
}

/// The peel resolves a GENERIC declaration's parameter from the concrete type
/// (B-2026-08-19-27). This is what makes the seeded `Option` work at all — its
/// `Some` declares a bare `T`, carrying no width — so a user-defined generic
/// exercises the same substitution through a path with nothing seeded about it.
/// Codegen cannot render a generic struct's Display at all (a separate,
/// pre-existing gap that refuses `Cell[i64]` just as readily), so this asserts
/// against the values rather than against a compiled twin.
#[test]
fn the_display_peel_substitutes_generic_parameters() {
    assert_eq!(
        run_no_errors(
            "#[derive(Display)]\n\
             struct Cell[T] { pub val: T }\n\
             #[derive(Display)]\n\
             enum MyOpt[T] { Has(T), Empty }\n\
             fn main() {\n\
             let c: Cell[u64] = Cell { val: 18446744073709551615u64 };\n\
             println(c);\n\
             let c2: Cell[u128] = Cell { val: 340282366920938463463374607431768211455u128 };\n\
             println(c2);\n\
             let e: MyOpt[u64] = MyOpt.Has(18446744073709551615u64);\n\
             println(e);\n\
             }"
        ),
        "Cell { val: 18446744073709551615 }\n\
         Cell { val: 340282366920938463463374607431768211455 }\n\
         Has(18446744073709551615)\n"
    );
}

#[test]
fn is_sorted_orders_strings_and_compound_elements() {
    let out = run("struct P { a: i64, b: i64 }\n\
    fn main() {\n\
        let s: Vec[String] = [\"apple\", \"banana\"];\n\
        let s2: Vec[String] = [\"banana\", \"apple\"];\n\
        let t: Vec[(i64, i64)] = [(1, 2), (1, 3), (2, 0)];\n\
        let n: Vec[Vec[i64]] = [[1, 2], [1, 3]];\n\
        let p: Vec[P] = [P { a: 1, b: 2 }, P { a: 1, b: 1 }];\n\
        println(f\"{s.is_sorted()} {s2.is_sorted()} {t.is_sorted()} \
                   {n.is_sorted()} {p.is_sorted()}\");\n\
    }");
    // Tuples and nested Vecs compare lexicographically; a struct compares by
    // field DECLARATION order, so `b` breaks the tie the equal `a` leaves.
    assert_eq!(out, "true false true true false\n");
}

#[test]
fn test_normalize_makes_the_spec_hazard_comparable() {
    // B-2026-08-20-41 — the exact example design.md § Strings (Equality) warns
    // about, and the remedy it names. `e` + COMBINING ACUTE and the precomposed
    // `é` are different byte strings, so `==` is false; normalizing both to a
    // common form makes them compare equal. Until this slice the bullet
    // described a real trap and pointed at an API that did not exist.
    let out = run("fn main() {\n\
            let a = \"e\\u{0301}\";\n\
            let b = \"\\u{00e9}\";\n\
            println(a == b);\n\
            println(a.normalize(Nfc) == b.normalize(Nfc));\n\
            println(a.normalize(Nfd) == b.normalize(Nfd));\n\
        }");
    assert_eq!(out, "false\ntrue\ntrue\n");
}

#[test]
fn test_normalize_changes_byte_length_in_both_directions() {
    // Composition shrinks and decomposition grows, which is why the result is
    // always a fresh String rather than an in-place edit. Lengths, not just
    // equality, so a normalize that returned its receiver unchanged would fail.
    let out = run("fn main() {\n\
            let nfd = \"e\\u{0301}\";\n\
            let nfc = \"\\u{00e9}\";\n\
            println(nfd.len());\n\
            println(nfd.normalize(Nfc).len());\n\
            println(nfc.len());\n\
            println(nfc.normalize(Nfd).len());\n\
        }");
    assert_eq!(out, "3\n2\n2\n3\n");
}

#[test]
fn test_normalize_compatibility_forms_differ_from_canonical_ones() {
    // U+FB01 LATIN SMALL LIGATURE FI is untouched by the canonical forms and
    // folds to `fi` under the compatibility ones. Without this, swapping NFC
    // for NFKC anywhere in the wiring would pass every other test here.
    let out = run("fn main() {\n\
            let lig = \"\\u{FB01}\";\n\
            println(lig.normalize(Nfc));\n\
            println(lig.normalize(Nfd));\n\
            println(lig.normalize(Nfkc));\n\
            println(lig.normalize(Nfkd));\n\
        }");
    assert_eq!(out, "\u{FB01}\n\u{FB01}\nfi\nfi\n");
}

#[test]
fn test_normalize_accepts_a_bare_variant_a_qualified_one_and_a_binding() {
    // design.md's spelling is the bare `Nfc`; the qualified path and a
    // variable holding a form must reach the same implementation. The binding
    // is the interesting one — codegen lowers it through the enum's
    // discriminant rather than a matched literal, and a mismatch there was a
    // silent wrong-form miscompile before the layout seed.
    let out = run("fn main() {\n\
            let a = \"e\\u{0301}\";\n\
            let bound = Nfc;\n\
            println(a.normalize(Nfc).len());\n\
            println(a.normalize(NormalizationForm.Nfc).len());\n\
            println(a.normalize(bound).len());\n\
        }");
    assert_eq!(out, "2\n2\n2\n");
}

#[test]
fn test_normalize_leaves_empty_and_ascii_untouched() {
    // Nothing to compose or decompose, on every form — the identity cases that
    // catch a transform accidentally rewriting text it should not.
    let out = run("fn main() {\n\
            println(\"\".normalize(Nfc).len());\n\
            println(\"plain ascii\".normalize(Nfd));\n\
            println(\"plain ascii\".normalize(Nfkd));\n\
        }");
    assert_eq!(out, "0\nplain ascii\nplain ascii\n");
}

/// B-2026-09-04-14 — the INTERPRETER half of
/// `e2e_retained_source_veto_does_not_escape_its_function`, pinned to the same
/// string.
///
/// This side never moved. The defect was a compiled-backend double free caused
/// by a codegen side table keyed by bare binding name outliving the function
/// body that filled it; the interpreter has no such roster, so it printed this
/// transcript before and after. Pinning it is what makes the codegen twin's
/// expected string an assertion about the compiled backends rather than about
/// the program.
#[test]
fn test_retained_source_veto_does_not_escape_its_function() {
    let out = run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}:{self.s}") } }
fn mk(n: i64) -> R { return R { id: n, s: f"s{n}" }; }

fn retained() { let t = (mk(4), Option.Some(mk(104))); let (_, o) = t; match o { Option.Some(r) => { println(f"  g{r.id}") }, Option.None => { println("  n") } } }
fn mover()    { let o = Option.Some(mk(11)); let q = o; println(f"  m{q.is_some()}") }
fn moverstr() { let o = Option.Some(f"z12"); let q = o; println(f"  s{q.is_some()}") }
fn unshared() { let d = Option.Some(mk(13)); let e = d; println(f"  d{e.is_some()}") }

fn main() {
  println("mover");    mover()
  println("moverstr"); moverstr()
  println("unshared"); unshared()
  println("retained"); retained()
  println("done")
}
"#);
    assert_eq!(
        out,
        r#"mover
  mtrue
dR11:s11
moverstr
  strue
unshared
  dtrue
dR13:s13
retained
dR4:s4
  g104
dR104:s104
done
"#
    );
}
