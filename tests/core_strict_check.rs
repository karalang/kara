//! `karac check` enforces the v2 core's ownership rules as errors
//! (`docs/core-semantics.md` §3.2 use after move, §3.3 moves in loops, §3.7
//! moves out of a borrowed place, §6.4 no implicit RC), and `karac fix`
//! repairs each with `.clone()`. `build` and `run` keep the legacy rules until
//! the v2 middle end lands, so their output stays the old-behaviour oracle.
//!
//! The programs are the review thread's core pins (`review/core-pins/`), plus
//! the places where legacy reports a move the core does not make.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-core-strict-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

/// `check` must fail with `expect` in its output; `fix` must then make it pass.
fn rejected_then_fixed(tag: &str, src: &str, expect: &str) {
    rejected_then_fixed_by(tag, src, expect, ".clone()");
}

/// [`rejected_then_fixed`], where the fix must write `edit`.
fn rejected_then_fixed_by(tag: &str, src: &str, expect: &str, edit: &str) {
    rejected_then_fixed_in(tag, src, expect, edit, 1);
}

/// [`rejected_then_fixed_by`], over `passes` runs of `karac fix`: a fix that
/// changes a signature leaves its call sites to the next pass.
fn rejected_then_fixed_in(tag: &str, src: &str, expect: &str, edit: &str, passes: usize) {
    let (dir, path) = fixture(tag, src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{tag}: check must reject: {err}");
    assert!(err.contains(expect), "{tag}: expected `{expect}` in: {err}");
    for _ in 0..passes {
        karac().arg("fix").arg(&path).output().unwrap();
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{tag}: check must pass after `karac fix`; source now:\n{fixed}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fixed.contains(edit),
        "{tag}: fix must write `{edit}`: {fixed}"
    );
    assert!(
        !fixed.contains(".clone().clone()"),
        "{tag}: fix applied one edit twice: {fixed}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn accepted(tag: &str, src: &str) {
    let (dir, path) = fixture(tag, src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{tag}: check must accept: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn use_after_move_is_an_error() {
    rejected_then_fixed(
        "uam",
        "fn take(s: own String) { println(s); }\n\
         fn main() {\n\
             let s = \"hi\".to_string();\n\
             take(s);\n\
             println(s);\n\
         }\n",
        "error[ownership]",
    );
}

#[test]
fn maybe_moved_is_an_error_not_an_rc() {
    rejected_then_fixed(
        "maybe",
        "fn take(s: own String) { println(s); }\n\
         fn f(c: bool) {\n\
             let s = \"hi\".to_string();\n\
             if c { take(s); }\n\
             println(s);\n\
         }\n\
         fn main() { f(true); }\n",
        "value 's' may have been moved (moved at line 4:",
    );
}

#[test]
fn moved_in_loop_is_an_error() {
    rejected_then_fixed(
        "loop",
        "fn take(s: own String) { println(s); }\n\
         fn main() {\n\
             let s = \"hi\".to_string();\n\
             for i in 0..2 { take(s); }\n\
         }\n",
        "value 's' is moved inside a loop and not assigned again",
    );
}

#[test]
fn moved_into_a_container_then_used_is_an_error() {
    rejected_then_fixed(
        "container",
        "struct Bag { items: Vec[String] }\n\
         impl Bag { fn add(mut ref self, s: own String) { self.items.push(s); } }\n\
         fn main() {\n\
             let mut a = Bag { items: Vec.new() };\n\
             let mut b = Bag { items: Vec.new() };\n\
             let s = \"x\".to_string();\n\
             a.add(s);\n\
             b.add(s);\n\
             println(f\"{a.items.len()} {b.items.len()}\");\n\
         }\n",
        "error[ownership]",
    );
}

#[test]
fn move_out_of_a_ref_place_is_an_error() {
    rejected_then_fixed(
        "outofref",
        "struct User { name: String }\n\
         fn name_of(u: ref User) -> String { u.name }\n\
         fn main() {\n\
             let u = User { name: \"x\".to_string() };\n\
             println(name_of(u));\n\
         }\n",
        "cannot move a non-`Copy` value out of a borrowed place",
    );
}

/// §4.6: `_` never binds and never moves. Legacy treats `let _ = x` as a move.
#[test]
fn let_underscore_moves_nothing() {
    accepted(
        "underscore",
        "struct R { id: i64 }\n\
         fn main() {\n\
             let x = R { id: 9 };\n\
             let _ = x;\n\
             println(f\"x{x.id}\");\n\
         }\n",
    );
}

/// §4.6: a `match` on a borrowed place binds `ref`s into it and moves nothing.
#[test]
fn match_on_a_borrowed_place_moves_nothing() {
    accepted(
        "scrutinee",
        "struct Sub { name: String }\n\
         struct App { sub: Option[Sub] }\n\
         impl App {\n\
             fn sub_name(ref self) -> Option[String] {\n\
                 match self.sub {\n\
                     Some(s) => Some(s.name.clone()),\n\
                     None => None,\n\
                 }\n\
             }\n\
         }\n\
         fn main() {\n\
             let a = App { sub: Some(Sub { name: \"n\".to_string() }) };\n\
             println(a.sub_name().unwrap());\n\
         }\n",
    );
}

/// §3.1: `String`'s `+` is `fn add(ref self, other: ref String)`, so neither
/// operand moves. Legacy classifies the right operand as consumed.
#[test]
fn string_concat_moves_neither_operand() {
    accepted(
        "concat",
        "fn main() {\n\
             let tag = \"7\".to_string();\n\
             let a = \"m\" + tag + \"=\";\n\
             let b = \"n\" + tag;\n\
             println(a + b + tag);\n\
         }\n",
    );
}

/// A function that silences the RC performance note does not silence the
/// core error: under the core there is no fallback left to accept.
#[test]
fn allow_rc_fallback_does_not_silence_the_error() {
    let (dir, path) = fixture(
        "allow",
        "fn take(s: own String) { println(s); }\n\
         #[allow(rc_fallback)]\n\
         fn f(c: bool) {\n\
             let s = \"hi\".to_string();\n\
             if c { take(s); }\n\
             println(s);\n\
         }\n\
         fn main() { f(true); }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(!out.status.success());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The legacy lane keeps compiling these programs, with the hidden copy, so
/// its output remains the old-behaviour oracle.
#[test]
fn legacy_run_still_accepts_use_after_move() {
    let (dir, path) = fixture(
        "legacy",
        "fn take(s: String) { println(s); }\n\
         fn main() {\n\
             let s = \"hi\".to_string();\n\
             take(s);\n\
             println(s);\n\
         }\n",
    );
    let out = karac()
        .args(["run", "--interp"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hi\nhi\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// §4.7: an impl may declare a weaker mode than its trait, never a stronger
/// one. `karac fix` rewrites the impl to the trait's mode. Under D5 a bare
/// `self` and a bare `v: Vec[i64]` borrow, which is the trait's `ref`, so
/// only the `mut ref` is stronger (B-2026-10-14-34; before it the check read
/// an impl's bare receiver and parameters by value and refused them too).
#[test]
fn impl_mode_stronger_than_trait_is_an_error() {
    let (dir, path) = fixture(
        "implmode",
        "trait Grab { fn grab(ref self, v: ref Vec[i64], w: ref Vec[i64]) -> i64; }\n\
         struct P { x: i64 }\n\
         impl Grab for P {\n\
             fn grab(self, v: Vec[i64], w: mut ref Vec[i64]) -> i64 { self.x + v.len() + w.len() }\n\
         }\n\
         fn main() { let p = P { x: 1 }; let v: Vec[i64] = Vec.new(); println(p.grab(v, v)); }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must reject: {err}");
    assert!(
        err.contains(
            "impl method 'P.grab' takes 'w' as `mut ref`, but 'Grab.grab' takes it as `ref`"
        ),
        "expected the stronger `w` in: {err}"
    );
    for bare in ["its receiver as `self`", "takes 'v' by value"] {
        assert!(
            !err.contains(bare),
            "a bare form borrows, as the trait's `ref` does: {err}"
        );
    }
    karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        fixed.contains("fn grab(self, v: Vec[i64], w: ref Vec[i64]) -> i64 {"),
        "fix must take the trait's modes: {fixed}"
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must pass after `karac fix`: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// §4.7: the weaker direction conforms. `String`'s `+` is the motivating case.
#[test]
fn impl_mode_weaker_than_trait_is_accepted() {
    accepted(
        "implweak",
        "trait Size { fn size(self, v: Vec[i64]) -> i64; }\n\
         struct P { x: i64 }\n\
         impl Size for P { fn size(ref self, v: ref Vec[i64]) -> i64 { self.x + v.len() } }\n\
         fn main() { let p = P { x: 1 }; let v: Vec[i64] = Vec.new(); println(p.size(v)); }\n",
    );
}

#[test]
fn move_out_of_a_collection_element_is_an_error() {
    // §3.7: the Vec still owns its element, so a by-value read needs a copy.
    rejected_then_fixed(
        "index-arg",
        "fn take(s: own String) { println(s); }\n\
         fn main() {\n\
             let v = vec![\"a\".to_string(), \"b\".to_string()];\n\
             take(v[0]);\n\
             println(v[1]);\n\
         }\n",
        // The typechecker reports this one already; check says it once.
        "E_INDEX_MOVE_NON_COPY",
    );
    rejected_then_fixed(
        "index-tail",
        "fn first(v: ref Vec[String]) -> String { v[0] }\n\
         fn main() {\n\
             let v = vec![\"a\".to_string()];\n\
             println(first(v));\n\
         }\n",
        "out of a collection element",
    );
    rejected_then_fixed(
        "index-push",
        "fn main() {\n\
             let v = vec![\"a\".to_string()];\n\
             let mut w: Vec[String] = Vec.new();\n\
             w.push(v[0]);\n\
             println(w.len());\n\
         }\n",
        "out of a collection element",
    );
}

#[test]
fn reading_a_collection_element_without_moving_is_accepted() {
    // Copy elements, range slices, `+` operands, numeric parses and
    // `extend_from_slice` arguments read the element in place.
    accepted(
        "index-reads",
        "fn main() {\n\
             let n = vec![1, 2, 3];\n\
             let a = n[0];\n\
             let v = vec![\"a\".to_string(), \"12\".to_string()];\n\
             let s = v[0] + v[0];\n\
             let k = i64.parse(v[1]);\n\
             let mut w: Vec[i64] = Vec.new();\n\
             w.extend_from_slice(n[0..2]);\n\
             println(f\"{a} {s} {k} {w.len()} {v[0].len()}\");\n\
         }\n",
    );
}

#[test]
fn passing_a_bare_for_element_by_value_is_an_error() {
    // §4.6: a bare `for` binds each element as a `ref`, so handing it to a
    // by-value parameter moves out of the collection. `.clone()` copies it.
    rejected_then_fixed(
        "for-elem-arg",
        "#[derive(Clone)]\n\
         struct T { name: String }\n\
         fn render(t: own T) -> String { t.name }\n\
         fn main() {\n\
             let v = vec![T { name: \"a\".to_string() }];\n\
             for t in v {\n\
                 println(render(t));\n\
             }\n\
             println(v.len());\n\
         }\n",
        "`.into_iter()`",
    );
}

#[test]
fn passing_an_into_iter_element_by_value_is_accepted() {
    accepted(
        "into-iter-arg",
        "struct S { name: String }\n\
         fn render(s: own S) -> String { s.name }\n\
         fn main() {\n\
             let v = vec![S { name: \"a\".to_string() }];\n\
             for s in v.into_iter() {\n\
                 println(render(s));\n\
             }\n\
         }\n",
    );
}

#[test]
fn a_result_holding_a_handle_copies_by_counting() {
    // §6.1 (since 2026-10-09): a `Result` of handles and `Copy` parts is a
    // handle aggregate like an `Option`, so reusing it counts.
    accepted(
        "result-handle",
        "shared struct Node { v: i64 }\n\
         fn take(r: Result[Node, i64]) -> i64 { match r { Ok(n) => n.v, Err(e) => e } }\n\
         fn main() {\n\
             let r: Result[Node, i64] = Ok(Node { v: 1 });\n\
             let a = take(r);\n\
             println(a + take(r));\n\
         }\n",
    );
}

#[test]
fn an_option_or_tuple_of_handles_copies_by_counting() {
    accepted(
        "option-handle",
        "shared struct Node { v: i64, next: Option[Node] }\n\
         fn take(o: Option[Node]) -> i64 { match o { Some(n) => n.v, None => 0 } }\n\
         fn pair(p: (Node, i64)) -> i64 { p.1 }\n\
         fn main() {\n\
             let o: Option[Node] = Some(Node { v: 1, next: None });\n\
             let a = take(o);\n\
             let p = (Node { v: 2, next: None }, 3);\n\
             let b = pair(p);\n\
             println(a + take(o) + b + pair(p));\n\
         }\n",
    );
}

#[test]
fn binding_a_part_of_a_drop_or_shared_value_must_borrow() {
    // §3.7 / §4.6: a plain binding would move the part out of a value the
    // pattern may not take apart; `karac fix` writes `ref`.
    rejected_then_fixed_by(
        "drop-payload",
        "struct R { id: i64 }\n\
         enum T { Pair(String, i64), Leaf(R) }\n\
         impl Drop for T { fn drop(mut ref self) { println(\"dT\"); } }\n\
         fn main() {\n\
             let t = T.Pair(\"x\".to_string(), 3);\n\
             match t {\n\
                 T.Pair(s, n) => println(f\"{s} {n}\"),\n\
                 T.Leaf(r) => println(f\"{r.id}\"),\n\
             }\n\
         }\n",
        "which has a `Drop` body",
        "T.Pair(ref s, n)",
    );
}

#[test]
fn a_shared_scrutinee_binds_refs() {
    // §4.6: a `shared` scrutinee counts as a `ref` scrutinee. Reading a
    // part needs no `ref`; moving it is the error, and `.clone()` the fix.
    accepted(
        "shared-payload-read",
        "shared enum H { Z(Vec[String]), N }\n\
         fn main() {\n\
             let h = H.Z([\"a\".to_string()]);\n\
             match h { H.Z(v) => println(f\"{v.len()}\"), H.N => println(\"n\") }\n\
         }\n",
    );
    rejected_then_fixed(
        "shared-payload-moved",
        "shared enum E { S(String), Other }\n\
         fn get(e: E) -> String {\n\
             match e { S(s) => s, Other => \"other\".to_string() }\n\
         }\n\
         fn main() { println(get(E.S(\"x\".to_string()))); }\n",
        "cannot move 's'",
    );
}

#[test]
fn a_ref_binding_cannot_be_moved() {
    // The `ref` binding borrows; handing it to a by-value parameter is a
    // move out of a borrow, and `karac fix` clones it.
    rejected_then_fixed(
        "ref-binding-moved",
        "shared enum H { Z(Vec[String]), N }\n\
         fn eat(v: own Vec[String]) -> i64 { v.len() }\n\
         fn main() {\n\
             let h = H.Z([\"a\".to_string()]);\n\
             match h { H.Z(ref v) => println(f\"{eat(v)}\"), H.N => println(\"n\") }\n\
         }\n",
        "cannot move 'v'",
    );
}

#[test]
fn a_ref_binding_runs_and_formats_under_legacy() {
    // `ref name` is an ordinary binding to `build` and `run`, and
    // `karac fmt` keeps the `ref`.
    let (dir, path) = fixture(
        "ref-binding-legacy",
        "enum T { Pair(String, i64), E }\n\
         fn main() {\n\
             let t = T.Pair(\"x\".to_string(), 3);\n\
             match t {\n\
                 T.Pair(ref s, n) => println(f\"{s} {n}\"),\n\
                 T.E => println(\"e\"),\n\
             }\n\
         }\n",
    );
    let out = karac()
        .arg("run")
        .arg("--interp")
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "x 3\n");
    let out = karac().arg("fmt").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let formatted = std::fs::read_to_string(&path).unwrap();
    assert!(formatted.contains("T.Pair(ref s, n)"), "{formatted}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bindings_over_a_borrowed_receiver_or_view_borrow() {
    // §4.6: `match self` under `ref self` and a destructuring `let` of a
    // `ref` binding bind `ref`s; reading them moves nothing, even through a
    // type with a `Drop` body.
    accepted(
        "borrowed-receiver",
        "enum Es { A(String), B }\n\
         impl Drop for Es { fn drop(mut ref self) { println(\"dEs\"); } }\n\
         impl Es { fn len(ref self) -> i64 { match self { Es.A(s) => s.len(), Es.B => 0 } } }\n\
         struct P { a: String, b: String }\n\
         shared enum H { W(P), N }\n\
         fn main() {\n\
             let e = Es.A(\"x\".to_string());\n\
             let h = H.W(P { a: \"a\".to_string(), b: \"b\".to_string() });\n\
             match h { H.W(p) => { let P { a, b } = p; println(f\"{a}{b}\"); } H.N => {} }\n\
             println(e.len());\n\
         }\n",
    );
}

#[test]
fn copying_a_ref_binding_copies_the_reference() {
    // §1.1: `ref T` is Copy. `let u = x;` with `x` a `ref` binding gives a
    // second `ref`; moving `u` on into an owned place is still C3.
    accepted(
        "ref-copy",
        "shared enum Sh { S((String, i64)), N }\n\
         fn main() {\n\
             let keep = Sh.S((\"a\".to_string(), 7));\n\
             match keep { Sh.S(x) => { let u = x; println(f\"{u.0}{x.1}\"); } Sh.N => {} }\n\
         }\n",
    );
    rejected_then_fixed(
        "ref-copy-moved",
        "shared enum Sh { S(String), N }\n\
         fn eat(s: own String) -> i64 { s.len() }\n\
         fn main() {\n\
             let keep = Sh.S(\"a\".to_string());\n\
             match keep { Sh.S(x) => { let u = x; println(eat(u)); } Sh.N => {} }\n\
         }\n",
        "cannot move 'u'",
    );
}

#[test]
fn a_map_get_payload_is_a_reference() {
    // `Map.get` hands out `Option[ref V]`: moving the payload out is C3.
    rejected_then_fixed(
        "map-get-unwrap",
        "fn take(s: own String) -> i64 { s.len() }\n\
         fn main() {\n\
             let mut m: Map[i64, String] = Map.new();\n\
             m.insert(1, \"a\".to_string());\n\
             println(take(m.get(1).unwrap()));\n\
         }\n",
        "out of a map",
    );
    rejected_then_fixed(
        "map-get-arm",
        "fn pick(m: ref Map[i64, String]) -> String {\n\
             match m.get(1) { Some(v) => v, None => \"x\".to_string() }\n\
         }\n\
         fn main() {\n\
             let mut m: Map[i64, String] = Map.new();\n\
             m.insert(1, \"a\".to_string());\n\
             println(pick(m));\n\
         }\n",
        "cannot move 'v'",
    );
    // `let s = m.get(k).unwrap();` copies the reference; moving `s` is C3.
    rejected_then_fixed(
        "map-get-let",
        "fn take(s: own String) -> i64 { s.len() }\n\
         fn main() {\n\
             let mut m: Map[i64, String] = Map.new();\n\
             m.insert(1, \"a\".to_string());\n\
             let s = m.get(1).unwrap();\n\
             println(s.len());\n\
             println(take(s));\n\
         }\n",
        "cannot move 's'",
    );
    accepted(
        "map-get-read",
        "fn main() {\n\
             let mut m: Map[i64, String] = Map.new();\n\
             m.insert(1, \"a\".to_string());\n\
             let s = m.get(1).unwrap();\n\
             println(s.len() + m.get(1).unwrap().len());\n\
             if let Some(v) = m.get(1) { println(v); }\n\
         }\n",
    );
}

#[test]
fn a_for_over_a_temporary_collection_binds_refs() {
    rejected_then_fixed(
        "for-temp",
        "fn mk() -> Vec[String] { [\"a\".to_string()] }\n\
         fn take(s: own String) -> i64 { s.len() }\n\
         fn main() { for s in mk() { println(take(s)); } }\n",
        "cannot move 's'",
    );
    rejected_then_fixed(
        "for-map-pair",
        "fn take(s: own String) -> i64 { s.len() }\n\
         fn main() {\n\
             let mut m: Map[i64, String] = Map.new();\n\
             m.insert(1, \"a\".to_string());\n\
             for (k, v) in m { println(take(v) + k); }\n\
         }\n",
        "cannot move 'v'",
    );
    accepted(
        "for-into-iter",
        "fn mk() -> Vec[String] { [\"a\".to_string()] }\n\
         fn take(s: own String) -> i64 { s.len() }\n\
         fn main() { for s in mk().into_iter() { println(take(s)); } }\n",
    );
}

#[test]
fn a_field_read_through_a_ref_binding_or_a_handle_is_a_borrow() {
    // A `ref`-mode pattern binding is a borrow, so its fields are too.
    rejected_then_fixed(
        "ref-binding-field",
        "struct F { name: String, ps: Vec[i64] }\n\
         enum It { Fu(F), Other }\n\
         fn f(it: ref It) -> i64 {\n\
             match it { It.Fu(x) => { let ps = x.ps; ps.len() } It.Other => 0 }\n\
         }\n\
         fn main() { println(f(It.Other)); }\n",
        "out of a borrowed place",
    );
    // A field of a `shared` value is reached through a handle (§4.6).
    rejected_then_fixed(
        "shared-field",
        "shared struct B { mut name: String }\n\
         fn main() {\n\
             let b = B { name: \"n\".to_string() };\n\
             let s = b.name;\n\
             println(s);\n\
         }\n",
        "out of a borrowed place",
    );
    // A struct variant of a `shared` enum binds refs too.
    rejected_then_fixed(
        "shared-struct-variant",
        "shared enum H { Y { v: Vec[String] }, N }\n\
         fn eat(v: own Vec[String]) -> i64 { v.len() }\n\
         fn f(h: own H) -> i64 { match h { H.Y { v } => eat(v), H.N => 0 } }\n\
         fn main() { println(f(H.N)); }\n",
        "cannot move 'v'",
    );
    accepted(
        "shared-field-read",
        "shared struct B { mut name: String, n: i64 }\n\
         fn main() {\n\
             let b = B { name: \"n\".to_string(), n: 1 };\n\
             let k = b.n;\n\
             println(b.name.len() + k);\n\
         }\n",
    );
}

#[test]
fn a_by_value_binding_moves_its_part_out_of_an_owned_scrutinee() {
    // §4.6: over an owned scrutinee a plain binding moves the part out, so a
    // later use of the scrutinee is a use after move; `karac fix` adds `ref`.
    rejected_then_fixed_by(
        "pattern-move-match",
        "fn main() {\n\
             let o: Option[String] = Some(\"a\".to_string());\n\
             match o { Some(s) => println(s), None => {} }\n\
             println(o.is_some());\n\
         }\n",
        "moves its part out",
        "Some(ref s)",
    );
    rejected_then_fixed_by(
        "pattern-move-if-let",
        "fn main() {\n\
             let o: Option[String] = Some(\"a\".to_string());\n\
             if let Some(s) = o { println(s); }\n\
             println(o.is_some());\n\
         }\n",
        "moves its part out",
        "Some(ref s)",
    );
    // `Copy` parts copy, and nothing used afterwards is fine either way.
    accepted(
        "pattern-copy",
        "fn main() {\n\
             let n: Option[i64] = Some(1);\n\
             match n { Some(k) => println(k), None => {} }\n\
             println(n.is_some());\n\
             let o: Option[String] = Some(\"a\".to_string());\n\
             match o { Some(s) => println(s), None => {} }\n\
         }\n",
    );
}

#[test]
fn a_loop_must_reassign_a_moved_place_before_it_loops_back() {
    // §3.3: every path back to the loop head must assign the place again;
    // an assignment earlier in the body does not count.
    rejected_then_fixed(
        "loop-reassign-before",
        "fn take(s: own String) { println(s); }\n\
         fn main() {\n\
             let mut t = \"a\".to_string();\n\
             let mut i = 0;\n\
             while i < 2 { t = \"b\".to_string(); take(t); i = i + 1; }\n\
         }\n",
        "not assigned again",
    );
    accepted(
        "loop-reassign-after",
        "fn take(s: own String) { println(s); }\n\
         fn main() {\n\
             let mut t = \"a\".to_string();\n\
             let mut i = 0;\n\
             while i < 2 { take(t); t = \"b\".to_string(); i = i + 1; }\n\
             println(t);\n\
         }\n",
    );
}

#[test]
fn a_move_out_of_a_drop_type_is_an_error_no_attribute_lowers() {
    // §3.6 / §3.7: a `Drop` body anywhere on the path from the root rules the
    // move out, and `#[allow]` cannot lower a core rule.
    rejected_then_fixed(
        "drop-field-allow",
        "struct D { name: String, n: i64 }\n\
         impl Drop for D { fn drop(mut ref self) { println(\"dD\"); } }\n\
         fn main() {\n\
             let d = D { name: \"a\".to_string(), n: 1 };\n\
             #[allow(partial_move_of_drop_struct)]\n\
             let x = d.name;\n\
             println(x);\n\
         }\n",
        "which has a `Drop` body",
    );
    // The fixed form is `w.inner.s.clone()`, a chained field receiver, which
    // the v2 check accepts since B-2026-10-14-70.
    rejected_then_fixed(
        "drop-field-path",
        "struct P { s: String }\n\
         struct W { inner: P }\n\
         impl Drop for W { fn drop(mut ref self) { println(\"dW\"); } }\n\
         fn main() {\n\
             let w = W { inner: P { s: \"b\".to_string() } };\n\
             let s = w.inner.s;\n\
             println(s);\n\
         }\n",
        "out of `W`",
    );
    rejected_then_fixed_by(
        "drop-field-let-pattern",
        "struct D { name: String, n: i64 }\n\
         impl Drop for D { fn drop(mut ref self) { println(\"dD\"); } }\n\
         fn main() {\n\
             let d = D { name: \"a\".to_string(), n: 1 };\n\
             let D { name, n } = d;\n\
             println(name);\n\
             println(n);\n\
         }\n",
        "which has a `Drop` body",
        "name: ref name",
    );
    accepted(
        "drop-field-copy",
        "struct D { name: String, n: i64 }\n\
         impl Drop for D { fn drop(mut ref self) { println(\"dD\"); } }\n\
         fn main() {\n\
             let d = D { name: \"a\".to_string(), n: 1 };\n\
             let k = d.n;\n\
             let D { name: ref name, n } = d;\n\
             println(k);\n\
             println(name);\n\
         }\n",
    );
}

/// `check` must fail with `expect` in its output (no machine fix exists).
fn rejected(tag: &str, src: &str, expect: &str) {
    let (dir, path) = fixture(tag, src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{tag}: check must reject: {err}");
    assert!(err.contains(expect), "{tag}: expected `{expect}` in: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_reference_to_a_temporary_cannot_outlive_its_statement() {
    // §5.5, pin `err_ref_from_temp`: the result borrows both `ref` arguments
    // (§5.4), and one of them is a temporary.
    rejected(
        "ref-from-temp-arg",
        "fn longer(a: ref String, b: ref String) -> ref String { if a.len() > b.len() { a } else { b } }\n\
         fn main() {\n\
             let s = \"abc\".to_string();\n\
             let r = longer(s, \"de\".to_string());\n\
             println(r);\n\
         }\n",
        "§5.5",
    );
    // A `ref self` method's result borrows its receiver, here an `if`.
    rejected(
        "ref-from-temp-receiver",
        "fn mk(n: i64) -> Vec[i64] { return [n, n + 1] }\n\
         fn main() {\n\
             let c = true;\n\
             let d = (if c { mk(5) } else { mk(7) }).first();\n\
             match d { Some(x) => println(x), None => println(0) }\n\
         }\n",
        "§5.5",
    );
    accepted(
        "ref-from-named",
        "fn longer(a: ref String, b: ref String) -> ref String { if a.len() > b.len() { a } else { b } }\n\
         fn mk(n: i64) -> Vec[i64] { return [n, n + 1] }\n\
         fn main() {\n\
             let s = \"abc\".to_string();\n\
             let t = \"de\".to_string();\n\
             let r = longer(s, t);\n\
             println(r);\n\
             let v = mk(5);\n\
             let d = v.first();\n\
             match d { Some(x) => println(x), None => println(0) }\n\
             let n = mk(3).len();\n\
             println(n);\n\
         }\n",
    );
}

/// §5.5 again: a `ref` binding in a `let` pattern borrows from the
/// initializer, so over a temporary it would outlive the `;` that drops it.
#[test]
fn a_ref_pattern_binding_cannot_borrow_from_a_temporary() {
    rejected(
        "ref-pattern-temp-let-else",
        "#[derive(Clone)]\n\
         enum E { A(String), B }\n\
         fn main() {\n\
             let v: Vec[E] = [E.A(\"x\".to_string())];\n\
             let E.A(ref r) = v[0].clone() else { return };\n\
             println(r);\n\
         }\n",
        "`ref r` borrows from a temporary",
    );
    rejected(
        "ref-pattern-temp-let",
        "fn mk() -> (String, i64) { return (\"a\".to_string(), 1) }\n\
         fn main() {\n\
             let (ref s, n) = mk();\n\
             println(f\"{s} {n}\");\n\
         }\n",
        "`ref s` borrows from a temporary",
    );
    accepted(
        "ref-pattern-named",
        "#[derive(Clone)]\n\
         enum E { A(String), B }\n\
         fn main() {\n\
             let v: Vec[E] = [E.A(\"x\".to_string())];\n\
             let e = v[0].clone();\n\
             let E.A(ref r) = e else { return };\n\
             println(r);\n\
             let E.A(q) = v[0].clone() else { return };\n\
             println(q);\n\
         }\n",
    );
}

/// §9.6: a closure whose result is a non-`Copy` capture moves it out, so
/// it runs once and cannot be handed out as a repeatable `Fn`.
#[test]
fn a_closure_returning_its_capture_runs_once() {
    rejected(
        "closure-returns-capture",
        "fn mk(x: own String) -> Fn() -> String { || x }\n\
         fn main() { let g = mk(\"E\".to_string()); println(g()); println(g()); }\n",
        "x",
    );
    accepted(
        "closure-returns-capture-once",
        "fn dup(x: own String) -> String { let g = || x; g() }\n\
         fn main() { println(dup(\"P\".to_string())); }\n",
    );
}

/// `check` under D5 (on by default; `KARAC_D5=1` pins it): `Ok(())` when it
/// accepts, else the stderr.
fn check_d5(tag: &str, src: &str) -> Result<(), String> {
    let (dir, path) = fixture(tag, src);
    let out = karac()
        .env("KARAC_D5", "1")
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

/// D5 (core-semantics §4.1): a bare parameter borrows and `own T` owns, so
/// a caller keeps what it passes to a bare parameter, and the callee cannot
/// give that parameter away.
#[test]
fn under_d5_a_bare_parameter_borrows() {
    let twice = "fn show(s: String) -> i64 { s.len() }\n\
                 fn main() { let s = \"ab\".to_string(); println(show(s)); println(show(s)); }\n";
    check_d5("d5-caller-keeps", twice).unwrap();
    // With D5 off (`KARAC_D5=0`) the first call moves `s`.
    {
        let (dir, path) = fixture("d5-off-caller-moves", twice);
        let out = karac()
            .env("KARAC_D5", "0")
            .arg("check")
            .arg(&path)
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success() && err.contains("moved"),
            "d5-off: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    let err = check_d5(
        "d5-callee-gives-away",
        "fn keep(s: String) -> String { s }\n\
         fn main() { println(keep(\"ab\".to_string())); }\n",
    )
    .unwrap_err();
    assert!(
        err.contains("cannot give away parameter `s`") && err.contains("`s: own String`"),
        "d5-callee-gives-away: {err}"
    );
    // A `ref self` method cannot hand back `self` as owned either.
    rejected(
        "ref-self-gives-away",
        "struct W { s: String }\n\
         impl W { fn me(ref self) -> W { self } }\n\
         fn main() { let w = W { s: \"a\".to_string() }; println(w.me().s); println(w.s); }\n",
        "cannot give away the borrowed receiver `self`",
    );
    // `karac fix` writes `own` on the parameter, and on a receiver.
    for (tag, src, edit) in [
        (
            "d5-fix-own",
            "struct P { s: String }\n\
             fn keep(p: P) -> P { p }\n\
             fn main() { let p = P { s: \"a\".to_string() }; println(keep(p).s); }\n",
            "fn keep(p: own P)",
        ),
        (
            "d5-fix-own-self",
            "struct W { s: String }\n\
             impl W { fn me(self) -> W { self } }\n\
             fn main() { let w = W { s: \"a\".to_string() }; println(w.me().s); }\n",
            "fn me(own self)",
        ),
    ] {
        let (dir, path) = fixture(tag, src);
        let d5 = || {
            let mut c = karac();
            c.env("KARAC_D5", "1");
            c
        };
        assert!(
            !d5()
                .arg("check")
                .arg(&path)
                .output()
                .unwrap()
                .status
                .success(),
            "{tag}"
        );
        d5().arg("fix").arg(&path).output().unwrap();
        let fixed = std::fs::read_to_string(&path).unwrap();
        let out = d5().arg("check").arg(&path).output().unwrap();
        assert!(
            out.status.success() && fixed.contains(edit),
            "{tag}: fix must write `{edit}` and pass; source now:\n{fixed}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    check_d5(
        "d5-own-gives-away",
        "fn keep(s: own String) -> String { s }\n\
         fn main() { let s = \"ab\".to_string(); println(keep(s)); }\n",
    )
    .unwrap();
    // A bare `self` borrows too, and `own self` consumes.
    check_d5(
        "d5-self",
        "struct W { s: String }\n\
         impl W {\n\
             fn n(self) -> i64 { self.s.len() }\n\
             fn take(own self) -> String { self.s }\n\
         }\n\
         fn main() { let w = W { s: \"ab\".to_string() }; println(w.n()); println(w.n()); println(w.take()); }\n",
    )
    .unwrap();
    // A task that hands a loop local to a bare parameter only borrows it,
    // and the local dies before the group, so the task takes it by move.
    check_d5(
        "d5-task-borrow",
        "struct R { id: i64, tag: String }\n\
         fn show(r: R) -> i64 { r.id }\n\
         fn main() {\n\
             let mut g = TaskGroup.new();\n\
             let mut hs: Vec[TaskHandle[i64]] = Vec.new();\n\
             let mut i = 0;\n\
             while i < 3 { let r = R { id: i, tag: \"t\".to_string() }; hs.push(g.spawn(|| show(r))); i = i + 1; }\n\
             for h in hs.into_iter() { println(h.join()); }\n\
         }\n",
    )
    .unwrap();
}

/// "Views at lookup" (Gowtham, 2026-10-11): a String-keyed map or set is
/// looked up by a `Str`, a `Vec`-keyed one by a `Slice`, with no copy.
#[test]
fn a_string_keyed_lookup_takes_a_str() {
    accepted(
        "lookup-by-str",
        "fn main() {\n\
             let s = \"abcd\".to_string();\n\
             let mut m: Map[String, i64] = Map.new();\n\
             m.insert(s[0..2].to_string(), 1);\n\
             let mut t: SortedMap[String, i64] = SortedMap.new();\n\
             t.insert(\"cd\".to_string(), 2);\n\
             let mut u: Set[String] = Set.new();\n\
             u.insert(\"ab\".to_string());\n\
             let k = s[0..2];\n\
             println(m.get(k) ?? 0);\n\
             println(m.contains_key(s[2..4]));\n\
             println(t.get(s[2..4]) ?? 0);\n\
             println(u.contains(k));\n\
             println(u.remove(s[0..2]));\n\
             println(m.remove(k) ?? 0);\n\
             println(m.get(s) ?? 0);\n\
         }\n",
    );
    // Storing still takes an owned `String`.
    rejected_then_fixed_by(
        "insert-by-str",
        "fn main() {\n\
             let s = \"abcd\".to_string();\n\
             let mut m: Map[String, i64] = Map.new();\n\
             m.insert(s[0..2], 1);\n\
             println(m.len());\n\
         }\n",
        "found 'Str'",
        "m.insert(s[0..2].to_string(), 1);",
    );
}

/// C3: the value of a `match` or `if let` is built from its arms' tails,
/// which are block tail values (§3.1), so an element taken there moves out
/// of its collection even when the result is only borrowed afterwards.
#[test]
fn an_element_in_an_arm_tail_moves_out_of_its_collection() {
    rejected_then_fixed_by(
        "arm-tail-index-match",
        "shared enum M { My(Vec[String]), N }\n\
         fn main() {\n\
             let t = M.My([\"a\".to_string(), \"b\".to_string()]);\n\
             println(match t { M.My(x) => x[1], _ => \"n\" });\n\
         }\n",
        "out of a collection element",
        "x[1].clone()",
    );
    rejected_then_fixed_by(
        "arm-tail-index-if-let",
        "shared enum M { My(Vec[String]), N }\n\
         fn main() {\n\
             let t = M.My([\"a\".to_string(), \"b\".to_string()]);\n\
             println(if let M.My(x) = t { x[1] } else { \"n\" });\n\
         }\n",
        "out of a collection element",
        "x[1].clone()",
    );
    // An arm that only reads the element takes nothing out.
    accepted(
        "arm-tail-index-read",
        "fn main() {\n\
             let v: Vec[String] = [\"a\".to_string()];\n\
             let c = true;\n\
             println(if c { v[0].len() } else { 0 });\n\
             println(match c { true => v[0].len(), false => 1 });\n\
         }\n",
    );
}

#[test]
fn a_by_value_receiver_moves_and_cannot_come_out_of_a_borrow() {
    // `unwrap` takes `self` by value, so it moves its receiver: a field
    // projection is named and cloned as itself.
    rejected_then_fixed_by(
        "receiver-unwrap-moves",
        "struct H { o: Option[String], n: i64 }\n\
         fn main() {\n\
             let h = H { o: Some(\"b\".to_string()), n: 1 };\n\
             let b = h.o.unwrap();\n\
             println(b);\n\
             println(h.o.is_some());\n\
         }\n",
        "value 'h.o' moved here",
        "h.o.clone().unwrap()",
    );
    // §3.7: a by-value receiver may not come out of a `ref` parameter or out
    // of a collection element.
    rejected_then_fixed_by(
        "receiver-out-of-ref",
        "struct H { o: Option[String], n: i64 }\n\
         fn f(r: ref H) -> String { r.o.unwrap() }\n\
         fn main() {\n\
             let h = H { o: Some(\"b\".to_string()), n: 1 };\n\
             println(f(h));\n\
         }\n",
        "out of a borrowed place",
        "r.o.clone().unwrap()",
    );
    rejected_then_fixed_by(
        "receiver-out-of-element",
        "struct H { o: Option[String], n: i64 }\n\
         fn main() {\n\
             let g = vec![H { o: Some(\"b\".to_string()), n: 1 }];\n\
             println(g[0].o.unwrap());\n\
         }\n",
        "out of a collection element",
        "g[0].o.clone().unwrap()",
    );
    accepted(
        "receiver-borrowing-methods",
        "struct H { o: Option[String], n: i64 }\n\
         fn f(r: ref H) -> bool { r.o.is_some() }\n\
         fn main() {\n\
             let h = H { o: Some(\"b\".to_string()), n: 1 };\n\
             println(f(h));\n\
             println(h.o.is_some());\n\
             let n: Option[i64] = Some(3);\n\
             println(n.unwrap());\n\
             println(n.unwrap());\n\
         }\n",
    );
}

#[test]
fn destructuring_a_borrowed_place_or_iterating_refs_binds_references() {
    // §4.6: a `let` destructure of a view binds `ref`s, so returning a part as
    // an owned value is a move out of the borrow.
    rejected_then_fixed(
        "let-destructure-view",
        "struct P { a: String, n: i64 }\n\
         fn main() {\n\
             let ps = vec![P { a: \"x\".to_string(), n: 1 }];\n\
             let mut out: Vec[String] = Vec.new();\n\
             for p in ps { let P { a, n } = p; out.push(a); }\n\
             println(out.len());\n\
         }\n",
        "it borrows its value",
    );
    // `c.iter()` hands out references, so its `for` binds them.
    rejected_then_fixed(
        "for-over-iter",
        "fn take(x: own Vec[i64]) -> i64 { return x.len() }\n\
         fn main() {\n\
             let v: Vec[(Vec[i64], i64)] = [([1], 1)];\n\
             for pair in v.iter() { match pair { (a, j) => println(take(a)) } }\n\
         }\n",
        "a borrowed value cannot be moved out of what it borrows",
    );
    // A `Map.get` payload bound in one arm, an owned value in the other.
    rejected_then_fixed(
        "mixed-ref-owned-arms",
        "fn main() {\n\
             let mut m: Map[i64, String] = Map.new();\n\
             m.insert(7, \"abc\".to_string());\n\
             let g = m.get(7);\n\
             let s = match g { Some(x) => x, None => \"n\".to_string() };\n\
             println(s.len());\n\
         }\n",
        "it borrows its value",
    );
}

#[test]
fn a_generic_move_out_of_a_borrow_is_checked_per_instantiation() {
    // §3.7 on a monomorphised instance, as §5.8 does for views: the body
    // moves a `T` out of `ref self` only when `T` is not `Copy`, so the
    // error names the call that instantiates it with `String`.
    rejected(
        "generic-ref-move",
        "struct Bx[T] { v: T }\n\
         impl[T] Bx[T] { fn get(ref self) -> T { self.v } }\n\
         fn main() {\n\
             let a = Bx { v: \"x\".to_string() };\n\
             println(a.get());\n\
         }\n",
        "instantiates `T` with `String`",
    );
    accepted(
        "generic-ref-move-copy",
        "struct Bx[T] { v: T }\n\
         impl[T] Bx[T] { fn get(ref self) -> T { self.v } }\n\
         fn main() {\n\
             let b = Bx { v: 3 };\n\
             println(b.get());\n\
             let c = Bx { v: (1, true) };\n\
             println(c.get().0);\n\
         }\n",
    );
}

#[test]
fn a_function_parameter_is_non_escaping_unless_declared_escaping() {
    // §9.3, pin `err_store_nonescaping_param`.
    rejected(
        "store-nonescaping-param",
        "struct Holder { f: Fn() -> i64 }\n\
         fn keep(f: Fn() -> i64) -> Holder { Holder { f: f } }\n\
         fn main() {\n\
             let h = keep(|| 5);\n\
             println(f\"{(h.f)()}\");\n\
         }\n",
        "parameter `f` is a non-escaping function value and cannot be stored",
    );
    rejected(
        "return-nonescaping-param",
        "fn pass(f: Fn() -> i64) -> Fn() -> i64 { f }\n\
         fn main() { println(pass(|| 5)()); }\n",
        "cannot be returned",
    );
    accepted(
        "store-escaping-param",
        "struct Holder { f: Fn() -> i64 }\n\
         fn keep(f: escaping Fn() -> i64) -> Holder { Holder { f: f } }\n\
         fn twice(g: Fn(i64) -> i64, x: i64) -> i64 { g(g(x)) }\n\
         fn apply(g: Fn(i64) -> i64) -> i64 { twice(g, 1) }\n\
         fn main() {\n\
             let h = keep(|| 5);\n\
             println(f\"{(h.f)()}\");\n\
             println(apply(|x| x + 1));\n\
         }\n",
    );
    // `escaping` belongs to a parameter only.
    rejected(
        "escaping-on-a-field",
        "struct H { f: escaping Fn() -> i64 }\nfn main() {}\n",
        "only accepted on a parameter's type",
    );
}

#[test]
fn an_escaping_closure_captures_by_move() {
    // §9.3, pin `err_escaping_capture_reused`: stored in a struct field, the
    // closure takes `s`, so the later `println(s)` is a use after move.
    rejected(
        "escaping-capture-reused",
        "struct Holder { f: Fn() -> i64 }\n\
         fn main() {\n\
             let s = \"abc\".to_string();\n\
             let h = Holder { f: || s.len() };\n\
             println(s);\n\
             println(f\"{(h.f)()}\");\n\
         }\n",
        "moved into an escaping closure",
    );
    rejected(
        "escaping-capture-let-bound",
        "fn main() {\n\
             let s = \"abc\".to_string();\n\
             let c = || s.len();\n\
             let mut v: Vec[Fn() -> i64] = Vec.new();\n\
             v.push(c);\n\
             println(s);\n\
         }\n",
        "moved into an escaping closure",
    );
    // Passed down to a non-escaping parameter, the closure borrows.
    accepted(
        "nonescaping-closure-borrows",
        "fn run(f: Fn() -> i64) -> i64 { f() }\n\
         fn main() {\n\
             let s = \"abc\".to_string();\n\
             println(run(|| s.len()));\n\
             println(s);\n\
         }\n",
    );
}

#[test]
fn the_formatter_keeps_escaping_and_a_called_field_in_parentheses() {
    // `escaping` lives beside the tree (`Program::escaping_fn_types`), and
    // `(h.f)()` without its parentheses reparses as the method call `h.f()`.
    let src = "struct Holder {\n    f: Fn() -> i64,\n}\n\n\
               fn keep(f: escaping Fn() -> i64, g: Fn(i64) -> i64) -> Holder {\n    \
               Holder {\n        f: f,\n    }\n}\n\n\
               fn main() {\n    let h = keep(|| 5, |x| x);\n    println((h.f)());\n}\n";
    let parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let formatted = karac::formatter::format_program(&parsed.program);
    assert_eq!(formatted, src, "round-trip mismatch:\n{formatted}");
}

#[test]
fn the_formatter_keeps_ref_and_mut_ref_patterns() {
    // Both live beside the tree (`Program::ref_binding_spans`,
    // `Program::mut_ref_binding_spans`).
    let src = "fn main() {\n    let mut t = (1, 2);\n    \
               let (mut ref a, ref b) = t;\n    a += b;\n    println(t.0);\n}\n";
    let parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let formatted = karac::formatter::format_program(&parsed.program);
    assert_eq!(formatted, src, "round-trip mismatch:\n{formatted}");
}

#[test]
fn storing_a_borrow_into_a_set_or_map_is_an_error() {
    // §3.7: a bare `for` element borrows, and `insert` moves its argument.
    rejected(
        "set-insert-for-element",
        "fn main() {\n\
             let ps: Vec[String] = [\"a\".to_string()];\n\
             let mut set: Set[String] = Set.new();\n\
             for p in ps { set.insert(p); }\n\
             println(set.len());\n\
         }\n",
        "cannot move 'p'",
    );
}

#[test]
fn a_task_group_borrows_what_its_tasks_capture() {
    // §9.5, pins `ok_taskgroup_borrows`, `err_taskgroup_write_while_borrowed`,
    // `err_taskgroup_origin_declared_after`.
    let total = "fn total(v: ref Vec[i64]) -> i64 { v.len() }\n";
    accepted(
        "taskgroup-borrows",
        &format!(
            "{total}fn main() {{\n\
                 let data: Vec[i64] = [1, 2, 3];\n\
                 let mut g = TaskGroup.new();\n\
                 let h = g.spawn(|| total(data));\n\
                 let k = g.spawn(|| total(data));\n\
                 println(h.join() + k.join());\n\
                 println(data.len());\n\
             }}\n"
        ),
    );
    rejected(
        "taskgroup-write-while-borrowed",
        &format!(
            "{total}fn main() {{\n\
                 let mut data: Vec[i64] = [1, 2, 3];\n\
                 let mut g = TaskGroup.new();\n\
                 let h = g.spawn(|| total(data));\n\
                 data.push(4);\n\
                 println(h.join());\n\
             }}\n"
        ),
        "`data` is written while a task of `g` borrows it",
    );
    // §9.1 (10-10): a place that drops before the group is moved into the
    // task, so the program is accepted until the place is used again.
    accepted(
        "taskgroup-origin-declared-after",
        &format!(
            "{total}fn main() {{\n\
                 let mut g = TaskGroup.new();\n\
                 let data: Vec[i64] = [1, 2, 3];\n\
                 let h = g.spawn(|| total(data));\n\
                 println(h.join());\n\
             }}\n"
        ),
    );
    rejected(
        "taskgroup-origin-declared-after-used",
        &format!(
            "{total}fn main() {{\n\
                 let mut g = TaskGroup.new();\n\
                 let data: Vec[i64] = [1, 2, 3];\n\
                 let h = g.spawn(|| total(data));\n\
                 println(data.len());\n\
                 println(h.join());\n\
             }}\n"
        ),
        "value 'data' was moved into a task of `g`",
    );
    accepted(
        "taskgroup-loop-body-locals",
        "fn work(s: ref String) -> i64 { s.len() }\n\
         fn main() {\n\
             let mut g = TaskGroup.new();\n\
             let mut hs: Vec[TaskHandle[i64]] = Vec.new();\n\
             let mut i = 0;\n\
             while i < 3 {\n\
                 let name = f\"n{i}\";\n\
                 let c = i;\n\
                 hs.push(g.spawn(|| work(name) + c));\n\
                 i = i + 1;\n\
             }\n\
             for h in hs.into_iter() { println(h.join()); }\n\
         }\n",
    );
    rejected(
        "taskgroup-copy-mut-capture-dies-first",
        "fn main() {\n\
             let mut g = TaskGroup.new();\n\
             let mut n = 1;\n\
             let h = g.spawn(|| { n = n + 1; n });\n\
             println(n);\n\
             println(h.join());\n\
         }\n",
        "`n` is used after a task of `g` captured it by `mut ref`",
    );
    rejected(
        "taskgroup-mut-capture-in-loop",
        "fn main() {\n\
             let mut b: Vec[i64] = Vec.new();\n\
             let mut g = TaskGroup.new();\n\
             let mut k = 0;\n\
             while k < 2 { let _ = g.spawn(|| { b.push(k); 0 }); k = k + 1; }\n\
         }\n",
        "captures `b` by `mut ref`",
    );
}

#[test]
fn an_allow_attribute_does_not_silence_a_core_rule() {
    // §3.6/§3.7: moving a field out of a Drop type was a lint; it is now a
    // core rule, so an `#[allow]` written for the lint is reported as having
    // no effect rather than suppressing the error.
    let drop_w = "struct R { name: String }\n\
                  struct W { r: R, n: i64 }\n\
                  impl Drop for W { fn drop(mut ref self) { println(\"drop\"); } }\n";
    rejected(
        "allow-field-move",
        &format!(
            "{drop_w}#[allow(partial_move_of_drop_struct)]\n\
             fn take(w: own W) -> R {{ return w.r; }}\n\
             fn main() {{ let r = take(W {{ r: R {{ name: \"a\" }}, n: 1 }}); println(r.name); }}\n"
        ),
        "`#[allow(partial_move_of_drop_struct)]` here has no effect any more",
    );
    rejected(
        "allow-pattern-move",
        &format!(
            "{drop_w}#[allow(partial_move_of_drop_struct)]\n\
             fn take(w: own W) -> R {{ let W {{ r, n }} = w; r }}\n\
             fn main() {{ let r = take(W {{ r: R {{ name: \"a\" }}, n: 1 }}); println(r.name); }}\n"
        ),
        "`#[allow(partial_move_of_drop_struct)]` here has no effect any more",
    );
}

#[test]
fn a_shared_ref_place_is_read_only() {
    // §5.9: no write through a `ref` parameter, a bare `for` item, a `ref`
    // pattern binding, or `ref self`.
    rejected(
        "write-through-ref-param",
        "fn add(v: ref Vec[i64]) { v.push(1); }\n\
         fn main() { let mut v: Vec[i64] = Vec.new(); add(v); println(v.len()); }\n",
        "`v` is a `ref` parameter",
    );
    rejected(
        "write-through-for-item",
        "fn main() {\n\
             let mut grid: Vec[Vec[i64]] = [Vec.new()];\n\
             for row in grid { row.push(9); }\n\
             println(grid[0].len());\n\
         }\n",
        "`row` is a `ref` to an element",
    );
    rejected(
        "write-through-ref-self",
        "struct C { n: i64 }\n\
         impl C { fn bump(ref self) { self.n = self.n + 1; } }\n\
         fn main() { let c = C { n: 0 }; c.bump(); println(c.n); }\n",
        "the receiver is `ref self`",
    );
    // `karac fix` makes each of those writable at the borrow's declaration.
    rejected_then_fixed_in(
        "fix-ref-param",
        "fn add(v: ref Vec[i64]) { v.push(1); v.push(2); }\n\
         fn main() { let mut v: Vec[i64] = Vec.new(); add(v); println(v.len()); }\n",
        "`v` is a `ref` parameter",
        "fn add(v: mut ref Vec[i64])",
        2,
    );
    rejected_then_fixed_by(
        "fix-for-item",
        "fn main() {\n\
             let mut grid: Vec[Vec[i64]] = [Vec.new()];\n\
             for row in grid { row.push(9); row.push(8); }\n\
             println(grid[0].len());\n\
         }\n",
        "`row` is a `ref` to an element",
        "for row in grid.iter_mut()",
    );
    rejected_then_fixed_by(
        "fix-ref-self",
        "struct C { n: i64 }\n\
         impl C { fn bump(ref self) { self.n = self.n + 1; } }\n\
         fn main() { let mut c = C { n: 0 }; c.bump(); println(c.n); }\n",
        "the receiver is `ref self`",
        "fn bump(mut ref self)",
    );
    // `iter_mut`, `mut ref` parameters, and a `ref self` method of a stdlib
    // type (`Arena.push`) all stay legal; so does a `mut` field of a
    // `shared` value reached through a `ref` (§6.2).
    accepted(
        "write-through-ref-legal",
        "shared struct Counter { mut hits: i64 }\n\
         fn bump(c: ref Counter) { c.hits = c.hits + 1; }\n\
         fn add(v: mut ref Vec[i64]) { v.push(1); }\n\
         fn grow(a: ref Arena[i64]) -> i64 { let r = a.push(3); a.get(r) }\n\
         fn main() {\n\
             let c = Counter { hits: 0 };\n\
             bump(c);\n\
             let mut v: Vec[i64] = Vec.new();\n\
             add(mut v);\n\
             let mut grid: Vec[Vec[i64]] = [Vec.new()];\n\
             for row in grid.iter_mut() { row.push(9); }\n\
             let a: Arena[i64] = Arena.new();\n\
             println(f\"{c.hits} {v.len()} {grid[0].len()} {grow(a)}\");\n\
         }\n",
    );
}

/// §4.6: `mut ref name` borrows a part of a mutable owned scrutinee mutably,
/// and moves nothing; neither does a dotted unit variant (`Slot.Empty`).
#[test]
fn a_mut_ref_pattern_writes_through_its_scrutinee() {
    let src = "enum Slot { Full(Vec[i64]), Empty }\n\
               struct Pair { a: Vec[i64], b: i64 }\n\
               fn main() {\n\
                   let mut s = Slot.Full([1]);\n\
                   match s {\n\
                       Slot.Full(mut ref v) => v.push(2),\n\
                       Slot.Empty => {}\n\
                   }\n\
                   if let Slot.Full(mut ref v) = s { v.push(3); }\n\
                   let mut p = Pair { a: [], b: 1 };\n\
                   let Pair { a: mut ref xs, .. } = p;\n\
                   xs.push(7);\n\
                   match s {\n\
                       Slot.Full(ref v) => println(f\"{v.len()} {p.a.len()} {p.b}\"),\n\
                       Slot.Empty => println(\"empty\"),\n\
                   }\n\
               }\n";
    accepted("mut-ref-pattern", src);
    // The legacy backends would bind a copy, so they refuse it.
    let (dir, path) = fixture("mut-ref-pattern-run", src);
    let out = karac()
        .args(["run", "--interp"])
        .arg(&path)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "legacy run must refuse: {err}");
    assert!(err.contains("not supported by the legacy"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
    rejected_then_fixed_by(
        "mut-ref-pattern-immutable",
        "enum Slot { Full(Vec[i64]), Empty }\n\
         fn main() {\n\
             let s = Slot.Full([1]);\n\
             match s { Slot.Full(mut ref v) => v.push(2), Slot.Empty => {} }\n\
         }\n",
        "cannot bind `mut ref v` into `s`",
        "let mut s",
    );
    rejected_then_fixed_in(
        "mut-ref-pattern-through-ref",
        "enum Slot { Full(Vec[i64]), Empty }\n\
         fn grow(s: ref Slot) {\n\
             match s { Slot.Full(mut ref v) => v.push(2), Slot.Empty => {} }\n\
         }\n\
         fn main() { let mut s = Slot.Full([1]); grow(s); }\n",
        "`s` is a `ref` parameter, so the `mut ref v` binding cannot modify it",
        "s: mut ref Slot",
        2,
    );
    rejected_then_fixed_by(
        "ref-pattern-written",
        "enum Slot { Full(Vec[i64]), Empty }\n\
         fn main() {\n\
             let mut s = Slot.Full([1]);\n\
             match s { Slot.Full(ref v) => v.push(2), Slot.Empty => {} }\n\
             match s { Slot.Full(ref v) => println(v.len()), Slot.Empty => {} }\n\
         }\n",
        "`v` is bound by `ref`",
        "Slot.Full(mut ref v)",
    );
}

/// One `karac fix` pass repairs every reused move, and two diagnostics that
/// ask for the same edit apply it once.
#[test]
fn fix_clones_every_reused_move_once() {
    rejected_then_fixed_by(
        "uam-every-site",
        "fn take(s: own String) -> i64 { s.len() }\n\
         fn main() {\n\
             let k = \"a\".to_string();\n\
             let a = take(k);\n\
             let b = take(k);\n\
             let c = take(k);\n\
             println(f\"{a} {b} {c} {k}\");\n\
         }\n",
        "value 'k' moved here",
        "let c = take(k.clone());",
    );
    rejected_then_fixed_by(
        "uam-and-borrow-move-same-site",
        "enum Cmd { Delete(String), Clear }\n\
         fn delete(k: own String) -> bool { k.len() > 0 }\n\
         fn main() {\n\
             let cmds: Vec[Cmd] = [Cmd.Delete(\"a\".to_string()), Cmd.Clear];\n\
             for cmd in cmds {\n\
                 match cmd {\n\
                     Cmd.Delete(k) => { let removed = delete(k); println(f\"del {k} {removed}\"); }\n\
                     Cmd.Clear => println(\"clear\"),\n\
                 }\n\
             }\n\
         }\n",
        "cannot move 'k'",
        "delete(k.clone());",
    );
}

/// §5.1: a `ref` local may borrow any named place or projection of one, and
/// a move out of a borrowed place whose type has no `.clone()` is fixed by
/// borrowing it.
#[test]
fn a_ref_local_borrows_a_projection() {
    accepted(
        "ref-projection",
        "struct Stats { hits: i64, names: Vec[String] }\n\
         struct Cache { stats: Stats, n: i64 }\n\
         fn main() {\n\
             let cache = Cache { stats: Stats { hits: 2, names: [] }, n: 1 };\n\
             let s = ref cache.stats;\n\
             println(f\"{s.hits} {cache.n}\");\n\
         }\n",
    );
    rejected_then_fixed_by(
        "move-out-of-ref-no-clone",
        "struct Inner { n: i64 }\n\
         struct S { r: Inner, m: i64 }\n\
         fn show(s: ref S) -> i64 {\n\
             let m = s.r;\n\
             m.n + s.m\n\
         }\n\
         fn main() { let s = S { r: Inner { n: 1 }, m: 2 }; println(show(s)); }\n",
        "cannot move a non-`Copy` value out of a borrowed place",
        "let m = ref s.r;",
    );
}

/// §4.6: a bare `for` element is a `ref`; moving it out is fixed by
/// iterating the owned collection with `.into_iter()`, when the element type
/// has no `.clone()` (with one, `.clone()` stays the fix: it is always valid).
#[test]
fn a_for_element_moves_out_by_into_iter() {
    rejected_then_fixed_by(
        "for-elem-into-iter",
        "struct Job { id: i64 }\n\
         fn run(j: own Job) { println(j.id) }\n\
         fn main() {\n\
             let jobs = vec![Job { id: 1 }, Job { id: 2 }];\n\
             for j in jobs {\n\
                 run(j);\n\
             }\n\
         }\n",
        "cannot move 'j'",
        "for j in jobs.into_iter() {",
    );
    rejected_then_fixed_by(
        "for-elem-drop-into-iter",
        "struct Res { id: i64 }\n\
         impl Drop for Res { fn drop(mut ref self) { println(self.id) } }\n\
         fn take(r: own Res) { println(r.id) }\n\
         fn main() {\n\
             let rs = vec![Res { id: 1 }, Res { id: 2 }];\n\
             let mut kept: Vec[Res] = Vec.new();\n\
             for r in rs {\n\
                 if r.id == 1 { take(r); } else { kept.push(r); }\n\
             }\n\
             println(kept.len());\n\
         }\n",
        "cannot move",
        "for r in rs.into_iter() {",
    );
    // A collection the function only borrows cannot be consumed: no fix.
    let (dir, path) = fixture(
        "for-elem-borrowed",
        "struct Job { id: i64 }\n\
         fn run(j: own Job) { println(j.id) }\n\
         fn all(jobs: ref Vec[Job]) {\n\
             for j in jobs {\n\
                 run(j);\n\
             }\n\
         }\n\
         fn main() { all(vec![Job { id: 1 }]); }\n",
    );
    karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        !fixed.contains("into_iter"),
        "a borrowed collection must not be consumed: {fixed}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// v2 core: reusing a moved value of a local type whose fields are all
/// `Copy` (and which has no `.clone()` and no `Drop` body) is fixed by
/// deriving `Copy` on the type.
#[test]
fn a_reused_all_copy_value_is_fixed_by_derive_copy() {
    rejected_then_fixed_by(
        "derive-copy-struct",
        "pub struct P { x: i64, y: i64 }\n\
         fn take(p: own P) -> i64 { p.x + p.y }\n\
         fn main() {\n\
             let p = P { x: 1, y: 2 };\n\
             println(take(p));\n\
             println(take(p));\n\
         }\n",
        "moved here, used again",
        "#[derive(Copy)]\npub struct P",
    );
    rejected_then_fixed_by(
        "derive-copy-enum",
        "enum Dir { North, South(i64) }\n\
         fn show(d: own Dir) -> i64 { match d { Dir.North => 0, Dir.South(n) => n } }\n\
         fn main() {\n\
             let d = Dir.South(3);\n\
             println(show(d));\n\
             println(show(d));\n\
         }\n",
        "moved here, used again",
        "#[derive(Copy)]\nenum Dir",
    );
    // A heap field or a `Drop` body rules it out: no derive is written.
    for (tag, src) in [
        (
            "derive-copy-heap-field",
            "struct N { s: String }\n\
             fn take(n: own N) {}\n\
             fn main() { let n = N { s: \"a\" }; take(n); take(n); }\n",
        ),
        (
            "derive-copy-drop",
            "struct D { x: i64 }\n\
             impl Drop for D { fn drop(mut ref self) { println(self.x) } }\n\
             fn take(d: own D) {}\n\
             fn main() { let d = D { x: 1 }; take(d); take(d); }\n",
        ),
    ] {
        let (dir, path) = fixture(tag, src);
        karac().arg("fix").arg(&path).output().unwrap();
        let fixed = std::fs::read_to_string(&path).unwrap();
        assert!(!fixed.contains("derive(Copy)"), "{tag}: {fixed}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// v2 core: an enum can carry another enum as its payload; the one-level
/// limit was the legacy backend's.
#[test]
fn an_enum_payload_may_be_an_enum() {
    accepted(
        "nested-enum-payload",
        "enum Command { Add(i64), Clear }\n\
         enum Step { Do(Command), Skip }\n\
         fn main() {\n\
             let s = Step.Do(Command.Add(3));\n\
             match s {\n\
                 Step.Do(Command.Add(n)) => println(n),\n\
                 Step.Do(Command.Clear) => println(0),\n\
                 Step.Skip => println(-1),\n\
             }\n\
         }\n",
    );
}

/// v2 core: `Map.get` returns `Option[ref V]`, so `*c` reads through the
/// payload binding. A format argument only reads, so `*c` of a non-`Copy`
/// value is read in place too, as the MIR backends run it.
#[test]
fn deref_reads_through_a_map_payload() {
    accepted(
        "map-get-deref",
        "fn main() {\n\
             let mut m: Map[String, i64] = Map.new();\n\
             m.insert(\"a\", 1);\n\
             match m.get(\"a\") {\n\
                 Some(c) => println(f\"{*c}\"),\n\
                 None => {},\n\
             }\n\
         }\n",
    );
    accepted(
        "map-get-deref-string",
        "fn main() {\n\
             let mut m: Map[String, String] = Map.new();\n\
             m.insert(\"a\", \"x\");\n\
             match m.get(\"a\") {\n\
                 Some(c) => println(f\"{*c}\"),\n\
                 None => {},\n\
             }\n\
         }\n",
    );
}

/// The `#[derive(Copy)]` fix covers a move inside a loop and a moved field.
#[test]
fn derive_copy_fixes_loop_and_field_moves() {
    rejected_then_fixed_by(
        "derive-copy-loop",
        "enum Reason { Capacity, Deleted }\n\
         fn note(r: own Reason) -> i64 { match r { Reason.Capacity => 1, Reason.Deleted => 2 } }\n\
         fn main() {\n\
             let r = Reason.Deleted;\n\
             let mut i = 0;\n\
             while i < 2 { println(note(r)); i = i + 1; }\n\
         }\n",
        "moved inside a loop",
        "#[derive(Copy)]\nenum Reason",
    );
    rejected_then_fixed_by(
        "derive-copy-field",
        "struct Stats { hits: i64 }\n\
         struct Cache { stats: Stats, size: i64 }\n\
         fn main() {\n\
             let c = Cache { stats: Stats { hits: 1 }, size: 2 };\n\
             let s = c.stats;\n\
             let d = c;\n\
             println(s.hits + d.size);\n\
         }\n",
        "moved here, used again",
        "#[derive(Copy)]\nstruct Stats",
    );
}

/// A type that cannot be `Copy` (a `String` field, a `Drop` body, a type
/// parameter) gets `#[derive(Clone)]`, and the next pass clones the reused
/// value at its move.
#[test]
fn derive_clone_then_clone_fixes_a_reused_move() {
    rejected_then_fixed_in(
        "derive-clone-generic",
        "enum G[T] { Y(T), N }\n\
         fn two(a: own G[String], b: own G[String]) {\n\
             match a { G.Y(v) => println(v), G.N => println(0) }\n\
             match b { G.Y(v) => println(v), G.N => println(0) }\n\
         }\n\
         fn main() {\n\
             let g: G[String] = G.Y(\"p\");\n\
             two(g, g);\n\
         }\n",
        "moved here, used again",
        "two(g.clone(), g)",
        2,
    );
    rejected_then_fixed_in(
        "derive-clone-drop",
        "struct R { id: i64, name: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(self.id) } }\n\
         fn eat(r: own R) { println(r.name) }\n\
         fn main() {\n\
             let r = R { id: 1, name: \"a\" };\n\
             eat(r);\n\
             eat(r);\n\
         }\n",
        "moved here, used again",
        "#[derive(Clone)]\nstruct R",
        2,
    );
}

/// A payload of a type with a `Drop` body borrows (§3.7); handing it on by
/// value takes `#[derive(Clone)]` and `.clone()`, which `karac fix` writes in
/// turn.
#[test]
fn ref_payload_handed_on_takes_derive_clone_then_clone() {
    rejected_then_fixed_in(
        "derive-clone-payload",
        "struct R { id: i64, tag: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(self.id) } }\n\
         enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(0) } }\n\
         fn eat(r: own R) -> i64 { return r.id }\n\
         fn call(b: own E) -> i64 { match b { E.A(r) => { return eat(r); } E.B => { return 1; } } }\n\
         fn main() { println(call(E.A(R { id: 2, tag: \"t\" }))); }\n",
        "cannot move 'r' out of 'E'",
        "eat(r.clone())",
        3,
    );
}

/// A tuple whose elements clone has `.clone()`, so the fix that writes it
/// type-checks.
#[test]
fn a_borrowed_tuple_payload_returned_is_cloned() {
    rejected_then_fixed_in(
        "tuple-clone",
        "shared enum Sh { S((Vec[String], Option[String])), N }\n\
         fn take(s: Sh) -> (Vec[String], Option[String]) {\n\
             match s { Sh.S(x) => { return x; } Sh.N => { return (Vec.new(), None); } }\n\
         }\n\
         fn main() { let t = take(Sh.S((vec![\"a\"], Some(\"b\")))); println(t.0.len()); }\n",
        "cannot move 'x'",
        "return x.clone();",
        1,
    );
}

/// An element of a generic collection is copied out with `.clone()`, which
/// needs `T: Clone`; a caller's own unbounded `T` handed to that function
/// gets the bound too. `karac fix` writes each edit in turn.
#[test]
fn a_generic_element_takes_a_clone_bound_up_the_call_chain() {
    rejected_then_fixed_in(
        "clone-bound-chain",
        "fn head[T](v: ref Vec[T]) -> T { return v[0]; }\n\
         fn via[T](v: ref Vec[T]) -> T { return head(v); }\n\
         fn main() { let a = vec![1, 2]; println(via(a)); }\n",
        "cannot move a non-`Copy` value out of a collection element",
        "fn via[T: Clone](v: ref Vec[T])",
        3,
    );
}

/// A type parameter passed where a bound is required must declare it.
#[test]
fn an_unbounded_type_parameter_does_not_satisfy_a_callee_bound() {
    rejected_with(
        "unbounded-param",
        "trait Sh { fn sh(ref self) -> i64; }\n\
         impl Sh for i64 { fn sh(ref self) -> i64 { return 1; } }\n\
         fn k[T: Sh](x: ref T) -> i64 { return x.sh(); }\n\
         fn j[T](x: ref T) -> i64 { return k(x); }\n\
         fn main() { println(j(2)); }\n",
        "type parameter 'T' is passed where 'T: Sh' is required",
    );
}

/// The derive goes on the innermost type first: `P` inside `Option[P]`,
/// and `R` before the `E` that holds it.
#[test]
fn derive_clone_reaches_through_option_and_payloads() {
    rejected_then_fixed_in(
        "derive-nested",
        "struct P { a: String }\n\
         struct H { p: Option[P] }\n\
         fn main() { let v = vec![H { p: Some(P { a: \"x\" }) }]; let q = v[0].p; println(q.is_some()); }\n",
        "cannot move a non-`Copy` value out of a collection element",
        "#[derive(Clone)]\nstruct P",
        2,
    );
    rejected_then_fixed_in(
        "derive-dep-first",
        "struct R { id: i64, v: Vec[i64] }\n\
         impl Drop for R { fn drop(mut ref self) { println(self.id) } }\n\
         enum E { A(R), B }\n\
         fn main() { let v = vec![E.A(R { id: 1, v: vec![1] })]; let e = v[0]; match e { E.A(r) => println(r.id), E.B => println(0) } }\n",
        "cannot move out of an index expression",
        "#[derive(Clone)]\nenum E",
        3,
    );
}

/// Moving an element out of a loop over a payload of an outer `for`
/// element: the outer loop's `.into_iter()` comes first, then the inner one.
#[test]
fn nested_loop_moves_take_into_iter_outside_in() {
    rejected_then_fixed_in(
        "into-iter-nested",
        "enum N { I(i64), L(Vec[N]) }\n\
         fn flat(level: own Vec[N]) -> Vec[N] {\n\
             let mut out: Vec[N] = Vec.new();\n\
             for item in level { match item { N.I(v) => println(v), N.L(inner) => { for x in inner { out.push(x); } } } }\n\
             return out;\n\
         }\n\
         fn main() { println(flat(vec![N.L(vec![N.I(1)])]).len()); }\n",
        "cannot move 'x'",
        "for item in level.into_iter()",
        3,
    );
}

/// A `ref` binding inside an f-string hole keeps its mark, so the fix that
/// writes it settles.
#[test]
fn a_ref_binding_inside_an_fstring_hole_borrows() {
    rejected_then_fixed_in(
        "fstring-ref",
        "enum Ve { A(String), B }\n\
         impl Drop for Ve { fn drop(mut ref self) { println(\"d\") } }\n\
         fn main() { let v = Ve.A(\"x\"); println(f\"[{match v { Ve.A(s) => { s.len() } Ve.B => { 0 } }}]\"); }\n",
        "cannot move 's' out of 'Ve'",
        "Ve.A(ref s)",
        1,
    );
}

/// The element parameter of a closure handed to an adaptor over `.iter()`
/// borrows the element (§4.6); moving a field out of it takes `.clone()`.
#[test]
fn an_iter_closure_param_borrows_its_element() {
    rejected_then_fixed_in(
        "iter-closure-view",
        "struct Req { path: String, ms: i64 }\n\
         fn main() {\n\
             let v = vec![Req { path: \"a\", ms: 200 }];\n\
             let slow: Vec[String] = v.iter().filter(|r| r.ms > 100).map(|r| r.path).collect();\n\
             let total = v.iter().fold(0, |acc, r| acc + r.ms);\n\
             println(slow.len() + total);\n\
         }\n",
        "cannot move a non-`Copy` value out of a borrowed place",
        ".map(|r| r.path.clone())",
        1,
    );
}

/// `Map.get` lends the value; `.cloned()` copies it out, after a derive
/// when the payload has no `.clone()`.
#[test]
fn map_get_cloned_checks_and_derives_clone() {
    rejected_then_fixed_in(
        "map-get-cloned",
        "struct P { a: String }\n\
         fn main() {\n\
             let mut m: Map[String, P] = Map.new();\n\
             m.insert(\"k\", P { a: \"x\" });\n\
             let o: Option[P] = m.get(\"k\").cloned();\n\
             println(o.is_some());\n\
         }\n",
        "`P` does not implement `Clone`",
        "#[derive(Clone)]\nstruct P",
        1,
    );
}

/// A `for` element stored as a map key, and an enumerated element handed
/// out in a tuple, are copies of borrowed values (§3.7): `.clone()`.
#[test]
fn a_borrowed_element_stored_or_returned_in_a_tuple_takes_clone() {
    rejected_then_fixed_in(
        "entry-key-view",
        "fn main() {\n\
             let words: Vec[String] = vec![\"a\", \"b\"];\n\
             let mut m: Map[String, i64] = Map.new();\n\
             for w in words { *m.entry(w).or_insert(0) += 1; }\n\
             println(words.len() + m.len());\n\
         }\n",
        "cannot move 'w'",
        "m.entry(w.clone())",
        1,
    );
    rejected_then_fixed_in(
        "enumerate-tuple-view",
        "fn main() {\n\
             let v: Vec[String] = vec![\"x\"];\n\
             let t: Vec[(i64, String)] = v.iter().enumerate().map(|p| (p.0, p.1)).collect();\n\
             println(t.len());\n\
         }\n",
        "cannot move a non-`Copy` value out of a borrowed place",
        "(p.0, p.1.clone())",
        1,
    );
}

/// §5.10: `collect` builds owned values, so collecting a chain over
/// `.iter()` moves each lent item out of its collection; `.cloned()` before
/// `collect` clones them, and a tuple with a lent part is reported too.
#[test]
fn collecting_lent_items_takes_cloned() {
    rejected_then_fixed_in(
        "collect-chain-lent",
        "fn main() {\n\
             let a: Vec[String] = vec![\"x\"];\n\
             let b: Vec[String] = vec![\"y\"];\n\
             let r: Vec[String] = a.iter().chain(b.iter()).collect();\n\
             println(r.len() + a.len());\n\
         }\n",
        "`collect` would move each one out of it",
        "a.iter().chain(b.iter()).cloned().collect()",
        1,
    );
    rejected_then_fixed_in(
        "collect-enumerate-lent",
        "fn main() {\n\
             let a: Vec[String] = vec![\"x\"];\n\
             let r: Vec[(i64, String)] = a.iter().enumerate().collect();\n\
             println(r.len());\n\
         }\n",
        "after the `.iter()` that lends them",
        "a.iter().cloned().enumerate().collect()",
        1,
    );
}

/// §5.10: a tuple's `.clone()` copies the references it holds, so a tuple
/// view moving out of a closure clones its non-`Copy` parts one by one.
#[test]
fn a_tuple_view_moving_out_clones_its_parts() {
    rejected_then_fixed_in(
        "tuple-view-tail",
        "fn main() {\n\
             let v: Vec[String] = vec![\"x\"];\n\
             let t: Vec[(i64, String)] = v.iter().enumerate().map(|p| p).collect();\n\
             println(t.len());\n\
         }\n",
        "copies the references it holds",
        "map(|p| (p.0, p.1.clone()))",
        1,
    );
}

/// §4.3: a `shared` type's methods take a borrowed `self` only; `karac fix`
/// rewrites `own self` and `mut ref self` to `self`.
#[test]
fn a_shared_types_methods_take_self() {
    rejected_then_fixed_in(
        "shared-own-self",
        "shared struct C { mut n: i64 }\n\
         impl C {\n\
             fn take(own self) -> i64 { self.n }\n\
             fn bump(mut ref self) { self.n = self.n + 1; }\n\
         }\n\
         fn main() { let c = C { n: 1 }; c.bump(); println(c.take()); }\n",
        "a shared type's methods take a borrowed `self` only",
        "fn take(self) -> i64",
        2,
    );
}

/// A collection's `.clone()` clones each element, so the element type
/// needs `.clone()`; `karac fix` derives it.
#[test]
fn cloning_a_vec_derives_clone_on_its_element() {
    rejected_then_fixed_in(
        "vec-clone-derive",
        "struct R { id: i64, s: String }\n\
         fn main() { let v = [R { id: 1, s: \"a\" }]; let w = v.clone(); println(w[0].id + v.len()); }\n",
        "clones each element, and 'R' has no `.clone()`",
        "#[derive(Clone)]",
        1,
    );
}

/// §5: a function declared `-> ref T` cannot return a value it makes.
#[test]
fn a_fresh_value_is_not_returned_by_reference() {
    rejected_then_fixed_in(
        "fresh-ref-return",
        "struct P { x: i64 }\n\
         fn mk(n: i64) -> ref P { P { x: n } }\n\
         fn main() { println(mk(3).x); }\n",
        "declared to return a reference, but its body makes a new value",
        "fn mk(n: i64) -> P {",
        1,
    );
}

/// §5.6: `let rel = ref self.rels[i]` borrows that element until `rel`'s
/// last use, and a `for` over `rel.deps` borrows it for the whole loop, so
/// a `mut ref self` call inside is a conflict; fix iterates a copy.
#[test]
fn a_loop_over_a_ref_local_borrows_what_it_points_into() {
    rejected_then_fixed_in(
        "ref-let-loop",
        "struct Rel { deps: Vec[i64] }\n\
         struct R { rels: Vec[Rel], seen: Vec[i64] }\n\
         impl R {\n\
             fn require(mut ref self, i: i64) {\n\
                 self.seen.push(i);\n\
                 let rel = ref self.rels[i];\n\
                 for d in rel.deps { self.require(d); }\n\
             }\n\
         }\n\
         fn main() {\n\
             let mut r = R { rels: [Rel { deps: [1] }, Rel { deps: [] }], seen: [] };\n\
             r.require(0);\n\
             println(r.seen.len());\n\
         }\n",
        "while the `for` loop over `rel.deps` borrows it",
        "for d in rel.deps.clone()",
        1,
    );
    rejected_with(
        "ref-let-used-after-write",
        "struct R { names: Vec[String], n: i64 }\n\
         impl R {\n\
             fn bump(mut ref self) { self.n = self.n + 1; }\n\
             fn run(mut ref self) {\n\
                 let first = ref self.names[0];\n\
                 self.bump();\n\
                 println(first);\n\
             }\n\
         }\n\
         fn main() { let mut r = R { names: [\"a\"], n: 0 }; r.run(); }\n",
        "while `ref first` borrows `self.names[..]`",
    );
    accepted(
        "ref-let-done-before-write",
        "struct R { names: Vec[String], n: i64 }\n\
         impl R {\n\
             fn bump(mut ref self) { self.n = self.n + 1; }\n\
             fn run(mut ref self) {\n\
                 let first = ref self.names[0];\n\
                 println(first);\n\
                 self.bump();\n\
             }\n\
         }\n\
         fn main() { let mut r = R { names: [\"a\"], n: 0 }; r.run(); }\n",
    );
}

/// §3.7: a `ref` binding's value whose type runs a `Drop` body cannot move
/// out; a type with no `.clone()` gets `#[derive(Clone)]`, then `.clone()`.
#[test]
fn a_drop_value_behind_a_ref_binding_derives_clone() {
    rejected_then_fixed_in(
        "ref-binding-drop-move",
        "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"d{self.id}\") } }\n\
         enum E { A(R), B }\n\
         struct H { f: R }\n\
         fn put(b: own E) -> i64 {\n\
             let mut h = H { f: R { id: 1 } };\n\
             match b { E.A(ref r) => { h.f = r; } E.B => { } }\n\
             h.f.id\n\
         }\n\
         fn main() { println(put(E.A(R { id: 2 }))); }\n",
        "its type runs a user `Drop` body",
        "h.f = r.clone()",
        2,
    );
}

/// `check` must fail with `expect` in its output (no fix is expected).
fn rejected_with(tag: &str, src: &str, expect: &str) {
    let (dir, path) = fixture(tag, src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{tag}: check must reject: {err}");
    assert!(err.contains(expect), "{tag}: expected `{expect}` in: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// §6.2 / §5.6: a `for` loop borrows the place it iterates for its whole
/// body, so writing that place through the same binding inside the loop is
/// an error, for a `mut` field of a shared value (pin
/// err_shared_field_same_handle) and for an owned collection alike.
#[test]
fn writing_a_place_a_for_loop_iterates_is_an_error() {
    rejected_with(
        "for-write-shared-field",
        "shared struct Bag { mut items: Vec[i64] }\n\
         fn main() {\n\
             let b = Bag { items: Vec.new() };\n\
             b.items.push(1);\n\
             for x in b.items {\n\
                 if x == 1 { b.items.push(2); }\n\
             }\n\
             println(f\"{b.items.len()}\");\n\
         }\n",
        "while the `for` loop over `b.items` borrows it",
    );
    rejected_with(
        "for-write-owned",
        "fn main() {\n\
             let mut v: Vec[i64] = Vec.new();\n\
             v.push(1);\n\
             for x in v {\n\
                 if x == 1 { v.push(2); }\n\
             }\n\
             println(f\"{v.len()}\");\n\
         }\n",
        "while the `for` loop over `v` borrows it (core-semantics.md §5.6)",
    );
    rejected_with(
        "for-write-self-method",
        "struct Q { mut items: Vec[i64], mut n: i64 }\n\
         impl Q {\n\
             fn grow(mut ref self) { self.items.push(0); }\n\
             fn run(mut ref self) {\n\
                 for x in self.items.iter() { if x > 0 { self.grow(); } }\n\
             }\n\
         }\n\
         fn main() { let mut q = Q { items: [1], n: 0 }; q.run(); println(q.n); }\n",
        "while the `for` loop over `self.items` borrows it",
    );
    // Another field, an element through `iter_mut`, a consumed or copied
    // collection, and a rebinding are not conflicts.
    accepted(
        "for-write-ok",
        "struct Q { mut items: Vec[i64], mut n: i64 }\n\
         impl Q {\n\
             fn run(mut ref self) {\n\
                 for x in self.items { self.n += x; }\n\
                 for x in self.items.iter_mut() { *x += 1; }\n\
             }\n\
         }\n\
         fn main() {\n\
             let mut q = Q { items: [1, 2], n: 0 };\n\
             q.run();\n\
             let mut v: Vec[i64] = [1, 2];\n\
             for x in v.clone() { v.push(x); }\n\
             let mut w: Vec[i64] = [3];\n\
             for x in w.into_iter() { println(x); }\n\
             println(f\"{q.n} {v.len()}\");\n\
         }\n",
    );
}

/// §9.6: a function type has a kind. A closure that writes a capture is a
/// `MutFn`, which a plain `Fn` parameter does not take (pins
/// ok_closure_kinds, err_fn_kind_mismatch); calling an `OnceFn` moves it,
/// so a second call is E0500 (pin err_once_called_twice).
#[test]
fn closure_kinds_follow_their_bodies() {
    accepted(
        "closure-kinds",
        "fn twice(f: MutFn()) { f(); f(); }\n\
         fn once(f: OnceFn() -> String) -> String { f() }\n\
         fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
         fn main() {\n\
             let mut count = 0;\n\
             twice(|| { count += 1; });\n\
             println(f\"{count}\");\n\
             let s = \"owned\".to_string();\n\
             let r = once(|| s);\n\
             println(r);\n\
             let k = 10;\n\
             println(f\"{apply(|x| x + k, 5)}\");\n\
         }\n",
    );
    rejected_with(
        "fn-kind-mismatch",
        "fn apply(f: Fn()) { f(); }\n\
         fn main() {\n\
             let mut n = 0;\n\
             apply(|| { n += 1; });\n\
             println(f\"{n}\");\n\
         }\n",
        "the closure mutates n, so it is a MutFn, but apply expects Fn (§9.6)",
    );
    rejected_with(
        "fn-kind-mutfn-param",
        "fn apply(f: Fn()) { f(); }\n\
         fn fwd(f: MutFn()) { apply(f); }\n\
         fn main() {\n\
             let mut n = 0;\n\
             fwd(|| { n += 1; });\n\
             println(f\"{n}\");\n\
         }\n",
        "`f` is a MutFn, but apply expects Fn (§9.6)",
    );
    rejected_with(
        "fn-kind-closure-local",
        "fn apply(f: Fn()) { f(); }\n\
         fn main() {\n\
             let mut v: Vec[i64] = Vec.new();\n\
             let c = || { v.push(1); };\n\
             apply(c);\n\
             println(f\"{v.len()}\");\n\
         }\n",
        "`c` mutates v, so it is a MutFn, but apply expects Fn (§9.6)",
    );
    rejected_with(
        "fn-kind-method",
        "struct Q { n: i64 }\n\
         impl Q {\n\
             fn apply(self, f: Fn()) { f(); }\n\
         }\n\
         fn main() {\n\
             let q = Q { n: 1 };\n\
             let mut c = 0;\n\
             q.apply(|| { c += 1; });\n\
             println(f\"{c}\");\n\
         }\n",
        "the closure mutates c, so it is a MutFn, but Q.apply expects Fn (§9.6)",
    );
    rejected_with(
        "once-called-twice",
        "fn call_twice(f: OnceFn()) {\n\
             f();\n\
             f();\n\
         }\n\
         fn main() {\n\
             let s = \"x\".to_string();\n\
             call_twice(|| { let t = s; });\n\
         }\n",
        "f moved by the first call at line 2:1 (an OnceFn is called once, §9.6)",
    );
}

/// §9.6: the capture prefixes `own |x|`, `ref |x|` and `mut ref |x|` are
/// removed, and `karac fix` deletes them.
#[test]
fn capture_prefixes_are_removed() {
    for prefix in ["own", "ref", "mut ref"] {
        rejected_then_fixed_by(
            &format!("capture-prefix-{}", prefix.replace(' ', "-")),
            &format!(
                "fn main() {{\n\
                     let k = 1;\n\
                     let a = {prefix} || k + 1;\n\
                     println(f\"{{a()}}\");\n\
                 }}\n"
            ),
            "closure capture prefixes are removed",
            "let a = || k + 1;",
        );
    }
}

/// §11.7: `collect_all` and `collect_all_vec` are removed; a `par` block
/// whose branches produce `Result`s already returns every result. §11.1:
/// so is the free `spawn`.
#[test]
fn collect_all_is_removed() {
    rejected_with(
        "collect-all-vec",
        "fn ok(n: i64) -> Result[i64, String] { Ok(n) }\n\
         fn main() {\n\
             let fs: Vec[Fn() -> Result[i64, String]] = vec![|| ok(1), || ok(2)];\n\
             let rs = collect_all_vec(fs);\n\
             println(f\"{rs.len()}\");\n\
         }\n",
        "`collect_all_vec` is removed (§11.7)",
    );
    rejected_with(
        "free-spawn",
        "fn add(a: i64, b: i64) -> i64 { a + b }\n\
         fn main() {\n\
             let h: TaskHandle[i64] = spawn(|| add(40, 2));\n\
             println(f\"{h.join()}\");\n\
         }\n",
        "free `spawn` is removed (§11.1)",
    );
}

/// design.md § 5: `i64`/`u64` -> `f64` rounds above 2^53, so it is not an
/// implicit widening; `check` refuses it at a return, a `let`, an argument,
/// a field and a push, and `fix` writes the `as f64`. `i32` -> `f64` is
/// exact and stays implicit, and `run` keeps accepting the old spelling.
#[test]
fn a_64_bit_integer_into_f64_needs_as() {
    let src = "\
fn tailf(v: u64) -> f64 { v }
fn takef(x: f64) -> f64 { x }
fn exact(v: i32) -> f64 { v }
struct S { f: f64 }
fn main() {
    let n: i64 = 3;
    let slot: f64 = n;
    let s = S { f: n };
    let mut vf: Vec[f64] = [];
    vf.push(n);
    println(f\"{tailf(7)} {takef(n)} {exact(2)} {slot} {s.f} {vf[0]}\");
}
";
    rejected_then_fixed_by(
        "int_to_f64",
        src,
        "implicit conversion from 'u64' to 'f64' can lose precision",
        "n as f64",
    );
    let (dir, path) = fixture("int_to_f64_run", src);
    let out = karac()
        .arg("run")
        .arg("--interp")
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "7 3 2 3 3 3\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// §3.1: a block's tail moves, also where the block's value is only
/// borrowed afterwards (`{ loc }.len()`), so a later use of `loc` is E0500.
#[test]
fn a_block_tail_moves_even_when_the_block_is_only_borrowed() {
    rejected_then_fixed(
        "block_tail_moves",
        r#"
fn main() {
    let loc = "abc".to_string();
    let n = { loc }.len();
    println(f"{n} {loc}");
}
"#,
        "value 'loc' moved here",
    );
}

/// §5.6: a `ref` pattern binding borrows its scrutinee until its last use,
/// so a write to the scrutinee before that use is an error and one after it
/// is not.
#[test]
fn a_write_to_a_scrutinee_while_its_ref_binding_is_live_is_an_error() {
    rejected(
        "ref_binding_live_write",
        r#"
struct T { tag: i64 }
enum E { X(T), Y }
impl E { fn clear(mut ref self) { self = E.Y; } }
fn main() {
    let mut g = E.X(T { tag: 1 });
    match g {
        E.X(ref t) => {
            g.clear();
            println(t.tag);
        }
        E.Y => println(0),
    }
}
"#,
        "while `ref t` borrows `g`",
    );
    accepted(
        "ref_binding_dead_write",
        r#"
struct T { tag: i64 }
enum E { X(T), Y }
fn main() {
    let mut g = E.X(T { tag: 1 });
    match g {
        E.X(ref t) => {
            println(t.tag);
            g = E.Y;
        }
        E.Y => println(0),
    }
    let mut i = 0;
    while let E.X(ref t) = g {
        i = i + 1;
        if i > 1 {
            println(t.tag);
            g = E.Y;
        } else {
            println(t.tag + 1);
        }
    }
}
"#,
    );
}

/// §3.1 makes a block's tail a move, so a closure body written as a block
/// moves what its `match` arm bound. That binding is the closure's own, not
/// a capture, so the closure stays callable more than once.
#[test]
fn a_closure_moving_its_own_match_binding_is_not_once_callable() {
    accepted(
        "closure_arm_binding",
        r#"
enum Ho { Full(String), Empty }
fn main() {
    let f = |q: Ho| { match q { Ho.Full(s) => s, Ho.Empty => "e".to_string() } };
    let a = f(Ho.Full("x".to_string()));
    let b = f(Ho.Empty);
    println(f"{a} {b}");
}
"#,
    );
}

/// A function value called through a type parameter with a function-trait
/// bound (`F: MutFn(ref I.Item)`) or through a struct field typed
/// `Fn(ref String)` borrows the argument the signature marks `ref`, so
/// using it again after the call is not a use after move.
#[test]
fn a_ref_parameter_of_a_bounded_or_field_held_function_borrows() {
    accepted(
        "fn-bound-ref",
        r#"
struct Counter { n: i64 }
impl Iterator for Counter {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] { self.n = self.n + 1; Some(self.n) }
}
struct Insp[I, F] { it: I, f: F }
impl[I: Iterator, F: MutFn(ref I.Item)] Iterator for Insp[I, F] {
    type Item = I.Item;
    fn next(mut ref self) -> Option[I.Item] {
        match self.it.next() {
            Some(x) => { (self.f)(x); Some(x) }
            None => None,
        }
    }
}
fn insp[I: Iterator, F: MutFn(ref I.Item)](it: own I, f: own F) -> Insp[I, F] { Insp { it: it, f: f } }
fn twice[F: Fn(ref String) -> i64](f: own F, s: own String) -> i64 { f(s) + f(s) }
struct H { f: Fn(ref String) -> i64 }
fn main() {
    let mut seen = 0;
    for x in insp(Counter { n: 0 }, |x| { seen = seen + 1; }) { if x == 4 { break; } }
    let h = H { f: |s: ref String| s.len() };
    let s = "abc";
    let n = (h.f)(s);
    println(f"{seen} {n} {s} {twice(|s| s.len(), "xy")}");
}
"#,
    );
}

/// The same call through an `own` parameter of the bound still moves.
#[test]
fn an_own_parameter_of_a_bounded_function_moves() {
    rejected_then_fixed(
        "fn-bound-own",
        r#"
fn take[F: Fn(own String) -> i64](f: own F, s: own String) -> i64 { f(s) + f(s) }
fn main() { println(take(|s| s.len(), "ab")); }
"#,
        "moved here",
    );
}

/// A closure's tail or `return` moves its value out as a fn's does, so
/// `|q: S| q.t` over a `shared struct S` moves a field out of a shared
/// value (core semantics §4.6), and `fix` clones it.
#[test]
fn a_closure_moving_a_field_out_of_a_shared_value_is_an_error() {
    rejected_then_fixed_in(
        "closure-shared-field",
        r#"
shared struct S { t: String }
fn main() {
    let j = |q: S| q.t;
    let k = |q: S| { return q.t; };
    println(j(S { t: "a" }));
    println(k(S { t: "b" }));
}
"#,
        "cannot move a non-`Copy` value out of a borrowed place",
        ".clone()",
        2,
    );
}

/// A default on a positional parameter: `fix` writes the `;` that makes it
/// and the parameters after it named (after the receiver, in place of the
/// comma; at the front, opening the list), then labels each call that passed
/// one of them positionally.
#[test]
fn a_positional_default_is_fixed_by_naming_it_and_labeling_its_calls() {
    rejected_then_fixed_in(
        "positional-default",
        r#"
struct S { id: i64 }
impl S {
    fn m(ref self, port: i64 = 8080) -> i64 { self.id + port }
    fn make(host: i64, port: i64 = 1, n: i64 = 2) -> i64 { host + port + n }
}
fn g(port: i64 = 1, n: i64 = 2) -> i64 { port + n }
fn main() {
    let s = S { id: 1 };
    println(s.m(2));
    println(S.make(1, 5, n: 3));
    println(g(5, 6));
    println(g());
}
"#,
        "only a named parameter can have a default",
        "fn m(ref self; port: i64 = 8080)",
        3,
    );
}

/// `v.into_iter()` consumes `v` (core semantics §4.3), so a use of `v`
/// after the loop is a use after move, and `fix` clones it at the call.
#[test]
fn a_collection_used_after_into_iter_is_an_error() {
    rejected_then_fixed_by(
        "into-iter-moves",
        r#"
fn show(h: own String) { println(h); }
fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("a");
    for h in v.into_iter() {
        show(h);
    }
    println(f"n{v.len()}");
}
"#,
        "moved here",
        "v.clone().into_iter()",
    );
}

/// An `own` argument of a `mut ref self` method is stored past the call, so
/// passing the same binding on every iteration moves it twice (core
/// semantics §3.3), as a free function's argument does.
#[test]
fn an_own_method_argument_moved_in_a_loop_is_an_error() {
    rejected_then_fixed_by(
        "loop-container-store",
        r#"
struct R { n: i64 }
impl R {
    fn req(mut ref self, by: own String) { self.n = self.n + by.len(); }
    fn go(mut ref self) {
        let who = "w";
        for i in 0..3 {
            self.req(who);
        }
    }
}
fn main() {
    let mut r = R { n: 0 };
    r.go();
    println(f"{r.n}");
}
"#,
        "is moved inside a loop",
        "self.req(who.clone())",
    );
}

/// §5.2: `StringSlice` is `Str`, a shared view, so reading one out of a
/// borrowed place copies it.
#[test]
fn a_string_slice_field_copies_out_of_a_borrow() {
    accepted(
        "string-slice-copy",
        "struct It { rest: StringSlice, done: bool }\n\
         impl It {\n\
             fn next(mut ref self) -> Option[StringSlice] {\n\
                 if self.done { return None; }\n\
                 self.done = true;\n\
                 return Some(self.rest);\n\
             }\n\
         }\n\
         fn main() {\n\
             let s = \"abc\".to_string();\n\
             let mut it = It { rest: s.slice(0, 2), done: false };\n\
             match it.next() { Some(f) => println(f.len()), None => {} }\n\
         }\n",
    );
}

/// A type holding only `shared` handles and clonable parts derives
/// `Clone`, so moving one out of a `shared enum`'s payload is fixed by the
/// derive and then a `.clone()`.
#[test]
fn a_payload_of_a_shared_enum_derives_clone_through_its_handles() {
    rejected_then_fixed_in(
        "shared-enum-payload-derive",
        "shared enum M { My(Vec[String]), N }\n\
         enum V { Z(M), N }\n\
         shared enum H { Z(V), N }\n\
         fn getv(h: H) -> V { match h { H.Z(v) => v, H.N => V.N } }\n\
         fn main() { match getv(H.Z(V.Z(M.N))) { V.Z(_) => println(\"z\"), _ => println(\"o\") } }\n",
        "cannot move 'v'",
        "H.Z(v) => v.clone()",
        2,
    );
}

/// §6.1: a `Result` of a handle and `Copy` parts is a handle aggregate, so
/// matching on a collection element of that type counts rather than moves.
#[test]
fn a_result_of_a_handle_is_counted_out_of_an_element() {
    accepted(
        "result-handle-element",
        "shared struct Node { val: i64 }\n\
         fn main() {\n\
             let mut dst: Vec[Result[Node, i64]] = Vec.new();\n\
             dst.push(Ok(Node { val: 10 }));\n\
             match dst[0] { Err(_) => {} Ok(nd) => { println(nd.val); } }\n\
             println(dst.len());\n\
         }\n",
    );
}

/// §4.6: only bindings move, so after a field moves out of a local a
/// struct pattern may still copy the other fields. Binding the moved field
/// again is still a use after move.
#[test]
fn a_struct_pattern_reads_only_the_parts_it_binds() {
    accepted(
        "pattern-after-partial-move",
        "struct S { name: String }\n\
         struct Q { u: S, n: i64 }\n\
         fn f(q: own Q) -> i64 { let x = q.u; let Q { n, .. } = q; println(x.name); n }\n\
         fn main() { println(f(Q { u: S { name: \"a\" }, n: 2 })) }\n",
    );
    rejected_with(
        "pattern-rebinds-moved-part",
        "struct S { name: String }\n\
         struct Q { u: S, n: i64 }\n\
         fn f(q: own Q) -> i64 { let x = q.u; let Q { u, n } = q; println(x.name); n }\n\
         fn main() { println(f(Q { u: S { name: \"a\" }, n: 2 })) }\n",
        "moved here, used again here",
    );
}

/// `m[k] = v` inserts a missing key (decided 2026-10-08), so the key moves
/// into the map; reusing it is a use after move, fixed with `.clone()`.
#[test]
fn an_index_assigned_map_key_moves() {
    rejected_then_fixed(
        "map-index-assign-key",
        "fn main() {\n\
             let mut m: Map[String, i64] = Map.new();\n\
             let k = \"a\".to_string();\n\
             m[k] = 1;\n\
             println(k);\n\
         }\n",
        "value 'k' moved here",
    );
}

#[test]
fn str_is_the_string_view() {
    // docs/library/strings.md, design.md § Strings: `Str` names the view; a
    // `String` passes as `Str`, borrowed; `s[a..b]` is a `Str`; `as_str`
    // views the whole string.
    accepted(
        "str-view",
        "fn first_word(s: Str) -> Str {\n\
             match s.find(\" \") { Some(i) => s[0..i], None => s }\n\
         }\n\
         fn main() {\n\
             let mut line = \"hello world\".to_string();\n\
             let w = first_word(line);\n\
             println(w);\n\
             println(line.len());\n\
             let v: Str = line.as_str();\n\
             let h = v[0..2];\n\
             println(f\"{h} {v.len()}\");\n\
             line.clear();\n\
             println(line.is_empty());\n\
         }\n",
    );
}

/// `peek()` lends the item it holds (iter.kara: `Option[ref T]`), so over
/// `v.iter()` it is `Option[ref ref T]`: one borrow from the iterator, one
/// from the vector. Keeping a peeked `String` takes a copy, and `karac fix`
/// writes it.
#[test]
fn a_peeked_item_is_borrowed() {
    rejected_then_fixed(
        "peek-borrowed",
        "fn main() {\n\
             let v = [\"a\".to_string(), \"b\".to_string()];\n\
             let mut p = v.iter().map(|s| s.clone()).peekable();\n\
             let s: String = p.peek().unwrap();\n\
             println(s);\n\
         }\n",
        "found 'ref String'",
    );
}

/// D5 covers function TYPES as well as items: a bare parameter of
/// `Fn(String) -> i64` borrows, so calling through it leaves the argument
/// with the caller, and an item whose parameter borrows has that type.
/// `own T` in a function type still takes the argument.
#[test]
fn a_function_type_parameter_borrows() {
    accepted(
        "fn-type-borrows",
        "fn nlen(s: String) -> i64 { s.len() }\n\
         fn twice(f: Fn(String) -> i64, s: String) -> i64 { f(s) + f(s) }\n\
         fn pick() -> Fn(String) -> i64 { nlen }\n\
         fn main() {\n\
             let s = \"abc\".to_string();\n\
             let g = pick();\n\
             println(twice(nlen, s) + g(s));\n\
             println(s);\n\
         }\n",
    );
    rejected_then_fixed(
        "fn-type-own",
        "fn main() {\n\
             let f: Fn(own String) -> i64 = |s| s.len();\n\
             let s = \"abc\".to_string();\n\
             println(f(s));\n\
             println(s);\n\
         }\n",
        "error[ownership]",
    );
}

/// §4.6 under D5: a bare struct parameter borrows, so a struct pattern in
/// its place destructures through the borrow: a Copy field is a copy, any
/// other a borrow. Moving a borrowed field out is still refused.
#[test]
fn a_struct_pattern_parameter_destructures_through_the_borrow() {
    accepted(
        "struct-param-pattern",
        "struct P { a: String, b: i64 }\n\
         fn take(P { a, b }: P) -> i64 { a.len() + b }\n\
         fn main() {\n\
             let p = P { a: \"abc\".to_string(), b: 4 };\n\
             println(take(p));\n\
             println(p.a);\n\
         }\n",
    );
    rejected_then_fixed(
        "struct-param-pattern-move",
        "struct P { a: String, b: i64 }\n\
         fn keep(P { a, b }: P) -> String { a }\n\
         fn main() { println(keep(P { a: \"abc\".to_string(), b: 4 })); }\n",
        "found 'ref String'",
    );
}

/// B-2026-10-12-2: `SortedSet.min()` / `.max()` lend the element
/// (collections.md: `Option[ref T]`), as `iter()` and `range()` do, so an
/// explicit `*x` reads it (§5.10) and keeping a `String` takes a copy.
#[test]
fn sorted_set_min_and_max_lend_their_element() {
    accepted(
        "sorted-set-min-deref",
        "fn main() {\n\
             let mut s: SortedSet[i64] = SortedSet.new();\n\
             s.insert(5);\n\
             s.insert(2);\n\
             let mut t = 0;\n\
             if let Some(x) = s.min() { t += *x; }\n\
             if let Some(x) = s.max() { t += *x; }\n\
             println(t);\n\
         }\n",
    );
    rejected_then_fixed(
        "sorted-set-max-string",
        "fn main() {\n\
             let mut s: SortedSet[String] = SortedSet.new();\n\
             s.insert(\"a\".to_string());\n\
             let m: String = s.max().unwrap();\n\
             println(m);\n\
         }\n",
        "found 'ref String'",
    );
}

/// §4.6: a bare `for` borrows its collection, so a method that takes `self`
/// by value (`TaskHandle.join(own self)`, a user `own self` method) cannot
/// run on its element. `karac fix` makes the loop move the elements out.
#[test]
fn an_owned_receiver_on_a_for_element_moves_the_loop() {
    rejected_then_fixed_by(
        "for-elem-join",
        "fn main() {\n\
             let mut g = TaskGroup.new();\n\
             let mut hs: Vec[TaskHandle[i64]] = Vec.new();\n\
             hs.push(g.spawn(|| 1));\n\
             let mut total = 0;\n\
             for h in hs { total = total + h.join(); }\n\
             println(total);\n\
         }\n",
        "cannot move a `for` element out of the collection",
        "for h in hs.into_iter()",
    );
    rejected_then_fixed_by(
        "for-elem-own-self",
        "struct W { s: String }\n\
         impl W { fn take(own self) -> String { self.s } }\n\
         fn main() {\n\
             let v = vec![W { s: \"a\".to_string() }];\n\
             for w in v { println(w.take()); }\n\
         }\n",
        "`.into_iter()`",
        "for w in v.into_iter()",
    );
}

/// B-2026-10-12-9: `*p.0` on an undestructured `SortedMap.iter()` item, and
/// `*k` / `*v` on a `floor` / `ceiling` entry (`Option[(ref K, ref V)]` in
/// docs/library/collections.md), read through the view.
#[test]
fn deref_of_a_map_item_part_and_a_floor_entry_is_accepted() {
    accepted(
        "map-item-deref",
        "fn main() {\n\
         \x20   let mut m: SortedMap[i64, i64] = SortedMap.new();\n\
         \x20   m.insert(1, 10);\n\
         \x20   for p in m.iter() {\n\
         \x20       println(f\"{*p.0} {*p.1}\");\n\
         \x20   }\n\
         \x20   if let Some((k, v)) = m.floor(4) {\n\
         \x20       println(f\"{*k} {*v}\");\n\
         \x20   }\n\
         \x20   if let Some((k, v)) = m.ceiling(0) {\n\
         \x20       println(f\"{*k} {*v}\");\n\
         \x20   }\n\
         }\n",
    );
}

/// B-2026-10-12-19: moving the elements out of `for c in node.children`,
/// with `node` an owned element of an outer `into_iter()` walk, is fixed by
/// consuming the field, not by deriving `Clone` and copying every subtree.
#[test]
fn moving_out_of_a_field_of_an_owned_local_is_fixed_with_into_iter() {
    let src = "struct Node { val: i64, children: Vec[Node] }\n\
               \n\
               fn flatten(root: own Node) -> Vec[i64] {\n\
               \x20   let mut out: Vec[i64] = Vec.new();\n\
               \x20   let mut level: Vec[Node] = vec![root];\n\
               \x20   while level.len() > 0 {\n\
               \x20       let mut next: Vec[Node] = Vec.new();\n\
               \x20       for node in level.into_iter() {\n\
               \x20           out.push(node.val);\n\
               \x20           for child in node.children {\n\
               \x20               next.push(child);\n\
               \x20           }\n\
               \x20       }\n\
               \x20       level = next;\n\
               \x20   }\n\
               \x20   out\n\
               }\n\
               \n\
               fn main() {\n\
               \x20   println(f\"{flatten(Node { val: 1, children: Vec.new() }).len()}\");\n\
               }\n";
    rejected_then_fixed_in(
        "field-into-iter",
        src,
        "cannot move 'child'",
        "for child in node.children.into_iter() {",
        2,
    );
    let (dir, path) = fixture("field-into-iter-no-clone", src);
    for _ in 0..2 {
        karac().arg("fix").arg(&path).output().unwrap();
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        !fixed.contains("Clone"),
        "fix must not derive Clone: {fixed}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-12-20: storing a `shared` parameter into a `weak` field
/// downgrades it and moves nothing, so the parameter stays usable, as a
/// local does.
#[test]
fn a_weak_store_of_a_shared_parameter_moves_nothing() {
    accepted(
        "weak-store-param",
        "shared struct N {\n\
         \x20   v: i64,\n\
         \x20   mut prev: weak N,\n\
         \x20   mut next: Option[N],\n\
         }\n\
         \n\
         fn link(at: N, b: N) {\n\
         \x20   b.prev = at;\n\
         \x20   at.next = Some(b);\n\
         \x20   println(f\"{at.v}\");\n\
         }\n\
         \n\
         fn main() {\n\
         \x20   let a = N { v: 1, prev: None, next: None };\n\
         \x20   let b = N { v: 2, prev: None, next: None };\n\
         \x20   link(a, b);\n\
         }\n",
    );
}

#[test]
fn a_mutating_method_on_a_range_view_of_a_mutable_place_is_accepted() {
    // B-2026-10-12-24: design.md § Slices — `v[a..b]` is a `mut Slice[T]`
    // where a mutable view is needed, and a `mut ref self` receiver is one.
    accepted(
        "range-view-mut-method",
        "struct S { v: Vec[i64] }\n\
         \n\
         fn flip(v: mut ref Vec[i64]) {\n\
         \x20   v[1..3].reverse();\n\
         }\n\
         \n\
         fn main() {\n\
         \x20   let mut v = vec![1, 2, 3, 4];\n\
         \x20   v[0..2].reverse();\n\
         \x20   flip(mut v);\n\
         \x20   v[2..4].swap(0, 1);\n\
         \x20   v[..].sort_by(|a, b| b.cmp(a));\n\
         \x20   let mut s = S { v: vec![1, 2, 3] };\n\
         \x20   s.v[0..2].fill(9);\n\
         \x20   println(f\"{v[0]} {s.v[0]}\");\n\
         }\n",
    );
    // The view is a write to `v`, which needs `let mut` like `flip(mut v[..])`.
    rejected_then_fixed_by(
        "range-view-mut-method-let",
        "fn main() {\n\
         \x20   let v = vec![1, 2, 3, 4];\n\
         \x20   v[0..2].reverse();\n\
         \x20   println(f\"{v[0]}\");\n\
         }\n",
        "cannot call `reverse` on `v`",
        "let mut v",
    );
    // A `ref` base still yields a read-only view.
    rejected(
        "range-view-mut-method-ref",
        "fn f(v: ref Vec[i64]) {\n\
         \x20   v[0..2].reverse();\n\
         }\n\
         \n\
         fn main() {\n\
         \x20   let v = vec![1, 2, 3, 4];\n\
         \x20   f(v);\n\
         }\n",
        "requires a mutable slice",
    );
    // The view still conflicts with a live slice of the same place.
    rejected(
        "range-view-mut-method-live",
        "fn main() {\n\
         \x20   let mut v = vec![1, 2, 3, 4];\n\
         \x20   let s = v.as_slice();\n\
         \x20   v[0..2].reverse();\n\
         \x20   println(f\"{s[0]}\");\n\
         }\n",
        "while another slice borrow is live",
    );
}

#[test]
fn get_or_insert_hands_out_a_mut_ref_under_check() {
    // `runtime/stdlib/source/option.kara`'s bodies return `mut ref T`, and
    // the MIR pipeline types them so; `karac check` must agree, or the
    // corpus's `*x = *x + 1` on the result is refused before it runs.
    accepted(
        "get-or-insert-mut-ref",
        "fn main() {\n\
         \x20   let mut a: Option[i64] = None;\n\
         \x20   let x = a.get_or_insert(5);\n\
         \x20   *x = *x + 1;\n\
         \x20   println(f\"{a.unwrap()}\");\n\
         \x20   let mut b: Option[String] = Some(\"keep\".to_string());\n\
         \x20   let y = b.get_or_insert(\"new\".to_string());\n\
         \x20   y.push('!');\n\
         \x20   println(b.unwrap());\n\
         \x20   let mut c: Option[i64] = None;\n\
         \x20   let z = c.get_or_insert_with(|| 2);\n\
         \x20   *z += 1;\n\
         \x20   println(f\"{c.unwrap()}\");\n\
         }\n",
    );
}

#[test]
fn a_shared_mut_field_is_writable_through_a_let_tuple_binding() {
    // B-2026-10-12-27: core-semantics §5.9 — a `mut` field of a shared value
    // is writable through any handle, including one a tuple pattern bound.
    accepted(
        "let-tuple-shared-write",
        "shared struct N { mut val: i64, mut next: Option[N] }\n\
         \n\
         fn pair() -> (N, i64) {\n\
         \x20   (N { val: 1, next: None }, 2)\n\
         }\n\
         \n\
         fn main() {\n\
         \x20   let (b, k) = pair();\n\
         \x20   b.val = 6 + k;\n\
         \x20   let (c, j): (N, i64) = pair();\n\
         \x20   c.next = Some(b);\n\
         \x20   println(f\"{c.next.unwrap().val} {j}\");\n\
         }\n",
    );
    // A plain struct bound the same way still needs `let mut`.
    rejected_then_fixed_by(
        "let-tuple-plain-write",
        "struct P { val: i64 }\n\
         \n\
         fn main() {\n\
         \x20   let (p, k) = (P { val: 1 }, 2);\n\
         \x20   p.val = k;\n\
         \x20   println(f\"{p.val}\");\n\
         }\n",
        "cannot assign through `p`",
        "let mut (p, k)",
    );
}

#[test]
fn an_index_read_of_a_counted_handle_composite_is_accepted() {
    // B-2026-10-12-26: `Option[shared]` and a tuple of handles and `Copy`
    // parts are passed by value under D5, and reading one out of a `Vec`
    // retains or copies each part, as a bare `shared` element already does.
    accepted(
        "index-read-option-shared",
        "shared struct Node {\n\
         \x20   val: i64,\n\
         }\n\
         \n\
         fn total(n: Option[Node]) -> i64 {\n\
         \x20   if let Some(x) = n { x.val } else { 0 }\n\
         }\n\
         \n\
         fn pair_total(p: (Node, i64)) -> i64 {\n\
         \x20   p.0.val + p.1\n\
         }\n\
         \n\
         fn main() {\n\
         \x20   let options: Vec[Option[Node]] = vec![Some(Node { val: 2 }), None];\n\
         \x20   let pairs: Vec[(Node, i64)] = vec![(Node { val: 3 }, 4)];\n\
         \x20   let o = options[0];\n\
         \x20   println(f\"{total(o)} {total(options[1])} {pair_total(pairs[0])}\");\n\
         }\n",
    );
    // An `Option` of an owned heap value is still a move out of the `Vec`.
    rejected_then_fixed(
        "index-read-option-string",
        "fn main() {\n\
         \x20   let v: Vec[Option[String]] = vec![Some(\"a\".to_string())];\n\
         \x20   let o = v[0];\n\
         \x20   println(o.unwrap());\n\
         }\n",
        "E_INDEX_MOVE_NON_COPY",
    );
}

#[test]
fn deref_of_a_copy_item_of_iter_next_is_accepted() {
    // B-2026-10-06-74: `c.iter().next()` yields `ref T` items, so `*k` reads
    // a `Copy` part bound out of one, as it does under `for k in c.iter()`.
    accepted(
        "iter-next-deref",
        "fn main() {\n\
         \x20   let v: Vec[i64] = vec![4, 5];\n\
         \x20   let Some(k) = v.iter().next() else { return; };\n\
         \x20   let x = *k;\n\
         \x20   let mut s: SortedSet[i64] = SortedSet.new();\n\
         \x20   s.insert(3);\n\
         \x20   let Some(t) = s.iter().next() else { return; };\n\
         \x20   let mut m: Map[i64, i64] = Map.new();\n\
         \x20   m.insert(1, 2);\n\
         \x20   let Some((a, b)) = m.iter().next() else { return; };\n\
         \x20   let mut sm: SortedMap[i64, i64] = SortedMap.new();\n\
         \x20   sm.insert(7, 8);\n\
         \x20   if let Some((c, d)) = sm.iter().next() {\n\
         \x20       println(f\"{x} {*t} {*a + *b} {*c + *d}\");\n\
         \x20   }\n\
         }\n",
    );
}

/// `KARAC_CHECK_LIBRARY=1`: check reads the Kāra library the MIR pipeline runs
/// (B-2026-10-12-23), so it agrees with `__mir-run` on what a library type
/// offers: `StrChars` is double-ended, and `collect` into an assigned `String`
/// goes through `FromIterator` rather than the legacy desugar.
#[test]
fn library_check_reads_the_kara_library() {
    let src = "fn main() {\n\
                   let s = \"héllo\".to_string();\n\
                   let mut it = s.chars();\n\
                   match it.next_back() {\n\
                       Some(c) => println(f\"{c}\"),\n\
                       None => println(\"none\"),\n\
                   }\n\
                   let cs: Vec[char] = s.chars().collect();\n\
                   let mut t = String.new();\n\
                   t = cs.iter().collect();\n\
                   println(t);\n\
               }\n";
    let (dir, path) = fixture("library-check", src);
    let out = karac()
        .env("KARAC_CHECK_LIBRARY", "1")
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "library check must accept: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "o\nhéllo\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Library check still reports the user's own errors, and only theirs: the
/// library's functions stay out of the JSON effect summary.
#[test]
fn library_check_keeps_user_errors_and_hides_the_library() {
    let src = "fn take(s: own String) { println(s); }\n\
               pub fn shout(s: ref String) { println(s); }\n\
               fn main() {\n\
                   let s = \"a\".to_string();\n\
                   take(s);\n\
                   take(s);\n\
               }\n";
    let (dir, path) = fixture("library-check-errors", src);
    let out = karac()
        .env("KARAC_CHECK_LIBRARY", "1")
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "library check must reject: {err}");
    assert!(err.contains("p.kara:6:"), "the error is the user's: {err}");
    let json = |lib: bool| {
        let mut c = karac();
        if lib {
            c.env("KARAC_CHECK_LIBRARY", "1");
        }
        let out = c
            .arg("check")
            .arg("--output=json")
            .arg(&path)
            .output()
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        v["public_function_effects"].clone()
    };
    let with = json(true);
    assert!(with.get("shout").is_some(), "{with}");
    assert_eq!(with, json(false));
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-13-19: `extend` takes owned items, so extending from a chain
/// over `.iter()` moves each lent item out of its collection, as `collect`
/// does; `.cloned()` on the iterator clones them.
#[test]
fn extending_from_lent_items_takes_cloned() {
    rejected_then_fixed_in(
        "extend-lent",
        "fn main() {\n\
             let a: Vec[String] = vec![\"x\"];\n\
             let mut d: Vec[String] = Vec.new();\n\
             d.extend(a.iter());\n\
             println(d.len() + a.len());\n\
         }\n",
        "`extend` would move each one out of it",
        "d.extend(a.iter().cloned())",
        1,
    );
}

// B-2026-10-12-16 — a tuple-literal scrutinee lends each element it borrows
// on its own, so `(Some(i), Some(j))` over `(m.get(a), m.get(b))` binds `ref`s
// exactly as `Some(i)` over `m.get(a)` does: `*i` reads through them, nested
// tuples and or-patterns included.
#[test]
fn tuple_scrutinee_keeps_each_element_borrow() {
    accepted(
        "tuple-scrutinee-ref",
        r#"fn main() {
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
    match ((m.get("a"), 1), 2) {
        ((Some(i), k), l) => println(f"{*i + k + l}"),
        _ => println("none"),
    }
}
"#,
    );
    rejected_then_fixed(
        "tuple-scrutinee-move",
        r#"fn take(s: own String) -> i64 { s.len() }
fn main() {
    let mut m: Map[String, String] = Map.new();
    m.insert(String.from("a"), String.from("xy"));
    match (m.get("a"), 1) {
        (Some(a), _) => println(f"{take(a)}"),
        _ => println("none"),
    }
}
"#,
        "cannot move 'a': it borrows its value",
    );
}

// B-2026-10-14-8 — a binding declared inside a closure body, used in a loop
// there and moved after it, is not "moved inside a loop": each call makes it
// afresh, so the moves are checked as any local's are. `collect()` into a
// `String` or `Set` inside a closure desugars to exactly that shape. A move
// that really is repeated by a loop inside the closure is still refused.
#[test]
fn closure_local_moved_after_its_loop_is_accepted() {
    accepted(
        "closure-local-after-loop",
        "fn main() {\n\
             let f = |n: i64| { let mut t = String.new(); for i in 0..n { t.push('x'); } t };\n\
             println(f\"{f(2)}\");\n\
         }\n",
    );
    accepted(
        "closure-collect-string",
        "fn main() {\n\
             let words: Vec[String] = vec![\"ab\".to_string(), \"cd\".to_string()];\n\
             let r: Vec[String] = words.iter().map(|w| { let s: String = w.chars().rev().collect(); s }).collect();\n\
             let f = |w: String| { let s: Set[char] = w.chars().collect(); s.len() };\n\
             println(f\"{r} {f(\"aab\".to_string())}\");\n\
         }\n",
    );
    rejected_then_fixed(
        "closure-local-moved-in-loop",
        "fn take(s: own String) -> i64 { s.len() }\n\
         fn main() {\n\
             let f = |n: i64| { let t = String.from(\"ab\"); let mut k = 0; for i in 0..n { k += take(t); } k };\n\
             println(f\"{f(2)}\");\n\
         }\n",
        "value 't' is moved inside a loop and not assigned again",
    );
}

// B-2026-10-14-7 — a `collect()` that a `map` closure yields is built as the
// element type of the outer `let` annotation, as Rust infers the inner
// `collect` from the outer one: there is no turbofish to write it with.
#[test]
fn map_closure_collect_takes_the_outer_element_type() {
    let src = "fn main() {\n\
                   let s = \"let us code\";\n\
                   let words: Vec[String] = s.split(\" \").map(|w| w.chars().rev().collect()).collect();\n\
                   let firsts: Vec[String] = s.split(\" \").map(|w| { let n = w.len(); w.chars().take(n - 1).collect() }).collect();\n\
                   let u: Vec[Set[char]] = s.split(\" \").map(|w| w.chars().collect()).collect();\n\
                   let v: Vec[Vec[char]] = s.split(\" \").map(|w| w.chars().collect()).collect();\n\
                   println(f\"{words.join(\" \")} {firsts} {u[1].len()} {u[2].len()} {v[0]}\");\n\
               }\n";
    accepted("map-closure-collect", src);
    let (dir, path) = fixture("map-closure-collect-run", src);
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "tel su edoc [le, u, cod] 2 4 [l, e, t]\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// B-2026-10-14-16 — `collect()` of non-`Copy` elements into a `Set`,
// `VecDeque` or `Map` annotation: the loop the desugar writes takes each
// element with `into_iter()`, since a bare `for` over a `Vec` lends them.
#[test]
fn collect_of_strings_into_set_deque_and_map() {
    let src = "fn main() {\n\
                   let words: Vec[String] = vec![\"b\".to_string(), \"a\".to_string(), \"b\".to_string()];\n\
                   let t: Set[String] = words.iter().map(|w| w.to_uppercase()).collect();\n\
                   let d: VecDeque[String] = words.iter().map(|w| w.to_uppercase()).collect();\n\
                   let m: Map[String, i64] = words.iter().map(|w| (w.to_uppercase(), 1)).collect();\n\
                   println(f\"{t.len()} {d.len()} {m.len()} {t.contains(\"A\")} {m.get(\"B\").is_some()}\");\n\
               }\n";
    accepted("collect-strings-into-set", src);
    let (dir, path) = fixture("collect-strings-into-set-run", src);
    let out = karac()
        .arg("run")
        .arg("--interp")
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "2 3 2 true true\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// B-2026-10-14-20 — `collect()` into a `SortedSet` or `SortedMap` annotation,
// the ordered twins of the `Set` and `Map` targets.
#[test]
fn collect_into_sorted_set_and_map() {
    let src = "fn main() {\n\
                   let v: Vec[i64] = vec![3, 1, 2, 3];\n\
                   let ss: SortedSet[i64] = v.iter().map(|x| x * 2).collect();\n\
                   let sm: SortedMap[i64, i64] = v.iter().map(|x| (x * 1, x * 10)).collect();\n\
                   let words: Vec[String] = vec![\"b\".to_string(), \"a\".to_string(), \"b\".to_string()];\n\
                   let ws: SortedSet[String] = words.iter().map(|w| w.to_uppercase()).collect();\n\
                   println(f\"{ss.len()} {ss.contains(4)} {sm.len()} {sm.get(2)} {ws.len()} {ws.contains(\"A\")}\");\n\
               }\n";
    accepted("collect-into-sorted", src);
    let (dir, path) = fixture("collect-into-sorted-run", src);
    let out = karac()
        .arg("run")
        .arg("--interp")
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "3 true 3 Some(20) 2 true\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// B-2026-10-14-12 — `Option.as_ref()` (docs/library/core-types.md) lends the
// payload, so a borrowed `Option` can be mapped without moving it.
#[test]
fn option_as_ref_is_accepted() {
    accepted(
        "option-as-ref",
        "struct Node { val: i64 }\n\
         fn depth(n: ref Node) -> i64 { n.val }\n\
         fn max_depth(root: Option[Node]) -> i64 { root.as_ref().map(|node| depth(node)).unwrap_or(0) }\n\
         fn main() {\n\
             let o: Option[Node] = Some(Node { val: 3 });\n\
             let n: Option[Node] = None;\n\
             println(f\"{o.as_ref().is_some()} {max_depth(o)} {max_depth(n)} {o.is_some()}\");\n\
         }\n",
    );
}

// B-2026-10-14-22 — `*n` through a view of a `shared` handle copies the
// handle and counts it (core-semantics §6.1), so it moves nothing out of
// the borrow; the same `*` on a plain struct still does.
#[test]
fn deref_of_a_shared_handle_view_is_a_copy() {
    accepted(
        "shared-handle-deref",
        "shared struct Node {\n\
             val: i64,\n\
         }\n\
         fn total(n: Option[Node]) -> i64 {\n\
             match n {\n\
                 Some(x) => x.val,\n\
                 None => 0,\n\
             }\n\
         }\n\
         fn main() {\n\
             let nodes: Vec[Node] = vec![Node { val: 1 }, Node { val: 2 }];\n\
             let mut best = nodes[0];\n\
             let mut sum = 0;\n\
             for n in nodes.iter() {\n\
                 sum += total(Some(*n));\n\
                 if n.val > best.val {\n\
                     best = *n;\n\
                 }\n\
             }\n\
             println(f\"{sum} {best.val}\");\n\
         }\n",
    );
    rejected_then_fixed_by(
        "plain-struct-deref",
        "struct Plain {\n\
             name: String,\n\
         }\n\
         fn main() {\n\
             let v: Vec[Plain] = vec![Plain { name: String.from(\"a\") }];\n\
             for p in v.iter() {\n\
                 let q = *p;\n\
                 println(q.name);\n\
             }\n\
         }\n",
        "out of a borrow with `*`",
        "let q = p;",
    );
}

/// B-2026-10-14-10 — v2 core §1.1: a `ref T` is `Copy`. Lending a bare `for`
/// element into a `Vec[ref Node]` (directly or as a tuple element) and
/// reading `v[0]` back out copy the reference and duplicate no `Node`, so
/// none of them is a move. A `Vec[Node]` still refuses both.
#[test]
fn refs_held_in_a_vec_of_refs_are_copies() {
    let header = "struct Node {\n\
             val: i64,\n\
             children: Vec[Node],\n\
         }\n\
         fn leaf(val: i64) -> Node {\n\
             Node { val: val, children: Vec.new() }\n\
         }\n";
    accepted(
        "vec-of-refs",
        &format!(
            "{header}\
             fn main() {{\n\
                 let t = Node {{ val: 1, children: vec![leaf(2), leaf(3)] }};\n\
                 let mut v: Vec[ref Node] = Vec.new();\n\
                 for c in t.children.iter() {{\n\
                     v.push(c);\n\
                 }}\n\
                 let a = v[0];\n\
                 let b = v[1];\n\
                 let mut stack: Vec[(ref Node, i64)] = Vec.new();\n\
                 for c in t.children.iter() {{\n\
                     stack.push((c, 1));\n\
                 }}\n\
                 let (d, depth) = stack[1];\n\
                 println(f\"{{a.val}} {{b.val}} {{d.val}} {{depth}}\");\n\
             }}\n"
        ),
    );
    for (tag, body, expect) in [
        (
            "vec-of-nodes-push",
            "let mut v: Vec[Node] = Vec.new();\n\
             for c in t.children.iter() {\n\
                 v.push(c);\n\
             }\n\
             println(f\"{v.len()}\");\n",
            // Against the library `c` is the `ref Node` the iterator lends.
            "expected 'Node', found 'ref Node'",
        ),
        (
            "vec-of-nodes-index",
            "let v: Vec[Node] = t.children;\n\
             let a = v[0];\n\
             println(f\"{a.val}\");\n",
            "E_INDEX_MOVE_NON_COPY",
        ),
    ] {
        let (dir, path) = fixture(
            tag,
            &format!(
                "{header}fn main() {{\n\
                 let t = Node {{ val: 1, children: vec![leaf(2), leaf(3)] }};\n\
                 {body}}}\n"
            ),
        );
        let out = karac().arg("check").arg(&path).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{tag}: check must reject: {err}");
        assert!(err.contains(expect), "{tag}: expected `{expect}` in: {err}");
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// B-2026-10-14-27 — design.md § `*` and `ref` in expressions: `==` and `<`
/// borrow `*r` and need no copy, so `*w == target` through a view of a
/// `String` moves nothing. A `*w` that IS a move (a `let` or a by-value
/// argument) is still refused.
#[test]
fn deref_of_a_view_compared_is_a_borrow() {
    accepted(
        "deref-compared",
        "enum Item {\n\
             File(String, i64),\n\
             Dir(String),\n\
         }\n\
         fn count(words: Vec[String], target: String) -> i64 {\n\
             let mut n = 0;\n\
             for w in words.iter() {\n\
                 if *w == target {\n\
                     n += 1;\n\
                 }\n\
                 if *w != target and *w < target {\n\
                     n += 10;\n\
                 }\n\
             }\n\
             n\n\
         }\n\
         fn find(es: ref Vec[Item], name: String) -> i64 {\n\
             for e in es.iter() {\n\
                 match e {\n\
                     Item.File(path, size) => {\n\
                         if *path == name {\n\
                             return *size;\n\
                         }\n\
                     }\n\
                     Item.Dir(path) => {\n\
                         if *path == \"/\" {\n\
                             return -1;\n\
                         }\n\
                     }\n\
                 }\n\
             }\n\
             0\n\
         }\n\
         fn main() {\n\
             let words = vec![\"a\".to_string(), \"b\".to_string(), \"0\".to_string()];\n\
             let es = vec![Item.Dir(\"x\".to_string()), Item.File(\"a\".to_string(), 7)];\n\
             println(f\"{count(words, \"a\".to_string())} {find(es, \"a\".to_string())}\");\n\
         }\n",
    );
    let (dir, path) = fixture(
        "deref-moved",
        "fn main() {\n\
             let words = vec![\"a\".to_string(), \"b\".to_string()];\n\
             for w in words.iter() {\n\
                 let s = *w;\n\
                 println(f\"{s}\");\n\
             }\n\
         }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "deref-moved: check must reject: {err}"
    );
    assert!(
        err.contains("out of a borrow with `*`"),
        "deref-moved: expected the move refusal in: {err}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// B-2026-10-14-28 — lending the same bare `for` element into a `ref` slot
/// more than once copies the reference each time (v2 core §1.1, §4.6), so
/// the ownership pass must not call the second lend a use after a move. An
/// owned local pushed twice into a `Vec[Node]` is still a move.
#[test]
fn a_view_lent_into_ref_slots_twice_is_not_moved() {
    let header = "struct Node {\n\
             val: i64,\n\
             children: Vec[Node],\n\
         }\n\
         fn leaf(val: i64) -> Node {\n\
             Node { val: val, children: Vec.new() }\n\
         }\n";
    accepted(
        "view-lent-twice",
        &format!(
            "{header}\
             fn main() {{\n\
                 let t = Node {{ val: 1, children: vec![leaf(2), leaf(3)] }};\n\
                 let mut v: Vec[ref Node] = Vec.new();\n\
                 let mut stack: Vec[(ref Node, i64)] = Vec.new();\n\
                 for c in t.children.iter() {{\n\
                     v.push(c);\n\
                     v.push(c);\n\
                     stack.push((c, 1));\n\
                     stack.push((c, 2));\n\
                     let r: ref Node = c;\n\
                     let s: ref Node = c;\n\
                     println(f\"{{r.val}} {{s.val}}\");\n\
                 }}\n\
                 println(f\"{{v.len()}} {{stack.len()}}\");\n\
             }}\n"
        ),
    );
    let (dir, path) = fixture(
        "owned-pushed-twice",
        &format!(
            "{header}\
             fn main() {{\n\
                 let t = leaf(1);\n\
                 let mut v: Vec[Node] = Vec.new();\n\
                 v.push(t);\n\
                 v.push(t);\n\
                 println(f\"{{v.len()}}\");\n\
             }}\n"
        ),
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "owned-pushed-twice: check must reject: {err}"
    );
    assert!(
        err.contains("moved here, used again here"),
        "owned-pushed-twice: expected the use-after-move in: {err}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// B-2026-10-12-14 — design.md § Strings: a `Str` "is `Copy`, and it borrows
/// exactly like a `ref`". Using a `Str` parameter twice, re-binding a `Str`
/// and reading `v[0]` out of a `Vec[Str]` move nothing.
#[test]
fn str_is_copy_under_the_ownership_pass() {
    accepted(
        "str-copy",
        "fn twice(s: Str) -> i64 {\n\
             let a = String.from(s);\n\
             let b = String.from(s);\n\
             a.len() + b.len()\n\
         }\n\
         fn main() {\n\
             let text = String.from(\"ab,cd\");\n\
             println(f\"{twice(text)}\");\n\
             let x: Str = text[0..2];\n\
             let y = x;\n\
             println(f\"{x} {y}\");\n\
             let v: Vec[Str] = vec![text[0..1], text[1..2]];\n\
             let first = v[0];\n\
             println(f\"{first} {v.len()}\");\n\
         }\n",
    );
}

/// B-2026-10-14-30 — design.md § Operator traits: `a + b` on a user type
/// calls its `impl Add`, so `karac check` accepts the impl and the operator,
/// and an `own self` impl moves its left operand like any by-value call.
#[test]
fn user_operator_impls_are_accepted() {
    let money = "#[derive(Copy, Clone)]\n\
         struct Money {\n\
             cents: i64,\n\
         }\n\
         impl Add for Money {\n\
             type Output = Money;\n\
             fn add(own self, rhs: own Money) -> Self.Output {\n\
                 Money { cents: self.cents + rhs.cents }\n\
             }\n\
         }\n";
    accepted(
        "user-add",
        &format!(
            "{money}\
             fn main() {{\n\
                 let a = Money {{ cents: 150 }};\n\
                 let mut c = a + Money {{ cents: 275 }};\n\
                 c += a;\n\
                 println(f\"{{c.cents}} {{a.cents}}\");\n\
             }}\n"
        ),
    );
    let (dir, path) = fixture(
        "user-add-moves",
        "struct Tag {\n\
             s: String,\n\
         }\n\
         impl Add for Tag {\n\
             fn add(own self, rhs: own Tag) -> Tag {\n\
                 Tag { s: self.s + rhs.s }\n\
             }\n\
         }\n\
         fn main() {\n\
             let a = Tag { s: String.from(\"a\") };\n\
             let c = a + Tag { s: String.from(\"b\") };\n\
             println(f\"{c.s} {a.s}\");\n\
         }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "user-add-moves: check must reject: {err}"
    );
    assert!(
        err.contains("value 'a' moved here, used again here"),
        "user-add-moves: expected the move of `a` in: {err}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// B-2026-10-14-34: a trait impl's own bare `self` and parameters borrow,
/// as an inherent impl's do, whatever the trait declares (core semantics
/// §4.7: an impl may be weaker than its trait, and a call on a concrete type
/// uses the impl's modes). The check used to give the impl the trait's
/// `own`, so it moved what the MIR pipeline borrows.
#[test]
fn trait_impl_bare_params_borrow() {
    accepted(
        "impl-bare-self",
        "trait Shrink {\n\
             fn shrink(own self) -> i64;\n\
         }\n\
         struct P {\n\
             x: i64,\n\
             s: String,\n\
         }\n\
         impl Shrink for P {\n\
             fn shrink(self) -> i64 {\n\
                 self.x - 1\n\
             }\n\
         }\n\
         fn main() {\n\
             let a = P { x: 2, s: String.from(\"hi\") };\n\
             let r = a.shrink();\n\
             println(f\"{r} {a.x} {a.s}\");\n\
         }\n",
    );
    let (dir, path) = fixture(
        "impl-bare-param",
        "trait Mk {\n\
             fn mk(v: own String) -> Self;\n\
         }\n\
         struct P {\n\
             s: String,\n\
         }\n\
         impl Mk for P {\n\
             fn mk(v: String) -> P {\n\
                 P { s: v }\n\
             }\n\
         }\n\
         fn main() {\n\
             let q = P.mk(String.from(\"yo\"));\n\
             println(f\"{q.s}\");\n\
         }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "impl-bare-param: check must reject: {err}"
    );
    assert!(
        err.contains("cannot give away parameter `v`"),
        "impl-bare-param: expected the borrowed `v` in: {err}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// B-2026-10-14-13 — an owned local pushed into a `ref` slot is lent, not
/// moved (v2 core §5.3): `v.push(t)` into a `Vec[ref Node]` leaves `t`
/// usable.
#[test]
fn an_owned_local_lent_into_a_ref_slot_is_not_moved() {
    accepted(
        "owned-lent-into-ref",
        "struct Node {\n\
             val: i64,\n\
             children: Vec[Node],\n\
         }\n\
         fn main() {\n\
             let t = Node { val: 1, children: Vec.new() };\n\
             let mut v: Vec[ref Node] = Vec.new();\n\
             v.push(t);\n\
             v.push(t);\n\
             println(f\"{v.len()} {v[0].val} {t.val}\");\n\
         }\n",
    );
}

/// B-2026-10-14-23: a closure that lends a capture to a closure-valued
/// local's `ref` parameter only reads it, as it would through a `fn` with
/// the same signature, so it can run any number of times. A capture handed
/// to a by-value parameter of that local is still consumed.
#[test]
fn a_capture_lent_through_a_closure_local_is_read() {
    accepted(
        "closure-local-lends",
        "fn twice(f: Fn(ref Vec[i64]) -> i64) -> i64 {\n\
             f(vec![1, 2]) + f(vec![3, 4])\n\
         }\n\
         fn main() {\n\
             let tree = vec![0, 0];\n\
             let nuts = vec![vec![1, 2], vec![3, 4]];\n\
             let far = |a: ref Vec[i64], b: ref Vec[i64]| (a[0] - b[0]).abs() + (a[1] - b[1]).abs();\n\
             let t: i64 = nuts.iter().map(|n| far(n, tree)).sum();\n\
             println(f\"{t} {twice(|n| far(n, tree))}\");\n\
         }\n",
    );
    let (dir, path) = fixture(
        "closure-local-takes",
        "fn twice(f: Fn(ref Vec[i64]) -> i64) -> i64 {\n\
             f(vec![1, 2]) + f(vec![3, 4])\n\
         }\n\
         fn main() {\n\
             let tree = vec![0, 0];\n\
             let eat = |a: ref Vec[i64], b: Vec[i64]| a[0] + b.len();\n\
             println(f\"{twice(|n| eat(n, tree))}\");\n\
         }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "closure-local-takes: check must reject: {err}"
    );
    assert!(
        err.contains("consumes captured binding 'tree'"),
        "closure-local-takes: expected the consumed `tree` in: {err}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// B-2026-10-14-25: design.md § Call-site `mut` markers marks an argument
/// to a `mut ref` / `mut Slice` parameter "whatever the argument's root",
/// so a field of a `mut ref` binding and a forwarded `mut ref` parameter
/// take the marker; `karac check` requires it on every such argument.
#[test]
fn a_marked_forward_of_a_mut_ref_is_accepted() {
    accepted(
        "marked-forward",
        "struct Session {\n\
             scores: Vec[i64],\n\
         }\n\
         fn sort_in_place(xs: mut Slice[i64]) {\n\
             xs.sort();\n\
         }\n\
         fn bump(n: mut ref i64) {\n\
             n += 1;\n\
         }\n\
         fn twice(n: mut ref i64) {\n\
             bump(mut n);\n\
             bump(mut n);\n\
         }\n\
         fn refresh(s: mut ref Session) {\n\
             sort_in_place(mut s.scores);\n\
         }\n\
         fn main() {\n\
             let mut s = Session { scores: vec![3, 1, 2] };\n\
             refresh(mut s);\n\
             let mut k = 1;\n\
             twice(mut k);\n\
             println(f\"{s.scores} {k}\");\n\
         }\n",
    );
}

/// B-2026-10-14-33: user `Neg` and `Not` impls are accepted on `karac check`,
/// and `-t` through an `own self` impl moves `t`.
#[test]
fn user_neg_and_not_impls_are_accepted() {
    let src = r#"
#[derive(Copy, Clone)]
struct V2 {
    x: i64,
    y: i64,
}

impl Neg for V2 {
    type Output = V2;
    fn neg(own self) -> Self.Output {
        V2 { x: -self.x, y: -self.y }
    }
}

struct Mask {
    bits: i64,
    name: String,
}

impl Not for Mask {
    fn not(self) -> Mask {
        Mask { bits: ~self.bits, name: self.name.clone() }
    }
}

struct Tag {
    s: String,
}

impl Neg for Tag {
    fn neg(own self) -> Tag {
        Tag { s: self.s + "!" }
    }
}

fn main() {
    let a = V2 { x: 1, y: -2 };
    let b = -a;
    let m = Mask { bits: 5, name: "m" };
    let n = ~m;
    let t = Tag { s: "hi" };
    let u = -t;
    println(f"{b.x} {b.y} {a.x} {n.bits} {m.name} {u.s}");
}
"#;
    accepted("user-neg-not", src);
    let moved = src.replace("{u.s}\");", "{u.s} {t.s}\");");
    assert_ne!(moved, src, "the probe edit must apply");
    let (dir, path) = fixture("user-neg-moves", &moved);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "user-neg-moves: check must reject: {err}"
    );
    assert!(
        err.contains("value 't' moved here, used again here"),
        "user-neg-moves: expected the move of `t` in: {err}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

// B-2026-10-14-40: a `ref` of a `Copy` `Option` / `Result` is read as the
// wrapper on the left of `??` (design.md § `*` and `ref` in expressions).
#[test]
fn coalesce_reads_a_ref_of_a_copy_wrapper() {
    let src = r#"
fn f(o: mut ref Option[i64]) -> i64 { o ?? 5 }

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
    accepted("coalesce-ref", src);
}

// B-2026-10-14-38: `ref <place>` returns a borrow of the place, pinned when
// its root is a borrowed parameter or a `ref` local, and assigning one `ref`
// local from another copies the reference (v2 core §1.1, §5.1). A borrow of
// an owned local or an `own` parameter still dangles.
#[test]
fn borrow_returns_of_a_ref_place_are_accepted() {
    accepted(
        "ref-place-return",
        r#"
struct User { name: String, age: i64 }
struct Node { val: i64, next: Option[Node] }
struct List { head: Node }
impl List {
    fn head(self) -> ref Node { ref self.head }
    fn last(self) -> ref Node {
        let mut node = ref self.head;
        while let Some(n) = node.next { node = n; }
        node
    }
}
fn name(user: User) -> ref String { ref user.name }
fn main() {
    let u = User { name: "ada", age: 36 };
    println(f"{name(u)} {u.age}");
    let l = List { head: Node { val: 1, next: Some(Node { val: 2, next: Some(Node { val: 3, next: None }) }) } };
    println(f"{l.head().val} {l.last().val}");
}
"#,
    );
    let (dir, path) = fixture(
        "ref-place-dangles",
        r#"
struct User { name: String, age: i64 }
fn bad(n: i64) -> ref String { let s = f"x{n}"; ref s }
fn bad2(u: own User) -> ref String { ref u.name }
fn bad3(u: User) -> ref String { let v = User { name: "q", age: 1 }; let r = ref v.name; r }
fn main() {
    let u = User { name: "ada", age: 36 };
    println(bad(1));
    println(bad2(u));
}
"#,
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    for at in ["3:49", "4:38", "5:90"] {
        assert!(
            err.contains(&format!("{at}: returned borrow does not originate")),
            "{at}: {err}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// B-2026-10-14-37: a user type's `impl Index` / `impl IndexMut` defines
// `c[k]` and `c[k] = v` (design.md § `Index` and `IndexMut`). A move out of
// the element and a write through a type with only `Index` stay errors.
#[test]
fn user_index_impls_are_accepted() {
    accepted(
        "user-index",
        r#"struct Grid { w: i64, cells: Vec[i64] }
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
"#,
    );
    let (dir, path) = fixture(
        "user-index-refused",
        r#"struct Names { all: Vec[String] }
impl Index[i64] for Names {
    type Output = String;
    fn index(self, i: i64) -> ref String { ref self.all[i] }
}
fn main() {
    let mut n = Names { all: vec!["ada".to_string()] };
    let s = n[0];
    n[0] = "x".to_string();
    println(s);
}
"#,
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("8:13: error[E_INDEX_MOVE_NON_COPY]"), "{err}");
    assert!(
        err.contains("9:5: `Names` implements `Index` but not `IndexMut`"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `?` is postfix, so `f()?.x` is `(f()?).x` and `f()?.m()` is `(f()?).m()`
/// (design.md § Results and propagation; B-2026-10-14-50). The check took
/// `?.` for the removed optional-chaining operator and refused it on every
/// `Result`; on an `Option` it is propagation too, not a chained `Option`.
#[test]
fn question_then_member_access_propagates() {
    let src = "fn f() -> Result[Vec[i64], String] { Ok(vec![1, 2]) }\n\
               fn h(ok: bool) -> Result[String, String] {\n\
                   if ok { Ok(\"abc\".to_string()) } else { Err(\"no\".to_string()) }\n\
               }\n\
               struct P { x: i64 }\n\
               fn mk(some: bool) -> Option[P] { if some { Some(P { x: 7 }) } else { None } }\n\
               fn g() -> Result[i64, String] {\n\
                   let n = f()?.len();\n\
                   Ok(n + h(true)?.len())\n\
               }\n\
               fn k(ok: bool) -> Result[i64, String] { Ok(h(ok)?.to_uppercase().len()) }\n\
               fn o(some: bool) -> Option[i64] { Some(mk(some)?.x + 1) }\n\
               fn main() {\n\
                   println(f\"{g().unwrap()}\");\n\
                   println(f\"{k(true).unwrap()}\");\n\
                   match k(false) { Ok(_) => println(\"?\"), Err(e) => println(e) }\n\
                   println(f\"{o(true).unwrap()} {o(false).is_none()}\");\n\
               }\n";
    let (dir, path) = fixture("question-member", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must accept `?.` as `?` then `.`: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "5\n3\nno\n8 true\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The library module paths of docs/library/README.md resolve to the
/// globals the implementation keeps (B-2026-10-14-48): `import std.sync`,
/// `std.io`, `std.net`, `std.hash` check and run. A specified name that is
/// not implemented yet, an unknown one and a rename each get their own
/// error rather than "unknown module".
#[test]
fn library_module_paths_resolve() {
    let src = "import std.sync.{TaskGroup, Sender, Receiver, Mutex};\n\
               import std.io.{File, BufReader};\n\
               import std.net.{TcpListener, TcpError};\n\
               import std.hash.{FxBuildHasher};\n\
               fn main() {\n\
                   let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();\n\
                   tx.send(4);\n\
                   println(f\"{rx.recv().unwrap()}\");\n\
               }\n";
    let (dir, path) = fixture("library-paths", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must accept the library paths: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "4\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);

    let src = "import std.sync.{MutexGuard, Mutx};\n\
               import std.io.{File as F};\n\
               fn main() {}\n";
    let (dir, path) = fixture("library-paths-bad", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    for want in [
        "`std.sync.MutexGuard` is in the library spec (docs/library) but not implemented yet",
        "`std.sync` has no item `Mutx`, did you mean `Mutex`?",
        "`std.io.File` cannot be renamed (`as F`) yet",
    ] {
        assert!(err.contains(want), "want {want:?} in\n{err}");
    }
    assert!(!err.contains("unknown module"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-14-61: a stored function value is called through a `ref`
/// (design.md § Function values), as a parameter and as an item of `iter()`.
#[test]
fn a_function_value_is_called_through_a_ref() {
    accepted(
        "fn-through-ref",
        "fn dbl(n: i64) -> i64 { n * 2 }\n\
         fn apply(f: ref Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
         fn main() {\n\
             let fs: Vec[Fn(i64) -> i64] = vec![dbl];\n\
             let mut t = 0;\n\
             for f in fs.iter() { t += f(10); }\n\
             println(f\"{t} {apply(fs[0], 7)}\");\n\
         }\n",
    );
}

/// B-2026-10-14-59: a closure's early `return None` agrees with a tail
/// `Some(..)`, and an early `return Err(..)` with a tail `Ok(..)`.
#[test]
fn a_closure_early_return_agrees_with_its_tail() {
    accepted(
        "closure-early-return",
        "fn main() {\n\
             let lim = 3;\n\
             let f = |n: i64| { if n < 0 { return None; } if n > lim { Some(f\"big {n}\") } else { None } };\n\
             let g = |n: i64| { if n == 0 { return Err(\"zero\".to_string()); } Ok(10 / n) };\n\
             println(f\"{f(5).unwrap()} {g(2).unwrap()}\");\n\
         }\n",
    );
}

/// docs/library/concurrency.md's channel and `TaskGroup` surface checks
/// (B-2026-10-15-9, B-2026-10-15-11): `channel(cap)` with the element type
/// pinned by use, `send` as a `Result`, `recv` as an `Option`, a receiver
/// drained by `into_iter`, `TaskGroup.new(limit: n)` and a returned
/// `TaskHandle`.
#[test]
fn channels_and_task_groups_check_as_specified() {
    accepted(
        "channel-surface",
        "fn producer(tx: own Sender[i64]) -> Result[(), SendError[i64]] {\n\
             tx.send(5)?;\n\
             Ok(())\n\
         }\n\
         fn start(group: ref TaskGroup, n: i64) -> TaskHandle[i64] {\n\
             group.spawn(|| n * 2)\n\
         }\n\
         fn main() {\n\
             let (tx, rx) = channel(8);\n\
             let _ = producer(tx);\n\
             match rx.recv() { Some(v) => println(v), None => println(\"closed\") }\n\
             for v in rx.into_iter() { println(v); }\n\
             let g = TaskGroup.new(limit: 4);\n\
             let h = start(g, 21);\n\
             println(h.join());\n\
         }\n",
    );
    // `channel(0)` is a compile error.
    let (dir, path) = fixture(
        "channel-zero",
        "fn main() {\n    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(0);\n}\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "channel(0) must be refused: {err}");
    assert!(err.contains("at least 1 value"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `recv` used where the legacy `T` was wanted is fixed with `.unwrap()`,
/// and `karac fix` rewrites the deprecated `Channel.bounded(n)` to
/// `channel(n)` (B-2026-10-15-11).
#[test]
fn legacy_channel_spellings_are_fixed() {
    rejected_then_fixed_by(
        "recv-as-t",
        "fn main() {\n\
             let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.bounded(4);\n\
             tx.send(3).unwrap();\n\
             let x: i64 = rx.recv();\n\
             println(x);\n\
         }\n",
        "`recv` returns `Option[T]`",
        "rx.recv().unwrap()",
    );
    let (dir, path) = fixture(
        "bounded-to-channel",
        "fn main() {\n\
             let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.bounded(  4);\n\
             tx.send(3).unwrap();\n\
             println(rx.recv().unwrap());\n\
         }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("deprecated item `Channel.bounded`"), "{err}");
    karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(fixed.contains("= channel(4);"), "{fixed}");
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && !err.contains("deprecated"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-15-76 — an operator impl's `type Output` is the operator's
/// result type, so a method returning something else is refused rather than
/// letting `a - b` and `T.Output` disagree.
#[test]
fn an_operator_method_must_return_its_impls_output() {
    let (dir, path) = fixture(
        "operator-output",
        "struct Duration { ns: i64 }\n\
         struct Instant { ns: i64 }\n\
         impl Sub for Instant {\n\
             type Output = Duration;\n\
             fn sub(own self, rhs: own Instant) -> Instant { Instant { ns: self.ns - rhs.ns } }\n\
         }\n\
         fn main() {\n\
             let d = Instant { ns: 5 } - Instant { ns: 2 };\n\
             println(d.ns);\n\
         }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "the mismatch must be refused: {err}");
    assert!(
        err.contains("`sub` returns `Instant`, but this impl's `type Output` is `Duration`"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A group passed in borrowed outlives the call, so a task spawned into it
/// takes the function's locals by move rather than borrowing them past
/// their scope (core-semantics.md §9.5; B-2026-10-15-11): using one after
/// the spawn is E0500.
#[test]
fn a_task_of_a_borrowed_group_takes_locals_by_move() {
    let (dir, path) = fixture(
        "param-group-local",
        "fn start(group: ref TaskGroup) -> TaskHandle[i64] {\n\
             let v: Vec[i64] = [1, 2, 3];\n\
             let h = group.spawn(|| v.len());\n\
             println(v.len());\n\
             h\n\
         }\n\
         fn main() {\n\
             let g = TaskGroup.new();\n\
             println(start(g).join());\n\
         }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "the use after the spawn must be refused: {err}"
    );
    assert!(err.contains("moved into a task of `group`"), "{err}");
    // No local can be declared before a parameter, so the advice names the
    // repairs that exist (B-2026-10-15-72).
    assert!(
        err.contains("`group` is a parameter, so every local drops before it"),
        "{err}"
    );
    assert!(err.contains("take 'v' as a parameter too"), "{err}");
    assert!(!err.contains("Declare 'v' before"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-15-25: an annotation sets one closure parameter's mode, as on a
/// `fn` (design.md § Function values): `|v: own Vec[i64]| ..`.
#[test]
fn a_closure_parameter_annotation_sets_its_mode() {
    accepted(
        "closure-param-own",
        "fn main() {\n\
             let f: Fn(own Vec[i64]) -> i64 = |v: own Vec[i64]| v.len();\n\
             let g = |w: own Vec[i64], k: i64| w.len() + k;\n\
             let h = |x: ref Vec[i64]| x.len();\n\
             let v: Vec[i64] = Vec.new();\n\
             println(h(v));\n\
             println(f(v));\n\
             let u: Vec[i64] = vec![1];\n\
             println(g(u, 10));\n\
         }\n",
    );
}

/// B-2026-10-15-29: a type may share a name with an enum variant whichever
/// is declared first; the variant is reached through its enum.
#[test]
fn a_type_after_an_enum_may_share_a_variant_name() {
    accepted(
        "variant-then-type",
        "enum Block { Heading(i64), Rule }\n\
         struct Heading { level: i64 }\n\
         enum Shape { Circle(Circle), Dot }\n\
         struct Circle { r: i64 }\n\
         fn main() {\n\
             let h = Heading { level: 3 };\n\
             match Block.Heading(2) {\n\
                 Block.Heading(n) => println(f\"{n} {h.level}\"),\n\
                 Block.Rule => println(\"rule\"),\n\
             }\n\
             match Shape.Circle(Circle { r: 4 }) {\n\
                 Shape.Circle(c) => println(c.r),\n\
                 Shape.Dot => println(\"dot\"),\n\
             }\n\
         }\n",
    );
}

/// B-2026-10-15-19: a `String` passes as a `Str` parameter of a method called through a trait bound, as it does on the concrete type.
#[test]
fn check_string_arg_to_a_str_param_through_a_trait_bound() {
    accepted(
        "str-through-bound",
        r#"
trait Store {
    fn get(self, key: Str) -> Option[i64];
}
struct Mem { n: i64 }
impl Store for Mem {
    fn get(self, key: Str) -> Option[i64] { if key == "a" { Some(self.n) } else { None } }
}
fn look[S: Store](s: ref S, k: ref String) -> i64 {
    s.get(k).unwrap_or(0)
}
fn main() {
    let m = Mem { n: 7 };
    let k = "a".to_string();
    println(f"{m.get(k)} {look(m, k)}");
}
"#,
    );
}

/// B-2026-10-15-37: a local or parameter named `ast` or `compiler` shadows the comptime module.
#[test]
fn check_a_local_named_ast_or_compiler_shadows_the_comptime_module() {
    accepted(
        "ast-param",
        r#"
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
"#,
    );
}

/// B-2026-10-14-66: width and alignment apply to a `Str` hole.
#[test]
fn check_format_width_on_a_str_hole() {
    accepted(
        "str-width",
        r#"
fn row(name: Str, n: i64) -> String { f"[{name:<6}|{name:>5}|{name:^7}|{n:>4}]" }
fn main() {
    let c: Str = "abc";
    println(f"[{c:<6}]");
    let s = "hello".to_string();
    println(row(s, 7));
    println(row("ab", 12));
}
"#,
    );
}

/// B-2026-10-15-22: `let mut t = since` with `since: ref i64` is a counter `t = t + 1` writes.
#[test]
fn check_let_mut_of_a_ref_to_a_copy_value_is_a_counter() {
    accepted(
        "let-mut-ref-copy",
        r#"
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
"#,
    );
}

/// B-2026-10-18-10: a program's own `Client`, `Response`, `HttpError` or
/// `RequestBuilder` is its own type, so its methods are found; an error
/// names it as written.
#[test]
fn check_a_user_type_named_like_the_http_client_is_its_own() {
    accepted(
        "user-http-names",
        r#"
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
"#,
    );
    let (dir, path) = fixture(
        "user-http-names-err",
        r#"
struct Client { out: String }
impl Client {
    fn error(mut ref self, msg: Str) { self.out.push_str(msg); }
}
fn main() {
    let mut c = Client { out: String.new() };
    c.error("x");
    let bad: i64 = c;
}
"#,
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "user-http-names-err: check must reject: {err}"
    );
    assert!(
        err.contains("expected 'i64', found 'Client'") && err.contains("1 error(s)"),
        "user-http-names-err: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-15-41: a program's own `Range` (or `IoError`) is its own type,
/// and range literals keep the library's; an error names it as written.
#[test]
fn check_a_user_type_named_range_is_its_own() {
    accepted(
        "user-range",
        r#"
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
"#,
    );
    let (dir, path) = fixture(
        "user-range-err",
        r#"
struct Range { lo: i64, hi: i64 }
fn main() {
    let r = Range { lo: 1, hi: 3 };
    let bad: i64 = r;
    for i in 0..2 { println(f"{i}"); }
}
"#,
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "user-range-err: check must reject: {err}"
    );
    assert!(
        err.contains("expected 'i64', found 'Range'") && err.contains("1 error(s)"),
        "user-range-err: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-14-67: `join` and `concat` take any element with a `Display`
/// form; an element without one is refused, naming `Display`.
#[test]
fn check_join_of_display_elements() {
    accepted(
        "join-str-num",
        r#"
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
"#,
    );
    accepted(
        "join-display",
        r#"
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
"#,
    );
    let (dir, path) = fixture(
        "join-no-display",
        r#"
struct Q { x: i64 }
fn main() {
    let q = vec![Q { x: 1 }];
    println(q.join(","));
}
"#,
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && err.contains("requires elements that implement Display"),
        "join-no-display: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_a_serde_program_reads_the_library() {
    // B-2026-10-12-23: `#[derive(Serialize)]` and `Json` live only in the Kāra
    // library, so `karac check` reads it for a serde program, as the build
    // does, and a derive's own error is reported at the user's type.
    accepted(
        "serde-accepted",
        r#"
#[derive(Serialize, Deserialize)]
struct Config {
    #[serde(rename: "server_name")]
    name: String,
    #[serde(default: 8080)]
    port: i64,
    shapes: Vec[Shape],
}
#[derive(Serialize, Deserialize)]
enum Shape { Dot, Circle(i64), Named { label: String } }
fn main() {
    let c = Config { name: "a".to_string(), port: 1, shapes: vec![Shape.Dot, Shape.Circle(2)] };
    let s = Json.serialize(c);
    println(s);
    let back: Result[Config, DeserializeError] = Json.deserialize(s);
    match back {
        Ok(c) => println(c.port),
        Err(e) => println(e.message),
    }
}
"#,
    );
    rejected(
        "serde-tagged-tuple",
        r#"
#[derive(Serialize)]
#[serde(tag: "type")]
enum Bad { A(i64) }
fn main() { println(Json.serialize(Bad.A(1))); }
"#,
        "`Bad.A` is a tuple variant",
    );
}

#[test]
fn check_string_literal_into_str_slots() {
    // B-2026-10-14-68: a literal is a `Str` in a method argument, a tuple
    // after a `Str` element and a tuple inside `Some`.
    accepted(
        "lit-str-slots",
        r#"
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
"#,
    );
}

#[test]
fn check_variant_constructed_through_a_type_alias() {
    // B-2026-10-15-50: a path through a type alias names the target's
    // variants, in a call, a struct-shaped literal and a unit variant.
    accepted(
        "alias-variant",
        r#"
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
"#,
    );
}

#[test]
fn check_struct_update_with_a_call_base() {
    // B-2026-10-15-47: `..base()` is any expression of the struct's type.
    accepted(
        "spread-call-base",
        r#"
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
"#,
    );
}

#[test]
fn check_zip_and_chain_items_are_lent() {
    // B-2026-10-15-1: the items of `a.iter().zip(b.iter())` and
    // `a.iter().chain(b.iter())` are views, as `enumerate`'s are; in a zip
    // with an owning side, only the lent half is.
    accepted(
        "zip-chain-lent",
        r#"
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
"#,
    );
    rejected(
        "zip-owned-half",
        r#"
fn main() {
    let a: Vec[i64] = vec![1, 2, 3];
    let b: Vec[i64] = vec![4, 5, 6];
    let mut d = 0;
    for (x, y) in a.iter().zip(b.into_iter()) { d += x + *y; }
    println(f"{d}");
}
"#,
        "unary '*' requires",
    );
}

#[test]
fn check_write_through_a_read_only_slice() {
    // B-2026-10-15-61: `bytes()`, an unmarked range subscript and a
    // `Slice[T]` parameter are read-only views; writing an element needs a
    // `mut Slice[T]`. Both backends used to write into the source's buffer.
    let expect = "read-only `Slice`, a view of another value's elements";
    rejected(
        "slice-write-bytes",
        r#"
fn main() {
    let b = String.from("hello");
    let mut t = b.bytes();
    t[4] = 97;
    println(f"{b} {String.from_utf8(t).unwrap()}");
}
"#,
        expect,
    );
    rejected(
        "slice-write-range",
        r#"
fn main() {
    let v: Vec[i64] = vec![1, 2, 3];
    let mut s = v[0..2];
    s[1] += 4;
    println(f"{v}");
}
"#,
        expect,
    );
    rejected(
        "slice-write-param",
        r#"
struct P { x: i64 }
fn set(ps: Slice[P]) {
    ps[0].x = 5;
}
fn main() {
    let mut ps: Vec[P] = vec![P { x: 1 }];
    set(ps[0..1]);
    println(f"{ps[0].x}");
}
"#,
        expect,
    );
    accepted(
        "slice-write-mut",
        r#"
fn bump(s: mut Slice[i64]) {
    s[0] += 9;
}
fn main() {
    let mut v: Vec[i64] = vec![1, 2, 3];
    bump(mut v[1..3]);
    let b = String.from("hello");
    let mut t = b.bytes().to_vec();
    t[4] = 97;
    let w = v[0..2];
    println(f"{v} {w[1]} {b} {String.from_utf8(t).unwrap()}");
}
"#,
    );
}

#[test]
fn check_unsuffixed_literal_beside_bitwise_and_in_branches() {
    // B-2026-10-15-51 / -52 / -53: a literal takes its peer's integer type
    // beside `& | ^` and in an `if` or `match` branch; two different integer
    // types under `& | ^` are refused as under `+`.
    accepted(
        "literal-peer-type",
        r#"
fn f(c: bool, y: u128) -> u128 {
    let x = if c { 0 } else { y };
    let w = match y { 0 => 1, _ => x };
    w
}
fn main() {
    let c: u8 = 20;
    let d: u8 = 0xC0 ^ c;
    let x: u8 = 0x80 | (c & 0x3F);
    let mut out: Vec[u8] = Vec.new();
    out.push(0xC0 | (c >> 6));
    println(f"{d} {x} {out[0]} {f(true, 5)}");
}
"#,
    );
    rejected(
        "bitwise-mixed-types",
        r#"
fn f(a: u128, b: i64) -> u128 {
    a & b
}
fn main() { println(f"{f(12, 4)}"); }
"#,
        "cannot mix integer types 'u128' and 'i64' in a bitwise operation",
    );
    rejected(
        "bitwise-literal-out-of-range",
        r#"
fn g(y: u8) -> u8 {
    300 | y
}
fn main() { println(f"{g(1)}"); }
"#,
        "integer literal 300 out of range for 'u8'",
    );
}

#[test]
fn check_arithmetic_literal_must_fit_its_peer() {
    // B-2026-10-17-1: an unsuffixed literal beside `+ - * / %` takes its
    // peer's type and must fit it; `300 + y` over `y: u8` wrapped to 45.
    rejected(
        "arith-literal-add",
        r#"
fn h(y: u8) -> u8 {
    300 + y
}
fn main() { println(f"{h(1)}"); }
"#,
        "integer literal 300 out of range for 'u8'",
    );
    rejected(
        "arith-literal-mul",
        r#"
fn k(y: u8) -> u8 {
    y * 1000
}
fn main() { println(f"{k(1)}"); }
"#,
        "integer literal 1000 out of range for 'u8'",
    );
    accepted(
        "arith-literal-fits",
        r#"
fn h(y: u8) -> u8 {
    255 - y
}
fn main() {
    let c: u8 = 7;
    let big = c < 300;
    println(f"{h(1)} {c * 30} {big}");
}
"#,
    );
}

#[test]
fn check_string_literal_patterns_on_a_str_scrutinee() {
    // B-2026-10-14-43: `match s.as_str() { "Year" => .. }` and a match on a
    // `Str` parameter were refused with "expected 'StringSlice', found
    // 'String'" at each literal arm.
    accepted(
        "str-literal-pattern",
        r#"
fn size(name: String) -> i64 {
    match name.as_str() {
        "Year" => 4,
        _ => 19,
    }
}
fn oid_name(oid: Str) -> String {
    match oid {
        "2.5.4.6" => "C".to_string(),
        _ => "?".to_string(),
    }
}
fn main() {
    println(f"{size(String.from("Year"))} {oid_name("2.5.4.6")}");
}
"#,
    );
}

#[test]
fn check_array_reaches_slice_read_methods() {
    // B-2026-10-15-60: `a.to_vec()` on an `Array[T, N]` was refused with
    // "no method 'to_vec' on type 'Array'".
    accepted(
        "array-to-vec",
        r#"
const MAGIC: Array[i64, 3] = [1, 2, 3];
fn main() {
    let a: Array[i64, 3] = [4, 5, 6];
    println(f"{a.to_vec().len()} {MAGIC.to_vec() == vec![1, 2, 3]}");
}
"#,
    );
}

#[test]
fn check_chained_field_receiver() {
    // B-2026-10-14-70: `self.env.to.push(..)` and `v.a.nodes[i].kind` were
    // refused with the legacy backend's `chained_field_receiver` deferral,
    // though every v2 surface runs them.
    accepted(
        "chained-field-receiver",
        r#"
struct Env { to: Vec[String] }
struct S { env: Env }
impl S {
    fn add(mut ref self, a: Str) -> i64 {
        self.env.to.push(a.to_string());
        self.env.to.len()
    }
}
struct Node { kind: i64, kids: Vec[i64] }
struct Arena { nodes: Vec[Node] }
struct V { a: Arena }
fn kind_of(v: ref V, i: usize) -> i64 {
    v.a.nodes[i].kind + v.a.nodes[i].kids.len() as i64
}
fn main() {
    let mut s = S { env: Env { to: Vec.new() } };
    println(f"{s.add("x")} {s.add("y")}");
    let v = V { a: Arena { nodes: vec![Node { kind: 7, kids: vec![1, 2] }] } };
    println(f"{kind_of(v, 0)}");
}
"#,
    );
}

#[test]
fn check_concrete_impl_on_a_generic_type() {
    // B-2026-09-27-115 / B-2026-10-01-48: `self` in `impl G[R]` was the bare
    // `G`, so its payload's fields and methods, an inferring `let` of the
    // payload, and sibling methods were refused.
    accepted(
        "concrete-impl-generic-type",
        r#"
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
"#,
    );
}

#[test]
fn check_vec_of_an_ord_element_is_ord() {
    // B-2026-10-06-92: design.md's `impl[T: Ord] Ord for Vec[T]`.
    accepted(
        "vec-is-ord",
        r#"
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
"#,
    );
}

#[test]
fn deref_of_an_item_of_a_named_iter_is_accepted_and_runs() {
    // B-2026-10-13-6: `let mut it = c.iter();` is the same lent iterator
    // `c.iter()` is, so `*k` reads a `Copy` item its `next()` gives, as the
    // MIR backends already ran it. A name rebound to another iterator loses
    // that: a range's items are values.
    let src = "fn main() {\n\
         \x20   let w = vec![4, 6, 9];\n\
         \x20   let mut it = w.iter();\n\
         \x20   let Some(k) = it.next() else { return; };\n\
         \x20   let mut c: VecDeque[i64] = VecDeque.new();\n\
         \x20   c.push_back(5);\n\
         \x20   c.push_back(7);\n\
         \x20   let mut ci = c.iter();\n\
         \x20   let mut sum = 0;\n\
         \x20   while let Some(x) = ci.next() {\n\
         \x20       sum += *x;\n\
         \x20   }\n\
         \x20   println(f\"{*k} {sum}\");\n\
         }\n";
    let (dir, path) = fixture("named-iter-deref", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must accept: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "4 12\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    let src = "fn main() {\n\
         \x20   let w = vec![4, 6, 9];\n\
         \x20   let it = w.iter();\n\
         \x20   let mut it = (0..3).into_iter();\n\
         \x20   let Some(k) = it.next() else { return; };\n\
         \x20   println(f\"{*k}\");\n\
         }\n";
    let (dir, path) = fixture("named-iter-rebound", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must refuse: {err}");
    assert!(err.contains("unary '*' requires"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_conditional_impl_bound_is_checked_at_a_generic_call() {
    // B-2026-10-13-5: `impl[T: Show] Show for Wrap[T]` makes `Wrap[X]` a
    // `Show` only when `X` is one, so `print_it(Wrap { v: Plain { .. } })`
    // is refused at check rather than stopping the MIR builder.
    let head = "trait Show { fn show(ref self) -> String; }\n\
         impl Show for i64 { fn show(ref self) -> String { \"i\".to_string() } }\n\
         struct Wrap[T] { v: T }\n\
         impl[T: Show] Show for Wrap[T] { fn show(ref self) -> String { f\"w{self.v.show()}\" } }\n\
         struct Plain { n: i64 }\n\
         fn print_it[X: Show](x: ref X) { println(x.show()); }\n";
    let bad = format!(
        "{head}fn main() {{\n\
         \x20   print_it(Wrap {{ v: 3 }});\n\
         \x20   print_it(Wrap {{ v: Plain {{ n: 1 }} }});\n\
         }}\n"
    );
    let (dir, path) = fixture("cond-impl-bound", &bad);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must refuse: {err}");
    assert!(
        err.contains("p.kara:9:"),
        "the refusal names the call: {err}"
    );
    assert!(err.contains("Show"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
    let good = format!(
        "{head}impl Show for Plain {{ fn show(ref self) -> String {{ f\"p{{self.n}}\" }} }}\n\
         fn main() {{\n\
         \x20   print_it(Wrap {{ v: 3 }});\n\
         \x20   print_it(Wrap {{ v: Wrap {{ v: Plain {{ n: 1 }} }} }});\n\
         }}\n"
    );
    let (dir, path) = fixture("cond-impl-bound-ok", &good);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must accept: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "wi\nwwp1\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
fn map_keys_and_values_are_lent_iterators() {
    // B-2026-10-13-1: docs/library gives `keys` / `values` as iterators of
    // borrowed items, so `next`, `count`, `max` and `rev` (on a `SortedMap`)
    // apply and nothing is copied.
    let src = "fn main() {\n\
         \x20   let mut m: Map[i64, i64] = Map.new();\n\
         \x20   m.insert(5, 50);\n\
         \x20   m.insert(7, 70);\n\
         \x20   let mut s: SortedMap[i64, String] = SortedMap.new();\n\
         \x20   s.insert(3, \"c\".to_string());\n\
         \x20   s.insert(1, \"a\".to_string());\n\
         \x20   let Some(first) = s.keys().next() else { return; };\n\
         \x20   let best = *m.values().max().unwrap();\n\
         \x20   let mut n = 0;\n\
         \x20   for k in m.keys() { n += *k; }\n\
         \x20   let last = s.values().rev().next().unwrap();\n\
         \x20   println(f\"{*first} {best} {n} {m.keys().count()} {last}\");\n\
         }\n";
    let (dir, path) = fixture("map-keys-iter", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must accept: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "1 70 12 2 c\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    // Where a `Vec` is wanted, the fix collects the borrowed items.
    rejected_then_fixed_by(
        "map-keys-into-vec",
        "fn main() {\n\
             let mut m: Map[String, i64] = Map.new();\n\
             m.insert(\"a\", 1);\n\
             let ks: Vec[String] = m.keys();\n\
             println(ks.len());\n\
         }\n",
        "an iterator: collect its items",
        "m.keys().cloned().collect()",
    );
}

#[test]
fn a_str_is_copy() {
    // B-2026-10-12-14: design.md § Strings makes `Str` `Copy`, so a `Str`
    // parameter used twice, a rebind of a `Str`, and a read of a `Vec[Str]`
    // element are all copies, not moves.
    accepted(
        "str-is-copy",
        "fn twice(s: Str) -> i64 {\n\
             let a = String.from(s);\n\
             let b = String.from(s);\n\
             a.len() + b.len()\n\
         }\n\
         fn main() {\n\
             let text = String.from(\"ab,cd\");\n\
             let x: Str = text[0..2];\n\
             let y = x;\n\
             println(f\"{twice(text)} {x} {y}\");\n\
             let v: Vec[Str] = vec![\"p\", \"q\"];\n\
             let first = v[0];\n\
             println(f\"{first} {v.len()}\");\n\
         }\n",
    );
}

#[test]
fn map_get_mut_lends_the_value_to_change() {
    // B-2026-10-12-3: `Map.get_mut` and `SortedMap.get_mut`, as
    // docs/library/collections.md gives them, return `Option[mut ref V]`.
    let src = "struct Bag { positions: Map[i64, Vec[i64]] }\n\
         fn main() {\n\
         \x20   let mut b = Bag { positions: Map.new() };\n\
         \x20   b.positions.insert(3, vec![1]);\n\
         \x20   match b.positions.get_mut(3) {\n\
         \x20       Some(list) => list.push(2),\n\
         \x20       None => {}\n\
         \x20   }\n\
         \x20   match b.positions.get_mut(9) {\n\
         \x20       Some(list) => list.push(7),\n\
         \x20       None => println(\"no 9\"),\n\
         \x20   }\n\
         \x20   b.positions.get_mut(3).unwrap()[0] = 5;\n\
         \x20   let mut c: Map[String, i64] = Map.new();\n\
         \x20   c.insert(\"a\", 1);\n\
         \x20   if let Some(n) = c.get_mut(\"a\") { *n += 41; }\n\
         \x20   let mut s: SortedMap[i64, String] = SortedMap.new();\n\
         \x20   s.insert(2, \"x\");\n\
         \x20   if let Some(t) = s.get_mut(2) { t.push_str(\"yz\"); }\n\
         \x20   let none5 = s.get_mut(5).is_none();\n\
         \x20   println(f\"{b.positions[3][0]} {b.positions[3].len()} {c[\"a\"]} {s[2]} {none5}\");\n\
         }\n";
    let (dir, path) = fixture("map-get-mut", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must accept: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "no 9\n5 2 42 xyz true\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn number_parse_returns_a_result_with_a_parse_error() {
    // B-2026-10-12-6: docs/library/core-types.md gives every number type
    // `fn parse(s: Str) -> Result[Self, ParseError]`, and `ParseError`
    // says why there is no number.
    let src = "fn show(r: Result[i64, ParseError]) -> String {\n\
         \x20   match r {\n\
         \x20       Ok(n) => f\"ok {n}\",\n\
         \x20       Err(e) => f\"err {e}\",\n\
         \x20   }\n\
         }\n\
         fn main() {\n\
         \x20   let line = \" 42 \";\n\
         \x20   match i64.parse(line.trim()) {\n\
         \x20       Ok(n) => println(f\"You guessed {n}\"),\n\
         \x20       Err(e) => println(f\"not a number: {e}\"),\n\
         \x20   }\n\
         \x20   println(show(i64.parse(\"\")));\n\
         \x20   println(show(i64.parse(\"4x\")));\n\
         \x20   println(show(i64.parse(\"99999999999999999999\")));\n\
         \x20   println(show(i64.parse(\"-99999999999999999999\")));\n\
         \x20   println(show(i64.parse(\"-\")));\n\
         \x20   let u = u8.parse(\"300\");\n\
         \x20   let v = u8.parse(\"-3\");\n\
         \x20   println(f\"{u.is_err()} {v.is_err()} {u8.parse(\"7\").unwrap()}\");\n\
         \x20   match u8.parse(\"-3\") { Ok(_) => println(\"?\"), Err(e) => println(f\"{e}\") }\n\
         \x20   let f = f64.parse(\"2.5\");\n\
         \x20   println(f\"{f.unwrap()} {f64.parse(\"x\").is_err()} {f64.parse(\"\").is_err()}\");\n\
         \x20   let s = String.from(\"123\");\n\
         \x20   let n: i64 = i64.parse(s.as_str()).unwrap_or(0);\n\
         \x20   println(f\"{n} {i64.parse(\"8\").ok().unwrap()}\");\n\
         }\n";
    let (dir, path) = fixture("parse-result", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must accept: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let want = "You guessed 42\n\
                err cannot parse a number from an empty string\n\
                err invalid digit found in string\n\
                err number too large to fit in target type\n\
                err number too small to fit in target type\n\
                err invalid digit found in string\n\
                true true 7\n\
                number too small to fit in target type\n\
                2.5 true true\n\
                123 8\n";
    let runners: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for runner in runners {
        let out = karac().arg(runner).arg(&path).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{runner}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    // The `Option` spellings written before are fixed with `.ok()`.
    rejected_then_fixed_by(
        "parse-some-arm",
        "fn main() {\n\
             match i64.parse(\"5\") {\n\
                 Some(n) => println(f\"{n}\"),\n\
                 None => println(\"none\"),\n\
             }\n\
         }\n",
        "`.ok()` on the scrutinee",
        "i64.parse(\"5\").ok()",
    );
    rejected_then_fixed_by(
        "parse-question-in-option-fn",
        "fn twice(s: Str) -> Option[i64] {\n\
             let n = i64.parse(s)?;\n\
             Some(n * 2)\n\
         }\n\
         fn main() { println(f\"{twice(\"4\").unwrap()}\"); }\n",
        "`.ok()?`",
        "i64.parse(s).ok()?",
    );
    rejected_then_fixed_by(
        "parse-into-option",
        "fn main() {\n\
             let n: Option[i64] = i64.parse(\"3\");\n\
             println(f\"{n.unwrap()}\");\n\
         }\n",
        "`.ok()` turns the `Result` into an `Option`",
        "i64.parse(\"3\").ok()",
    );
}

#[test]
fn splitters_are_lazy_iterators_of_str_views() {
    // B-2026-10-12-7: docs/library/strings.md gives `split`, `lines` and
    // `split_whitespace` as lazy iterators of `Str` views, with Rust's
    // pieces: empty pieces kept by `split`, `\r\n` ends by `lines`.
    let src = "fn show(label: Str, parts: Vec[Str]) {\n\
         \x20   let mut out = String.new();\n\
         \x20   for p in parts {\n\
         \x20       out.push_str(\"[\");\n\
         \x20       out.push_str(p);\n\
         \x20       out.push_str(\"]\");\n\
         \x20   }\n\
         \x20   println(f\"{label}: {out}\");\n\
         }\n\
         fn main() {\n\
         \x20   let line = \"a,b,,c\".to_string();\n\
         \x20   show(\"comma\", line.split(\",\").collect());\n\
         \x20   show(\"empty\", \"\".split(\",\").collect());\n\
         \x20   show(\"edges\", \",a,\".split(\",\").collect());\n\
         \x20   show(\"multi\", \"a::b::::c\".split(\"::\").collect());\n\
         \x20   show(\"emptysep\", \"h\u{e9}y\".split(\"\").collect());\n\
         \x20   show(\"lines\", \"one\\ntwo\\r\\nthree\\n\".lines().collect());\n\
         \x20   show(\"ws\", \"  the  quick\\tbrown\\n fox \".split_whitespace().collect());\n\
         \x20   let n = line.split(\",\").count();\n\
         \x20   let owned: Vec[String] = line.split(\",\").map(|p| p.to_string()).collect();\n\
         \x20   let total: i64 = \"1 2 3 40\".split_whitespace().map(|w| i64.parse(w).unwrap()).sum();\n\
         \x20   println(f\"{n} {owned[3]} {total}\");\n\
         \x20   for kv in \"x=1;y=2\".split(\";\") {\n\
         \x20       let parts: Vec[Str] = kv.split(\"=\").collect();\n\
         \x20       match parts[0] {\n\
         \x20           \"x\" => println(f\"x is {parts[1]}\"),\n\
         \x20           other => println(f\"{other} -> {parts[1]}\"),\n\
         \x20       }\n\
         \x20   }\n\
         }\n";
    let (dir, path) = fixture("splitters", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must accept: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let want = "comma: [a][b][][c]\n\
                empty: []\n\
                edges: [][a][]\n\
                multi: [a][b][][c]\n\
                emptysep: [][h][\u{e9}][y][]\n\
                lines: [one][two][three]\n\
                ws: [the][quick][brown][fox]\n\
                4 c 46\n\
                x is 1\n\
                y -> 2\n";
    let runners: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for runner in runners {
        let out = karac().arg(runner).arg(&path).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{runner}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    // The `Vec` spellings written before are collected where they are bound.
    rejected_then_fixed_by(
        "split-bound-then-indexed",
        "fn main() {\n\
             let parts = \"a b\".split(\" \");\n\
             println(f\"{parts.len()} {parts[1]}\");\n\
         }\n",
        "`parts` is an iterator",
        "let parts: Vec[Str] = \"a b\".split(\" \").collect();",
    );
    rejected_then_fixed_by(
        "split-bound-then-lent-as-strings",
        "fn first(v: ref Vec[String]) -> String { v[0].clone() }\n\
         fn main() {\n\
             let parts = \"a b\".split(\" \");\n\
             println(f\"{parts.len()} {first(parts)}\");\n\
         }\n",
        "`parts` is used as a `Vec[String]`",
        "let parts: Vec[String] = \"a b\".split(\" \").map(|p| p.to_string()).collect();",
    );
    rejected_then_fixed_by(
        "split-returned-as-strings",
        "fn words(s: Str) -> Vec[String] {\n\
             s.split(\" \")\n\
         }\n\
         fn main() { println(f\"{words(\"a b\").len()}\"); }\n",
        "an iterator: collect its items",
        "s.split(\" \").map(|p| p.to_string()).collect()",
    );
    rejected_then_fixed_by(
        "split-len",
        "fn main() { println(f\"{\"a,b\".split(\",\").len()}\"); }\n",
        "`count()` walks it",
        "\"a,b\".split(\",\").count()",
    );
    rejected_then_fixed_by(
        "split-char-separator",
        "fn main() {\n\
             let n = \"a,b\".split(',').count();\n\
             println(f\"{n}\");\n\
         }\n",
        "write it as the string literal",
        "split(\",\")",
    );
    // `trim` is a view too, so a split of it borrows the text, not a copy.
    accepted(
        "split-of-trim",
        "fn main() {\n\
             let line = \"  a b  \".to_string();\n\
             let parts: Vec[Str] = line.trim().split(\" \").collect();\n\
             println(f\"{parts.len()} [{line.trim_start()}] [{line.trim_end()}]\");\n\
         }\n",
    );
    // A splitter over a temporary would read freed text once the `let`
    // ends (core-semantics §5.5), so check refuses it.
    rejected(
        "split-of-a-temporary",
        "fn main() {\n\
             let e = \"x,y\".to_string();\n\
             let parts = e.to_uppercase().split(\",\");\n\
             for p in parts { println(p); }\n\
         }\n",
        "a reference to a temporary outlives its statement",
    );
    rejected_then_fixed_by(
        "split-piece-to-string-param",
        "fn shout(s: String) -> String { s.to_uppercase() }\n\
         fn main() {\n\
             for w in \"a b\".split(\" \") {\n\
                 println(shout(w));\n\
             }\n\
         }\n",
        "`.to_string()` copies the text",
        "shout(w.to_string())",
    );
}

#[test]
fn hand_written_clone_and_default_impls_run() {
    // B-2026-10-06-112: `impl Clone` / `impl Default` resolve like any trait
    // impl, and a hand `clone` is the one every clone reaches, including the
    // clones of a `Vec`, `Option`, `Map` or derived struct that holds the type.
    let src = "struct Counter { n: i64, label: String }\n\
         impl Clone for Counter {\n\
         \x20   fn clone(ref self) -> Counter { Counter { n: self.n + 100, label: f\"{self.label}'\" } }\n\
         }\n\
         impl Default for Counter {\n\
         \x20   fn default() -> Counter { Counter { n: 7, label: \"d\".to_string() } }\n\
         }\n\
         #[derive(Clone)]\n\
         struct Holder { c: Counter, k: i64 }\n\
         enum Side { L, R }\n\
         impl Clone for Side { fn clone(ref self) -> Side { match self { Side.L => Side.R, Side.R => Side.L } } }\n\
         fn dup[T: Clone](x: ref T) -> T { x.clone() }\n\
         fn main() {\n\
         \x20   let c = Counter { n: 1, label: \"c\".to_string() };\n\
         \x20   let d = c.clone();\n\
         \x20   let g = dup(c);\n\
         \x20   let z = Counter.default();\n\
         \x20   println(f\"{d.n} {d.label} {g.n} {z.n} {z.label}\");\n\
         \x20   let v = vec![Counter { n: 2, label: \"v\".to_string() }];\n\
         \x20   let w = v.clone();\n\
         \x20   let h = Holder { c: Counter { n: 3, label: \"h\".to_string() }, k: 1 };\n\
         \x20   let h2 = h.clone();\n\
         \x20   let o = Some(Counter { n: 4, label: \"o\".to_string() });\n\
         \x20   let o2 = o.clone();\n\
         \x20   let mut m: Map[i64, Counter] = Map.new();\n\
         \x20   m.insert(1, Counter { n: 5, label: \"m\".to_string() });\n\
         \x20   let m2 = m.clone();\n\
         \x20   println(f\"{w[0].n} {w[0].label} {h2.c.n} {h2.c.label} {o2.unwrap().n} {m2[1].n}\");\n\
         \x20   match Side.L.clone() { Side.L => println(\"left\"), Side.R => println(\"right\") }\n\
         }\n";
    let (dir, path) = fixture("hand-clone", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must accept: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let want = "101 c' 101 7 d\n102 v' 103 h' 104 105\nright\n";
    let runners: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for runner in runners {
        let out = karac().arg(runner).arg(&path).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{runner}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    // `Copy` comes only from the derive, which checks the fields.
    rejected(
        "hand-copy",
        "struct P { x: i64 }\n\
         impl Copy for P {}\n\
         fn main() { let p = P { x: 1 }; println(f\"{p.x}\"); }\n",
        "a manual `impl Copy for P` is not allowed",
    );
    // A derive beside a hand impl of the same trait is a duplicate.
    rejected(
        "derive-and-hand-clone",
        "#[derive(Clone)]\n\
         struct P { x: i64 }\n\
         impl Clone for P { fn clone(ref self) -> P { P { x: self.x } } }\n\
         fn main() { let p = P { x: 1 }; println(f\"{p.clone().x}\"); }\n",
        "both derives `Clone`",
    );
}

#[test]
fn a_hand_written_ord_orders_searches_and_sorted_containers() {
    // B-2026-10-06-109: `is_sorted`, `binary_search`, `SortedSet` and
    // `SortedMap` follow the type's own `cmp`, as `sort` already did, not
    // its field order. `Rev` orders its keys descending; `Size` puts
    // `Large` first.
    let src = "#[derive(Clone, PartialEq, Eq)]\n\
         struct Rev { k: i64 }\n\
         impl PartialOrd for Rev { fn partial_cmp(ref self, other: ref Rev) -> Option[Ordering] { Some(self.cmp(other)) } }\n\
         impl Ord for Rev { fn cmp(ref self, other: ref Rev) -> Ordering { other.k.cmp(self.k) } }\n\
         #[derive(Clone, Copy, PartialEq, Eq)]\n\
         enum Size { Small, Large }\n\
         impl PartialOrd for Size { fn partial_cmp(ref self, other: ref Size) -> Option[Ordering] { Some(self.cmp(other)) } }\n\
         impl Ord for Size {\n\
         \x20   fn cmp(ref self, other: ref Size) -> Ordering {\n\
         \x20       match (self, other) {\n\
         \x20           (Size.Large, Size.Small) => Ordering.Less,\n\
         \x20           (Size.Small, Size.Large) => Ordering.Greater,\n\
         \x20           _ => Ordering.Equal,\n\
         \x20       }\n\
         \x20   }\n\
         }\n\
         fn main() {\n\
         \x20   let v = vec![Rev { k: 9 }, Rev { k: 5 }, Rev { k: 2 }];\n\
         \x20   println(f\"is_sorted {v.is_sorted()} {vec![Rev { k: 1 }, Rev { k: 3 }].is_sorted()}\");\n\
         \x20   println(f\"binary_search {v.binary_search(Rev { k: 2 }).unwrap()}\");\n\
         \x20   let mut set: SortedSet[Rev] = SortedSet.new();\n\
         \x20   for k in [2, 8, 4, 6] { set.insert(Rev { k: k }); }\n\
         \x20   let mut s = String.new();\n\
         \x20   for r in set.iter() { s.push_str(f\"{r.k} \"); }\n\
         \x20   println(f\"set {s}| first {set.min().unwrap().k}\");\n\
         \x20   let mut q = String.new();\n\
         \x20   for r in set.range(Rev { k: 9 }, Rev { k: 4 }) { q.push_str(f\"{r.k} \"); }\n\
         \x20   println(f\"set range {q}\");\n\
         \x20   let mut m: SortedMap[Rev, i64] = SortedMap.new();\n\
         \x20   for k in [5, 1, 9, 3, 7] { m.insert(Rev { k: k }, k * 10); }\n\
         \x20   let mut t = String.new();\n\
         \x20   for (k, x) in m.iter() { t.push_str(f\"{k.k}={x} \"); }\n\
         \x20   println(f\"map {t}\");\n\
         \x20   match m.floor(Rev { k: 4 }) { Some((k, _)) => println(f\"floor {k.k}\"), None => println(\"floor none\") }\n\
         \x20   let mut sizes: SortedSet[Size] = SortedSet.new();\n\
         \x20   sizes.insert(Size.Small);\n\
         \x20   sizes.insert(Size.Large);\n\
         \x20   match sizes.min().unwrap() { Size.Large => println(\"least Large\"), Size.Small => println(\"least Small\") }\n\
         }\n";
    let (dir, path) = fixture("hand-ord", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must accept: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let want = "is_sorted true false\n\
                binary_search 2\n\
                set 8 6 4 2 | first 8\n\
                set range 8 6 \n\
                map 9=90 7=70 5=50 3=30 1=10 \n\
                floor 5\n\
                least Large\n";
    let runners: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for runner in runners {
        let out = karac().arg(runner).arg(&path).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{runner}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_vec_of_ordered_elements_is_ordered() {
    // B-2026-10-06-92: design.md gives `impl[T: Ord] Ord for Vec[T]`, so a
    // `Vec` is a sorted-table key, compares with `<` and `cmp`, and has a
    // `max`, lexicographically, as `sort` already ordered it.
    let src = "fn bigger[T: Ord](a: own T, b: own T) -> T { if a > b { a } else { b } }\n\
         fn main() {\n\
         \x20   let mut m: SortedMap[Vec[i64], i64] = SortedMap.new();\n\
         \x20   m.insert(vec![2, 1], 5);\n\
         \x20   m.insert(vec![1, 9], 3);\n\
         \x20   m.insert(vec![1], 7);\n\
         \x20   for (k, v) in m.iter() { println(f\"{k} {v}\"); }\n\
         \x20   let mut s: SortedSet[Vec[String]] = SortedSet.new();\n\
         \x20   s.insert(vec![\"b\".to_string()]);\n\
         \x20   s.insert(vec![\"a\".to_string(), \"z\".to_string()]);\n\
         \x20   for k in s.iter() { println(f\"{k}\"); }\n\
         \x20   let a: Vec[i64] = vec![1, 2];\n\
         \x20   let b: Vec[i64] = vec![1, 3];\n\
         \x20   println(f\"{a < b} {a >= b} {a.cmp(b) == Ordering.Less}\");\n\
         \x20   let vv = vec![vec![1, 9], vec![2], vec![1, 9, 0]];\n\
         \x20   println(f\"{vv.iter().max().unwrap()} {bigger(vec![1, 2], vec![1, 2, 0])}\");\n\
         }\n";
    let (dir, path) = fixture("vec-ord", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must accept: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let want = "[1] 7\n[1, 9] 3\n[2, 1] 5\n[a, z]\n[b]\ntrue false true\n[2] [1, 2, 0]\n";
    let runners: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for runner in runners {
        let out = karac().arg(runner).arg(&path).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{runner}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_collection_constructor_is_a_function_value() {
    // B-2026-10-06-118: design.md § Standard Data Structures writes
    // `entry(key).or_insert_with(Vec.new)`; the empty constructors of the
    // collections and `String` name a function, as a user type's `new` does.
    let src = "fn main() {\n\
         \x20   let mut m: Map[i64, Vec[i64]] = Map.new();\n\
         \x20   m.entry(1).or_insert_with(Vec.new).push(3);\n\
         \x20   m.entry(1).or_insert_with(Vec.new).push(4);\n\
         \x20   let mut w: Map[i64, String] = Map.new();\n\
         \x20   w.entry(2).or_insert_with(String.new).push_str(\"a\");\n\
         \x20   let mut g: Map[i64, Set[i64]] = Map.new();\n\
         \x20   g.entry(5).or_insert_with(Set.new).insert(9);\n\
         \x20   let mut q: Map[i64, SortedMap[i64, i64]] = Map.new();\n\
         \x20   q.entry(1).or_insert_with(SortedMap.new).insert(4, 5);\n\
         \x20   let f = Vec.new;\n\
         \x20   let mut v: Vec[i64] = f();\n\
         \x20   v.push(1);\n\
         \x20   let h = String.default;\n\
         \x20   let mut s: String = h();\n\
         \x20   s.push_str(\"z\");\n\
         \x20   println(f\"{m[1]} {w[2]} {g[5].len()} {q[1][4]} {v} {s}\");\n\
         }\n";
    let (dir, path) = fixture("ctor-value", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must accept: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let runners: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for runner in runners {
        let out = karac().arg(runner).arg(&path).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "[3, 4] a 1 5 [1] z\n",
            "{runner}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_value_enum_carries_another_value_enum() {
    // B-2026-10-06-86: one level of enum nesting was a limit of the legacy
    // backend. `karac check` already dropped it; the MIR pipeline applied it
    // still, so a checked program was refused there.
    let src = "#[derive(Clone, PartialEq)]\n\
         enum Kind { Round, Square }\n\
         enum Tok { Open(Kind), Close(Kind), Word(String) }\n\
         enum Wrap[T] { Val(T), No }\n\
         fn main() {\n\
         \x20   let toks = vec![Tok.Open(Kind.Square), Tok.Word(\"x\".to_string()), Tok.Close(Kind.Square)];\n\
         \x20   let mut stack: Vec[Kind] = Vec.new();\n\
         \x20   let mut ok = 0;\n\
         \x20   for t in toks.iter() {\n\
         \x20       match t {\n\
         \x20           Tok.Open(k) => stack.push(k.clone()),\n\
         \x20           Tok.Close(k) => { if stack.pop() == Some(k.clone()) { ok += 1; } }\n\
         \x20           Tok.Word(w) => println(f\"word {w}\"),\n\
         \x20       }\n\
         \x20   }\n\
         \x20   let w: Wrap[Kind] = Wrap.Val(Kind.Round);\n\
         \x20   match w { Wrap.Val(Kind.Round) => println(f\"ok {ok} round\"), _ => println(\"other\") }\n\
         }\n";
    let (dir, path) = fixture("nested-enum", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must accept: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let runners: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for runner in runners {
        let out = karac().arg(runner).arg(&path).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "word x\nok 1 round\n",
            "{runner}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sorted_ranges_are_lazy_iterators() {
    // B-2026-10-06-68: docs/library/collections.md gives `SortedSet.range`,
    // `SortedMap.range` and their `range_inclusive` as iterators of borrowed
    // items, half-open and closed. They returned a `Vec`. Collecting a
    // `SortedMapIter` into `Vec[(ref K, ref V)]` also checks the iterator
    // methods' own generic no longer collides with the struct's `V`.
    let src = "fn main() {\n\
         \x20   let mut s: SortedSet[i64] = SortedSet.new();\n\
         \x20   for x in [5, 1, 9, 3, 7] { s.insert(x); }\n\
         \x20   let a: Vec[i64] = s.range(3, 7).cloned().collect();\n\
         \x20   let b: Vec[ref i64] = s.range_inclusive(3, 7).collect();\n\
         \x20   println(f\"{a} {b} {s.range(4, 2).count()} {s.range(6, 6).count()}\");\n\
         \x20   let mut m: SortedMap[String, i64] = SortedMap.new();\n\
         \x20   for (k, v) in [(\"b\", 2), (\"d\", 4), (\"a\", 1), (\"c\", 3)] { m.insert(k.to_string(), v); }\n\
         \x20   for (k, v) in m.range(\"b\".to_string(), \"d\".to_string()) { println(f\"{k}={v}\"); }\n\
         \x20   let total: i64 = m.range_inclusive(\"b\".to_string(), \"d\".to_string()).map(|(_, v)| *v).sum();\n\
         \x20   let all: Vec[(ref String, ref i64)] = m.iter().collect();\n\
         \x20   println(f\"{total} {s.range(1, 9).rev().next()} {all.len()}\");\n\
         }\n";
    let (dir, path) = fixture("sorted-range", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must accept: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let runners: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for runner in runners {
        let out = karac().arg(runner).arg(&path).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "[3, 5] [3, 5, 7] 0 0\nb=2\nc=3\n9 Some(7) 4\n",
            "{runner}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn iterator_terminals_lend_their_items_and_flatten_needs_iterators() {
    // B-2026-10-14-24: against the library, `min` / `max` / `next` / `last`
    // over `v.iter()` give `Option[ref T]`, so `*` reads the item, and the
    // `_by` / `_by_key` terminals exist, as both MIR backends run them.
    let src = "fn main() {\n\
         \x20   let nums = vec![5, 3, 8, 1, 9];\n\
         \x20   let a: i64 = *nums.iter().min().unwrap();\n\
         \x20   let b: i64 = *nums.iter().max().unwrap();\n\
         \x20   let c = *nums[1..=3].iter().min().unwrap();\n\
         \x20   let m: Option[ref i64] = nums.iter().max();\n\
         \x20   let d = *m.unwrap();\n\
         \x20   let e = *nums.iter().next().unwrap();\n\
         \x20   let f = *nums.iter().last().unwrap();\n\
         \x20   let g = *nums.iter().min_by_key(|v| -*v).unwrap();\n\
         \x20   let h = *nums.iter().max_by(|x, y| x.cmp(y)).unwrap();\n\
         \x20   println(f\"{a} {b} {c} {d} {e} {f} {g} {h}\");\n\
         }\n";
    let (dir, path) = fixture("iter-terminals", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must accept: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "1 9 1 9 5 9 9 9\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    // B-2026-10-14-18: `flatten` takes items that are iterators, so check
    // refuses it over a `Vec[Vec[i64]]` as the MIR backends do, and says to
    // write `flat_map`; that spelling passes.
    let src = "fn main() {\n\
         \x20   let mat = vec![vec![1, 2], vec![3]];\n\
         \x20   let flat: Vec[i64] = mat.iter().flatten().collect();\n\
         \x20   println(f\"{flat}\");\n\
         }\n";
    let (dir, path) = fixture("flatten-vecs", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must refuse: {err}");
    assert!(err.contains(".flat_map(|v| v.iter())"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
    let src = "fn main() {\n\
         \x20   let mat = vec![vec![1, 2], vec![3]];\n\
         \x20   let flat: Vec[i64] = mat.iter().flat_map(|v| v.iter()).collect();\n\
         \x20   println(f\"{flat}\");\n\
         }\n";
    let (dir, path) = fixture("flat-map-vecs", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must accept: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "[1, 2, 3]\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_argument_to_a_mutating_parameter_is_marked() {
    // B-2026-10-14-25: design.md § Call-site `mut` markers marks every
    // argument to a `mut ref T` / `mut Slice[T]` parameter, whatever its
    // root. The spec's own example, a field of a `mut ref` binding, and a
    // forwarded `mut ref` parameter are accepted marked and run on both MIR
    // backends; `h.1` of a `split_at_mut` pair held by a plain `let` is a
    // borrow it forwards, so it needs no `let mut`.
    let src = "struct Session {\n\
         \x20   scores: Vec[i64],\n\
         }\n\
         fn sort_in_place(xs: mut Slice[i64]) {\n\
         \x20   xs.sort();\n\
         }\n\
         fn bump(n: mut ref i64) {\n\
         \x20   *n += 1;\n\
         }\n\
         fn twice(n: mut ref i64) {\n\
         \x20   bump(mut n);\n\
         \x20   bump(mut n);\n\
         }\n\
         fn refresh(s: mut ref Session) {\n\
         \x20   sort_in_place(mut s.scores);\n\
         }\n\
         fn main() {\n\
         \x20   let mut s = Session { scores: vec![3, 1, 2] };\n\
         \x20   refresh(mut s);\n\
         \x20   let mut k = 1;\n\
         \x20   twice(mut k);\n\
         \x20   let mut b = vec![9, 8, 3, 1, 2];\n\
         \x20   let mut bs = b.as_slice_mut();\n\
         \x20   let h = bs.split_at_mut(2);\n\
         \x20   sort_in_place(mut h.1);\n\
         \x20   println(f\"{s.scores} {k} {b}\");\n\
         }\n";
    let (dir, path) = fixture("mut-marker-forwarded", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must accept: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let runners: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for runner in runners {
        let out = karac().arg(runner).arg(&path).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "[1, 2, 3] 3 [9, 8, 1, 2, 3]\n",
            "{runner}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    // A missing marker on a forwarded `mut ref` is an error with a fix-it
    // that adds it.
    rejected_then_fixed_by(
        "mut-marker-missing",
        "fn bump(n: mut ref i64) {\n\
             *n += 1;\n\
         }\n\
         fn twice(n: mut ref i64) {\n\
             bump(n);\n\
         }\n\
         fn main() {\n\
             let mut k = 1;\n\
             twice(mut k);\n\
             println(f\"{k}\");\n\
         }\n",
        "every argument to a mutating parameter is marked",
        "bump(mut n);",
    );
}

/// B-2026-10-14-38: design.md § Returns returns a borrow of a place with
/// `ref <place>`, pinned by the place's root, and a `ref` local walked down a
/// tree copies the reference on each step.
#[test]
fn a_borrow_of_a_place_rooted_at_a_borrowed_parameter_is_returned() {
    accepted(
        "borrow-return-place",
        "struct User { name: String, age: i64 }\n\
         struct Node { val: i64, next: Option[Node], kids: Map[String, Node] }\n\
         struct List { head: Node }\n\
         impl List {\n\
             fn head(self) -> ref Node { ref self.head }\n\
             fn first_val(self) -> ref i64 { ref self.head.val }\n\
             fn last(self) -> ref Node {\n\
                 let mut node = ref self.head;\n\
                 while let Some(n) = node.next { node = n; }\n\
                 node\n\
             }\n\
             fn find(self, path: Vec[String]) -> ref Node {\n\
                 let mut node = ref self.head;\n\
                 for name in path.iter() { node = ref node.kids[name.clone()]; }\n\
                 node\n\
             }\n\
         }\n\
         fn name(user: User) -> ref String { ref user.name }\n\
         fn main() {\n\
             let u = User { name: \"ada\".to_string(), age: 36 };\n\
             println(f\"{name(u)} {u.age}\");\n\
             let l = List { head: Node { val: 1, next: Some(Node { val: 3, next: None, kids: Map.new() }), kids: Map.new() } };\n\
             println(f\"{l.head().val} {l.last().val} {l.first_val()} {l.find(Vec.new()).val}\");\n\
         }\n",
    );
}

/// B-2026-10-14-38, B-2026-10-14-71: a `ref <place>` or `ref` local rooted at
/// an owned value still dangles, including a `ref` local a later assignment
/// points at one.
#[test]
fn a_borrow_of_an_owned_place_is_not_returned() {
    for (tag, src) in [
        (
            "owned-param",
            "struct User { name: String }\n\
             fn name(user: own User) -> ref String { ref user.name }\n\
             fn main() {}\n",
        ),
        (
            "owned-local",
            "struct User { name: String }\n\
             fn name() -> ref String { let u = User { name: \"x\".to_string() }; ref u.name }\n\
             fn main() {}\n",
        ),
        (
            "reassigned-to-local",
            "struct Node { val: i64, next: Option[Node] }\n\
             struct List { head: Node }\n\
             impl List {\n\
                 fn pick(self) -> ref Node {\n\
                     let other = Node { val: 9, next: None };\n\
                     let mut node = ref self.head;\n\
                     node = ref other;\n\
                     node\n\
                 }\n\
             }\n\
             fn main() {}\n",
        ),
        (
            "projected-from-local",
            "struct Node { val: i64, next: Option[Node] }\n\
             struct List { head: Node }\n\
             impl List {\n\
                 fn pick(self) -> ref Node {\n\
                     let local = Node { val: 9, next: None };\n\
                     let mut node = ref self.head;\n\
                     if let Some(n) = local.next { node = n; }\n\
                     node\n\
                 }\n\
             }\n\
             fn main() {}\n",
        ),
    ] {
        rejected(
            tag,
            src,
            "returned borrow does not originate from a `ref` parameter",
        );
    }
}

/// B-2026-10-13-24 (Gowtham, 2026-10-14): there is no `Vec.append`.
/// `karac fix` writes `extend` over the moved elements, `mem.take` for a
/// borrowed source, and `extend_from_slice` stays.
#[test]
fn append_is_written_as_extend() {
    rejected_then_fixed_by(
        "append-borrowed",
        "fn drain_into(a: mut ref Vec[i64], b: mut ref Vec[i64]) {\n\
             a.append(mut b);\n\
         }\n\
         fn main() {\n\
             let mut d: Vec[i64] = vec![1];\n\
             let mut x: Vec[i64] = vec![5];\n\
             drain_into(mut d, mut x);\n\
             d.extend_from_slice(x.as_slice());\n\
             println(f\"{d} {x}\");\n\
         }\n",
        "there is no `Vec.append`",
        "a.extend(mem.take(mut b));",
    );
    rejected_then_fixed_by(
        "append-owned",
        "fn main() {\n\
             let mut d: Vec[i64] = vec![1];\n\
             let y: Vec[i64] = vec![7];\n\
             d.append(y);\n\
             println(f\"{d}\");\n\
         }\n",
        "there is no `Vec.append`",
        "d.extend(y);",
    );
    rejected_then_fixed_by(
        "try-append",
        "fn grow(d: mut ref Vec[i64]) -> Result[(), AllocError] {\n\
             let y: Vec[i64] = vec![7];\n\
             d.try_append(y)?;\n\
             Ok(())\n\
         }\n\
         fn main() {\n\
             let mut d: Vec[i64] = vec![1];\n\
             grow(mut d).unwrap();\n\
             println(f\"{d}\");\n\
         }\n",
        "there is no `Vec.try_append`",
        "d.try_extend(y)?;",
    );
}
