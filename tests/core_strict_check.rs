//! `karac check` enforces the v2 core's ownership rules as errors
//! (`docs/core-semantics.md` §3.2 use after move, §3.3 moves in loops, §3.7
//! moves out of a borrowed place, §6.4 no implicit RC), and `karac fix`
//! repairs each with `.clone()`. `build` and `run` check the same way, so a
//! program here that runs is run on v2.
//!
//! The programs are the review thread's core pins (`review/core-pins/`), plus
//! the places where the legacy checker reported a move the core does not make.

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
fn a_ref_binding_runs_and_formats() {
    // `ref name` is an ordinary binding to `run`, and
    // `karac fmt` keeps the `ref`.
    let (dir, path) = fixture(
        "ref-binding-run",
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
        .args(["run", "--interp"])
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
    let (dir, path) = fixture("mut-ref-pattern-run", src);
    let out = karac()
        .args(["run", "--interp"])
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "3 1 1\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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
        "cannot move `v[0]` out of an index",
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

/// §5.6, two-phase: an assignment's right side and a call's arguments run
/// before the write, so a borrow last used there does not conflict with it.
#[test]
fn a_borrow_last_used_by_the_writing_statement_itself_is_accepted() {
    accepted(
        "two-phase-assign",
        "struct S { x: i64 }\n\
         fn main() {\n\
             let mut bs: Vec[S] = Vec.new();\n\
             bs.push(S { x: 1 });\n\
             let b = ref bs[0];\n\
             bs[0] = S { x: b.x + 1 };\n\
             println(bs[0].x);\n\
         }\n",
    );
    accepted(
        "two-phase-method-arg",
        "fn main() {\n\
             let mut v: Vec[i64] = Vec.new();\n\
             v.push(1);\n\
             let b = ref v[0];\n\
             v.push(*b + 1);\n\
             println(v.len());\n\
         }\n",
    );
    rejected_with(
        "two-phase-assign-used-after",
        "struct S { x: i64 }\n\
         fn main() {\n\
             let mut bs: Vec[S] = Vec.new();\n\
             bs.push(S { x: 1 });\n\
             let b = ref bs[0];\n\
             bs[0] = S { x: b.x + 1 };\n\
             println(b.x);\n\
         }\n",
        "while `ref b` borrows `bs[..]`",
    );
    rejected_with(
        "two-phase-method-arg-used-after",
        "fn main() {\n\
             let mut v: Vec[i64] = Vec.new();\n\
             v.push(1);\n\
             let b = ref v[0];\n\
             v.push(*b + 1);\n\
             println(*b);\n\
         }\n",
        "while `ref b` borrows `v[..]`",
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
        "fn twice(f: own MutFn()) { f(); f(); }\n\
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
/// exact and stays implicit.
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

/// B-2026-10-12-9: `*p.0` on an undestructured `SortedMap.iter()` item reads
/// through the view. A `Copy` part bound out of a `floor` / `ceiling` entry
/// is a copy (core-semantics §4.6), read as `k`: `*k` there is the
/// drop-the-`*` error (B-2026-10-24-36).
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
         \x20       println(f\"{k} {v}\");\n\
         \x20   }\n\
         \x20   if let Some((k, v)) = m.ceiling(0) {\n\
         \x20       println(f\"{k} {v}\");\n\
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
        .args(["run", "--interp"])
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
        .args(["run", "--interp"])
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
/// `String` moves nothing. (`size`, a `Copy` part, is a copy and read bare,
/// B-2026-10-24-36.) A `*w` that IS a move (a `let` or a by-value
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
                             return size;\n\
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

// B-2026-10-14-47: a plain `c[k] = v` stores through `IndexSet` when the
// type implements it; the value is checked against `index_set`'s, and a
// type with only `IndexSet` cannot be read or compound-assigned through
// `[]`.
#[test]
fn user_index_set_is_accepted() {
    accepted(
        "user-index-set",
        r#"struct Registry { keys: Vec[String], vals: Vec[i64] }
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
"#,
    );
    let (dir, path) = fixture(
        "user-index-set-refused",
        r#"struct Sparse { keys: Vec[i64], vals: Vec[i64] }
impl IndexSet[i64] for Sparse {
    type Output = i64;
    fn index_set(mut ref self, k: own i64, v: own i64) { self.keys.push(k); self.vals.push(v); }
}
fn main() {
    let mut s = Sparse { keys: Vec.new(), vals: Vec.new() };
    s[1] = "x";
    s[2] += 1;
    println(f"{s[1]}");
}
"#,
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains("8:12: expected 'i64', found 'String'"),
        "{err}"
    );
    assert!(
        err.contains("9:5: `Sparse` implements `IndexSet` but not `Index`"),
        "{err}"
    );
    assert!(
        err.contains("10:16: `Sparse` implements `IndexSet` but not `Index`"),
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

    let src = "import std.sync.{Mutx};\n\
               import std.iter.{Extend};\n\
               import std.io.{File as F};\n\
               fn main() {}\n";
    let (dir, path) = fixture("library-paths-bad", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    for want in [
        "`std.iter.Extend` is in the library spec (docs/library) but not implemented yet",
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

/// B-2026-10-18-2: a `const` initialized by an integer literal past i64
/// checks instead of panicking the evaluator, and an enum discriminant past
/// i64 is the non-constant-discriminant error.
#[test]
fn check_const_literals_past_i64() {
    accepted(
        "const-past-i64",
        r#"
const BIG: i128 = 18446744073709551615;
const UU: u64 = 18446744073709551615;
const WW: u64 = 18446744073709551615u64;
const XX: u128 = 340282366920938463463374607431768211455u128;
fn main() {
    println(f"{BIG} {UU} {WW} {XX}");
}
"#,
    );
    let (dir, path) = fixture(
        "discriminant-past-i64",
        "#[repr(i64)]\nenum Ee { Aa = 9223372036854775808, Bb = 1 }\nfn main() { println(\"x\"); }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains("E_NON_CONSTANT_DISCRIMINANT") && !err.contains("panicked"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-18-1: `T.Output` under `T: Add` is the `Output` of the solved
/// type's `impl Add`, not of whichever operator impl of it came last.
#[test]
fn check_a_projection_reads_its_bound_traits_output() {
    accepted(
        "projection-bound-trait",
        r#"
struct V { x: i64 }
impl Add for V { type Output = V; fn add(self, rhs: V) -> V { V { x: self.x + rhs.x } } }
impl Mul for V { type Output = i64; fn mul(self, rhs: V) -> i64 { self.x * rhs.x } }
struct W[T] { v: T }
impl[T: Add] Add for W[T] { type Output = W[T.Output]; fn add(self, rhs: W[T]) -> W[T.Output] { W { v: self.v + rhs.v } } }
impl[T] Mul for W[T] { type Output = bool; fn mul(self, rhs: W[T]) -> bool { true } }
fn sum2[T: Add](a: own T, b: own T) -> T.Output { a + b }
fn prod2[T: Mul](a: own T, b: own T) -> T.Output { a * b }
fn main() {
    let v = sum2(V { x: 1 }, V { x: 2 });
    println(f"{v.x}");
    let p: i64 = prod2(V { x: 3 }, V { x: 4 });
    println(f"{p}");
    let w = sum2(W { v: V { x: 1 } }, W { v: V { x: 9 } });
    println(f"{w.v.x}");
}
"#,
    );
}

/// B-2026-10-15-107: an error in a spliced library module's item is
/// reported by `karac check`, as the build stops on it, at the program's
/// import of the module and naming where in the module it is. The error here
/// is a module type the import lists colliding with the program's own (an
/// unlisted one stays private to the module since B-2026-10-19-10).
#[test]
fn an_error_in_a_library_module_is_reported_at_its_import() {
    let src = "import std.time.{sleep, Duration};\n\
               struct Duration { n: i64 }\n\
               fn main() { let d = Duration { n: 1 }; println(f\"{d.n}\"); }\n";
    let (dir, path) = fixture("library-module-error", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must reject: {err}");
    assert!(
        err.contains(":1:1: 'Duration' is already defined")
            && err.contains("(in the library module `std.time` at 7:5, imported here)"),
        "{err}"
    );
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert!(!out.status.success(), "the build stops on it too");
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-18-54: a library module's helper function the import did not
/// list stays private to the module, so a program's own `uuid_hex` beside
/// `import std.uuid.{v4};` is its own function.
#[test]
fn a_library_helper_does_not_meet_a_program_item_of_its_name() {
    accepted_and_runs(
        "library-helper-private",
        "import std.uuid.{v4};\n\
         fn uuid_hex(x: i64) -> i64 { x }\n\
         fn main() { println(f\"{v4().len()} {uuid_hex(3)}\"); }\n",
        "36 3\n",
    );
}

/// B-2026-10-19-4: a nested `if` (or `match`) made only of string literals
/// takes the `Str` of a sibling arm or branch, as a bare literal does.
#[test]
fn a_literal_only_nested_if_takes_a_sibling_str() {
    accepted_and_runs(
        "literal-nested-if-str",
        r#"fn step(b: u8) -> Str { "x" }
fn pair(b: u8) -> (i64, Str) { (0, "y") }
fn main() {
    let x = "hello".to_string();
    for b in [0u8, 5u8, 200u8] {
        let reason = match b { 0 => step(b), _ => if b < 128 { "" } else { "bad" } };
        let t = match b { 0 => pair(b), _ => if b < 128 { (1, "") } else { (-1, "bad") } };
        let r2 = if b > 3 { x.as_str() } else if b > 2 { "a" } else { "b" };
        println(f"[{reason}] {t.0} [{t.1}] {r2}");
    }
}
"#,
        "[x] 0 [y] b\n[] 1 [] hello\n[bad] -1 [bad] hello\n",
    );
}

/// B-2026-10-19-5: `Vec.join` takes a `Str` separator, as
/// docs/library/collections.md gives it.
#[test]
fn vec_join_takes_a_str_separator() {
    accepted_and_runs(
        "vec-join-str-sep",
        r#"fn glue(lines: ref Vec[String], sep: Str) -> String { lines.join(sep) }
fn main() {
    let v = vec!["a".to_string(), "b".to_string()];
    println(glue(v, ", "));
    let s = "+".to_string();
    let parts: Vec[Str] = "x,y".split(",").collect();
    println(parts.join(s.as_str()));
}
"#,
        "a, b\nx+y\n",
    );
}

/// B-2026-10-13-32, B-2026-10-13-6: a closure parameter is an ordinary bare
/// parameter of the item type it is lent (core-semantics.md §5.10). Over an
/// iterator of `Copy` values it is a copy, so `*j` is an error whose fix drops
/// the `*`; over `.iter()`, and from a named `VecIter` / `DequeIter`, the item
/// is a `ref T` and `*k` reads it.
#[test]
fn a_closure_parameter_over_copy_items_is_a_copy() {
    for (tag, src, at) in [
        (
            "closure-copy-find",
            "fn main() {\n    let w = vec![4, 6, 9];\n    let n = w.len();\n    \
             let r = (1..n).find(|j| w[0] + w[*j] == 13);\n    println(f\"{r.unwrap()}\");\n}\n",
            ":4:38: unary '*'",
        ),
        (
            "closure-copy-chars",
            "fn main() {\n    let w = \"QqqQ\";\n    \
             println(f\"{w.chars().filter(|c| *c == 'Q').count()}\");\n}\n",
            ":3:37: unary '*'",
        ),
    ] {
        let (dir, path) = fixture(tag, src);
        let out = karac().arg("check").arg(&path).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{tag}: check must refuse `*`: {err}");
        assert!(
            err.contains(at) && err.contains("drop the `*`"),
            "{tag}: {err}"
        );
        let out = karac().arg("__mir-run").arg(&path).output().unwrap();
        assert!(
            !out.status.success(),
            "{tag}: the MIR pipeline refuses it too"
        );
        let out = karac().arg("fix").arg(&path).output().unwrap();
        assert!(
            out.status.success(),
            "{tag}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let fixed = std::fs::read_to_string(&path).unwrap();
        assert!(
            !fixed.contains("*j") && !fixed.contains("*c"),
            "{tag}: {fixed}"
        );
        let out = karac().arg("__mir-run").arg(&path).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "2\n", "{tag}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    accepted_and_runs(
        "closure-copy-values",
        r#"fn main() {
    let w = vec![4, 6, 9];
    let n = w.len();
    let r = (1..n).find(|j| w[0] + w[j] == 13);
    let word = "QqqQ";
    let c = word.chars().filter(|c| c == 'Q').count();
    let up: Vec[char] = word.chars().map(|c| c.to_ascii_uppercase()).filter(|c| c != 'X').collect();
    let s = (0..5).fold(0, |acc, k| acc + k * k);
    let e: Vec[(i64, char)] = word.chars().enumerate().filter(|p| p.1 == 'Q').collect();
    let big = v_iter(w);
    println(f"{r.unwrap()} {c} {up.len()} {s} {e.len()} {big}");
}
fn v_iter(w: ref Vec[i64]) -> i64 { w.iter().filter(|x| *x > 5).count() }
"#,
        "2 2 4 30 2 2\n",
    );
    accepted_and_runs(
        "named-iter-ref-items",
        r#"fn main() {
    let w = vec![4, 6, 9];
    let mut it = w.iter();
    let Some(k) = it.next() else { return; };
    let x = *k;
    let mut c: VecDeque[i64] = VecDeque.new();
    c.push_back(5);
    let mut dit = c.iter();
    let Some(d) = dit.next() else { return; };
    let y = *d;
    println(f"{x} {y}");
}
"#,
        "4 5\n",
    );
}

/// B-2026-10-15-10: `std.process.exit` is a library function now. It ends
/// the process with its code after flushing output, and runs no `Drop` body or
/// `defer` (docs/library/io.md § Process exit). Its `-> Never` also stands in
/// for any type at the call.
#[test]
fn std_process_exit_ends_the_process_with_its_code() {
    let src = r#"import std.process.{exit};
struct Noisy { n: i64 }
impl Drop for Noisy {
    fn drop(mut ref self) { println(f"drop {self.n}"); }
}
fn bail(code: i32) -> i64 {
    exit(code)
}
fn main() {
    let a = Noisy { n: 1 };
    defer { println("deferred"); }
    print("no newline");
    let v = bail(7);
    println(f"{v} {a.n}");
}
"#;
    accepted("process-exit", src);
    let (dir, path) = fixture("process-exit", src);
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "no newline",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(7));
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-15-10: `std.iter.DoubleEndedIterator` resolves. A program type
/// implements it, gets `rev` from it, and bounds a generic on it, as a library
/// iterator does (docs/library/iterators.md § `DoubleEndedIterator`).
#[test]
fn std_iter_double_ended_iterator_imports_and_runs() {
    accepted_and_runs(
        "double-ended",
        r#"import std.iter.{DoubleEndedIterator};

struct Count {
    lo: i64,
    hi: i64,
}

impl Iterator for Count {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        if self.lo >= self.hi {
            return None;
        }
        self.lo += 1;
        Some(self.lo - 1)
    }
}

impl DoubleEndedIterator for Count {
    fn next_back(mut ref self) -> Option[i64] {
        if self.lo >= self.hi {
            return None;
        }
        self.hi -= 1;
        Some(self.hi)
    }
}

fn last_of[I: DoubleEndedIterator](it: own I) -> Option[I.Item] {
    let mut it = it;
    it.next_back()
}

fn main() {
    let mut c = Count { lo: 0, hi: 4 };
    println(f"{c.next_back()} {c.next()}");
    for x in Count { lo: 0, hi: 3 }.rev() {
        println(f"{x}");
    }
    println(f"{last_of(Count { lo: 1, hi: 9 })}");
    let v = vec![1, 2, 3];
    println(f"{last_of(v.iter())}");
}
"#,
        "Some(3) Some(0)\n2\n1\n0\nSome(8)\nSome(3)\n",
    );
}

/// B-2026-10-19-19: a `let` that keeps a view of a temporary gets a machine
/// fix where one edit makes the value owned: `.to_string()` after a `Str` read
/// from a temporary `String`, and `.cloned()` before an `unwrap_or` whose
/// default is the temporary. Each fixed program runs.
#[test]
fn a_view_of_a_temporary_is_fixed_by_owning_it() {
    let want = "a reference to a temporary outlives its statement";
    let str_view = r#"fn main() {
    let line = "key = value".to_string();
    let pos = 4;
    let name = line.substring(0, pos).trim();
    println(name);
}
"#;
    rejected_then_fixed_by("temp-str-view", str_view, want, ".trim().to_string();");
    accepted_and_runs(
        "temp-str-view-fixed",
        &str_view.replace(".trim();", ".trim().to_string();"),
        "key\n",
    );
    let default = r#"fn main() {
    let mut m: Map[String, Vec[i64]] = Map.new();
    m.insert("a".to_string(), vec![3, 1, 2]);
    let k = "a".to_string();
    let mut samples = m.get(k).unwrap_or(Vec.new());
    samples.sort();
    println(f"{samples.len()}");
}
"#;
    rejected_then_fixed_by(
        "temp-default",
        default,
        want,
        "m.get(k).cloned().unwrap_or(Vec.new())",
    );
    accepted_and_runs(
        "temp-default-fixed",
        &default.replace(".get(k).unwrap_or", ".get(k).cloned().unwrap_or"),
        "3\n",
    );
}

/// B-2026-10-19-7, B-2026-10-19-18: `karac check` runs the MIR borrow check
/// once its own rules pass, so a borrow held in a local across a mutation of
/// what it borrows is refused at check, as every MIR surface refuses it, and
/// the message names the binding that holds the borrow. The copying form
/// passes and runs.
#[test]
fn check_refuses_what_the_mir_borrow_check_refuses() {
    let lru = r#"shared struct Node {
    value: i64,
}

struct Cache {
    index: Map[String, Node],
    hits: i64,
}

impl Cache {
    fn touch(mut ref self, node: Node) {
        self.hits += node.value;
    }

    fn get(mut ref self, key: String) -> Option[i64] {
        let found = self.index.get(key);
        match found {
            Some(node) => {
                self.touch(node);
                Some(node.value)
            }
            None => None,
        }
    }
}

fn main() {
    let mut c = Cache { index: Map.new(), hits: 0 };
    c.index.insert("a".to_string(), Node { value: 5 });
    println(f"{c.get("a".to_string()).unwrap_or(0)} {c.hits}");
}
"#;
    rejected(
        "mir-borrowck-lru",
        lru,
        "cannot mutably borrow `self` while `node` still holds a borrow of it, taken at 16:21",
    );
    accepted_and_runs(
        "mir-borrowck-lru-cloned",
        &lru.replace("self.index.get(key);", "self.index.get(key).cloned();"),
        "5 5\n",
    );
    rejected(
        "mir-borrowck-map-default",
        r#"fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1, 5);
    let before = m.get(1).unwrap_or(0);
    m.insert(1, 6);
    println(f"{before}");
}
"#,
        "cannot mutably borrow `m` while `before` still holds a borrow of it",
    );
    rejected(
        "mir-borrowck-vec-last",
        r#"fn main() {
    let mut v = vec![1, 9];
    match v.last() {
        Some(top) => {
            let largest = top;
            v.pop();
            v.push(largest);
        }
        None => {}
    }
    println(f"{v}");
}
"#,
        "cannot mutably borrow `v` while `largest` still holds a borrow of it",
    );
    rejected(
        "mir-borrowck-field",
        r#"struct S { v: Vec[i64], n: i64 }
impl S {
    fn bump(mut ref self) { self.n += 1; }
    fn go(mut ref self) -> i64 {
        let f = self.v.get(0);
        self.v.push(3);
        f.unwrap_or(0)
    }
}
fn main() {
    let mut s = S { v: vec![1], n: 0 };
    println(f"{s.go()}");
}
"#,
        "cannot mutably borrow `self.v` while `f` still holds a borrow of it",
    );
}

/// B-2026-10-19-14: an iterator held in a variable gets §5.10's copy rule
/// from its type, as the same chain written inline does, so `*c` over a named
/// `chars()` is refused with the drop-the-`*` fix. A named `v.iter()` still
/// lends `ref` items.
#[test]
fn a_named_value_iterator_closure_parameter_is_a_copy() {
    rejected_then_fixed_by(
        "named-chars-deref",
        r#"fn main() {
    let w = "QqqQ";
    let it = w.chars();
    println(f"{it.filter(|c| *c == 'Q').count()}");
}
"#,
        "a 'char' is copied, so drop the `*`",
        "it.filter(|c| c == 'Q')",
    );
    accepted_and_runs(
        "named-iters",
        r#"fn main() {
    let w = "QqqQ";
    let it = w.chars();
    println(f"{it.filter(|c| c == 'Q').count()}");
    let r = (0..10).rev();
    println(f"{r.filter(|j| j % 2 == 0).count()}");
    let v = vec![1, 2, 3];
    let vi = v.iter();
    println(f"{vi.filter(|x| *x > 1).count()}");
}
"#,
        "2\n5\n2\n",
    );
}

/// B-2026-10-19-10: a library module's type the import did not list stays
/// private to the module, so a program's own `Duration` beside
/// `import std.time.{sleep};` is its own type, and an enum's patterns in the
/// module's own `impl` follow the rename. A mix-up of the two reads by the
/// library type's path.
#[test]
fn a_library_type_does_not_meet_a_program_type_of_its_name() {
    accepted_and_runs(
        "library-type-private",
        "import std.time.{sleep};\n\
         struct Duration { n: i64 }\n\
         fn main() { let d = Duration { n: 1 }; println(f\"{d.n}\"); }\n",
        "1\n",
    );
    accepted_and_runs(
        "library-enum-private",
        "import std.encoding.{base64_decode};\n\
         enum DecodeError { Mine }\n\
         fn main() {\n\
             let e = DecodeError.Mine;\n\
             match e { DecodeError.Mine => println(\"mine\") }\n\
             match base64_decode(\"abc\") {\n\
                 Ok(b) => println(f\"{b.len()}\"),\n\
                 Err(err) => println(err.to_string()),\n\
             }\n\
         }\n",
        "mine\ninvalid length\n",
    );
    rejected(
        "library-type-private-mixup",
        "import std.time.{sleep};\n\
         struct Duration { n: i64 }\n\
         fn main() { sleep(Duration { n: 1 }); }\n",
        "std.time.Duration",
    );
}

/// B-2026-10-19-15: `let v = counter;` with `counter: mut ref i64` reborrows
/// (a `mut ref` is not `Copy`, core-semantics.md §1.1), so a use of
/// `counter` while `v` is live is refused, and the error names the use's
/// line rather than the `let`'s.
#[test]
fn a_use_of_a_reborrowed_mut_ref_is_refused_at_the_use() {
    rejected(
        "reborrowed-mut-ref-use",
        "fn take(counter: mut ref i64) -> i64 {\n\
             let v = counter;\n\
             *counter += 1;\n\
             v\n\
         }\n\
         fn main() {\n\
             let mut c = 5;\n\
             let got = take(mut c);\n\
             println(f\"{got} {c}\");\n\
         }\n",
        ":3:1: cannot read `counter` while `v` still holds a borrow of it, taken at 2:9",
    );
    accepted_and_runs(
        "reborrowed-mut-ref-copy",
        "fn take(counter: mut ref i64) -> i64 {\n\
             let v = *counter;\n\
             *counter += 1;\n\
             v\n\
         }\n\
         fn main() {\n\
             let mut c = 5;\n\
             let got = take(mut c);\n\
             println(f\"{got} {c}\");\n\
         }\n",
        "5 6\n",
    );
}

/// B-2026-10-19-17: a library type is not offered a `#[derive(Clone)]`, whose
/// edit would land outside the program's file, so `karac fix` applies the
/// `.collect()` fixes of an uncollected iterator reused in a loop instead of
/// aborting the whole file.
#[test]
fn fix_collects_an_iterator_reused_in_a_loop() {
    rejected_then_fixed_by(
        "fix-library-derive",
        "fn main() {\n\
             let text = String.from(\"zero one two\");\n\
             let names = text.split_whitespace();\n\
             for i in 0..2 {\n\
                 println(f\"{names[i]}\");\n\
                 for w in names {\n\
                     println(f\"{w}\");\n\
                 }\n\
             }\n\
         }\n",
        "is moved inside a loop",
        "let names: Vec[Str] = text.split_whitespace().collect();",
    );
}

/// B-2026-10-19-16: plain `=` does not write through a `mut ref` binding
/// (design.md § Auto-deref), so `counter = counter + 1` is refused and
/// `karac fix` writes `*counter = counter + 1`. A `let mut` binding of an
/// explicit `mut ref` stays a borrow (B-2026-10-19-29): `r += 1` writes the
/// place it borrows, and `r = mut ref b` rebinds it.
#[test]
fn plain_assign_does_not_write_through_a_mut_ref() {
    rejected_then_fixed_by(
        "plain-assign-mut-ref",
        "fn inc(counter: mut ref i64) {\n\
             counter = counter + 1;\n\
         }\n\
         fn main() {\n\
             let mut c = 0;\n\
             inc(mut c);\n\
             println(f\"{c}\");\n\
         }\n",
        "plain `=` does not write through it",
        "*counter = counter + 1;",
    );
    rejected(
        "plain-assign-mut-ref-variant",
        "fn set(x: mut ref Result[i64, i64]) { x = Err(7); }\n\
         fn main() {\n\
             let mut r: Result[i64, i64] = Ok(1);\n\
             set(mut r);\n\
             println(f\"{r.is_err()}\");\n\
         }\n",
        "`x` is a `mut ref Result[i64, i64]`, and plain `=` does not write through it",
    );
    accepted_and_runs(
        "let-mut-mut-ref-rebinds",
        "fn main() {\n\
             let mut a = 1;\n\
             let mut b = 2;\n\
             let mut r = mut ref a;\n\
             r += 1;\n\
             r = mut ref b;\n\
             r += 10;\n\
             println(f\"{a} {b}\");\n\
         }\n",
        "2 12\n",
    );
}

/// B-2026-10-19-36 (core-semantics §1.1): a closure that writes a capture is
/// a MutFn, never Copy, so binding it to another name moves it. An `Fn`
/// closure with Copy captures still copies.
#[test]
fn a_mut_fn_closure_moves_when_bound_to_another_name() {
    rejected(
        "mut-fn-not-copy",
        "fn main() {\n\
             let mut n = 0;\n\
             let mut f = || { n += 1; };\n\
             let mut g = f;\n\
             f();\n\
             g();\n\
             println(f\"{n}\");\n\
         }\n",
        "use of moved value `f`: it writes a captured binding, so it is a MutFn",
    );
    accepted_and_runs(
        "mut-fn-moved-then-used-through-new-name",
        "fn main() {\n\
             let mut n = 0;\n\
             let mut f = || { n += 1; };\n\
             let mut g = f;\n\
             g();\n\
             g();\n\
             println(f\"{n}\");\n\
         }\n",
        "2\n",
    );
    accepted_and_runs(
        "fn-closure-copies",
        "fn main() {\n\
             let k = 3;\n\
             let f = |x: i64| x + k;\n\
             let g = f;\n\
             println(f\"{f(1)} {g(2)}\");\n\
         }\n",
        "4 5\n",
    );
}

/// B-2026-10-19-61 (core-semantics §3.7): an index move reports the code the
/// spec names, in JSON as well as in the text, and names the element as
/// written.
#[test]
fn an_index_move_reports_e_index_move_non_copy() {
    let (dir, path) = fixture(
        "index-move-code",
        "fn take(s: own String) { println(s); }\n\
         fn main() {\n\
             let v: Vec[String] = [\"a\".to_string()];\n\
             take(v[0]);\n\
         }\n",
    );
    let out = karac()
        .arg("check")
        .arg("--output=json")
        .arg(&path)
        .output()
        .unwrap();
    let json =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        json.contains("\"code\":\"E_INDEX_MOVE_NON_COPY\""),
        "{json}"
    );
    assert!(
        json.contains("cannot move `v[0]` out of an index"),
        "{json}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-19-42 (core-semantics §4.6): a struct pattern over a `shared
/// struct` is irrefutable, as over a plain one, and a binding through the
/// handle borrows rather than moves.
#[test]
fn a_shared_struct_pattern_is_exhaustive() {
    accepted_and_runs(
        "shared-struct-pattern-exhaustive",
        "shared struct H { name: String, n: i64 }\n\
         fn main() {\n\
             let h = H { name: \"n\".to_string(), n: 2 };\n\
             match h {\n\
                 H { name, n } => println(f\"{name} {n}\"),\n\
             }\n\
         }\n",
        "n 2\n",
    );
    rejected(
        "shared-struct-pattern-refutable",
        "shared struct H { name: String, n: i64 }\n\
         fn main() {\n\
             let h = H { name: \"n\".to_string(), n: 2 };\n\
             match h {\n\
                 H { n: 1, .. } => println(\"one\"),\n\
             }\n\
         }\n",
        "non-exhaustive match",
    );
    rejected(
        "shared-struct-pattern-move",
        "shared struct H { name: String }\n\
         fn take(s: own String) { println(s); }\n\
         fn main() {\n\
             let h = H { name: \"n\".to_string() };\n\
             match h {\n\
                 H { name } => take(name),\n\
             }\n\
         }\n",
        "cannot move 'name'",
    );
}

/// B-2026-10-19-60 (design.md § Collection literals): `[:]` is the empty
/// map, typed from what is expected, and a map literal is a `SortedMap`
/// where one is expected.
#[test]
fn the_empty_map_literal_and_a_sorted_map_literal() {
    accepted_and_runs(
        "empty-and-sorted-map-literals",
        "fn main() {\n\
             let mut m: Map[String, i64] = [:];\n\
             m.insert(\"a\".to_string(), 1);\n\
             let mut n = [:];\n\
             n.insert(4, true);\n\
             let mut s: SortedMap[i64, i64] = [:];\n\
             s.insert(3, 1);\n\
             s.insert(2, 3);\n\
             let t: SortedMap[String, i64] = [\"b\": 2, \"a\": 1];\n\
             for (k, v) in t.iter() { println(f\"{k} {v}\"); }\n\
             for (k, v) in s.iter() { println(f\"{k} {v}\"); }\n\
             println(f\"{m.len()} {n.len()}\");\n\
         }\n",
        "a 1\nb 2\n2 3\n3 1\n1 1\n",
    );
}

/// B-2026-10-19-33: `extend` consumes its argument, so a bare (borrowed)
/// `Vec` parameter is refused there as it is by `push`, with `.clone()` as
/// the fix; an owned one is accepted.
#[test]
fn extend_cannot_consume_a_borrowed_collection() {
    rejected_then_fixed(
        "extend-borrowed-param",
        "fn tagged(tag: Str, rest: Vec[String]) -> Vec[String] {\n\
             let mut v = vec![tag.to_string()];\n\
             v.extend(rest);\n\
             v\n\
         }\n\
         fn main() {\n\
             let r = tagged(\"a\", vec![\"b\".to_string()]);\n\
             println(f\"{r.len()}\");\n\
         }\n",
        "`extend` consumes its argument, and this is a borrowed 'Vec[String]'",
    );
    accepted_and_runs(
        "extend-owned-param",
        "fn tagged(tag: Str, rest: own Vec[String]) -> Vec[String] {\n\
             let mut v = vec![tag.to_string()];\n\
             v.extend(rest);\n\
             v\n\
         }\n\
         fn main() {\n\
             let r = tagged(\"a\", vec![\"b\".to_string()]);\n\
             println(f\"{r.len()} {r[1]}\");\n\
         }\n",
        "2 b\n",
    );
}

/// B-2026-10-15-106 (design.md § Collection literals): a bare `[v; n]` always
/// builds an `Array`. A constant count gives an `Array[T, n]`; a count that is
/// not a constant, and a `[v; n]` where a `Vec` is wanted, are refused with
/// the `Vec.filled(n, v)` fix-it, nested ones over two passes.
#[test]
fn a_repeat_literal_is_an_array_and_a_vec_is_vec_filled() {
    accepted_and_runs(
        "repeat-literal-constant-array",
        "const SIZE: usize = 4;\n\
         fn main() {\n\
             let mut counts = [0; 26];\n\
             counts[2] += 5;\n\
             let b = [7; SIZE];\n\
             let grid = [[0u8; 3]; 2];\n\
             println(f\"{counts.len()} {counts[2]} {b.len()} {b[3]} {grid[1].len()}\");\n\
         }\n",
        "26 5 4 7 3\n",
    );
    rejected_then_fixed_by(
        "repeat-literal-runtime-count",
        "fn main() {\n\
             let n = 5;\n\
             let mut v = [0; n];\n\
             v.push(1);\n\
             println(f\"{v.len()}\");\n\
         }\n",
        "its count must be a compile-time constant: write `Vec.filled(n, 0)`",
        "let mut v = Vec.filled(n, 0);",
    );
    rejected_then_fixed_by(
        "repeat-literal-vec-annotation",
        "fn total(v: Vec[i64]) -> i64 { v.len() as i64 }\n\
         fn main() {\n\
             let v: Vec[i64] = [0; 8];\n\
             println(f\"{v.len()} {total([3; 2])}\");\n\
         }\n",
        "a `[v; n]` literal builds an `Array`, not a `Vec`: write `Vec.filled(8, 0)`",
        "total(Vec.filled(2, 3))",
    );
    rejected_then_fixed_in(
        "repeat-literal-nested-vec",
        "fn main() {\n\
             let g: Vec[Vec[i64]] = [[0; 3]; 3];\n\
             println(f\"{g.len()}\");\n\
         }\n",
        "not a `Vec`: write",
        "Vec.filled(3, Vec.filled(3, 0))",
        2,
    );
}

/// B-2026-10-19-23: `m.lock()` returns a `MutexGuard` that releases the
/// mutex when it drops, and a `lock` block is one in a scope of its own.
/// Four tasks that hold the mutex across a sleep end at 4 (plus the 100 a
/// `lock` body adds after an early `return` released it), and
/// docs/library/concurrency.md's `g.value.count += 1` through a `let g`
/// guard checks and runs, on both MIR backends.
#[test]
fn a_mutex_guard_excludes_and_releases_on_every_exit() {
    for (tag, src, want) in [
        (
            "guard-across-sleep",
            r#"import std.time.{sleep, Duration};
fn bump(m: Mutex[i64]) {
    let g = m.lock();
    let v = *g.value;
    sleep(Duration.ms(2));
    *g.value = v + 1;
}
fn early(m: Mutex[i64], stop: bool) -> i64 {
    lock m x {
        if stop { return 9; }
        x += 100;
    }
    0
}
fn main() {
    let m = Mutex.new(0);
    {
        let g = TaskGroup.new();
        for _ in 0..4 {
            g.spawn(|| bump(m));
        }
    }
    let a = early(m, true);
    let b = early(m, false);
    let g = m.lock();
    println(f"{*g.value} {a} {b}");
}
"#,
            "104 9 0
",
        ),
        (
            "guard-doc-example",
            r#"struct Totals { count: i64, sum: i64 }

sync struct Tally {
    name: String,
    totals: Mutex[Totals],
}

impl Tally {
    fn record(self, n: i64) {
        let g = self.totals.lock();
        g.value.count += 1;
        g.value.sum += n;
    }
}

fn first_even(m: Mutex[Vec[i64]]) -> Option[i64] {
    lock m v {
        for x in v.iter() {
            if *x % 2 == 0 { return Some(*x); }
        }
    }
    None
}

fn main() {
    let s = Tally { name: "s", totals: Mutex.new(Totals { count: 0, sum: 0 }) };
    s.record(3);
    s.record(4);
    let g = s.totals.lock();
    println(f"{s.name} {g.value.count} {g.value.sum}");
    let m = Mutex.new(vec![1, 3, 4, 5]);
    let a = first_even(m);
    let b = first_even(m);
    println(f"{a} {b}");
}
"#,
            "s 2 7
Some(4) Some(4)
",
        ),
    ] {
        accepted(tag, src);
        let (dir, path) = fixture(tag, src);
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
                "{tag} {runner}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// B-2026-10-18-44, B-2026-10-18-43: a caller's `T.Output` passed to a
/// generic call is not projected again, and `W { .. } + W { .. }` on
/// `impl[T: Add] Add for W[T]` reads the impl's `T` at the operands'.
#[test]
fn check_a_callers_projection_passes_through_a_generic_call() {
    accepted(
        "projection-through-call",
        r#"
enum E[T] { A(T), N }
struct V { x: i64 }
impl Add for V { type Output = V; fn add(self, rhs: V) -> V { V { x: self.x + rhs.x } } }
impl Neg for V { type Output = V; fn neg(self) -> V { V { x: 0 - self.x } } }
struct W[T] { v: T }
impl[T: Add] Add for W[T] { type Output = W[T.Output]; fn add(self, rhs: W[T]) -> W[T.Output] { W { v: self.v + rhs.v } } }
impl[T: Neg] Neg for W[T] { type Output = W[T.Output]; fn neg(self) -> W[T.Output] { W { v: -self.v } } }
fn pair[T: Add](a: own T, b: own T) -> Option[T.Output] { Some(a + b) }
fn tagged[T: Add](a: own T, b: own T) -> E[T.Output] { let s = a + b; E.A(s) }
fn id[T](x: own T) -> T { x }
fn through[T: Add](a: own T, b: own T) -> T.Output { let s = a + b; id(s) }
fn twice[T: Add](a: own W[T], b: own W[T]) -> W[T.Output] { a + b }
fn main() {
    println(f"{pair(3, 4).unwrap()} {through(5, 6)}");
    match tagged(V { x: 1 }, V { x: 2 }) {
        E.A(v) => println(f"{v.x}"),
        E.N => println("none"),
    }
    let w = W { v: V { x: 1 } } + W { v: V { x: 9 } };
    let n = -W { v: V { x: 4 } };
    let i = W { v: 3 } + W { v: 4 };
    let t = twice(W { v: V { x: 2 } }, W { v: V { x: 5 } });
    println(f"{w.v.x} {n.v.x} {i.v} {t.v.x}");
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
import std.json.{Json};

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
         \x20   let mut it = (0..3).iter();\n\
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
    rejected_then_fixed_by(
        "append-call-arg",
        "fn seven() -> Vec[i64] { vec![7] }\n\
         fn main() {\n\
             let mut d: Vec[i64] = vec![1];\n\
             d.append(seven());\n\
             println(f\"{d}\");\n\
         }\n",
        "there is no `Vec.append`",
        "d.extend(seven());",
    );
}

/// A library iterator has no `.iter()`: `karac fix` drops the call.
#[test]
fn iter_on_an_iterator_is_dropped() {
    rejected_then_fixed_by(
        "iter-on-split",
        "fn main() {\n\
             let mut n = 0;\n\
             for w in \"a b c\".split(\" \").iter() {\n\
                 n += w.len();\n\
             }\n\
             println(n);\n\
         }\n",
        "which is already an iterator",
        "for w in \"a b c\".split(\" \") {",
    );
}

/// design.md § String literals: a literal is a `Str` wherever one is
/// expected, which includes a branch beside a `Str` view (B-2026-10-15-110)
/// and a `Vec[Str]` push (B-2026-10-14-68).
#[test]
fn a_string_literal_takes_the_str_type_of_its_context() {
    accepted(
        "literal-beside-view",
        "fn rest(line: Str) -> i64 {\n\
             let (n, a) = match line.find(\" \") {\n\
                 Some(i) => (1, line[i + 1..line.len()].trim()),\n\
                 None => (2, \"\"),\n\
             };\n\
             let b = if a.is_empty() { \"-\" } else { a };\n\
             n + b.len()\n\
         }\n\
         fn main() {\n\
             let mut v: Vec[Str] = Vec.new();\n\
             v.push(\"a\");\n\
             println(f\"{rest(\"x y\")} {v.len()}\");\n\
         }\n",
    );
    rejected_with(
        "owned-beside-view",
        "fn rest(line: Str) -> i64 {\n\
             let a = match line.find(\" \") {\n\
                 Some(i) => line[i + 1..line.len()].trim(),\n\
                 None => line.to_uppercase(),\n\
             };\n\
             a.len()\n\
         }\n\
         fn main() { println(rest(\"x y\")); }\n",
        "match arms have incompatible types",
    );
}

/// The app rows of 2026-10-14: programs `karac check` refused or let through
/// while every MIR backend agreed with the spec.
#[test]
fn app_rows_of_2026_10_14() {
    // B-2026-10-14-53: `fs.write` borrows its arguments (`Str` parameters).
    accepted(
        "fs-write-borrows",
        "fn main() {\n\
             let out = \"hello\".to_string();\n\
             let r = fs.write(\"o.txt\".to_string(), out);\n\
             println(f\"{out.len()} {r.is_ok()}\");\n\
         }\n",
    );
    // B-2026-10-14-60: an element of a borrowing walk of a `ref` parameter
    // is returned as a borrow of it.
    accepted(
        "return-walked-element",
        "fn pick(v: ref Vec[(String, i64)], name: Str) -> Option[ref i64] {\n\
             for (k, it) in v.iter() { if k == name { return Some(it); } }\n\
             None\n\
         }\n\
         fn first(v: ref Vec[String]) -> Option[ref String] { for s in v.iter() { return Some(s); } None }\n\
         fn main() {\n\
             let v: Vec[(String, i64)] = [(\"a\".to_string(), 1), (\"b\".to_string(), 2)];\n\
             match pick(v, \"b\") { Some(x) => println(f\"{*x}\"), None => println(\"none\") }\n\
             let w: Vec[String] = [\"q\".to_string()];\n\
             match first(w) { Some(s) => println(s), None => println(\"none\") }\n\
         }\n",
    );
    // The assignment form (flag_eval): the element is stored in an
    // `Option[ref Flag]` declared outside the loop.
    accepted(
        "assign-walked-element",
        "struct Flag { name: String, on: bool }\n\
         fn find(flags: ref Vec[Flag], name: Str) -> bool {\n\
             let mut found: Option[ref Flag] = None;\n\
             for f in flags.iter() { if f.name == name { found = Some(f); } }\n\
             match found { Some(f) => f.on, None => false }\n\
         }\n\
         fn main() {\n\
             let flags = vec![Flag { name: \"a\".to_string(), on: true }];\n\
             println(f\"{find(flags, \"a\")} {find(flags, \"c\")}\");\n\
         }\n",
    );
    rejected(
        "return-walked-local",
        "fn bad() -> Option[ref String] {\n\
             let v: Vec[String] = vec![\"q\".to_string()];\n\
             for s in v.iter() { return Some(s); }\n\
             None\n\
         }\n\
         fn main() { match bad() { Some(s) => println(s), None => println(\"none\") } }\n",
        "borrows function-local storage",
    );
    // B-2026-10-14-51: a range view of a field is a view of `self`, not of a
    // temporary.
    accepted(
        "self-field-view-let",
        "struct Cursor { bytes: Vec[u8], at: i64 }\n\
         impl Cursor {\n\
             fn peek(ref self, n: i64) -> Slice[u8] {\n\
                 let s = self.bytes[self.at..self.at + n];\n\
                 s\n\
             }\n\
         }\n\
         fn main() {\n\
             let c = Cursor { bytes: vec![1u8, 2u8, 3u8], at: 0 };\n\
             println(f\"{c.peek(2).len()}\");\n\
         }\n",
    );
    // B-2026-10-14-55: an unknown method on an ambient resource.
    rejected(
        "ambient-unknown-method",
        "fn main() { let r = fs.bogus_op(\"x\".to_string()); println(\"ran\"); }\n",
        "no method 'bogus_op' on 'FileSystem'",
    );
    // B-2026-10-14-62: a stored function value is called in place, not
    // moved out of a borrow.
    rejected_with(
        "fn-field-move",
        "struct R { h: Fn(i64) -> i64 }\n\
         fn dbl(x: i64) -> i64 { x * 2 }\n\
         fn main() {\n\
             let rs = vec![R { h: dbl }];\n\
             let mut t = 0;\n\
             for r in rs.iter() { let h = r.h; t += h(1000); }\n\
             println(t);\n\
         }\n",
        "cannot move a non-`Copy` value out of a borrowed place",
    );
    accepted(
        "fn-field-call-in-place",
        "struct R { h: Fn(i64) -> i64 }\n\
         fn dbl(x: i64) -> i64 { x * 2 }\n\
         fn main() {\n\
             let rs = vec![R { h: dbl }];\n\
             let mut t = 0;\n\
             for r in rs.iter() { t += (r.h)(1000); }\n\
             println(t);\n\
         }\n",
    );
    // B-2026-10-14-70: a chained field receiver runs on every MIR backend.
    accepted(
        "chained-field-receiver",
        "struct Env { to: Vec[String] }\n\
         struct S { env: Env }\n\
         impl S {\n\
             fn add(mut ref self, a: Str) -> i64 {\n\
                 self.env.to.push(a.to_string());\n\
                 self.env.to.len()\n\
             }\n\
         }\n\
         fn main() { let mut s = S { env: Env { to: Vec.new() } }; println(s.add(\"x\")); }\n",
    );
    // Views at lookup: `m[k]` reads a `String`-keyed map by a `Str`.
    accepted(
        "map-index-by-view",
        "fn main() {\n\
             let mut m: Map[String, i64] = Map.new();\n\
             m.insert(\"ab\".to_string(), 3);\n\
             for name in \"x/ab\".split(\"/\") {\n\
                 if m.contains_key(name) { println(m[name]); }\n\
             }\n\
         }\n",
    );
}

/// The app rows of 2026-10-15 and the widened 10-14 ones.
#[test]
fn app_rows_of_2026_10_15() {
    // B-2026-10-14-53, widened: `fs.write`'s path and `env.var`'s name
    // borrow too.
    accepted(
        "ambient-path-borrows",
        "fn main() {\n\
             let name = \"HOME\".to_string();\n\
             let v = env.var(name);\n\
             let path = \"x.txt\".to_string();\n\
             let r = fs.write(path, \"abc\".to_string());\n\
             println(f\"{name} {path} {r.is_ok()} {v.is_ok()}\");\n\
         }\n",
    );
    // B-2026-10-14-68, widened: a literal beside a `Str` const in `vec!`.
    accepted(
        "vec-literal-beside-str-const",
        "const AB: Str = \"ab\";\n\
         fn main() {\n\
             let files = vec![(\"one\", AB), (\"two\", \"cd\")];\n\
             println(f\"{files.len()} {files[1].1}\");\n\
         }\n",
    );
    // B-2026-10-14-68, widened again: an array literal mixing a `Str` const
    // and a literal, in either order.
    accepted(
        "array-literal-beside-str-const",
        "const DB: Str = \"kv.db\";\n\
         fn main() {\n\
             for p in [DB, \"kv.wal\"].iter() { println(f\"{p}\"); }\n\
             let b = [\"kv.wal\", DB];\n\
             println(f\"{b.len()}\");\n\
         }\n",
    );
    // B-2026-10-15-7: a `match v[i]` arm binding is a `ref`; reading it
    // with `==` or in a format hole is no move, and moving it still is.
    accepted(
        "index-match-arm-reads",
        "enum Tok { Sym(String), End }\n\
         fn is_sym(toks: ref Vec[Tok], s: Str) -> bool {\n\
             match toks[0] { Tok.Sym(t) => t == s, _ => false }\n\
         }\n\
         fn main() {\n\
             let v = vec![Tok.Sym(\"ab\".to_string())];\n\
             match v[0] { Tok.Sym(t) => println(f\"local {t}\"), Tok.End => {} }\n\
             println(f\"{is_sym(v, \"ab\")} {v.len()}\");\n\
         }\n",
    );
    rejected(
        "index-match-arm-moves",
        "enum Tok { Sym(String), End }\n\
         fn take(s: own String) -> i64 { s.len() }\n\
         fn main() {\n\
             let v = vec![Tok.Sym(\"ab\".to_string())];\n\
             let n = match v[0] { Tok.Sym(t) => take(t), Tok.End => 0 };\n\
             println(f\"{n} {v.len()}\");\n\
         }\n",
        "cannot move 't'",
    );
    // B-2026-10-15-12: a closure run by `any` stops borrowing its captures
    // when `any` returns.
    accepted(
        "closure-arg-borrow-ends",
        "struct D { name: String, n: i64 }\n\
         fn main() {\n\
             let skip: Vec[String] = vec![\"b\".to_string()];\n\
             let all = vec![D { name: \"a\".to_string(), n: 1 }, D { name: \"b\".to_string(), n: 2 }];\n\
             let mut kept: Vec[D] = Vec.new();\n\
             for d in all.into_iter() {\n\
                 let redo = skip.iter().any(|s| s == d.name);\n\
                 if redo { continue; }\n\
                 kept.push(d);\n\
             }\n\
             println(f\"{kept.len()}\");\n\
         }\n",
    );
    // B-2026-10-15-20: a loop-local `let mut` written in an `if` and then
    // stored is declared afresh each iteration, so nothing is reused; a
    // binding declared outside the loop, or read after the store, still is.
    let body = |decl_in: bool, after: &str| {
        format!(
            "struct It {{ n: i64 }}\n\
             struct S {{ v: Vec[It] }}\n\
             impl S {{ fn take(mut ref self, x: own It) {{ self.v.push(x); }} }}\n\
             fn main() {{\n\
                 let mut s = S {{ v: Vec.new() }};\n\
                 let ks = vec![1, 2, 3];\n\
                 {outer}\n\
                 for k in ks.iter() {{\n\
                     {inner}\n\
                     if k > 1 {{ i.n -= 1; }}\n\
                     s.take(i);\n\
                     {after}\n\
                 }}\n\
                 println(f\"{{s.v.len()}}\");\n\
             }}\n",
            outer = if decl_in {
                ""
            } else {
                "let mut i = It { n: 0 };"
            },
            inner = if decl_in {
                "let mut i = It { n: k };"
            } else {
                ""
            },
        )
    };
    accepted("loop-local-written-then-stored", &body(true, ""));
    rejected(
        "outer-written-then-stored",
        &body(false, ""),
        "is moved inside a loop",
    );
    rejected(
        "loop-local-read-after-store",
        &body(true, "println(f\"{i.n}\");"),
        "moved into a container",
    );
    // B-2026-10-15-23: the arguments of `find` on a `Str` and of `replacen`
    // are read; a `String` method declared to own its argument still moves.
    accepted(
        "string-builtin-args-are-read",
        "fn f(line: Str) -> i64 {\n\
             let op = \"+=\";\n\
             let k = line.find(op);\n\
             op.len() + k.unwrap_or(0)\n\
         }\n\
         fn main() {\n\
             let s = \"x\".to_string();\n\
             let a = \"%.o\".replacen(\"%\", s, 1);\n\
             let b = \"%.c\".replacen(\"%\", s, 1);\n\
             println(f\"{f(\"a+=b\")} {a} {b} {s}\");\n\
         }\n",
    );
    rejected(
        "string-method-owning-its-arg",
        "impl String { fn eat(ref self, x: own String) -> i64 { x.len() } }\n\
         fn main() {\n\
             let s = \"x\".to_string();\n\
             let a = \"ab\".to_string();\n\
             let n = a.eat(s);\n\
             println(f\"{n} {s}\");\n\
         }\n",
        "moved",
    );
    // B-2026-10-15-16: a `let` that keeps `unwrap_or(String.new())` or a
    // `??` with an f-string default borrows a temporary; the empty `String`
    // is fixed to the static `""`. Used within the statement it is fine.
    let get_or = |default: &str, keep: bool| {
        let read = format!("m.get(\"a\").unwrap_or({default})");
        let body = if keep {
            format!("let v = {read};\n    println(f\"{{v}}\");")
        } else {
            format!("println(f\"{{{read}.len()}}\");")
        };
        format!(
            "fn main() {{\n    let mut m: SortedMap[String, String] = SortedMap.new();\n    \
             m.insert(\"a\".to_string(), \"1\".to_string());\n    {body}\n}}\n"
        )
    };
    rejected_then_fixed_by(
        "unwrap-or-string-new-kept",
        &get_or("String.new()", true),
        "a reference to a temporary outlives its statement",
        "unwrap_or(\"\")",
    );
    accepted(
        "unwrap-or-string-new-in-statement",
        &get_or("String.new()", false),
    );
    accepted("unwrap-or-literal-kept", &get_or("\"\"", true));
    rejected(
        "coalesce-fstring-kept",
        "fn main() {\n\
             let mut m: SortedMap[String, String] = SortedMap.new();\n\
             let v = m.get(\"a\") ?? f\"{1}\";\n\
             println(f\"{v}\");\n\
         }\n",
        "a reference to a temporary outlives its statement",
    );
}

/// B-2026-10-18-38: a string literal lent to a view parameter is static
/// text (§5.1), so a result that keeps it is fine; an f-string is still a
/// temporary.
#[test]
fn string_literal_argument_to_a_view_parameter_is_not_a_temporary() {
    let prog = |arg: &str| {
        format!(
            "fn tail(line: Str) -> Str {{ line }}\n\
             fn main() {{\n\
                 let n = 1;\n\
                 let g = tail({arg});\n\
                 println(f\"{{g}}{{n}}\");\n\
             }}\n"
        )
    };
    accepted("literal-into-view-param-kept", &prog("\"abc\""));
    rejected(
        "fstring-into-view-param-kept",
        &prog("f\"a{n}\""),
        "a reference to a temporary outlives its statement",
    );
}

/// B-2026-10-15-14: `mut ref place` borrows a place for writing; a `let`
/// annotation never borrows, and its fix writes the operator.
#[test]
fn mut_ref_place_borrows_a_place_for_writing() {
    let prog = |body: &str| {
        format!(
            "struct P {{ n: i64 }}\n\
             struct W {{ p: P, ps: Vec[P] }}\n\
             fn main() {{\n\
                 {body}\n\
             }}\n"
        )
    };
    let w = "let mut w = W { p: P { n: 1 }, ps: vec![P { n: 1 }, P { n: 2 }] };";
    accepted(
        "mut-ref-place",
        &prog(&format!(
            "{w}\n let p = mut ref w.p;\n p.n += 5;\n let e = mut ref w.ps[1];\n \
             e.n += 1;\n println(f\"{{w.p.n}} {{w.ps[1].n}}\");"
        )),
    );
    rejected(
        "mut-ref-place-needs-let-mut",
        &prog("let w = W { p: P { n: 1 }, ps: Vec.new() };\n let p = mut ref w.p;\n p.n += 1;"),
        "cannot borrow `w` as `mut ref`",
    );
    rejected(
        "mut-ref-place-read-while-live",
        &prog(&format!(
            "{w}\n let p = mut ref w.p;\n println(f\"{{w.p.n}}\");\n p.n += 1;"
        )),
        "cannot read `w.p.n` while `mut ref p` borrows `w.p`",
    );
    rejected(
        "mut-ref-place-write-while-live",
        &prog(&format!(
            "{w}\n let p = mut ref w.p;\n w.p.n = 4;\n p.n += 1;"
        )),
        "cannot assign `w.p.n` while `mut ref p` borrows `w.p`",
    );
    rejected(
        "mut-ref-place-twice",
        &prog(&format!(
            "{w}\n let p = mut ref w.p;\n let q = mut ref w.p;\n q.n += 1;\n p.n += 1;"
        )),
        "while `mut ref p` borrows `w.p`",
    );
    rejected(
        "mut-ref-place-through-a-ref-param",
        "struct P { n: i64 }\n\
         fn f(p: ref P) { let q = mut ref p; q.n += 1; }\n\
         fn main() { let p = P { n: 1 }; f(p); }\n",
        "`p` is a `ref` parameter",
    );
    rejected(
        "mut-ref-of-a-temporary",
        &prog("let r = mut ref (P { n: 2 });\n println(f\"{r.n}\");"),
        "`mut ref` borrows a place",
    );
    // A use of the borrow before the place is read again is fine.
    accepted(
        "mut-ref-place-last-use-first",
        &prog(&format!(
            "{w}\n let p = mut ref w.p;\n p.n += 1;\n println(f\"{{w.p.n}}\");"
        )),
    );
    rejected_then_fixed_by(
        "annotation-is-not-a-mut-borrow",
        &prog(&format!(
            "{w}\n let p: mut ref P = w.p;\n p.n += 5;\n println(f\"{{w.p.n}}\");"
        )),
        "a type annotation does not borrow",
        "let p: mut ref P = mut ref w.p;",
    );
    rejected_then_fixed_by(
        "annotation-is-not-a-borrow",
        &prog(&format!(
            "{w}\n let r: ref P = w.ps[0];\n println(f\"{{r.n}} {{w.ps.len()}}\");"
        )),
        "a type annotation does not borrow",
        "let r: ref P = ref w.ps[0];",
    );
    // A place that already holds a reference is copied, not borrowed.
    accepted(
        "annotated-ref-copy",
        "struct P { n: i64 }\n\
         fn f(q: mut ref P) { let r: mut ref P = q; r.n += 1; }\n\
         fn main() { let mut p = P { n: 1 }; f(mut p); println(f\"{p.n}\"); }\n",
    );
}

/// Rows from the real-world apps of 2026-10-15 (mini_lua, mini_jinja).
#[test]
fn app_rows_of_2026_10_15_lua_jinja() {
    // B-2026-10-15-31: a `return` ends a `ref` local's reach, so a use in a
    // later branch does not keep the borrow live at a write in this one.
    accepted(
        "ref-borrow-ends-at-return",
        r#"struct P { toks: Vec[i64], p: i64, out: Vec[i64] }
impl P {
    fn add(mut ref self, v: i64) -> i64 {
        self.out.push(v);
        self.out.len() - 1
    }
    fn simple(mut ref self) -> i64 {
        let t = ref self.toks[self.p];
        if *t == 1 {
            self.p += 1;
            return self.add(10);
        }
        if *t == 2 {
            self.p += 1;
            return self.add(20);
        }
        -1
    }
}
fn main() {
    let mut p = P { toks: vec![2, 1], p: 0, out: Vec.new() };
    let a = p.simple();
    let b = p.simple();
    println(f"{a} {b} {p.out.len()}");
}
"#,
    );
    // ...but a use reachable after the write still does, through a branch
    // that falls through, inside the returned value, or after a `continue`.
    rejected(
        "ref-borrow-live-without-return",
        r#"struct P { toks: Vec[i64], out: Vec[i64] }
impl P {
    fn add(mut ref self, v: i64) -> i64 {
        self.out.push(v);
        self.out.len() - 1
    }
    fn simple(mut ref self) -> i64 {
        let t = ref self.toks[0];
        if *t == 1 {
            self.add(10);
        }
        if *t == 3 {
            return self.add(5) + *t;
        }
        *t
    }
}
fn main() {
    let mut p = P { toks: vec![2, 1], out: Vec.new() };
    println(f"{p.simple()}");
}
"#,
        "while `ref t` borrows",
    );
    rejected(
        "ref-borrow-live-past-continue",
        r#"struct P { toks: Vec[i64], p: i64, out: Vec[i64] }
impl P {
    fn add(mut ref self, v: i64) -> i64 {
        self.out.push(v);
        self.out.len() - 1
    }
    fn simple(mut ref self) -> i64 {
        let t = ref self.toks[self.p];
        if *t == 1 {
            self.add(10);
            return 0;
        }
        for k in 0..2 {
            if *t == 2 {
                self.add(20);
                continue;
            }
        }
        *t
    }
}
fn main() {
    let mut p = P { toks: vec![2, 1], p: 0, out: Vec.new() };
    println(f"{p.simple()}");
}
"#,
        "while `ref t` borrows",
    );
    // B-2026-10-15-32: a pattern binding moves its part out of an owned
    // scrutinee only in the arm or path where it bound.
    accepted(
        "scrutinee-moves-only-where-bound",
        r#"enum V { Text(String), Int(i64) }
fn show(v: ref V) -> String {
    match v {
        V.Text(s) => s.clone(),
        V.Int(i) => f"{i}",
    }
}
fn describe(v: own V) -> String {
    match v {
        V.Text(s) => s,
        V.Int(_) => show(v),
    }
}
fn first_text(v: own V) -> String {
    if let V.Text(s) = v {
        return s;
    }
    show(v)
}
fn main() {
    println(describe(V.Text("a".to_string())));
    println(describe(V.Int(7)));
    println(first_text(V.Text("b".to_string())));
    println(first_text(V.Int(8)));
}
"#,
    );
    rejected(
        "scrutinee-moved-where-bound",
        r#"enum V { Text(String), Int(i64) }
fn show(v: ref V) -> String {
    match v {
        V.Text(s) => s.clone(),
        V.Int(i) => f"{i}",
    }
}
fn a(v: own V) -> String {
    let r = match v {
        V.Text(s) => s,
        V.Int(_) => "i".to_string(),
    };
    r + show(v)
}
fn b(v: own V) -> String {
    if let V.Text(s) = v {
        println(s);
    }
    show(v)
}
fn c(v: own V) -> String {
    match v {
        V.Text(s) => { let t = s; show(v) + t }
        V.Int(_) => show(v),
    }
}
fn main() {
    println(a(V.Int(1)));
    println(b(V.Int(1)));
    println(c(V.Int(1)));
}
"#,
        "moved",
    );
    // B-2026-10-15-38: a number type's `parse` borrows its text, a field
    // included.
    accepted(
        "numeric-parse-borrows-a-field",
        r#"struct T { text: String, f: f64 }
fn main() {
    let mut out: Vec[T] = Vec.new();
    let mut t = T { text: "1.5".to_string(), f: 0.0 };
    t.f = f64.parse(t.text).unwrap_or(0.0);
    out.push(t);
    let text = "2.5".to_string();
    let g = f64.parse(text).unwrap_or(0.0);
    out.push(T { text: text, f: g });
    println(f"{out[0].f} {out[1].f} {out[1].text}");
}
"#,
    );
    accepted(
        "numeric-parse-borrows-a-field-2",
        r#"struct T { text: String, f: f64 }
fn g(s: Str) -> i64 { s.len() }
fn main() {
    let mut out: Vec[T] = Vec.new();
    let mut t = T { text: "1.5".to_string(), f: 0.0 };
    let n = g(t.text);
    let k = t.text.len();
    out.push(t);
    let mut u = T { text: "1.5".to_string(), f: 0.0 };
    let r = f64.parse(u.text);
    println(f"{n} {k} {out.len()} {u.text}");
}
"#,
    );
    // B-2026-10-15-20, third case: a loop-body binding read in an `if`,
    // then moved into a constructor that is a call argument.
    accepted(
        "loop-body-binding-into-ctor-arg",
        r#"enum E { F(i64, String), G(i64) }
struct A { es: Vec[E] }
impl A {
    fn add(mut ref self, e: own E) -> i64 {
        self.es.push(e);
        self.es.len() - 1
    }
}
struct P { i: i64, bad: String }
impl P {
    fn next_name(mut ref self) -> Result[String, String] {
        self.i += 1;
        Ok(f"n{self.i}")
    }
    fn run(mut ref self, tree: mut ref A) -> Result[i64, String] {
        let mut e: i64 = 0;
        loop {
            if self.i < 2 {
                let name = self.next_name()?;
                if self.bad == "" {
                    self.bad = f"bad {name}";
                }
                e = tree.add(E.F(e, name));
            } else {
                return Ok(e);
            }
        }
    }
}
fn main() {
    let mut p = P { i: 0, bad: String.new() };
    let mut a = A { es: Vec.new() };
    println(f"{p.run(mut a).unwrap()} {p.bad}");
}
"#,
    );
}

/// B-2026-10-15-45: a derive opens a type's method surface only until it
/// has run. A method the derive did not generate is refused, and the
/// derived serialization still checks.
#[test]
fn a_derive_closes_the_method_surface_once_it_has_run() {
    rejected(
        "derive-unknown-method",
        r#"
#[derive(Serialize)]
struct R { code: i64 }
fn main() {
    let r = R { code: 3 };
    println(f"{r.code()}");
}
"#,
        "no method 'code' on type 'R'",
    );
    accepted(
        "derive-serialize-json",
        r#"
import std.json.{Json};

#[derive(Serialize)]
struct R { code: i64 }
fn main() {
    let r = R { code: 3 };
    println(Json.serialize(r));
}
"#,
    );
}

/// B-2026-10-15-5: the I/O library's unit results are `Result[(), IoError]`,
/// so a user helper of that type shares an `if` with `fs.write`.
#[test]
fn an_io_unit_result_is_the_unit_type() {
    accepted(
        "io-unit-result",
        r#"
fn append_file(path: Str, text: Str) -> Result[(), IoError] {
    fs.write(path, text)
}
fn main() {
    let r: Result[(), IoError] = fs.write("y.txt", "a");
    let r2 = if r.is_ok() { append_file("y.txt", "b") } else { fs.write("y.txt", "c") };
    println(f"{r.is_ok()} {r2.is_ok()}");
}
"#,
    );
}

/// B-2026-10-15-47: the base of a struct update must have the literal's type.
#[test]
fn a_struct_update_base_of_another_type_is_refused() {
    rejected(
        "spread-base-type",
        r#"
struct D { a: i64, c: bool }
struct E { a: i64, c: bool }

fn other() -> E { E { a: 1, c: true } }

fn main() {
    let d = D { a: 5, ..other() };
    println(f"{d.a}");
}
"#,
        "must be a 'D', found 'E'",
    );
}

/// B-2026-10-14-53: `fs.read_dir` borrows its path, as the other file-system
/// calls do, so the path is still usable in the error arm.
#[test]
fn read_dir_borrows_its_path() {
    accepted(
        "read-dir-borrows",
        r#"
fn main() {
    let full = "data".to_string();
    match fs.read_dir(full) {
        Ok(v) => println(f"{v.len()}"),
        Err(e) => println(f"cannot list {full}: {e}"),
    }
}
"#,
    );
}

/// B-2026-10-15-1: the items of `zip` and `chain` over `iter()` are the
/// references `iter()` gives, so `*x` reads them.
#[test]
fn zip_and_chain_items_are_references() {
    accepted(
        "zip-chain-refs",
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
    let mut c = 0;
    for x in a.iter().chain(b.iter()) { c += *x; }
    println(f"{same(a, b)} {d} {c}");
}
"#,
    );
}

/// B-2026-10-15-10: `std.time` splices its Kāra items beside its globals, and
/// still reports an unknown item and one that is specified but missing.
#[test]
fn std_time_reports_unknown_and_missing_items() {
    rejected(
        "std-time-items",
        "import std.time.{Duration, Bogus};\nfn main() { println(f\"{Duration.ms(2).as_ms()}\"); }\n",
        "`std.time` has no item `Bogus`",
    );
    // `Instant` landed (B-2026-10-15-112), and `MutexGuard`
    // (B-2026-10-19-23); `Extend` keeps the missing case covered.
    accepted(
        "std-time-instant",
        "import std.time.{Instant};\nfn main() { let i = Instant.now(); println(f\"{i.elapsed().as_ms() >= 0}\"); }\n",
    );
    rejected(
        "std-iter-extend",
        "import std.iter.{Extend};\nfn main() { }\n",
        "`std.iter.Extend` is in the library spec (docs/library) but not implemented yet",
    );
}

/// B-2026-10-15-53: `&`, `|` and `^` take two operands of one integer type.
#[test]
fn a_bitwise_operation_on_mixed_integer_types_is_refused() {
    rejected(
        "bitwise-mixed",
        "fn main() {\n    let a: u128 = 12;\n    let b: i64 = 10;\n    let z = a & b;\n    println(f\"{z}\");\n}\n",
        "cannot mix integer types 'u128' and 'i64' in a bitwise operation",
    );
    rejected(
        "bitwise-mixed-width",
        "fn main() {\n    let a: u8 = 12;\n    let b: u32 = 10;\n    println(f\"{a | b}\");\n}\n",
        "cannot mix integer types 'u8' and 'u32' in a bitwise operation",
    );
}

/// design.md § Parameter modes: an annotated bare closure parameter borrows,
/// and a body that gives it away is an error at the closure, with the `own`
/// spelling as its fix: in the result (`|e: String| (id, e)`) and in a slot
/// that wants the value (a struct field).
#[test]
fn a_closure_that_gives_away_a_bare_param_is_refused_at_the_closure() {
    // The closure half of D5 is opt-in (`KARAC_D5_CLOSURES=1`) until the
    // programs that move a bare closure parameter are migrated to `own`.
    let check = |tag: &str, src: &str| {
        let (_, path) = fixture(tag, src);
        let out = karac()
            .env("KARAC_D5_CLOSURES", "1")
            .arg("check")
            .arg(&path)
            .output()
            .unwrap();
        (path, out)
    };
    for (tag, src, expect) in [
        (
            "closure-give-away-tail",
            "fn parse(id: i64, s: own String) -> Result[i64, (i64, String)] {\n    let fail = |e: String| (id, e);\n    if s.len() > 3 { Err(fail(s)) } else { Ok(1) }\n}\nfn main() {\n    match parse(4, \"hello\".to_string()) { Ok(v) => println(f\"{v}\"), Err((i, m)) => println(f\"{i} {m}\") }\n}\n",
            "cannot give away closure parameter `e`",
        ),
        (
            "closure-give-away-field",
            "struct P { id: i64, e: String }\nfn main() {\n    let mk = |e: String| P { id: 1, e: e };\n    println(mk(\"x\".to_string()).e);\n}\n",
            "Take it by value with `|e: own String|`",
        ),
    ] {
        let (path, out) = check(tag, src);
        let text = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success() && text.contains(expect),
            "{tag}: check must reject with {expect:?}: {text}"
        );
        let fixed = karac()
            .env("KARAC_D5_CLOSURES", "1")
            .arg("fix")
            .arg(&path)
            .output()
            .unwrap();
        let now = std::fs::read_to_string(&path).unwrap();
        assert!(
            now.contains("|e: own String|"),
            "{tag}: fix must write `own`: {now}\n{fixed:?}"
        );
        let (_, out) = check(tag, &now);
        assert!(
            out.status.success(),
            "{tag}: check must pass after fix: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
    let (_, out) = check(
        "closure-keeps-a-bare-param",
        "fn main() {\n    let a = |e: String| e.len();\n    let b = |e: String| { let e = e.clone(); (1, e) };\n    let c = |e: own String| (1, e);\n    println(f\"{a(\"x\".to_string())} {b(\"y\".to_string()).1} {c(\"z\".to_string()).1}\");\n}\n",
    );
    assert!(
        out.status.success(),
        "closure-keeps-a-bare-param: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// B-2026-10-15-58: Rust's `f64.from_bits(bits)` names the library's
/// `bits.bits_as_f64()`, and `karac fix` writes it.
#[test]
fn f64_from_bits_names_bits_as_f64() {
    rejected_then_fixed_by(
        "f64-from-bits",
        "fn main() {\n    let bits: u64 = 4591870180066957722;\n    let x = f64.from_bits(bits + 0);\n    println(f\"{x}\");\n}\n",
        "the integer method `bits.bits_as_f64()`",
        "(bits + 0).bits_as_f64()",
    );
}

/// B-2026-10-15-61: a `Slice[T]` is a read-only view, so assigning through
/// one (`s.bytes()`, `v[a..b]`) is refused; a `mut Slice[T]` parameter and a
/// copied `Vec` still write.
#[test]
fn an_assignment_through_a_read_only_slice_is_refused() {
    rejected(
        "slice-bytes-write",
        "fn main() {\n    let b = String.from(\"hello\");\n    let mut t = b.bytes();\n    t[4] = 97;\n    println(b);\n}\n",
        "`t` is a read-only `Slice[u8]`",
    );
    rejected(
        "slice-range-write",
        "fn main() {\n    let v: Vec[i64] = [1, 2, 3];\n    let mut w = v[0..2];\n    w[0] = 9;\n    println(f\"{v}\");\n}\n",
        "`w` is a read-only `Slice[i64]`",
    );
    rejected(
        "slice-param-write",
        "fn set(xs: Slice[i64]) { xs[0] = 1; }\nfn main() {\n    let v: Vec[i64] = [1, 2];\n    set(v[0..2]);\n}\n",
        "`xs` is a read-only `Slice[i64]`",
    );
    accepted(
        "slice-mut-write",
        "fn bump(xs: mut Slice[i64]) { xs[0] = xs[0] + 1; }\nfn main() {\n    let mut v: Vec[i64] = [1, 2, 3];\n    bump(mut v[0..2]);\n    let mut t = String.from(\"hello\").bytes().to_vec();\n    t[4] = 97;\n    println(f\"{v} {String.from_utf8(t).unwrap()}\");\n}\n",
    );
}

/// B-2026-10-15-70: `match (a, b)` builds a tuple by value, so `a` and `b`
/// move into it even when every binding is `ref`; inside a loop that is a
/// move on every iteration.
#[test]
fn a_tuple_scrutinee_moves_its_operands() {
    rejected(
        "tuple-scrutinee-in-a-loop",
        "struct D { n: i64 }\nfn main() {\n    let mut a: Option[D] = Some(D { n: 1 });\n    let b: Option[String] = Some(\"x\".to_string());\n    for _ in 0..3 {\n        match (a, b) {\n            (Some(mut ref d), Some(ref s)) => {\n                d.n += 1;\n                println(f\"{d.n} {s}\");\n            }\n            _ => println(\"none\"),\n        }\n    }\n}\n",
        "value 'a' is moved inside a loop",
    );
    rejected(
        "tuple-scrutinee-then-used",
        "struct D { n: i64 }\nfn main() {\n    let a: Option[D] = Some(D { n: 1 });\n    let b: i64 = 2;\n    match (a, b) {\n        (Some(_), _) => println(\"some\"),\n        _ => println(\"none\"),\n    }\n    match a { Some(d) => println(f\"{d.n}\"), None => {} }\n}\n",
        "value 'a' moved here, used again here",
    );
    accepted(
        "tuple-scrutinee-of-copies",
        "fn main() {\n    let a: i64 = 1;\n    let b: Option[i64] = Some(2);\n    for _ in 0..2 {\n        match (a, b) {\n            (1, Some(ref n)) => println(f\"{n}\"),\n            _ => println(\"other\"),\n        }\n    }\n    println(f\"{a}\");\n}\n",
    );
}

/// B-2026-10-15-71: an `if` or `match` lent to a `ref T` parameter is a
/// temporary, so an arm that is a field of a borrowed struct moves out of
/// the borrow, as the same `if` in a `let` does; `ref two.a` lends it.
#[test]
fn an_if_of_borrowed_fields_lent_to_a_ref_param_is_refused() {
    let src = "struct P {\n    v: Vec[i64],\n}\nstruct Two {\n    a: P,\n    b: P,\n}\nfn total(p: ref P) -> i64 {\n    let mut t: i64 = 0;\n    for x in p.v {\n        t += x;\n    }\n    t\n}\nfn pick_total(two: ref Two, first: bool) -> i64 {\n    total(if first { two.a } else { two.b })\n}\nfn main() {\n    let two = Two { a: P { v: vec![1, 2] }, b: P { v: vec![10, 20] } };\n    println(f\"{pick_total(two, true)} {pick_total(two, false)}\");\n}\n";
    rejected(
        "if-of-borrowed-fields-lent",
        src,
        "cannot move a non-`Copy` value out of a borrowed place",
    );
    accepted(
        "if-of-borrowed-fields-lent-by-ref",
        &src.replace("{ two.a }", "{ ref two.a }")
            .replace("{ two.b }", "{ ref two.b }"),
    );
}

/// B-2026-10-15-101: the base of a struct update must have the literal's
/// struct type, a binding base as much as a call (B-2026-10-15-47).
#[test]
fn a_struct_update_binding_base_of_another_type_is_refused() {
    rejected(
        "spread-binding-of-another-type",
        "struct D { a: i64, c: bool }\nstruct E { a: i64, c: bool }\nfn other() -> E { E { a: 1, c: true } }\nfn main() {\n    let o = other();\n    let d = D { a: 5, ..o };\n    println(f\"{d.a} {d.c}\");\n}\n",
        "the base of a struct update must be a 'D', found 'E'",
    );
    accepted(
        "spread-binding-of-the-same-type",
        "struct D { a: i64, c: bool }\nfn show(base: ref D) -> D { D { a: 7, ..base } }\nfn main() {\n    let o = D { a: 1, c: true };\n    let d = D { a: 5, ..o };\n    let e = show(d);\n    println(f\"{d.a} {d.c} {e.a}\");\n}\n",
    );
}

/// Rows the library-backed check settles: `Map.get` is `Option[ref V]`
/// (B-2026-10-12-22 refused, B-2026-10-13-28 accepted), `collect` reads an
/// assignment's target (B-2026-10-12-33), and a closure passed to `any`
/// releases its captures when the call returns (B-2026-10-15-12).
#[test]
fn rows_the_library_check_settles() {
    rejected(
        "map-get-against-an-owned-option",
        "fn main() {\n    let mut m: Map[u8, i64] = Map.new();\n    m.insert(1, 5);\n    let n: i64 = 5;\n    println(f\"{m.get(1) == Some(n)}\");\n}\n",
        "cannot compare 'Option[ref i64]' and 'Option[i64]'",
    );
    accepted("collect-into-an-assigned-string", "struct W { name: String }\nfn main() {\n    let cs: Vec[char] = \"abc\".chars().collect();\n    let mut s = String.new();\n    s = cs.iter().collect();\n    let mut w = W { name: String.new() };\n    w.name = cs.iter().collect();\n    println(f\"{s} {w.name}\");\n}\n");
    accepted("map-get-kept-in-a-tuple-local", "fn main() {\n    let mut m: Map[String, i64] = Map.new();\n    m.insert(String.from(\"a\"), 1);\n    let t = (m.get(\"a\"), 5);\n    match t {\n        (Some(i), k) => println(f\"{*i + k}\"),\n        _ => println(\"none\"),\n    }\n}\n");
    accepted("any-closure-then-move-the-element", "struct D { name: String, n: i64 }\nfn main() {\n    let skip: Vec[String] = vec![\"b\".to_string()];\n    let all = vec![D { name: \"a\".to_string(), n: 1 }, D { name: \"b\".to_string(), n: 2 }];\n    let mut kept: Vec[D] = Vec.new();\n    for d in all.into_iter() {\n        let redo = skip.iter().any(|s| s == d.name);\n        if redo {\n            continue;\n        }\n        kept.push(d);\n    }\n    println(f\"{kept.len()} {kept[0].name}\");\n}\n");
}

/// The library surface the spec gives: `values()` is an iterator of
/// `ref V` (B-2026-10-13-1), `split` yields `Str` views (B-2026-10-12-7),
/// `Map.get_mut` exists (B-2026-10-12-3), and `PriorityQueue.peek` lends
/// (B-2026-10-13-11).
#[test]
fn the_spec_library_surface_checks() {
    accepted("values-max-split-views-get-mut", "fn main() {\n    let mut counts: Map[i64, i64] = Map.new();\n    counts.insert(1, 3);\n    counts.insert(2, 7);\n    let best = *counts.values().max().unwrap();\n    let line = \"a,b,c\";\n    let parts: Vec[Str] = line.split(\",\").collect();\n    let mut m: Map[i64, Vec[i64]] = Map.new(); m.insert(3, vec![1]);\n    match m.get_mut(3) { Some(list) => list.push(2), None => {} }\n    println(f\"{best} {parts.len()} {m[3].len()}\");\n}\n");
    // B-2026-10-13-11: `peek` lends the smallest element, `Option[ref T]`.
    accepted("priority-queue-peek-lends", "fn main() {\n    let mut pq: PriorityQueue[i64] = PriorityQueue.new();\n    pq.push(3);\n    pq.push(7);\n    let top = *pq.peek().unwrap();\n    match pq.peek() { Some(x) => println(f\"{*x} {top}\"), None => {} }\n}\n");
}

/// `karac check` accepts `src`, and `__mir-run` prints exactly `want`.
fn accepted_and_runs(tag: &str, src: &str, want: &str) {
    accepted(tag, src);
    let (dir, path) = fixture(tag, src);
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        want,
        "{tag}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The apps thread's rows of 10-18 whose programs `main` now checks and runs
/// (B-2026-10-18-3, -6, -20, -21, -22, -27): each program, its row's answer.
#[test]
fn apps_rows_of_10_18_check_and_run() {
    accepted_and_runs(
        "loop-branch-method-move",
        r#"struct It {
    n: i64,
}

struct Store {
    v: Vec[It],
}

impl Store {
    fn link(mut ref self, it: own It) {
        self.v.push(it);
    }
}

fn run(s: mut ref Store, cmds: ref Vec[i64]) {
    for k in cmds {
        let it = It { n: *k };
        if *k == 0 {
            s.link(it);
        } else {
            s.link(it);
        }
    }
}

fn main() {
    let mut s = Store { v: Vec.new() };
    run(mut s, vec![0, 1, 2]);
    println(f"{s.v.len()}");
}
"#,
        "3\n",
    );
    accepted_and_runs(
        "if-literal-or-view",
        r#"fn show(s: Str) { println(s); }
fn main() {
    let name = "ACCEPT".to_string();
    let a = if name.is_empty() { "-" } else { name.as_str() };
    show(a);
}
"#,
        "ACCEPT\n",
    );
    accepted_and_runs(
        "infer-t-from-item-binding",
        r#"struct VecIter[T] {
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

struct Skip1[I, T] {
    it: I,
    started: bool,
}

impl[I: Iterator[Item = T], T] Iterator for Skip1[I, T] {
    type Item = T;
    fn next(mut ref self) -> Option[T] {
        if not self.started {
            self.started = true;
            self.it.next()?;
        }
        self.it.next()
    }
}

fn skip1[I: Iterator[Item = T], T](it: own I) -> Skip1[I, T] {
    Skip1 { it: it, started: false }
}

fn main() {
    let v: Vec[i64] = vec![1, 2, 3];
    for x in skip1(VecIter { items: v, pos: 0 }) {
        println(f"{x + 1}");
    }
}
"#,
        "3\n4\n",
    );
    accepted_and_runs(
        "assign-matched-place-from-ref",
        r#"fn main() {
    let mut title: Option[String] = Some("a".to_string());
    let s = "b".to_string();
    match title {
        Some(ref t) => title = Some(f"{t}{s}"),
        None => {}
    }
    match title {
        Some(ref t) => println(t),
        None => {}
    }
}
"#,
        "ab\n",
    );
    accepted_and_runs(
        "projection-local-in-generic-fn",
        r#"struct VecIter[T] {
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

fn to_vec[I: Iterator](it: own I) -> Vec[I.Item] {
    let mut it = it;
    let mut out: Vec[I.Item] = Vec.new();
    while let Some(x) = it.next() {
        out.push(x);
    }
    out
}

fn main() {
    let v: Vec[i64] = vec![1, 2, 3];
    let w = to_vec(VecIter { items: v, pos: 0 });
    println(f"{w.len()}");
}
"#,
        "3\n",
    );
    // B-2026-10-18-21: impl parameters bind by name, not position. Check only:
    // the MIR instance still binds them by position (B-2026-10-18-48).
    accepted(
        "impl-params-by-name",
        r#"struct VecIter[T] {
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

struct Tagged[I, T] {
    it: I,
    tag: T,
}

impl[T: Clone, I: Iterator] Iterator for Tagged[I, T] {
    type Item = (T, I.Item);
    fn next(mut ref self) -> Option[(T, I.Item)] {
        let x = self.it.next()?;
        Some((self.tag.clone(), x))
    }
}

fn main() {
    let v: Vec[i64] = vec![1, 2];
    let mut t: Tagged[VecIter[i64], String] = Tagged { it: VecIter { items: v, pos: 0 }, tag: "a".to_string() };
    while let Some((s, x)) = t.next() {
        println(f"{s} {x}");
    }
}
"#,
    );
}

/// B-2026-10-18-22: a struct literal field typed by a projection of another
/// field's parameter takes the solved type: `f: Fn(I.Item) -> I.Item` beside
/// `it: VecIter[i64]`. Check only: the MIR builder does not lower the
/// projection yet (B-2026-10-18-49).
#[test]
fn struct_literal_field_projection_is_normalized() {
    accepted(
        "projection-field",
        r#"struct VecIter[T] {
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
"#,
    );
}

/// B-2026-10-18-17: `*self` reads through a `ref self` receiver.
#[test]
fn deref_of_a_ref_self_receiver_reads_it() {
    accepted_and_runs(
        "deref-ref-self",
        r#"trait Twice { fn twice(ref self) -> i64; }
impl Twice for i64 {
    fn twice(ref self) -> i64 { *self * 2 }
}
#[derive(Copy, Clone)]
struct W { n: i64 }
impl Twice for W {
    fn twice(ref self) -> i64 { let w = *self; w.n * 2 }
}
fn main() {
    let x: i64 = 21;
    println(f"{x.twice()} {W { n: 4 }.twice()}");
}
"#,
        "42 8\n",
    );
}

/// B-2026-10-18-5 / -14: a range view copied out of by a method, or iterated
/// by a `for` header, borrows its source only until the copy or the loop ends.
#[test]
fn range_view_borrows_end_with_the_copy_or_the_loop() {
    accepted_and_runs(
        "match-arm-slice-copy",
        r#"fn last_slash(s: Str) -> Option[i64] { let b = s.bytes(); let mut i = b.len(); while i > 0 { i -= 1; if b[i] == b'/' { return Some(i); } } None }
fn take(s: own String) -> i64 { s.len() }
fn main() {
    let path = "data/a/b.txt".to_string();
    let dir = match last_slash(path.as_str()) {
        Some(i) => path[..i].to_string(),
        None => ".".to_string(),
    };
    println(dir);
    println(f"{take(path)}");
}
"#,
        "data/a\n12\n",
    );
    accepted_and_runs(
        "for-header-range-view",
        r#"fn main() {
    let mut v: Vec[i64] = vec![1, 2, 3];
    let mut n = 0;
    for x in v[1..].iter() {
        n += *x;
    }
    v.push(4);
    println(f"{v.len()} {n}");
}
"#,
        "4 5\n",
    );
    rejected(
        "for-header-range-view-written-in-body",
        "fn main() {\n    let mut v: Vec[i64] = vec![1, 2, 3];\n    for x in v[1..].iter() {\n        v.push(*x);\n    }\n}\n",
        "because it is also borrowed as `Slice[T]`",
    );
}

/// B-2026-10-18-28: the arm writes through the binding, so the move's fix
/// binds it `mut ref`, not `ref`.
#[test]
fn moving_binding_written_in_its_arm_is_fixed_to_mut_ref() {
    rejected_then_fixed_by(
        "move-fix-mut-ref",
        r#"struct H {
    text: String,
}

fn main() {
    let mut cur: Option[H] = Some(H { text: String.new() });
    let words: Vec[String] = vec!["a".to_string(), "b".to_string()];
    for w in words.iter() {
        match cur {
            Some(h) => h.text.push_str(w),
            None => {}
        }
    }
    match cur {
        Some(mut ref h) => println(h.text),
        None => {}
    }
}
"#,
        "bind it as `ref name`",
        "Some(mut ref h) => h.text.push_str(w)",
    );
}

/// B-2026-10-18-32: `clone` on a type that does not derive `Clone` says so,
/// and `karac fix` adds the derive.
#[test]
fn clone_without_the_derive_is_fixed_by_deriving_it() {
    rejected_then_fixed_by(
        "clone-needs-derive",
        r#"enum Value {
    Int(i64),
    Str(String),
}

fn main() {
    let v = Value.Str("a".to_string());
    let w = v.clone();
    match w {
        Value.Int(n) => println(f"{n}"),
        Value.Str(s) => println(s),
    }
}
"#,
        "`Value` does not derive `Clone`",
        "#[derive(Clone)]\nenum Value",
    );
}

/// B-2026-10-18-15: a generic struct's derived ordering holds at an instance
/// whose arguments are ordered (design.md § Derive, "Bounds propagate").
#[test]
fn derived_ord_on_a_generic_struct_orders_its_instances() {
    accepted_and_runs(
        "generic-derive-ord",
        r#"#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Dec[K] { k: K, order: i64 }
fn least[T: Ord + Clone](v: ref Vec[T]) -> T {
    let mut best = v[0].clone();
    for x in v.iter() { if *x < best { best = x.clone(); } }
    best
}
fn main() {
    let v: Vec[Dec[String]] = vec![Dec { k: "b".to_string(), order: 0 }, Dec { k: "a".to_string(), order: 1 }];
    let m = least(v);
    println(f"{m.k} {m.order}");
    println(f"{v[0] < v[1]}");
}
"#,
        "a 1\nfalse\n",
    );
}

/// B-2026-10-18-36: `for g in given` consumes an iterator held in a local, so
/// walking it again on a later pass of an outer loop is a move in a loop.
#[test]
fn for_over_an_iterator_local_consumes_it() {
    let prog = |outer: bool| {
        let walk =
            "for g in given {\n        if g == \"x\" {\n            println(g);\n        }\n    }";
        let body = if outer {
            format!("for s in wanted.iter() {{\n        println(*s);\n        {walk}\n    }}")
        } else {
            walk.to_string()
        };
        format!(
            "fn main() {{\n    let a = \"x,y\".to_string();\n    let given = a.split(\",\");\n    \
             let wanted = vec![\"x\", \"y\"];\n    println(wanted.len());\n    {body}\n}}\n"
        )
    };
    rejected(
        "iterator-local-walked-in-a-loop",
        &prog(true),
        "moved inside a loop",
    );
    accepted_and_runs("iterator-local-walked-once", &prog(false), "2\nx\n");
}

/// B-2026-10-18-35: `karac fix` migrates the shapes the parse→Result and lazy
/// `split`/`lines` change broke: `.ok_or` on a `Result`, indexing a `split` or
/// `lines`, and collecting their pieces into a `Vec[String]`.
#[test]
fn fix_migrates_parse_result_and_lazy_split_shapes() {
    let prog = |body: &str| {
        format!(
            "fn num(s: ref String) -> Result[i64, String] {{\n    i64.parse(s).map_err(|_| \"bad\")\n}}\n\
             fn main() {{\n    {body}\n    match num(\"7\".to_string()) {{\n        Ok(n) => println(f\"{{n}}\"),\n        Err(e) => println(e),\n    }}\n}}\n"
        )
    };
    rejected_then_fixed_by(
        "ok-or-on-result",
        "fn num(s: ref String) -> Result[i64, String] {\n    i64.parse(s).ok_or(\"bad\")\n}\n\
         fn main() {\n    match num(\"7\".to_string()) {\n        Ok(n) => println(f\"{n}\"),\n        Err(e) => println(e),\n    }\n}\n",
        "no method 'ok_or' on type 'Result'",
        ".map_err(|_| \"bad\")",
    );
    rejected_then_fixed_by(
        "split-index",
        &prog("let line = \"GET /a HTTP/1.1\".to_string();\n    let path = line.split(\" \")[1];\n    println(path);"),
        "does not support indexing with []",
        "line.split(\" \").nth(1).unwrap();",
    );
    rejected_then_fixed_by(
        "lines-index",
        &prog("let text = \"a\\nb\".to_string();\n    let first = text.lines()[0];\n    println(first);"),
        "does not support indexing with []",
        "text.lines().nth(0).unwrap();",
    );
    rejected_then_fixed_by(
        "split-collect-strings",
        &prog("let csv = \"1,,2\".to_string();\n    let parts: Vec[String] = csv.split(\",\").filter(|p| p.len() > 0).collect();\n    println(f\"{parts.len()}\");"),
        "FromIterator[Str]",
        ".filter(|p| p.len() > 0).map(|p| p.to_string()).collect()",
    );
}

/// B-2026-10-18-42: an `if` or `match` whose every value is an unsuffixed
/// number takes `Vec.push`'s element type, as a bare literal does.
#[test]
fn literal_branches_pushed_take_the_element_type() {
    accepted_and_runs(
        "push-literal-branches",
        r#"fn main() {
    let c = false;
    let k = 2;
    let mut v: Vec[u8] = Vec.new();
    v.push(if c { 9 } else { 8 });
    v.push(match k {
        1 => 4,
        _ => 5,
    });
    v.push(if c { 9 } else if k == 2 { 7 } else { 8 });
    println(f"{v[0]} {v[1]} {v[2]}");
}
"#,
        "8 5 7\n",
    );
    rejected(
        "push-literal-branch-out-of-range",
        r#"fn main() {
    let c = true;
    let mut v: Vec[u8] = Vec.new();
    v.push(if c { 300 } else { 8 });
    println(f"{v.len()}");
}
"#,
        "integer literal 300 out of range for 'u8'",
    );
}

/// B-2026-10-18-7: a pattern binding (`for`, `match` arm, `if let`,
/// `while let`, closure parameter) reads the type the checker gave it, not
/// the type of an earlier local of the same name, live or out of scope.
#[test]
fn pattern_bindings_do_not_take_an_earlier_locals_type() {
    accepted_and_runs(
        "for-after-dead-vec-local",
        r#"fn main() {
    for r in 0..2 {
        let p: Vec[i64] = vec![r];
        println(f"{p.len()}");
    }
    for p in 0..4 {
        let a = p + 1;
        let b = p + 2;
        println(f"{a} {b}");
    }
}
"#,
        "1\n1\n1 2\n2 3\n3 4\n4 5\n",
    );
    accepted_and_runs(
        "patterns-after-dead-vec-local",
        r#"fn main() {
    {
        let p: Vec[i64] = vec![1];
        println(f"{p.len()}");
    }
    let o: Option[i64] = Some(3);
    match o {
        Some(p) => {
            let a = p + 1;
            let b = p + 2;
            println(f"{a} {b}");
        }
        None => {}
    }
    if let Some(p) = o {
        let a = p + 1;
        let b = p + 2;
        println(f"{a} {b}");
    }
    let mut it = vec![5, 6].into_iter();
    while let Some(p) = it.next() {
        let a = p + 1;
        let b = p + 2;
        println(f"{a} {b}");
    }
    let f = |p: i64| {
        let a = p + 1;
        let b = p + 2;
        a + b
    };
    println(f"{f(1)}");
}
"#,
        "1\n4 5\n4 5\n6 7\n7 8\n5\n",
    );
    accepted_and_runs(
        "patterns-shadowing-live-vec-local",
        r#"fn eat(v: own Vec[i64]) -> i64 {
    v.len()
}

fn main() {
    let p: Vec[i64] = vec![1, 2];
    let o: Option[i64] = Some(3);
    match o {
        Some(p) => {
            let a = p + 1;
            let b = p + 2;
            println(f"{a} {b}");
        }
        None => {}
    }
    for p in 0..2 {
        let a = p + 1;
        println(f"{a} {p}");
    }
    println(f"{eat(p)}");
}
"#,
        "4 5\n1 0\n2 1\n2\n",
    );
    rejected(
        "shadowed-vec-local-still-moves",
        r#"fn eat(v: own Vec[i64]) -> i64 {
    v.len()
}

fn main() {
    let p: Vec[i64] = vec![1, 2];
    let o: Option[i64] = Some(3);
    match o {
        Some(p) => {
            let a = p + 1;
            let b = p + 2;
            println(f"{a} {b}");
        }
        None => {}
    }
    for p in 0..2 {
        let a = p + 1;
        println(f"{a} {p}");
    }
    println(f"{eat(p)}");
    println(f"{eat(p)}");
}
"#,
        "value 'p' moved here, used again here",
    );
}

/// B-2026-10-15-111: an index read on a `String`-keyed map takes a `Str`
/// key, as `get` and `contains_key` do.
#[test]
fn map_index_read_takes_a_str_key() {
    accepted_and_runs(
        "map-index-str-key",
        r#"fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("ab".to_string(), 3);
    for name in "x/ab".split("/") {
        if m.contains_key(name) {
            println(m[name]);
        }
    }
}
"#,
        "3\n",
    );
}

/// B-2026-10-14-53: a `ref String` parameter of an ambient builtin
/// (`fs.write`'s path and contents, `env.var`, `fs.read_dir`) borrows its
/// argument. Check only: the programs touch the file system.
#[test]
fn ambient_builtin_ref_string_params_borrow() {
    accepted(
        "fs-write-env-var-borrow",
        r#"fn main() {
    let out = "hello".to_string();
    match fs.write("o.txt".to_string(), out) {
        Ok(_) => println(f"{out.len()}"),
        Err(_) => println("err"),
    }
    let o2 = "x".to_string();
    let r = fs.write("p.txt".to_string(), o2);
    println(f"{o2.len()} {r.is_ok()}");
    let name = "HOME".to_string();
    match env.var(name) {
        Ok(_) => println("set"),
        Err(_) => println("unset"),
    }
    println(f"asked for {name}");
}
"#,
    );
    accepted(
        "fs-write-path-read-dir-borrow",
        r#"fn main() {
    let path = "x.txt".to_string();
    match fs.write(path, "abc".to_string()) {
        Ok(_) => println("ok"),
        Err(_) => println("err"),
    }
    println(f"{path}");
    let dir = ".".to_string();
    let names = match fs.read_dir(dir) {
        Ok(ns) => ns.len(),
        Err(e) => {
            println(f"cannot list {dir}: {e}");
            0
        }
    };
    println(f"{names > 0} {dir}");
}
"#,
    );
}

/// B-2026-10-15-32: a pattern that binds nothing leaves its owned scrutinee
/// whole on that path (core-semantics.md §4.6).
#[test]
fn a_path_that_binds_nothing_keeps_the_scrutinee() {
    accepted_and_runs(
        "bind-nothing-keeps-scrutinee",
        r#"enum V { Text(String), Int(i64) }
fn show(v: ref V) -> String {
    match v {
        V.Text(s) => s.clone(),
        V.Int(i) => f"{i}",
    }
}
fn describe(v: own V) -> String {
    match v {
        V.Text(s) => s,
        V.Int(_) => show(v),
    }
}
fn first_text(v: own V) -> String {
    if let V.Text(s) = v {
        return s;
    }
    show(v)
}
fn main() {
    println(describe(V.Text("a".to_string())));
    println(describe(V.Int(7)));
    println(first_text(V.Text("b".to_string())));
    println(first_text(V.Int(8)));
}
"#,
        "a\n7\nb\n8\n",
    );
}

/// B-2026-10-15-102: the string literal after `??` on an `Option[Str]` is a
/// `Str`.
#[test]
fn nil_coalesce_literal_fallback_of_a_str_option() {
    accepted_and_runs(
        "str-option-coalesce-literal",
        r#"fn main() {
    let opt: Option[Str] = None;
    let s = opt ?? "";
    println(f"[{s}]");
}
"#,
        "[]\n",
    );
}

/// B-2026-10-14-55: an unknown method on an ambient resource is an error,
/// whether the resource is named by its module (`stdout`) or its type
/// (`Clock`); a known one checks.
#[test]
fn an_unknown_method_on_a_resource_is_an_error() {
    rejected(
        "resource-type-unknown-method",
        r#"fn main() {
    let r = Clock.bogus_op();
    stdout.bogus_op();
    println("ran");
}
"#,
        // Through its bound since B-2026-10-20-8 (core-semantics.md §14.2).
        "no bound of `Clock` declares `bogus_op`",
    );
    rejected(
        "resource-module-unknown-method",
        r#"fn main() {
    let r = Clock.bogus_op();
    stdout.bogus_op();
    println("ran");
}
"#,
        "no method 'bogus_op' on 'Stdout' (the `stdout` resource)",
    );
    accepted_and_runs(
        "resource-type-known-methods",
        r#"fn main() {
    let t = Clock.now();
    let w = Clock.wall_ns();
    println("ran");
}
"#,
        "ran\n",
    );
}

/// B-2026-10-14-62: a stored `Fn` is move-only, so it is not copied out of a
/// borrowed element; `(r.h)(..)` calls it in place.
#[test]
fn a_stored_fn_is_not_copied_out_of_a_borrow() {
    rejected(
        "stored-fn-out-of-borrow",
        r#"fn dbl(x: i64) -> i64 { x * 2 }
fn inc(x: i64) -> i64 { x + 1 }
struct R { h: Fn(i64) -> i64 }
fn main() {
    let rs = vec![R { h: dbl }, R { h: inc }];
    let mut t = 0;
    for r in rs.iter() {
        let h = r.h;
        t += h(1000);
    }
    println(f"{t}");
}
"#,
        "cannot move a non-`Copy` value out of a borrowed place",
    );
    accepted_and_runs(
        "stored-fn-called-in-place",
        r#"fn dbl(x: i64) -> i64 { x * 2 }
fn inc(x: i64) -> i64 { x + 1 }
struct R { h: Fn(i64) -> i64 }
fn main() {
    let rs = vec![R { h: dbl }, R { h: inc }];
    let mut t = 0;
    for r in rs.iter() {
        t += (r.h)(1000);
    }
    println(f"{t}");
}
"#,
        "3001\n",
    );
}

/// B-2026-10-15-38: `f64.parse(t.text)` borrows the field, as it does a
/// local `String`.
#[test]
fn parse_of_a_field_borrows_it() {
    accepted_and_runs(
        "parse-field-borrows",
        r#"struct T { text: String, f: f64 }
fn main() {
    let mut out: Vec[T] = Vec.new();
    let mut t = T { text: "1.5".to_string(), f: 0.0 };
    t.f = f64.parse(t.text).unwrap_or(0.0);
    out.push(t);
    let text = "2.5".to_string();
    let g = f64.parse(text).unwrap_or(0.0);
    out.push(T { text: text, f: g });
    println(f"{out[0].f} {out[1].f} {out[1].text}");
}
"#,
        "1.5 2.5 2.5\n",
    );
}

/// B-2026-10-18-16: `PriorityQueue.pop();` discards the removed element,
/// like `Vec.pop();`, so the implicit `Option` must_use does not fire.
#[test]
fn priority_queue_pop_discard_is_not_must_use() {
    let (dir, path) = fixture(
        "pq-pop-discard",
        r#"fn main() {
    let mut q: PriorityQueue[i64] = PriorityQueue.new();
    q.push(3);
    q.push(1);
    q.pop();
    let mut v: Vec[i64] = vec![1, 2];
    v.pop();
    println(f"{q.len()} {v.len()}");
}
"#,
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "check must pass: {err}");
    assert!(
        !err.contains("must_use"),
        "no must_use warning expected: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-15-105: `import std.path;` binds the module as `path` in a single
/// file (design.md § Imports), and a parameter or `let` named `path` shadows it
/// only where it is in scope.
#[test]
fn a_gated_module_binds_as_a_module_in_a_single_file() {
    accepted_and_runs(
        "std-path-module-binding",
        r#"import std.path;

fn show(path: Str) {
    println(path.len());
}

fn main() {
    let p = "/a/b/c.txt";
    println(path.parent(p) ?? "none");
    println(path.file_name(p) ?? "none");
    show(p);
    let path = "x";
    println(path.len());
}
"#,
        "/a/b\nc.txt\n10\n1\n",
    );
    // A member the program also declares takes an alias, not its bare name.
    accepted_and_runs(
        "std-path-module-binding-contested",
        r#"import std.path;

fn parent(n: i64) -> i64 {
    n - 1
}

fn main() {
    println(path.parent("/a/b") ?? "none");
    println(parent(3));
}
"#,
        "/a\n2\n",
    );
    // A type reached through the binding, and its associated functions.
    accepted_and_runs(
        "std-time-module-binding",
        r#"import std.time;

fn main() {
    let d = time.Duration.ms(1500);
    println(d.as_ms());
    time.sleep(time.Duration.ms(1));
    println("ok");
}
"#,
        "1500\nok\n",
    );
}

/// B-2026-10-19-1: an unsolved `Vec.new()` handed to a `Slice[T]` parameter is
/// pinned by it, as a `Vec[T]` parameter pins it.
#[test]
fn a_slice_slot_pins_an_unsolved_vec() {
    accepted_and_runs(
        "slice-slot-pins-vec",
        r#"fn take(b: ref Slice[u8]) -> i64 {
    b.len()
}

fn main() {
    println(f"{take(Vec.new())}");
    let v: Vec[u8] = Vec.new();
    println(f"{take(v)}");
}
"#,
        "0\n0\n",
    );
}

/// B-2026-10-19-3: an unknown method on a number or a `String` names the
/// method it most likely meant, including Kāra's name for a Rust spelling.
#[test]
fn an_unknown_number_or_string_method_names_the_nearest() {
    for (tag, call, want) in [
        (
            "powf",
            "x.powf(3.0)",
            "no method 'powf' on type 'f64', did you mean 'pow'?",
        ),
        ("sqr", "x.sqr()", "did you mean 'sqrt'?"),
        ("count-one", "n.count_one()", "did you mean 'count_ones'?"),
        (
            "trimm",
            "s.trimm()",
            "no method 'trimm' on type 'String', did you mean 'trim'?",
        ),
        ("length", "s.length()", "did you mean 'len'?"),
        ("to-upper", "s.to_upper()", "did you mean 'to_uppercase'?"),
    ] {
        rejected(
            &format!("method-typo-{tag}"),
            &format!(
                "fn main() {{\n    let x: f64 = 2.0;\n    let n: i64 = 5;\n    \
                 let s = \"ab\".to_string();\n    println(f\"{{{call}}}\");\n}}\n"
            ),
            want,
        );
    }
}

/// B-2026-10-18-40: a local bound to a borrowed parameter is a `ref`
/// (core semantics §5.1), so an owned temporary stored into it would drop at
/// the end of the statement. Check accepted `out = " " + out` and the MIR
/// builder then stored a `String` into a `ref String` local. Rebinding it to
/// another borrow, or to a binding over a borrowed scrutinee, stays accepted.
#[test]
fn an_owned_temporary_cannot_be_stored_into_a_local_bound_to_a_borrow() {
    rejected(
        "owned-into-ref-local",
        r#"fn pad_left(s: String, width: i64) -> String {
    let mut out = s;
    while out.len() < width {
        out = " " + out;
    }
    out.clone()
}

fn main() {
    println(f"[{pad_left("ab".to_string(), 5)}]");
}
"#,
        "`out` is a `ref String` (it was bound to a borrow), so it cannot hold this owned `String`",
    );
    rejected(
        "literal-into-ref-local",
        "fn f(s: String) -> i64 {\n    let mut out = s;\n    out = \"lit\";\n    out.len()\n}\n\nfn main() {\n    println(f(\"a\".to_string()));\n}\n",
        "so it cannot hold this owned `String`",
    );
    accepted_and_runs(
        "owned-copy-of-a-borrow",
        r#"fn pad_left(s: String, width: i64) -> String {
    let mut out = s.clone();
    while out.len() < width {
        out = " " + out;
    }
    out
}

struct Node { val: i64, next: Option[Node] }

fn last(head: Node) -> i64 {
    let mut node = ref head;
    while let Some(n) = node.next {
        node = n;
    }
    node.val
}

fn longer(a: String, b: String) -> i64 {
    let mut out = a;
    if b.len() > out.len() {
        out = b;
    }
    out.len()
}

fn main() {
    println(f"[{pad_left("ab".to_string(), 5)}]");
    let l = Node { val: 1, next: Some(Node { val: 2, next: None }) };
    println(f"{last(l)} {longer("a".to_string(), "bcd".to_string())}");
}
"#,
        "[   ab]\n2 3\n",
    );
}

/// B-2026-10-19-2: `pop` and `truncate` write the `String`, so a `let`
/// binding cannot call them.
#[test]
fn string_pop_and_truncate_need_a_mut_binding() {
    for call in ["let _ = s.pop();", "s.truncate(1);"] {
        rejected(
            "string-pop-truncate-immutable",
            &format!("fn main() {{\n    let s = \"ab\".to_string();\n    {call}\n}}\n"),
            "is declared without `mut`",
        );
    }
}

/// B-2026-10-19-12: a misspelled `Vec` method names the nearest one, and a
/// known one called with the wrong arguments says so rather than that it
/// does not exist.
#[test]
fn an_unknown_vec_method_names_the_nearest() {
    for (tag, call, want) in [
        (
            "lenn",
            "v.lenn()",
            "no method 'lenn' on type 'Vec', did you mean 'len'?",
        ),
        ("pushh", "v.pushh(1)", "did you mean 'push'?"),
        ("length", "v.length()", "did you mean 'len'?"),
        ("size", "v.size()", "did you mean 'len'?"),
        ("push-arity", "v.push()", "`push` takes arguments"),
    ] {
        rejected(
            &format!("vec-method-typo-{tag}"),
            &format!(
                "fn main() {{\n    let mut v: Vec[i64] = Vec.new();\n    let _ = {call};\n}}\n"
            ),
            want,
        );
    }
}

/// B-2026-10-19-11: the overflow families check their operands: a shift
/// amount or `pow` exponent is a `u32`, a divisor is `Self`, `neg` takes
/// none, and `saturating_` has no `rem` or shift (and no unsigned `neg`).
#[test]
fn overflow_family_operands_are_checked() {
    for (tag, call, want) in [
        (
            "shl-amount",
            "let n: i64 = 3; let _ = x.wrapping_shl(n);",
            "wrapping_shl expects an amount of type `u32`, got `i64` (cast with `as u32`)",
        ),
        (
            "div-peer",
            "let y: i32 = 2; let _ = x.checked_div(y);",
            "checked_div expects an argument of type `i64`, got `i32`",
        ),
        (
            "neg-arity",
            "let _ = x.checked_neg(1);",
            "checked_neg expects 0 argument(s), got 1",
        ),
        (
            "saturating-rem",
            "let _ = x.saturating_rem(2);",
            "no method 'saturating_rem' on type 'i64'",
        ),
        (
            "unsigned-saturating-neg",
            "let u: u8 = 3; let _ = u.saturating_neg();",
            "no method 'saturating_neg' on type 'u8'",
        ),
        (
            "typo",
            "let _ = x.wraping_neg();",
            "did you mean 'wrapping_neg'?",
        ),
    ] {
        rejected(
            &format!("overflow-family-{tag}"),
            &format!("fn main() {{\n    let x: i64 = 5;\n    {call}\n}}\n"),
            want,
        );
    }
}

/// B-2026-10-19-13: the place half of B-2026-10-18-40. A local that owns a
/// fresh value cannot be stored by value into a local bound to a borrow;
/// `out = ref t` borrows it, and a binding over a borrowed scrutinee is a
/// borrow already.
#[test]
fn an_owned_local_cannot_be_stored_into_a_local_bound_to_a_borrow() {
    rejected_then_fixed_by(
        "owned-local-into-ref-local",
        r#"fn f(s: String) -> i64 {
    let t = "xyz".to_string();
    let mut out = s;
    out = t;
    out.len()
}

fn main() {
    println(f"{f("a".to_string())}");
}
"#,
        "`out` is a `ref String` (it was bound to a borrow), so it cannot take `t` by value; write `out = ref t`",
        "out = ref t;",
    );
    accepted_and_runs(
        "borrowed-owned-local-into-ref-local",
        r#"fn f(s: String) -> i64 {
    let t = "xyz".to_string();
    let mut out = s;
    out = ref t;
    out.len()
}

struct Node { val: i64, next: Option[Node] }

fn last(head: Node) -> i64 {
    let mut node = ref head;
    while let Some(n) = node.next {
        node = n;
    }
    node.val
}

fn main() {
    let l = Node { val: 1, next: Some(Node { val: 2, next: None }) };
    println(f"{f("a".to_string())} {last(l)}");
}
"#,
        "3 2\n",
    );
    // B-2026-10-19-24: an `own` parameter owns its value, and so does a
    // local made by an `if` whose branches each make a fresh value; a
    // `match` whose arms borrow is a borrow and stays legal.
    for (tag, src) in [
        (
            "own-param-into-ref-local",
            r#"fn f(s: String, t: own String) -> i64 {
    let mut out = s;
    out = t;
    out.len()
}
fn main() { println(f"{f("ab".to_string(), "xyz".to_string())}"); }
"#,
        ),
        (
            "if-local-into-ref-local",
            r#"fn f(s: String, c: bool) -> i64 {
    let t = if c { "xyz".to_string() } else { "q".to_string() };
    let mut out = s;
    out = t;
    out.len()
}
fn main() { println(f"{f("ab".to_string(), true)}"); }
"#,
        ),
    ] {
        rejected_then_fixed_by(
            tag,
            src,
            "`out` is a `ref String` (it was bound to a borrow), so it cannot take `t` by value; write `out = ref t`",
            "out = ref t;",
        );
    }
    accepted_and_runs(
        "borrowing-match-into-ref-local",
        r#"fn f(s: String, v: Vec[String], c: bool) -> i64 {
    let mut out = s;
    let u = match c { true => ref v[0], false => ref v[1] };
    out = u;
    out.len()
}
fn g(s: String, t: own String) -> i64 {
    let mut out = s;
    out = ref t;
    out.len()
}
fn main() { println(f"{f("ab".to_string(), vec!["xyz".to_string(), "q".to_string()], true)} {g("ab".to_string(), "xyz".to_string())}"); }
"#,
        "3 3\n",
    );
}

/// `get_mut` takes the key's view, as `get` does: a `Str` on a `String`
/// key and a `Slice` on a `Vec` key are accepted, and a key of another
/// type is still refused (B-2026-10-18-39).
#[test]
fn get_mut_takes_the_keys_view() {
    accepted(
        "get-mut-key-view",
        "fn bump(m: mut ref Map[String, i64], k: Str) {\n    if let Some(v) = m.get_mut(k) {\n        *v = *v + 1;\n    }\n}\nfn main() {\n    let mut m: Map[String, i64] = Map.new();\n    m.insert(\"a\".to_string(), 1);\n    bump(mut m, \"a\");\n    let mut s: SortedMap[Vec[i64], i64] = SortedMap.new();\n    s.insert(vec![1], 2);\n    let w: Vec[i64] = vec![1, 2];\n    if let Some(n) = s.get_mut(w[0..1]) { *n += 1; }\n    println(f\"{m.get(\"a\").unwrap_or(0)}\");\n}\n",
    );
    rejected(
        "get-mut-key-of-another-type",
        "fn main() {\n    let mut m: Map[String, i64] = Map.new();\n    m.insert(\"a\".to_string(), 1);\n    if let Some(v) = m.get_mut(5) { *v += 1; }\n}\n",
        "expected 'ref String', found 'i64'",
    );
}

/// B-2026-10-19-67: an atomic operation takes no ordering. The old trailing
/// `MemoryOrdering` argument is a deprecated form, still accepted, that
/// `karac fix` deletes; `#[allow(deprecated)]` silences it, and a call with
/// the wrong number of arguments is still refused.
#[test]
fn an_atomic_ordering_argument_is_deprecated_and_fixed() {
    accepted(
        "atomic-no-ordering",
        "fn main() {\n    let c = Atomic.new(0);\n    c.fetch_add(5);\n    c.store(7);\n    let old = c.swap(9);\n    let r = c.compare_exchange(9, 11);\n    println(f\"{old} {c.load()} {r.is_ok()}\");\n}\n",
    );
    let (dir, path) = fixture(
        "atomic-ordering-deprecated",
        "fn main() {\n    let c = Atomic.new(0);\n    c.fetch_add(5, MemoryOrdering.Relaxed);\n    let r = c.compare_exchange(5, 6, MemoryOrdering.AcqRel, MemoryOrdering.Acquire);\n    println(f\"{c.load(MemoryOrdering.Acquire)} {r.is_ok()}\");\n}\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    for m in ["fetch_add", "compare_exchange", "load"] {
        assert!(
            err.contains(&format!(
                "use of deprecated form `Atomic.{m}(.., MemoryOrdering)`"
            )),
            "{m}: {err}"
        );
    }
    karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        fixed.contains("c.fetch_add(5);")
            && fixed.contains("c.compare_exchange(5, 6);")
            && fixed.contains("{c.load()}"),
        "{fixed}"
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && !err.contains("deprecated"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
    let (dir, path) = fixture(
        "atomic-ordering-allowed",
        "#[allow(deprecated)]\nfn main() {\n    let c = Atomic.new(0);\n    c.store(1, MemoryOrdering.Release);\n    println(c.load());\n}\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && !err.contains("deprecated"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
    rejected(
        "atomic-too-many-arguments",
        "fn main() {\n    let c = Atomic.new(0);\n    c.store(1, MemoryOrdering.Release, 2);\n}\n",
        "Atomic.store takes (value) — 1 argument",
    );
}

/// B-2026-10-19-39, B-2026-10-19-47: `karac check` refuses the `par`
/// conflicts the MIR effect pass refuses a run for (core semantics §11.2):
/// two printing branches, two sends on one channel, and two iterations of a
/// `par for` pushing to one `Vec` from outside. Branches that only read one
/// value, or that each push to a `Vec` of their own, pass and run.
#[test]
fn check_refuses_the_par_conflicts_the_mir_effect_pass_refuses() {
    rejected(
        "par-two-printing-branches",
        "fn main() {\n    let (a, b) = par { println(\"a\"), println(\"b\") };\n}\n",
        "branches 1 and 2 of the `par` conflict: `writes(Stdout)` and `writes(Stdout)`",
    );
    rejected(
        "par-two-sends",
        "fn send_one(tx: Sender[i64], v: i64) -> i64 {\n    tx.send(v);\n    v\n}\n\n\
         fn main() {\n    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();\n    \
         let tx2 = tx.clone();\n    \
         let (a, b) = par { send_one(tx, 1), send_one(tx2, 2) };\n    \
         println(f\"{a + b}\");\n}\n",
        "`sends(Channel",
    );
    rejected(
        "par-for-iterations-push",
        "fn main() {\n    let xs: Vec[i64] = [1, 2];\n    let mut out: Vec[i64] = Vec.new();\n    \
         let v = par for x in xs {\n        out.push(x);\n        0\n    };\n    \
         println(f\"{out.len()} {v.len()}\");\n}\n",
        "two iterations of the `par for` conflict: `mutable borrow of out`",
    );
    accepted_and_runs(
        "par-reads-and-own-vecs",
        "fn main() {\n    let xs: Vec[i64] = [1, 2, 3];\n    \
         let (a, b) = par {\n        { let mut v: Vec[i64] = Vec.new(); v.push(xs.len()); v.len() },\n        \
         { let mut v: Vec[i64] = Vec.new(); v.push(xs[0]); v.push(xs[1]); v.len() },\n    };\n    \
         let w = par for x in xs { x * 2 };\n    \
         println(f\"{a} {b} {w.len()}\");\n}\n",
        "1 2 3\n",
    );
}

/// B-2026-10-19-40, -41, -50: three rules of the v2 core that check's legacy
/// effect rules contradicted. A function-typed parameter with no effect
/// clause takes the argument's effects (§12), so a printing closure passes
/// to it, and two printing closures in `par` branches conflict (§11.2); a
/// `Drop` body may panic (§7.11); and the removed `with _` is refused.
#[test]
fn check_follows_the_core_effect_defaults() {
    let apply = "fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n";
    accepted_and_runs(
        "fn-param-takes-closure-effects",
        &format!(
            "{apply}fn main() {{\n    let y = apply(|x| {{ println(\"in closure\"); x + 1 }}, 1);\n    \
             println(f\"{{y}}\");\n}}\n"
        ),
        "in closure\n2\n",
    );
    rejected(
        "fn-param-closures-conflict-in-par",
        &format!(
            "{apply}fn main() {{\n    let (a, b) = par {{\n        apply(|x| {{ println(\"a\"); x }}, 1),\n        \
             apply(|x| {{ println(\"b\"); x }}, 2),\n    }};\n    println(f\"{{a + b}}\");\n}}\n"
        ),
        "branches 1 and 2 of the `par` conflict: `writes(Stdout)`",
    );
    rejected(
        "fn-param-effects-charged-to-a-pub-caller",
        &format!(
            "{apply}pub fn quiet() {{\n    let y = apply(|x| {{ println(\"c\"); x }}, 1);\n}}\n\
             fn main() {{ quiet(); }}\n"
        ),
        "public function 'quiet' performs effects [writes(Stdout)]",
    );
    accepted(
        "drop-body-may-panic",
        "struct R {\n    n: i64,\n}\n\nimpl Drop for R {\n    fn drop(mut ref self) {\n        \
         println(\"in drop\");\n        if self.n > 0 {\n            panic(\"drop\");\n        }\n    }\n}\n\n\
         fn main() {\n    let r = R { n: 1 };\n    println(\"end\");\n}\n",
    );
    rejected(
        "with-underscore-removed",
        "fn apply(f: Fn() -> i64 with _) -> i64 { f() }\nfn main() {\n    println(f\"{apply(|| 1)}\");\n}\n",
        "the `with _` annotation is removed",
    );
}

/// B-2026-10-19-37 (core-semantics §5.3, §5.4): a method returning a
/// reference lends the element its result names, and a method call's result
/// borrows its receiver, so `self.items[0]`, `self.items.get(0).unwrap()`
/// and `m.get(k).unwrap_or("")` may be returned. A receiver or default that
/// owns its value is still a temporary that dies at the return.
#[test]
fn a_returned_reference_lends_its_element_and_traces_a_method_call() {
    accepted(
        "ref-return-lends",
        "struct Bag { items: Vec[String], name: String }\n\
         impl Bag {\n\
             fn first(self) -> ref String { ref self.items[0] }\n\
             fn pick(self, i: i64) -> ref String { if i > 0 { ref self.items[0] } else { ref self.name } }\n\
             fn early(self) -> ref String { if self.items.len() == 0 { return ref self.name; } return ref self.items[0]; }\n\
             fn got(self) -> ref String { self.items.get(0).unwrap_or(self.name) }\n\
             fn or(self, d: String) -> ref String { self.items.get(0) ?? d }\n\
         }\n\
         fn name_or_empty(m: Map[String, String], k: String) -> ref String { m.get(k).unwrap_or(\"\") }\n\
         fn main() {\n\
             let b = Bag { items: [\"x\".to_string()], name: \"n\".to_string() };\n\
             println(f\"{b.first()} {b.pick(0)} {b.early()} {b.got()} {b.or(\"d\".to_string())}\");\n\
             let m: Map[String, String] = Map.new();\n\
             println(name_or_empty(m, \"k\".to_string()));\n\
         }\n",
    );
    for (tag, body) in [
        ("temp-receiver", "mk().get(0).unwrap()"),
        ("owned-local-receiver", "let v = mk(); v.get(0).unwrap()"),
        (
            "owned-default",
            "let w = String.new(); self.items.get(0).unwrap_or(w)",
        ),
    ] {
        let src = format!(
            "struct Bag {{ items: Vec[String] }}\n\
             fn mk() -> Vec[String] {{ [\"a\".to_string()] }}\n\
             impl Bag {{ fn f(self) -> ref String {{ {body} }} }}\n\
             fn main() {{ let b = Bag {{ items: [\"x\".to_string()] }}; println(b.f()); }}\n"
        );
        let (dir, path) = fixture(tag, &src);
        let out = karac().arg("check").arg(&path).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{tag}: check must reject: {err}");
        assert!(err.contains("dangling reference"), "{tag}: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// B-2026-10-19-38 (core-semantics §9.3): a returned capturing closure may be
/// called directly as a call result. The deferral the legacy backend needs
/// does not apply to the strict check.
#[test]
fn a_returned_closure_is_callable_as_a_call_result() {
    accepted(
        "returned-closure-called",
        "fn pick(x: i64) -> Fn(i64) -> i64 { |y| y + x }\n\
         fn main() {\n\
             let s = (pick(10))(4);\n\
             println(f\"{s}\");\n\
         }\n",
    );
}

/// B-2026-10-19-45 (core-semantics §5.9): the item of a bare `for` is a
/// `ref` into the collection, so assigning it is refused, also when it is a
/// `Copy` value. A local bound from the item is its own binding and may be
/// rebound.
#[test]
fn a_for_item_is_not_assignable_but_a_copy_of_it_is() {
    let (dir, path) = fixture(
        "for-item-write",
        "fn main() {\n    let v: Vec[i64] = [1, 2];\n    for x in v { x += 1; }\n}\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must reject: {err}");
    assert!(
        err.contains("an assignment through `x` cannot modify it"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    accepted(
        "for-item-copy-rebound",
        "fn main() {\n    let v: Vec[i64] = [3, 1];\n    let mut t = 0;\n    \
         for x in v {\n        let mut largest = x;\n        largest = largest.max(2);\n        \
         t += largest;\n    }\n    println(f\"{t}\");\n}\n",
    );
}

/// B-2026-10-19-49 (core-semantics §9.6): a `MutFn` is called only through
/// a unique access, and a bare parameter is a shared borrow (§4.1). The fix
/// declares it `own`.
#[test]
fn a_borrowed_mut_fn_cannot_be_called() {
    rejected_then_fixed_by(
        "borrowed-mut-fn-call",
        "fn call(f: MutFn()) { f(); }\n\
         fn main() {\n    let mut n = 0;\n    call(|| { n += 1; });\n    println(f\"{n}\");\n}\n",
        "is a borrowed `MutFn`, so it cannot be called",
        "f: own MutFn()",
    );
}

/// B-2026-10-19-35: a program's own type renamed apart for the check
/// (`Field`, `IoError`) is quoted as written in every excerpt and in the
/// JSON record's `expected`.
#[test]
fn check_quotes_a_renamed_user_type_as_written() {
    let (dir, path) = fixture(
        "renamed-type-excerpt",
        r#"struct IoError { code: i64 }

struct Field { n: i64 }

fn a(f: ref Field, k: i64) -> i64 {
    if k == 0 { f.n } else { b(f, k - 1) }
}

fn b(f: ref Field, k: i64) -> i64 {
    a(f, k)
}

fn main() {
    let e: IoError = "x";
    println(f"{a(Field { n: 3 }, 2)} {e.code}");
}
"#,
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must reject: {err}");
    assert!(
        err.contains("expected 'IoError', found 'String'")
            && err.contains("let e: IoError = \"x\";")
            && err.contains("fn a(f: ref Field, k: i64) -> i64 {")
            && !err.contains("Fiel_")
            && !err.contains("IoErro_"),
        "renamed-type-excerpt: {err}"
    );
    let json = karac()
        .arg("check")
        .arg("--output=json")
        .arg(&path)
        .output()
        .unwrap();
    let json =
        String::from_utf8_lossy(&json.stdout).to_string() + &String::from_utf8_lossy(&json.stderr);
    assert!(
        json.contains("\"expected\":\"IoError\"") && !json.contains("IoErro_"),
        "renamed-type-excerpt json: {json}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-20-12: an `if` or `match` local with a branch that leaves
/// (`return`) still owns what its other branches make.
#[test]
fn a_branch_made_local_with_a_leaving_branch_cannot_be_stored_into_a_borrow() {
    rejected_then_fixed_by(
        "match-with-return-into-ref-local",
        r#"fn f(s: String, n: i64) -> i64 {
    let t = match n {
        0 => "zero".to_string(),
        1 => { "one".to_string() }
        _ => return -1,
    };
    let mut out = s;
    out = t;
    out.len()
}

fn main() {
    println(f"{f("ab".to_string(), 0)}");
}
"#,
        "`out` is a `ref String` (it was bound to a borrow), so it cannot take `t` by value; write `out = ref t`",
        "out = ref t;",
    );
    rejected_then_fixed_by(
        "if-with-return-into-ref-local",
        r#"fn f(s: String, c: bool) -> i64 {
    let t = if c { "xyz".to_string() } else { return 0; };
    let mut out = s;
    out = t;
    out.len()
}

fn main() {
    println(f"{f("ab".to_string(), true)} {f("ab".to_string(), false)}");
}
"#,
        "`out` is a `ref String` (it was bound to a borrow), so it cannot take `t` by value; write `out = ref t`",
        "out = ref t;",
    );
}

/// B-2026-10-20-1: a tuple literal's string literals are `Str` under a
/// tuple slot with `Str` in it, also where the slot's check infers the
/// argument first: a method argument, a nested tuple, a user method and a
/// generic call. A `String` value in that position is still refused.
#[test]
fn a_tuple_literal_takes_str_from_its_slot() {
    accepted(
        "tuple-literal-str-slot",
        "fn take[T](x: own T) -> T { x }\n\
         struct Z { names: Vec[(Str, i64)] }\n\
         impl Z { fn add(mut ref self, p: own (Str, i64)) { self.names.push(p); } }\n\
         fn main() {\n\
             let mut v: Vec[(Str, Str)] = Vec.new();\n\
             v.push((\"a\", \"b\"));\n\
             let mut w: Vec[(Str, i64)] = Vec.new();\n\
             w.push((\"e\", 1));\n\
             let mut n: Vec[((Str, i64), Str)] = Vec.new();\n\
             n.push(((\"x\", 2), \"y\"));\n\
             let t: (Str, Str) = take((\"c\", \"d\"));\n\
             let mut z = Z { names: Vec.new() };\n\
             z.add((\"q\", 3));\n\
             println(f\"{v[0].0} {w[0].1} {n[0].1} {t.0} {z.names[0].0}\");\n\
         }\n",
    );
    let (dir, path) = fixture(
        "tuple-literal-string-value",
        "fn main() {\n    let s = \"x\".to_string();\n    \
         let mut v: Vec[(Str, Str)] = Vec.new();\n    v.push((s, \"b\"));\n}\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must reject: {err}");
    assert!(err.contains("expected '(Str, Str)'"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// review/PLAN_AMENDMENT_2026-10-19.md ("TLS and
/// WebSocket"): the servers built on the TLS server and WebSocket
/// (`Server.serve_tls`, `serve_ws`, `serve_ws_tls`) kept their M4a
/// placement, refused by `karac check`, until M4 item 8 put them on v2
/// (runtime/stdlib/source/http_server.kara). They check clean now, with a
/// closure or a function as the handler, as do `TlsListener`, `WebSocket`
/// and a program's own `WebSocket` type.
#[test]
fn the_tls_and_websocket_servers_check_clean() {
    accepted(
        "m4a-serve-tls",
        "fn main() {\n    let _r = Server.serve_tls(\"127.0.0.1:0\".to_string(), \"c\".to_string(), \"k\".to_string(), |req| Response { status: 200, body: req.path() });\n}\n",
    );
    accepted(
        "m4a-serve-ws",
        "fn route(req: Request) -> Response { Response { status: 101, body: req.path() } }\n\
         fn on_ws(ws: own WebSocket) { let mut ws = ws; let _ = ws.send_text(\"hi\"); }\n\
         fn main() {\n    let _a = Server.serve_ws(\"127.0.0.1:0\".to_string(), route, on_ws);\n    \
         let _b = Server.serve_ws_tls(\"127.0.0.1:0\".to_string(), \"c\".to_string(), \"k\".to_string(), route, on_ws);\n}\n",
    );
    accepted(
        "m4-websocket-accept",
        "fn main() {\n    let l = TcpListener.bind(\"127.0.0.1:0\").unwrap();\n    let _w = WebSocket.accept(l);\n}\n",
    );
    accepted(
        "m4-tls-listener",
        "fn main() {\n    let _l = TlsListener.bind_tls(\"127.0.0.1:0\", \"c\", \"k\");\n}\n",
    );
    accepted(
        "m4a-own-websocket-type",
        "struct WebSocket { n: i64 }\n\
         impl WebSocket { fn accept(n: i64) -> WebSocket { WebSocket { n: n } } }\n\
         fn main() { let w = WebSocket.accept(2); println(f\"{w.n}\"); }\n",
    );
}

/// B-2026-10-20-6: a `Vec` literal of string literals takes `Str` from the
/// slot it is handed to, as a tuple literal does (B-2026-10-20-1): `extend`
/// on a `Vec[Str]`, a generic method's `Vec[T]` at `T = Str`, a `Vec[Str]`
/// parameter, and tuples inside the literal. A `String` value is still not a
/// `Str`.
#[test]
fn a_vec_literal_takes_str_from_its_slot() {
    accepted_and_runs(
        "vec-literal-str-slot",
        "struct Bag[T] { items: Vec[T] }\n\
         impl[T] Bag[T] {\n\
             fn add_all(mut ref self, xs: own Vec[T]) { self.items.extend(xs); }\n\
         }\n\
         fn total(v: Vec[Str]) -> i64 { v.len() }\n\
         fn main() {\n\
             let v: Vec[Str] = vec![\"a\", \"b\"];\n\
             let mut w = v;\n\
             w.extend(vec![\"c\", \"d\"]);\n\
             w.extend(Vec[\"e\"]);\n\
             let mut b: Bag[Str] = Bag { items: Vec.new() };\n\
             b.add_all(vec![\"x\", \"y\"]);\n\
             let mut p: Vec[(Str, i64)] = Vec.new();\n\
             p.extend(vec![(\"q\", 1)]);\n\
             p.extend([(\"r\", 2)]);\n\
             println(f\"{w.len()} {w[4]} {b.items[1]} {total(vec![\"f\"])} {p[1].0}\");\n\
         }\n",
        "5 e y 1 r\n",
    );
    let (dir, path) = fixture(
        "vec-literal-string-value",
        "fn main() {\n    let s = \"x\".to_string();\n    \
         let mut v: Vec[Str] = Vec.new();\n    v.extend(vec![s]);\n}\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must reject: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-20-13 (core-semantics.md §5.10, §4.1): a parameter of a `Copy`
/// type reads the same lent or copied, so `Fn(ref i64) -> i64` and
/// `Fn(i64) -> i64` are one function type. A generic `Fn(V)` borrows its
/// bare `V`, and at `V = i64` a `fn neg(x: i64)` serves it. An owned
/// `String` parameter is not a borrowed one, so that pairing is still
/// refused.
#[test]
fn a_copy_parameter_is_the_same_lent_or_copied_in_a_function_type() {
    accepted_and_runs(
        "fn-copy-param",
        "#[derive(Copy, Clone)]\n\
         struct P { x: i64, y: i64 }\n\
         fn neg(x: i64) -> i64 { -x }\n\
         fn px(p: P) -> i64 { p.x * 10 + p.y }\n\
         fn twice(x: ref i64) -> i64 { *x * 2 }\n\
         fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
         fn apply_r(f: Fn(ref i64) -> i64, x: i64) -> i64 { f(x) }\n\
         fn apply_g[V, K](f: Fn(V) -> K, x: V) -> K { f(x) }\n\
         fn main() {\n\
             let g: Fn(ref i64) -> i64 = neg;\n\
             println(f\"{apply_g(neg, 4)} {apply_g(px, P { x: 1, y: 2 })} {apply(twice, 5)} {apply_r(neg, 6)} {g(7)}\");\n\
         }\n",
        "-4 12 10 -6 -7\n",
    );
    let (dir, path) = fixture(
        "fn-owned-string-param",
        "fn take(s: own String) -> i64 { s.len() }\n\
         fn apply_g[V, K](f: Fn(V) -> K, x: V) -> K { f(x) }\n\
         fn main() {\n    println(f\"{apply_g(take, \"ab\".to_string())}\");\n}\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must reject: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-19-58: a generic `shared struct`'s field that is not `mut` is
/// still refused a write through a handle.
#[test]
fn check_a_generic_shared_struct_keeps_its_field_rules() {
    let (dir, path) = fixture(
        "generic-shared-field",
        r#"shared struct Slot[T] { mut items: Vec[T], name: String }
fn main() {
    let s: Slot[i64] = Slot { items: Vec.new(), name: "n".to_string() };
    s.name = "m".to_string();
    println(s.name);
}
"#,
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success()
            && err.contains("shared struct field 'Slot.name' is not declared mut"),
        "generic-shared-field: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-20-8 (core-semantics.md §14.1, §14.2, §14.7): a call on a
/// resource goes through its bounds, which are its interface. A resource with
/// no bound has none to call and cannot be bound; a method no bound declares
/// is an error even when the provider type has it; an `own self` method
/// cannot be called through a resource; a fake for `Env` implements
/// `EnvProvider` and states only the methods it uses; and until M4a only the
/// library declares a resource's default.
#[test]
fn a_resource_is_called_through_its_bounds() {
    accepted(
        "provider-through-bound",
        "trait Log { fn count(self) -> i64; fn bump(mut ref self) -> i64; }\n\
         effect resource AuditLog: Log;\n\
         struct FakeLog { n: i64 }\n\
         impl Log for FakeLog {\n\
             fn count(self) -> i64 { self.n }\n\
             fn bump(mut ref self) -> i64 { self.n += 1; self.n }\n\
         }\n\
         fn tally() -> i64 { AuditLog.count() + AuditLog.bump() }\n\
         fn main() { with_provider[AuditLog](FakeLog { n: 1 }, || { println(tally()); }); }\n",
    );
    accepted(
        "provider-env-fake",
        "struct FakeEnv {}\n\
         impl EnvProvider for FakeEnv {\n\
             fn var(self, name: Str) -> Result[String, VarError] { Ok(\"mocked\") }\n\
         }\n\
         fn main() {\n\
             with_provider[Env](FakeEnv {}, || {\n\
                 match Env.var(\"X\") { Ok(v) => println(v), Err(_) => println(\"unset\") }\n\
             });\n\
         }\n",
    );
    rejected(
        "provider-boundless",
        "effect resource AuditLog;\n\
         struct FakeLog { n: i64 }\n\
         impl FakeLog { fn count(self) -> i64 { self.n } }\n\
         fn tally() -> i64 { AuditLog.count() }\n\
         fn main() { with_provider[AuditLog](FakeLog { n: 1 }, || { println(tally()); }); }\n",
        "`effect resource AuditLog;` declares no provider trait, so it cannot be bound",
    );
    rejected(
        "provider-boundless-call",
        "effect resource AuditLog;\n\
         fn tally() -> i64 { AuditLog.count() }\n\
         fn main() { println(tally()); }\n",
        "so `AuditLog.count` has no method to call",
    );
    rejected(
        "provider-method-not-in-bound",
        "trait Log { fn count(self) -> i64; }\n\
         effect resource AuditLog: Log;\n\
         struct FakeLog { n: i64 }\n\
         impl Log for FakeLog { fn count(self) -> i64 { self.n } }\n\
         impl FakeLog { fn extra(self) -> i64 { 2 } }\n\
         fn tally() -> i64 { AuditLog.extra() }\n\
         fn main() { with_provider[AuditLog](FakeLog { n: 1 }, || { println(tally()); }); }\n",
        "no bound of `AuditLog` declares `extra`",
    );
    rejected(
        "provider-own-self",
        "trait Log { fn take(own self) -> i64; }\n\
         effect resource AuditLog: Log;\n\
         struct FakeLog { n: i64 }\n\
         impl Log for FakeLog { fn take(own self) -> i64 { self.n } }\n\
         fn tally() -> i64 { AuditLog.take() }\n\
         fn main() { with_provider[AuditLog](FakeLog { n: 1 }, || { println(tally()); }); }\n",
        "`Log.take` takes `own self`",
    );
    rejected(
        "provider-env-inherent-fake",
        "struct FakeEnv {}\n\
         impl FakeEnv { fn var(self, name: Str) -> Result[String, VarError] { Ok(\"mocked\") } }\n\
         fn main() {\n\
             with_provider[Env](FakeEnv {}, || {\n\
                 match Env.var(\"X\") { Ok(v) => println(v), Err(_) => println(\"unset\") }\n\
             });\n\
         }\n",
        "does not implement `EnvProvider`",
    );
    rejected(
        "provider-user-default",
        "trait Log { fn count(self) -> i64; }\n\
         struct RealLog {}\n\
         impl Log for RealLog { fn count(self) -> i64 { 0 } }\n\
         effect resource AuditLog: Log = RealLog {};\n\
         fn main() { println(AuditLog.count()); }\n",
        "until the services track (M4a) only the library declares one",
    );
}

/// B-2026-10-20-18: a `Vec.new()` passed as `fold`'s init takes its element
/// type from the closure's annotated accumulator, or from what it returns.
#[test]
fn a_fold_init_takes_its_element_from_the_closure() {
    accepted_and_runs(
        "fold-vec-new-annotated-acc",
        "fn main() {\n\
             let v: Vec[i64] = [1, 2, 3];\n\
             let out = v.iter().fold(Vec.new(), |acc: Vec[i64], x| { let mut acc = acc; acc.push(*x * 2); acc });\n\
             println(f\"{out.len()} {out[2]}\");\n\
             let pairs = v.iter().map(|x| *x).fold(Vec.new(), |acc: Vec[(i64, i64)], x: i64| { let mut acc = acc; acc.push((x, x + 1)); acc });\n\
             println(f\"{pairs.len()} {pairs[0].1}\");\n\
             let typed: Vec[i64] = v.iter().fold(Vec.new(), |acc, x| { let mut acc = acc; acc.push(*x); acc });\n\
             println(f\"{typed.len()}\");\n\
         }\n",
        "3 6\n3 2\n3\n",
    );
    rejected(
        "fold-vec-new-wrong-return",
        "fn main() {\n\
             let v: Vec[i64] = [1, 2, 3];\n\
             let out = v.iter().fold(Vec.new(), |acc: Vec[i64], x| { let s: Vec[String] = Vec.new(); s });\n\
             println(f\"{out.len()}\");\n\
         }\n",
        "Vec[String]",
    );
}

/// B-2026-10-20-15: a local bound to a `for` item is a `ref` (§4.6, §5.9),
/// so it cannot take an owned value, as for a borrowed parameter.
#[test]
fn a_local_bound_to_a_for_item_cannot_hold_an_owned_value() {
    rejected(
        "for-item-copy-owned-assign",
        "fn main() {\n\
             let v: Vec[String] = \"x y\".split(\" \").map(|s| s.to_string()).collect();\n\
             for s in v {\n\
                 let mut t = s;\n\
                 if t == \"y\" { t = \"z\".to_string(); }\n\
                 println(t);\n\
             }\n\
         }\n",
        "`t` is a `ref String` (it was bound to a borrow), so it cannot hold this owned `String`",
    );
    accepted_and_runs(
        "for-item-clone-owned-assign",
        "fn main() {\n\
             let v: Vec[String] = \"x y\".split(\" \").map(|s| s.to_string()).collect();\n\
             for s in v {\n\
                 let mut t = s.clone();\n\
                 if t == \"y\" { t = \"z\".to_string(); }\n\
                 println(t);\n\
             }\n\
         }\n",
        "x\nz\n",
    );
}

/// B-2026-10-20-23: an unknown constant on a primitive is a check error at
/// the use, and floats have `PI`, `TAU` and `E`.
#[test]
fn a_primitive_constant_is_checked_and_floats_have_pi() {
    rejected(
        "primitive-const-unknown",
        "fn main() { let x = f64.FOO; println(f\"{x}\"); }\n",
        "no associated constant `FOO` on `f64`",
    );
    rejected(
        "primitive-const-near-miss",
        "fn main() { let x = u8.MAXX; println(f\"{x}\"); }\n",
        "did you mean `u8.MAX`?",
    );
    accepted_and_runs(
        "float-pi-tau-e",
        "fn main() {\n\
             println(f\"{f64.PI > 3.14} {f64.TAU == 2.0 * f64.PI} {f64.E > 2.71} {f32.PI > 3.0}\");\n\
         }\n",
        "true true true true\n",
    );
}

/// B-2026-10-20-17: a `par` block's branches are comma-separated expressions
/// (core-semantics.md §11.1); the statement form is refused, and the branch
/// form it rewrites to runs.
#[test]
fn a_par_block_takes_branches_not_statements() {
    rejected(
        "par-statement-form",
        "fn work(n: i64) -> String { f\"v{n}\" }\n\
         fn main() {\n\
             par {\n\
                 let a = work(7);\n\
                 let b = work(8);\n\
             }\n\
             println(f\"{a} {b}\");\n\
         }\n",
        "a `par` block's branches are comma-separated expressions, not statements",
    );
    accepted_and_runs(
        "par-branch-form",
        "fn work(n: i64) -> String { f\"v{n}\" }\n\
         fn main() {\n\
             let (a, b) = par {\n\
                 work(7),\n\
                 work(8),\n\
             };\n\
             let one = par { work(9) };\n\
             println(f\"{a} {b} {one}\");\n\
         }\n",
        "v7 v8 v9\n",
    );
}

/// B-2026-10-20-22: every pattern parameter of the string methods is
/// `pat: Str` (docs/library/strings.md), `find` included, so a char is
/// refused with a fix: the string literal for a literal, `c.to_string()`
/// for anything else. A `Vec[char]`'s `contains` takes its element and is
/// left alone.
#[test]
fn a_string_pattern_is_a_str_not_a_char() {
    rejected_then_fixed_by(
        "char-pattern-literals",
        "fn main() {\n\
             let s = \"a/b#d\";\n\
             println(f\"{s.find('/')} {s.starts_with('#')} {s.ends_with('d')}\");\n\
         }\n",
        "'find' takes a `Str` pattern, found 'char'",
        "s.find(\"/\")",
    );
    rejected_then_fixed_by(
        "char-pattern-variable",
        "fn main() {\n\
             let s = \"a/b#d\";\n\
             let c = 'b';\n\
             println(f\"{s.contains(c)} {s.rfind(c)}\");\n\
         }\n",
        "write it as `c.to_string()`",
        "s.rfind(c.to_string())",
    );
    accepted(
        "char-vec-contains",
        "fn main() {\n\
             let cs: Vec[char] = vec!['a', 'b'];\n\
             println(f\"{cs.contains('a')} {\"ab\".contains(\"a\")}\");\n\
         }\n",
    );
}

/// B-2026-10-21-1: a closure that matches on a field of its OWN local moves
/// out of that local, which is a fresh move and not a capture; matching on a
/// captured binding's field and using it after the closure is still refused.
#[test]
fn a_closure_moves_out_of_its_own_local_freely() {
    accepted(
        "closure-local-field-match",
        "struct Opts { sub: Option[String] }\n\
         fn main() {\n\
             let f = || {\n\
                 let o = Opts { sub: Some(\"bob\".to_string()) };\n\
                 match o.sub {\n\
                     Some(s) => println(s),\n\
                     None => println(\"none\"),\n\
                 }\n\
             };\n\
             f();\n\
         }\n",
    );
    rejected(
        "captured-field-match-then-used",
        "struct Opts { sub: Option[String] }\n\
         fn main() {\n\
             let o = Opts { sub: Some(\"bob\".to_string()) };\n\
             let f = || {\n\
                 match o.sub {\n\
                     Some(s) => println(s),\n\
                     None => println(\"none\"),\n\
                 }\n\
             };\n\
             f();\n\
             println(f\"{o.sub.is_some()}\");\n\
         }\n",
        "was moved into a closure",
    );
}

/// B-2026-10-21-2: a `for` over a method that returns an owned collection
/// borrows nothing, whatever the method is called, so the body may write the
/// receiver; a builtin `iter()` / `keys()` view still holds it.
#[test]
fn a_loop_over_an_owned_result_borrows_nothing() {
    accepted(
        "user-keys-returns-a-vec",
        "struct Bag { xs: Vec[i64], n: i64 }\n\
         impl Bag {\n\
             fn keys(ref self) -> Vec[i64] { self.xs.clone() }\n\
             fn values(ref self) -> Vec[i64] { self.xs.clone() }\n\
             fn bump(mut ref self, k: i64) { self.n = self.n + k; }\n\
         }\n\
         fn main() {\n\
             let mut b = Bag { xs: vec![1, 2, 3], n: 0 };\n\
             for k in b.keys() { b.bump(k); }\n\
             for v in b.values() { b.bump(v); }\n\
             println(f\"{b.n}\");\n\
         }\n",
    );
    rejected(
        "builtin-keys-is-a-view",
        "fn main() {\n\
             let mut m: Map[i64, i64] = Map.new();\n\
             m[1] = 2;\n\
             for k in m.keys() { m.insert(k + 10, 0); }\n\
         }\n",
        "while the `for` loop",
    );
}

/// A program's own `struct Response` serves when it has the documented shape
/// (`status: i64`, `body: String`, optional `headers`); any other shape is a
/// check error at the `Server.serve` call, as legacy codegen made it.
#[test]
fn a_handler_response_has_the_documented_shape() {
    rejected(
        "handler-response-without-body",
        "struct Response { status: i64 }\n\
         fn handle(req: Request) -> Response { Response { status: 200 } }\n\
         fn main() with sends(Network) receives(Network) {\n\
             Server.serve(\"127.0.0.1:0\", handle);\n\
         }\n",
        "must return a `Response` with `status: i64` and `body: String`",
    );
    accepted(
        "handler-response-with-headers",
        "struct Response { status: i64, body: String, headers: Vec[(String, String)] }\n\
         fn handle(req: Request) -> Response {\n\
             Response { status: 200, body: \"{}\".to_string(), headers: Vec.new() }\n\
         }\n\
         fn main() with sends(Network) receives(Network) {\n\
             Server.serve(\"127.0.0.1:0\", handle);\n\
         }\n",
    );
}

/// B-2026-10-20-33: a collection where an adapter wants an `Iterator` (a
/// `flat_map` closure's result, a `chain` or `zip` argument) is one error
/// whose fix writes `.into_iter()`, not the bound, its repeat at the next
/// method and the unsolved item's uses.
#[test]
fn a_collection_where_an_iterator_is_wanted_takes_into_iter() {
    let flat_map = "fn pairs(n: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(n); v.push(n + 1); v }\n\
         fn main() {\n\
             let xs = vec![1, 2, 3];\n\
             let all: Vec[i64] = xs.iter().flat_map(|x| pairs(*x)).collect();\n\
             println(f\"{all}\");\n\
         }\n";
    rejected_then_fixed_by(
        "flat-map-into-iter",
        flat_map,
        "`Vec[i64]` is a collection, not an `Iterator`; iterate it with `.into_iter()`",
        "pairs(*x).into_iter()",
    );
    rejected_then_fixed_by(
        "chain-into-iter",
        "fn main() {\n\
             let v = [3, 1, 2];\n\
             let m = v.iter().cloned().chain([0]).min().unwrap();\n\
             println(f\"{m}\");\n\
         }\n",
        "iterate it with `.into_iter()`",
        ".chain([0].into_iter())",
    );
    let zip = "fn main() {\n\
             let a = vec![1, 2];\n\
             let b = vec![3, 4];\n\
             let s: i64 = a.iter().cloned().zip(b).map(|p| p.0 * p.1).sum();\n\
             let words = vec![\"a.b\", \"c\"];\n\
             let n = words.iter().flat_map(|w| { let v: Vec[String] = vec![w.to_string()]; v }).map(|k| k.len()).count();\n\
             println(f\"{s} {n}\");\n\
         }\n";
    let (dir, path) = fixture("zip-into-iter-one-error", zip);
    let out = karac()
        .arg("check")
        .arg("--output=json")
        .arg(&path)
        .output()
        .unwrap();
    let json = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        json.matches("\"severity\":\"error\"").count(),
        2,
        "one error per adapter, none for the items: {json}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    rejected_then_fixed_by(
        "zip-into-iter",
        zip,
        "iterate it with `.into_iter()`",
        ".zip(b.into_iter())",
    );
}

/// A misshaped handler `Response` is a type mismatch, not an M4a deferral:
/// its JSON code is E0200, not `E_DEFERRED_M4A`.
#[test]
fn a_misshaped_handler_response_is_a_type_mismatch() {
    let (dir, path) = fixture(
        "handler-response-code",
        "struct Response { status: i64 }\n\
         fn handle(req: Request) -> Response { Response { status: 200 } }\n\
         fn main() with sends(Network) receives(Network) {\n\
             Server.serve(\"127.0.0.1:0\", handle);\n\
         }\n",
    );
    let out = karac()
        .arg("check")
        .arg("--output=json")
        .arg(&path)
        .output()
        .unwrap();
    let json = String::from_utf8_lossy(&out.stdout);
    assert!(json.contains("\"code\":\"E0200\""), "{json}");
    assert!(!json.contains("E_DEFERRED_M4A"), "{json}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-21-19: tuple branches join when one leaves a slot unsolved
/// (`Vec.new()`) and the other a bare `None`, in either order.
#[test]
fn tuple_branches_with_an_empty_vec_and_a_bare_none_join() {
    accepted(
        "tuple-branches-vec-new-none",
        "fn main() {\n\
             let e: Vec[i64] = vec![3];\n\
             let flag = true;\n\
             let p = if flag { (Vec.new(), Some(1)) } else { (e, None) };\n\
             let e2: Vec[i64] = vec![3, 4];\n\
             let q = if not flag { (e2, Some(1)) } else { (Vec.new(), None) };\n\
             let r = match flag { true => (Vec.new(), Some(\"x\".to_string())), false => (vec![1, 2], None) };\n\
             println(f\"{p.0.len()} {q.1.is_some()} {r.0.len()}\");\n\
         }\n",
    );
}

/// B-2026-10-21-36: a range assignment writes through its target and keeps
/// no view of it, so a later `mut` borrow of the same Vec is accepted.
#[test]
fn a_range_assignment_keeps_no_borrow() {
    accepted(
        "range-assign-then-mut",
        "fn bump(v: mut ref Vec[i64]) { v[0] = 9; }\n\
         fn fill(s: mut Slice[i64], x: i64) { s[0] = x; }\n\
         fn main() {\n\
             let mut v: Vec[i64] = [1, 2, 3];\n\
             v[1..] = [5, 6];\n\
             bump(mut v);\n\
             fill(mut v, 7);\n\
             println(f\"{v}\");\n\
         }\n",
    );
}

/// B-2026-10-21-39: one call may borrow one field and move a different one,
/// in either order; borrowing and moving the SAME field is still refused.
#[test]
fn a_call_borrows_one_field_and_moves_another() {
    accepted(
        "borrow-a-move-b",
        "struct P { a: Vec[i64], b: Vec[i64] }\n\
         fn take(x: ref Vec[i64], y: own Vec[i64]) -> i64 { x.len() + y.len() }\n\
         fn give(y: own Vec[i64], x: ref Vec[i64]) -> i64 { x.len() + y.len() }\n\
         fn main() {\n\
             let p = P { a: vec![1, 2], b: vec![3] };\n\
             let q = P { a: vec![1, 2], b: vec![3] };\n\
             println(f\"{take(p.a, p.b)} {give(q.b, q.a)}\");\n\
         }\n",
    );
    rejected(
        "borrow-a-move-a",
        "struct P { a: Vec[i64], b: Vec[i64] }\n\
         fn take(x: ref Vec[i64], y: own Vec[i64]) -> i64 { x.len() + y.len() }\n\
         fn main() {\n\
             let p = P { a: vec![1, 2], b: vec![3] };\n\
             println(f\"{take(p.a, p.a)}\");\n\
         }\n",
        "cannot move `p` while a borrow into it",
    );
}

/// B-2026-10-21-53: a `pub fn` may name a library type (a socket, a process,
/// a regex, an iterator); a program's own private type still may not leak.
#[test]
fn a_pub_fn_may_name_a_library_type() {
    accepted(
        "pub-fn-library-types",
        "import std.regex.{Match, Regex};\n\
         pub fn f(x: Option[TcpStream]) -> bool { x.is_some() }\n\
         pub fn g(c: ref Command) -> i64 { 1 }\n\
         pub fn h(r: ref Regex) -> bool { r.is_match(\"a\") }\n\
         pub fn k(m: Option[Match]) -> bool { m.is_none() }\n\
         pub fn n(it: own VecIter[i64]) -> i64 { it.count() }\n\
         fn main() { println(f\"{f(None)} {k(None)}\"); }\n",
    );
    rejected(
        "pub-fn-private-type",
        "struct Mine { a: i64 }\n\
         pub fn bad(m: Mine) -> i64 { m.a }\n\
         fn main() { println(f\"{bad(Mine { a: 1 })}\"); }\n",
        "private type 'Mine' leaks through parameter of 'bad'",
    );
}

/// B-2026-10-21-32: only `Some`, `None`, `Ok` and `Err` are bare in an
/// expression. `Less`, `Release`, `Nfc` and the other prelude enums'
/// variants are written qualified there, with that as the fix; a pattern
/// over a known scrutinee still writes them bare.
#[test]
fn a_prelude_enum_variant_is_qualified_in_an_expression() {
    rejected_then_fixed_by(
        "bare-ordering-variant",
        "fn main() {\n\
             let o = 3.cmp(4);\n\
             let b = o == Less;\n\
             let r = match b { true => Greater, false => Equal };\n\
             let n = \"x\".normalize(Nfc);\n\
             println(f\"{b} {r == Ordering.Equal} {n}\");\n\
         }\n",
        "`Less` is a variant of `Ordering`: write `Ordering.Less`",
        "let n = \"x\".normalize(NormalizationForm.Nfc);",
    );
    accepted(
        "bare-variant-in-pattern",
        "fn main() {\n\
             match 3.cmp(4) {\n\
                 Less => println(\"lt\"),\n\
                 Equal => println(\"eq\"),\n\
                 Greater => println(\"gt\"),\n\
             }\n\
         }\n",
    );
}

/// Every `Atomic` method takes a borrowed `self` (concurrency.md § Atomics),
/// so `swap` on a borrowed parameter is accepted like `store` and the
/// `fetch_*` family, not refused as the collection method of that name
/// (B-2026-10-21-75).
#[test]
fn atomic_swap_on_a_borrowed_parameter_is_accepted() {
    accepted_and_runs(
        "atomic-swap-borrowed",
        "import std.sync.{Atomic};\n\
         pub fn tick(c: Atomic[i64]) -> i64 {\n\
             c.fetch_add(1);\n\
             c.swap(c.load())\n\
         }\n\
         fn main() {\n\
             let c: Atomic[i64] = Atomic.new(0);\n\
             println(tick(c));\n\
         }\n",
        "1\n",
    );
}

/// B-2026-10-21-46: an open range `a..` is the library's `RangeFrom`, an
/// iterator, so it takes adaptors and stops when they do. `.into_iter()` on
/// any range takes the "already an iterator" fix-it, whose edit starts at
/// the `.` even when the receiver is parenthesised.
#[test]
fn an_open_range_takes_iterator_adaptors() {
    let src = "fn main() {\n\
         \x20   let s: i64 = (1..).take(3).sum();\n\
         \x20   let n = 15;\n\
         \x20   let k = (1..).take_while(|k| k * (k + 1) / 2 <= n)\n\
         \x20       .filter(|k| (n - k * (k + 1) / 2) % k == 0).count();\n\
         \x20   let mut r = 5..;\n\
         \x20   let a = r.next();\n\
         \x20   let b = r.next();\n\
         \x20   let c: Vec[i64] = (10..).iter().step_by(5).take(3).collect();\n\
         \x20   println(f\"{s} {k} {a == Some(5)} {b == Some(6)} {c.len()} {(3..).contains(7)}\");\n\
         }\n";
    let want = "6 4 true true 3 true\n";
    accepted("open-range-adaptors", src);
    let (dir, path) = fixture("open-range-adaptors", src);
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
    rejected_then_fixed_by(
        "open-range-into-iter",
        "fn main() {\n\
             let s: i64 = (1..).into_iter().take(2).sum();\n\
             let t: i64 = (0..3).into_iter().sum();\n\
             println(f\"{s} {t} {(0..4).len()}\");\n\
         }\n",
        "no method 'into_iter' on type 'RangeFrom', which is already an iterator",
        "let s: i64 = (1..).take(2).sum();",
    );
}

/// An item a library module does not define is refused at its import,
/// naming the sibling module that has it, or the nearest name, or the
/// module's items (B-2026-10-21-7); a project build says the same.
#[test]
fn an_unknown_item_in_a_library_import_is_refused_at_the_import() {
    rejected(
        "gated-item-elsewhere",
        "import std.crypto.sha2.{sha1};\nfn main() { println(\"x\"); }\n",
        "unknown item `sha1` in module `std.crypto.sha2`; `sha1` is in `std.crypto.legacy`",
    );
    rejected(
        "gated-item-typo",
        "import std.crypto.sha2.{sha265};\nfn main() { println(\"x\"); }\n",
        "unknown item `sha265` in module `std.crypto.sha2`, did you mean `sha256`?",
    );
    rejected(
        "gated-item-none",
        "import std.encoding.{nonexistent};\nfn main() { println(\"x\"); }\n",
        "unknown item `nonexistent` in module `std.encoding`; available: DecodeError",
    );
    rejected(
        "gated-module-call",
        "import std.path;\nfn main() { println(path.parnt(\"a/b\") ?? \"none\"); }\n",
        "unknown item `parnt` in module `std.path`, did you mean `parent`?",
    );
    accepted(
        "gated-item-known",
        "import std.crypto.legacy.{sha1};\nimport std.encoding.{hex_encode};\nfn main() { println(\"x\"); }\n",
    );
    let (dir, _) = fixture("gated-item-project", "");
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import std.crypto.sha2.{sha1};\nfn main() { println(\"x\"); }\n",
    )
    .unwrap();
    let out = karac().arg("check").current_dir(&dir).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "project check must reject: {err}");
    assert!(
        err.contains("`sha1` is in `std.crypto.legacy`"),
        "project check: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `fetch_add` and `fetch_sub` are for an integer `Atomic[T]`
/// (concurrency.md § Atomics; B-2026-10-21-74).
#[test]
fn fetch_add_on_an_atomic_bool_is_refused() {
    rejected(
        "atomic-bool-fetch-add",
        "import std.sync.{Atomic};\nfn main() {\n    let f: Atomic[bool] = Atomic.new(false);\n    f.fetch_add(true);\n}\n",
        "`fetch_add` is for an integer `Atomic[T]`",
    );
    accepted(
        "atomic-bool-swap",
        "import std.sync.{Atomic};\nfn main() {\n    let f: Atomic[bool] = Atomic.new(false);\n    println(f.swap(true));\n}\n",
    );
}

/// A compound assignment to a `mut ref` field writes the value it borrows,
/// so `g.value += 1` on a guard bound without `mut` is accepted, as
/// `*g.value += 1` and `g.value.count += 1` are (concurrency.md § Mutex;
/// B-2026-10-21-72).
#[test]
fn compound_assign_through_a_guards_mut_ref_field_is_accepted() {
    accepted_and_runs(
        "mutex-guard-compound",
        "import std.sync.{Mutex};\n\
         fn bump(m: Mutex[i64]) {\n\
             let g = m.lock();\n\
             g.value += 1;\n\
             g.value *= 10;\n\
         }\n\
         fn main() {\n\
             let m = Mutex.new(4);\n\
             bump(m);\n\
             let g = m.lock();\n\
             println(g.value);\n\
         }\n",
        "50\n",
    );
}

/// B-2026-10-21-63: `Option.unwrap_or_default` (core-types.md, `T: Default`)
/// gives the value or `T`'s default, and is refused where `T` has none.
#[test]
fn option_unwrap_or_default_needs_default() {
    let src = "fn main() {\n\
         \x20   let a: Option[i64] = None;\n\
         \x20   let b: Option[Vec[i64]] = None;\n\
         \x20   let c: Option[String] = None;\n\
         \x20   let d: Option[i64] = Some(3);\n\
         \x20   println(f\"{a.unwrap_or_default()} {b.unwrap_or_default()} {c.unwrap_or_default().len()} {d.unwrap_or_default()}\");\n\
         }\n";
    let want = "0 [] 0 3\n";
    accepted("unwrap-or-default", src);
    let (dir, path) = fixture("unwrap-or-default", src);
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
    let (dir, path) = fixture(
        "unwrap-or-default-no-default",
        "struct P { x: i64 }\n\
         fn main() {\n\
             let a: Option[P] = None;\n\
             println(f\"{a.unwrap_or_default().x}\");\n\
         }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must refuse: {err}");
    assert!(err.contains("`P` does not implement `Default`"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-21-61: `Iterator.max` / `min` need `Self.Item: Ord`
/// (iterators.md), so float items are refused, with `total_cmp` as the way
/// to compare them; ordered items, viewed or owned, still work.
#[test]
fn iterator_max_and_min_need_ord_items() {
    let src = "#[derive(PartialEq, Eq, PartialOrd, Ord)]\n\
         struct P { x: i64 }\n\
         fn main() {\n\
         \x20   let v: Vec[i64] = [3, 1, 2];\n\
         \x20   let ps: Vec[P] = [P { x: 2 }, P { x: 5 }];\n\
         \x20   let fs: Vec[f64] = [1.0, 2.0];\n\
         \x20   let a = v.iter().max() ?? 0;\n\
         \x20   let b = v.into_iter().min() ?? 0;\n\
         \x20   let c = match ps.iter().max() { Some(p) => p.x, None => 0 };\n\
         \x20   let d = (1..5).max() ?? 0;\n\
         \x20   let e = fs.iter().max_by(|a, b| a.total_cmp(b)) ?? 0.0;\n\
         \x20   println(f\"{a} {b} {c} {d} {e}\");\n\
         }\n";
    let want = "3 1 5 4 2.0\n";
    accepted("iter-max-ord", src);
    let (dir, path) = fixture("iter-max-ord", src);
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
    for (tag, call) in [
        ("iter-max-f64", "v.iter().max()"),
        ("iter-min-f64", "v.into_iter().min()"),
    ] {
        let (dir, path) = fixture(
            tag,
            &format!(
                "fn main() {{\n\
                 \x20   let v: Vec[f64] = [1.0, 2.0];\n\
                 \x20   let m = {call};\n\
                 }}\n"
            ),
        );
        let out = karac().arg("check").arg(&path).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{tag}: check must refuse: {err}");
        assert!(err.contains("has no total order (`Ord`)"), "{tag}: {err}");
        assert!(err.contains("total_cmp"), "{tag}: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The collections.md surface `karac check` used to refuse: the `from`
/// constructors, `VecDeque`'s traits, `remove_first`, `f64.total_cmp` as a
/// comparator, `clone_from_slice` and an `Array`'s `Slice` methods
/// (B-2026-10-21-30).
#[test]
fn the_collections_page_surface_is_accepted() {
    accepted(
        "collections-page",
        "fn main() {\n\
             let primes = Set.from([2, 3, 5, 7]);\n\
             let ages = Map.from([(\"ann\", 31), (\"bo\", 27)]);\n\
             println(f\"{primes.len()} {ages[\"ann\"]}\");\n\
             let pq: PriorityQueue[i64] = [3, 1, 2].into_iter().collect();\n\
             println(f\"{pq.len()}\");\n\
             let d: VecDeque[i64] = VecDeque.new();\n\
             let e: VecDeque[i64] = VecDeque.new();\n\
             println(f\"{d} {d == e} {d < e}\");\n\
             let mut v: Vec[i64] = [1, 4, 6];\n\
             let x = v.remove_first(|x| x % 2 == 0);\n\
             let mut w: Vec[f64] = [2.5, -1.5];\n\
             w.sort_by(f64.total_cmp);\n\
             let src: Vec[i64] = [9, 8];\n\
             v[0..2].clone_from_slice(src);\n\
             let mut a: Array[i64, 4] = Array.from_fn(4, |i| i * i);\n\
             a.sort();\n\
             a.reverse();\n\
             a.swap(0, 3);\n\
             a.fill(1);\n\
             println(f\"{a} {v} {w} {src}\");\n\
         }\n",
    );
}

/// An `Array`'s in-place `Slice` methods write it, so they need `let mut`;
/// `Array.from_fn`'s count must be the array's length (B-2026-10-21-30).
#[test]
fn an_arrays_in_place_methods_and_from_fn_are_checked() {
    rejected(
        "array-sort-immutable",
        "fn main() {\n    let a: Array[i64, 3] = [3, 1, 2];\n    a.sort();\n    println(f\"{a}\");\n}\n",
        "cannot call `sort` on `a`",
    );
    rejected(
        "array-from-fn-count",
        "fn main() {\n    let a: Array[i64, 3] = Array.from_fn(4, |i| i);\n    println(f\"{a}\");\n}\n",
        "Array.from_fn builds 4 elements, but the array has 3",
    );
}

/// Core §11.2 (B-2026-10-22-3): a local only one `par` branch writes is no
/// conflict, and a real one is reported once, naming both branches, with
/// none of the legacy `Arc` / `RwLock` / `#[thread_local]` advice.
#[test]
fn a_par_branch_may_write_a_local_no_other_branch_uses() {
    accepted(
        "par-lone-writer",
        "fn main() {\n\
         \x20   let mut v: Vec[i64] = Vec.new();\n\
         \x20   let mut n = 0;\n\
         \x20   par {\n\
         \x20       v.push(1),\n\
         \x20       { n = 3; },\n\
         \x20   };\n\
         \x20   println(f\"{v.len()} {n}\");\n\
         }\n",
    );
    let src = "fn main() {\n\
               \x20   let mut v: Vec[i64] = Vec.new();\n\
               \x20   par {\n\
               \x20       v.push(1),\n\
               \x20       v.push(2),\n\
               \x20   };\n\
               \x20   println(f\"{v.len()}\");\n\
               }\n";
    let (dir, path) = fixture("par-two-writers", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must reject: {err}");
    assert!(
        err.contains("branches 1 and 2 of the `par` conflict"),
        "{err}"
    );
    assert!(
        err.contains("1 error(s) found"),
        "one error, not three: {err}"
    );
    for stale in ["Arc", "RwLock", "thread_local", "sync struct"] {
        assert!(!err.contains(stale), "no `{stale}` advice: {err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Core §11.3 (B-2026-10-19-46, pin 6.3-shared-into-par): a `shared` handle
/// may not enter a `par` branch or a `par for` iteration, one branch alone
/// included; a `frozen` parameter or `freeze` binding may.
#[test]
fn a_shared_handle_cannot_enter_a_par_branch() {
    let shared = "shared struct S { mut n: i64 }\n\
                  fn get(s: S) -> i64 { s.n }\n";
    rejected(
        "shared-into-par",
        &format!(
            "{shared}fn main() {{\n\
             \x20   let s = S {{ n: 1 }};\n\
             \x20   let (a, b) = par {{ get(s), get(s) }};\n\
             \x20   println(f\"{{a + b}}\");\n\
             }}\n"
        ),
        "`s` (a `shared` handle of type `S`) cannot enter a `par` branch",
    );
    rejected(
        "shared-into-one-branch",
        &format!(
            "{shared}fn main() {{\n\
             \x20   let s = S {{ n: 1 }};\n\
             \x20   let (a, b) = par {{ s.n + 1, 2 }};\n\
             \x20   println(f\"{{a + b}}\");\n\
             }}\n"
        ),
        "cannot enter a `par` branch",
    );
    rejected(
        "shared-into-par-for",
        &format!(
            "{shared}fn main() {{\n\
             \x20   let s = S {{ n: 1 }};\n\
             \x20   let out = par for x in vec![1, 2] {{ x + s.n }};\n\
             \x20   println(f\"{{out}}\");\n\
             }}\n"
        ),
        "cannot enter a `par` branch",
    );
    accepted(
        "frozen-into-par",
        "shared struct S { n: i64 }\n\
         fn twice(s: frozen S) -> i64 {\n\
         \x20   let (a, b) = par { s.n, s.n };\n\
         \x20   a + b\n\
         }\n\
         fn main() {\n\
         \x20   let root = S { n: 2 };\n\
         \x20   let g = freeze root;\n\
         \x20   let (a, b) = par { twice(g), twice(g) };\n\
         \x20   println(f\"{a + b}\");\n\
         }\n",
    );
}

/// `std.regex` and `std.json` are imported before their items are used
/// (docs/library/README.md): a bare use is an error whose fix adds the
/// import, after the program's last one or above its first item
/// (B-2026-10-21-56).
#[test]
fn regex_and_json_items_need_their_import() {
    rejected_then_fixed_by(
        "bare-regex",
        "// A comment stays first.\n\
         fn main() {\n\
         \x20   let re = Regex.compile(\"a+\").unwrap();\n\
         \x20   let m: Option[Match] = re.find(\"baa\");\n\
         \x20   println(f\"{m.is_some()}\");\n\
         }\n",
        "`Regex` is in the library module `std.regex`, which a program imports before it uses it",
        "// A comment stays first.\nimport std.regex.{Match, Regex};\n\nfn main() {",
    );
    rejected_then_fixed_by(
        "bare-json-and-regex",
        "import std.io.{File};\n\
         fn main() {\n\
         \x20   let j: Result[Json, JsonError] = Json.parse(\"[1]\");\n\
         \x20   let r: Result[Regex, RegexError] = Regex.compile(\"(\");\n\
         \x20   println(f\"{j.is_ok()} {r.is_err()}\");\n\
         }\n",
        "`Json` is in the library module `std.json`",
        "import std.io.{File};\nimport std.json.{Json, JsonError};\nimport std.regex.{Regex, RegexError};\n",
    );
    rejected(
        "misspelled-regex-item",
        "import std.regex.{Regx};\nfn main() { println(\"x\"); }\n",
        "`std.regex` has no item `Regx`, did you mean `Regex`?",
    );
}

/// The imports resolve, by name or as a wildcard; a program's own type or
/// variant named like one of their items needs no import and shadows nothing
/// (B-2026-10-21-56).
#[test]
fn imported_regex_and_json_items_and_own_names_are_accepted() {
    accepted(
        "imported-regex-json",
        "import std.regex.{Regex, Match, Captures, RegexError};\n\
         import std.json.*;\n\
         fn main() {\n\
         \x20   let re = Regex.compile(\"(a)\").unwrap();\n\
         \x20   let c: Option[Captures] = re.captures(\"a\");\n\
         \x20   let j: Result[Json, JsonError] = Json.parse(\"1\");\n\
         \x20   println(f\"{c.is_some()} {j.is_ok()}\");\n\
         }\n",
    );
    let (dir, path) = fixture(
        "own-match-json",
        "struct Match { a: i64 }\n\
         enum Format { Json, Csv }\n\
         fn main() {\n\
         \x20   let m = Match { a: 3 };\n\
         \x20   let f = Format.Json;\n\
         \x20   match f { Format.Json => println(f\"{m.a}\"), Format.Csv => println(\"c\") }\n\
         }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "own-match-json: {err}");
    assert!(
        !err.contains("shadows the prelude"),
        "own-match-json: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-22-5: an E0516 whose borrow is a `let` initializer gets a
/// machine-applicable copy when copying keeps the meaning: a `shared` value
/// is retained, a `Copy` one is read out, an `Option[ref T]` is `.cloned()`.
/// A plain value, whose copy would be a deep clone, gets none.
#[test]
fn a_held_borrow_of_a_shared_or_copy_value_is_fixed_by_a_copy() {
    rejected_then_fixed_by(
        "held-shared-borrow",
        "shared struct Node { val: i64, mut seen: i64 }\n\
         fn main() {\n\
         \x20   let mut m: Map[i64, Node] = Map.new();\n\
         \x20   m.insert(1, Node { val: 1, seen: 0 });\n\
         \x20   let cur = m.get(1).unwrap();\n\
         \x20   m.insert(2, Node { val: 2, seen: 0 });\n\
         \x20   cur.seen = cur.seen + 1;\n\
         \x20   println(f\"{cur.val} {m.len()}\");\n\
         }\n",
        "[E0516]",
        "m.get(1).unwrap().clone();",
    );
    rejected_then_fixed_by(
        "held-copy-borrow",
        "fn main() {\n\
         \x20   let mut v = vec![3, 4];\n\
         \x20   let first = v.get(0).unwrap();\n\
         \x20   v.push(5);\n\
         \x20   println(f\"{first} {v.len()}\");\n\
         }\n",
        "[E0516]",
        "let first = *v.get(0).unwrap();",
    );
    rejected_then_fixed_by(
        "held-option-borrow",
        "shared struct Node { val: i64 }\n\
         fn main() {\n\
         \x20   let mut m: Map[i64, Node] = Map.new();\n\
         \x20   m.insert(1, Node { val: 1 });\n\
         \x20   let found = m.get(1);\n\
         \x20   m.insert(2, Node { val: 2 });\n\
         \x20   match found {\n\
         \x20       Some(n) => println(f\"{n.val}\"),\n\
         \x20       None => println(\"none\"),\n\
         \x20   }\n\
         }\n",
        "[E0516]",
        "m.get(1).cloned();",
    );
    let src = "fn main() {\n\
               \x20   let mut v = vec![String.from(\"a\")];\n\
               \x20   let first = v.get(0).unwrap();\n\
               \x20   v.push(String.from(\"b\"));\n\
               \x20   println(f\"{first} {v.len()}\");\n\
               }\n";
    let (dir, path) = fixture("held-plain-borrow", src);
    karac().arg("fix").arg(&path).output().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        src,
        "no fix for a plain value"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-10-22-8: design.md § Constants makes `const` the only value
/// declared at module scope. A module `let` whose initializer is already
/// constant is fixed to `const`, the annotation inferred when it was elided.
#[test]
fn a_module_let_is_refused_with_a_fix_to_const() {
    rejected_then_fixed_by(
        "module-let-annotated",
        "pub let MAX_RETRIES: i64 = 5;\n\
         let APP_NAME: StringSlice = \"karac\";\n\
         fn main() { println(f\"{APP_NAME} {MAX_RETRIES}\") }\n",
        "E_MODULE_LET",
        "pub const MAX_RETRIES: i64 = 5;\nconst APP_NAME: StringSlice = \"karac\";",
    );
    rejected_then_fixed_by(
        "module-let-inferred",
        "let SPREAD = 4096;\n\
         fn main() { println(f\"{SPREAD}\") }\n",
        "E_MODULE_LET",
        "const SPREAD: i64 = 4096;",
    );
}

/// B-2026-10-22-8: there are no mutable globals. A module `let mut`, or a
/// module `let` of a cell (a `const` would build a fresh one at every use),
/// is refused with no fix, and the help names the `main`-and-context
/// pattern. The legacy `module_mut_binding` warning does not fire as well.
#[test]
fn a_module_let_mut_or_cell_is_refused_without_a_fix() {
    for (tag, src) in [
        (
            "module-let-mut",
            "let mut COUNT: i64 = 0;\n\
             fn bump() { COUNT = COUNT + 1; }\n\
             fn main() { bump(); println(f\"{COUNT}\") }\n",
        ),
        (
            "module-let-cell",
            "let COUNTER: Atomic[i64] = Atomic.new(0);\n\
             fn main() { COUNTER.store(7); println(COUNTER.load()) }\n",
        ),
    ] {
        let (dir, path) = fixture(tag, src);
        let out = karac().arg("check").arg(&path).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{tag}: check must reject: {err}");
        assert!(err.contains("E_MODULE_LET"), "{tag}: {err}");
        assert!(err.contains("create the value in `main`"), "{tag}: {err}");
        assert!(!err.contains("module_mut_binding"), "{tag}: {err}");
        karac().arg("fix").arg(&path).output().unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            src,
            "{tag}: no fix for mutable global state"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// B-2026-10-22-19: a `const` initializer is a compile-time constant
/// (design.md § Constants, "Forbidden initializers"). A call, a method call
/// or a collection constructor is refused with the `main`-and-context help
/// and no fix; it used to be accepted by `karac check`.
#[test]
fn a_const_initializer_that_runs_code_is_refused_without_a_fix() {
    for (tag, src) in [
        (
            "const-init-call",
            "fn compute() -> i64 { 6 * 7 }\n\
             const ANSWER: i64 = compute();\n\
             fn main() { println(f\"{ANSWER}\") }\n",
        ),
        (
            "const-init-vec-new",
            "const ITEMS: Vec[i64] = Vec.new();\n\
             fn main() { println(f\"{ITEMS.len()}\") }\n",
        ),
        (
            "const-init-map-new",
            "const SEEN: Map[i64, i64] = Map.new();\n\
             fn main() { println(f\"{SEEN.len()}\") }\n",
        ),
        (
            "const-init-method",
            "const SIZE: i64 = \"abc\".len();\n\
             fn main() { println(f\"{SIZE}\") }\n",
        ),
    ] {
        let (dir, path) = fixture(tag, src);
        let out = karac().arg("check").arg(&path).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{tag}: check must reject: {err}");
        assert!(
            err.contains("must be a compile-time constant"),
            "{tag}: {err}"
        );
        assert!(err.contains("build the value in `main`"), "{tag}: {err}");
        karac().arg("fix").arg(&path).output().unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            src,
            "{tag}: no fix for a run-time initializer"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// B-2026-10-22-19: a constant cannot have a heap type ("String is
/// heap-allocated, so it cannot be a constant's type; use Str"). A bare
/// `String`, one nested in an `Array` / tuple / `Option`, and a `Vec` built
/// from a literal are each refused, and `karac fix` writes the `Str` or
/// `Array` form.
#[test]
fn a_const_of_a_heap_type_is_fixed_to_str_or_array() {
    for (tag, src, edit) in [
        (
            "const-string",
            "const HOST: String = \"localhost\";\nfn main() { println(HOST) }\n",
            "const HOST: Str = \"localhost\";",
        ),
        (
            "const-vec-literal",
            "const NUMS: Vec[i64] = [1, 2];\nfn main() { println(f\"{NUMS[1]}\") }\n",
            "const NUMS: Array[i64, 2] = [1, 2];",
        ),
        (
            "const-array-of-string",
            "const WORDS: Array[String, 2] = [\"a\", \"bb\"];\n\
             fn main() { println(f\"{WORDS[1].len()}\") }\n",
            "const WORDS: Array[Str, 2] = [\"a\", \"bb\"];",
        ),
        (
            "const-tuple-with-string",
            "const PAIR: (i64, String) = (1, \"x\");\n\
             fn main() { let p = PAIR; println(f\"{p.0} {p.1}\") }\n",
            "const PAIR: (i64, Str) = (1, \"x\");",
        ),
        (
            "const-option-string",
            "const MAYBE: Option[String] = Some(\"m\");\n\
             fn main() { match MAYBE { Some(s) => println(s), None => {} } }\n",
            "const MAYBE: Option[Str] = Some(\"m\");",
        ),
    ] {
        rejected_then_fixed_by(tag, src, "E_MODULE_BINDING_HEAP_TYPE", edit);
    }
}

/// B-2026-10-22-19: every "Allowed initializers" form of design.md
/// § Constants passes, a bare prelude variant constructor (`Some(42)`)
/// included; a user variant is built qualified (B-2026-10-22-46).
#[test]
fn a_const_accepts_every_allowed_initializer_form() {
    accepted(
        "const-allowed-forms",
        "enum Color { Red, Rgb(i64, i64, i64) }\n\
         struct Point { x: i64, y: i64 }\n\
         const MAX_RETRIES: i64 = 5;\n\
         const TIMEOUT_MS: i64 = 60 * 1000;\n\
         const RETRY_PLUS: i64 = MAX_RETRIES + 1;\n\
         const DEBUG: bool = TIMEOUT_MS > 0 and true;\n\
         const APP_NAME: Str = \"karac\";\n\
         const ANSWER: Option[i64] = Some(42);\n\
         const SHADE: Color = Color.Rgb(1, 2, 3);\n\
         const BARE: Color = Color.Rgb(4, 5, 6);\n\
         const ORIGIN: Point = Point { x: 0, y: 0 };\n\
         const TRIPLE: (i64, i64, i64) = (1, 2, 3);\n\
         const TABLE: Array[i64, 3] = [1, 2, 3];\n\
         const ZEROS: Array[u8, 4] = [0; 4];\n\
         fn main() {\n\
             let o = ORIGIN;\n\
             let t = TRIPLE;\n\
             match BARE { Rgb(r, _, _) => println(f\"{r}\"), Color.Red => {} }\n\
             match SHADE { Rgb(r, _, _) => println(f\"{r}\"), Color.Red => {} }\n\
             println(f\"{RETRY_PLUS} {DEBUG} {APP_NAME} {ANSWER} {o.x} {t.2} {TABLE[2]} {ZEROS.len()}\")\n\
         }\n",
    );
}

/// B-2026-10-22-19: a `const` of a cell type is mutable global state, so it
/// gets the module-cell answer a module `let` of one gets (E_MODULE_LET,
/// cells deferred, `main`-and-context help), not a constness error about its
/// constructor, and no fix.
#[test]
fn a_const_of_a_cell_type_gets_the_module_cell_refusal() {
    for (tag, src) in [
        (
            "const-lazy-lock",
            "fn build() -> i64 { 3 }\n\
             const TABLE: LazyLock[i64] = LazyLock.new(|| build());\n\
             fn main() { let w = TABLE.get(); println(f\"{w}\") }\n",
        ),
        (
            "const-atomic",
            "const HITS: Atomic[i64] = Atomic.new(0);\n\
             fn main() { HITS.store(1); println(HITS.load()) }\n",
        ),
    ] {
        let (dir, path) = fixture(tag, src);
        let out = karac().arg("check").arg(&path).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{tag}: check must reject: {err}");
        assert!(err.contains("E_MODULE_LET"), "{tag}: {err}");
        assert!(err.contains("module cells are deferred"), "{tag}: {err}");
        assert!(err.contains("create the value in `main`"), "{tag}: {err}");
        assert!(!err.contains("compile-time constant"), "{tag}: {err}");
        karac().arg("fix").arg(&path).output().unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            src,
            "{tag}: no fix for a cell"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// `chunks` / `windows` are slice methods, not iterator adaptors
/// (B-2026-10-22-22): `v.iter().chunks(n)` is fixed by dropping the hop, and
/// any other iterator is told to collect first.
#[test]
fn iterator_chunks_and_windows_point_at_the_slice_methods() {
    let src = "fn main() {
    let v: Vec[i64] = [1, 2, 3, 4, 5];
    for c in v.iter().chunks(2) {
        println(c.len());
    }
    for w in v.iter().windows(2) {
        println(w.len());
    }
}
";
    rejected_then_fixed_by(
        "iter-chunks",
        src,
        "no method 'chunks' on type 'VecIter', an iterator: `chunks` is a `Vec` / `Slice` / `Array` method; drop `.iter()`",
        "for c in v.chunks(2) {",
    );
    let src = "fn main() {
    let s = \"abc\";
    for w in s.chars().windows(2) {
        println(w.len());
    }
}
";
    let (dir, path) = fixture("chars-windows", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must reject: {err}");
    assert!(
        err.contains("collect the items into a `Vec` first (`let items: Vec[char] = it.collect();`), then call `items.windows(n)`"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// design.md § returning a reference: "The `ref` is written". A non-`Copy`
/// place returned bare into a `ref` / `mut ref` slot of the return type moves
/// out of a borrowed place; `karac fix` writes the `ref` (B-2026-10-22-28).
#[test]
fn a_returned_place_writes_its_ref() {
    let src = "enum Side { Left, Right }
struct Pair { first: String, second: String }
fn choose(s: ref Side, p: ref Pair) -> ref String {
    match s {
        Side.Left => p.first,
        Side.Right => p.second,
    }
}
struct Bag { items: Vec[String], n: i64 }
impl Bag {
    fn first(ref self) -> ref String { self.items[0] }
    fn first_mut(mut ref self) -> mut ref String { self.items[0] }
    fn pair(ref self) -> (ref String, i64) { (self.items[0], self.n) }
    fn maybe_mut(mut ref self) -> Option[mut ref String] { if self.n > 0 { Some(self.items[0]) } else { None } }
}
fn main() {
    let p = Pair { first: \"a\", second: \"b\" };
    println(choose(Side.Right, p));
    let mut b = Bag { items: [\"x\"], n: 1 };
    println(b.first());
    println(b.first_mut());
    println(b.pair().0);
    println(b.maybe_mut().is_some());
}
";
    rejected_then_fixed_by(
        "bare-ref-return",
        src,
        "the function returns a reference, and the return type does not borrow for you",
        "Side.Right => ref p.second,",
    );
    let (dir, path) = fixture("bare-ref-return-fixed", src);
    karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    for edit in [
        "{ ref self.items[0] }",
        "{ mut ref self.items[0] }",
        "(ref self.items[0], self.n)",
        "Some(mut self.items[0])",
    ] {
        assert!(fixed.contains(edit), "fix must write `{edit}`: {fixed}");
    }
    let _ = std::fs::remove_dir_all(&dir);
    // `ref` is not written on a call argument, so a `ref` slot of an
    // `Option` is borrowed into a binding first: help, no edit.
    let src = "struct Bag { items: Vec[String] }
impl Bag {
    fn maybe(ref self) -> Option[ref String] { Some(self.items[0]) }
}
fn main() { println(Bag { items: [\"x\"] }.maybe().is_some()); }
";
    let (dir, path) = fixture("bare-ref-return-some", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains(
            "borrow it into a binding first (`let r = ref <place>;`) and return `Some(r)`"
        ),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// What B-2026-10-22-28 leaves alone: a `Copy` place, a borrowed parameter
/// returned as it is, a call returning a reference and a literal default.
#[test]
fn a_returned_borrow_or_copy_needs_no_ref() {
    accepted(
        "ref-return-unchanged",
        "struct Pair { first: String, n: i64 }
struct Bag { p: Pair }
impl Bag {
    fn count(ref self) -> ref i64 { self.p.n }
    fn pair(ref self) -> ref Pair { ref self.p }
}
fn same(p: ref Pair) -> ref Pair { p }
fn via(b: ref Bag) -> ref Pair { b.pair() }
fn dflt(m: ref Map[String, String], k: ref String) -> ref String { m.get(k).unwrap_or(\"\") }
fn main() {
    let b = Bag { p: Pair { first: \"a\", n: 2 } };
    println(b.count());
    println(same(b.pair()).first);
    println(via(b).n);
    let m: Map[String, String] = Map.new();
    println(dflt(m, \"k\"));
}
",
    );
}

/// docs/library/collections.md: `push_front` / `push_back` / `pop_front` /
/// `pop_back` are `VecDeque` methods. On a `Vec`, `karac check` refuses each
/// by name; `karac fix` writes the back pair as `push` / `pop`, and the front
/// pair points at `insert(0, x)` / `remove(0)` or a `VecDeque`
/// (B-2026-10-22-48). A `VecDeque` still takes all four.
#[test]
fn deque_methods_on_a_vec_name_the_vec_spelling() {
    let src = "fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push_back(1);
    v.push_back(2);
    let last = v.pop_back();
    println(\"{v.len()} {last.is_some()}\");
}
";
    rejected_then_fixed_by(
        "vec-push-back",
        src,
        "no method 'push_back' on type 'Vec': `push_back` is a `VecDeque` method; a `Vec` grows and shrinks at its back with `push`",
        "let last = v.pop();",
    );
    let src = "fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push_front(2);
    let first = v.pop_front();
    println(\"{v.len()} {first.is_some()}\");
}
";
    let (dir, path) = fixture("vec-push-front", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must reject: {err}");
    for want in [
        "no method 'push_front' on type 'Vec': `push_front` is a `VecDeque` method; on a `Vec` write `insert(0, x)`",
        "no method 'pop_front' on type 'Vec': `pop_front` is a `VecDeque` method; on a `Vec` write `remove(0)` (which returns the element, not an `Option`)",
    ] {
        assert!(err.contains(want), "expected `{want}` in: {err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
    accepted(
        "deque-four",
        "fn main() {
    let mut d: VecDeque[i64] = VecDeque.new();
    d.push_back(1);
    d.push_front(2);
    let a = d.pop_back();
    let b = d.pop_front();
    println(\"{d.len()} {a.is_some()} {b.is_some()}\");
}
",
    );
}

/// A file checked on its own that belongs to no package is the whole program
/// beside the library, so an import of a module outside `std` names nothing
/// (B-2026-10-22-30). `karac check` and `karac run` refuse it; the same import
/// in a package resolves to the sibling module, checked by file or by package.
#[test]
fn an_import_from_no_module_is_refused_outside_a_package() {
    rejected(
        "lone-file-import",
        "import nosuch.{run};\nfn main() { run(); }\n",
        "unknown module `nosuch`: this file belongs to no package, so the only modules it can import are the library's (`std.*`)",
    );
    let (dir, path) = fixture(
        "lone-file-import-run",
        "import nosuch.{run};\nfn main() { run(); }\n",
    );
    let out = karac().arg("run").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "run must refuse: {err}");
    assert!(err.contains("unknown module `nosuch`"), "run: {err}");
    let _ = std::fs::remove_dir_all(&dir);
    let (dir, _) = fixture("lone-file-import-package", "");
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import util.{run};\nfn main() { run(); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/util.kara"),
        "pub fn run() with writes(Stdout) { println(\"hi\"); }\n",
    )
    .unwrap();
    for args in [&["check"][..], &["check", "src/main.kara"][..]] {
        let out = karac().args(args).current_dir(&dir).output().unwrap();
        assert!(
            out.status.success(),
            "{args:?} must pass in a package: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let out = karac()
        .args(["run", "src/main.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hi\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A named place passed as the default of `unwrap_or` on an `Option[ref T]`
/// is lent, as under `??`: `default: own T` with `T = ref Vec[i64]` borrows
/// `empty`, in a loop and before a later use (B-2026-10-23-2). An owned
/// `Option[Vec]` default still moves it.
#[test]
fn a_named_default_of_an_option_ref_is_lent() {
    accepted_and_runs(
        "unwrap-or-named-default",
        "fn main() {
    let mut m: Map[i64, Vec[i64]] = Map.new();
    m.insert(1, [5, 1]);
    let empty: Vec[i64] = Vec.new();
    for k in 0..3 {
        let line = m.get(k).unwrap_or(empty);
        let alt = m.get(k) ?? empty;
        println(f\"{k} {line.len()} {alt.len()}\");
    }
    let last = m.get(9).unwrap_or(empty);
    println(f\"{last.len()} {empty.len()}\");
}
",
        "0 0 0\n1 2 2\n2 0 0\n0 0\n",
    );
    rejected(
        "unwrap-or-owned-default",
        "fn main() {
    let o: Option[Vec[i64]] = None;
    let empty: Vec[i64] = Vec.new();
    let a = o.unwrap_or(empty);
    println(f\"{a.len()} {empty.len()}\");
}
",
        "value 'empty' moved here, used again here",
    );
}

/// GPU compute goes to the archive with the legacy backend at M3 and comes
/// back on v2 on demand, so check refuses `gpu.*` by name rather than
/// leaving it to fail later in the build (B-2026-10-22-21).
#[test]
fn gpu_compute_is_refused_as_archived() {
    rejected(
        "gpu-dispatch-archived",
        "#[gpu] fn double(x: f32) -> f32 { x * 2.0 }\n\
         fn main() {\n    let buf: Vec[f32] = [1.0, 2.0, 3.0, 4.0];\n    \
         let out = gpu.dispatch(double, buf);\n    for v in out { println(f\"{v}\") }\n}\n",
        "`gpu.dispatch` is not available: GPU compute",
    );
    rejected(
        "gpu-sum-archived",
        "fn main() {\n    let xs: Vec[f32] = [1.0, 2.0];\n    println(f\"{gpu.sum(xs)}\")\n}\n",
        "`gpu.sum` is not available: GPU compute (`gpu.*` and `#[gpu]` kernels) goes to the archive",
    );
}

const CLK: &str = "trait Ticker { fn now(self) -> i64; }
effect resource Clk: Ticker;
struct F { t: i64 }
impl Ticker for F { fn now(self) -> i64 { self.t } }
fn tick() -> i64 { Clk.now() }
";

/// core §14.5: a closure that uses `R`, directly or through a callee, and a
/// function taken as a value whose effects reach `R`, cannot leave the
/// `with_provider` that binds `R` (B-2026-10-20-8). Plain data can.
#[test]
fn a_provider_cannot_leave_its_scope_through_a_callee_or_a_function_value() {
    let cases = [
        (
            "provider-escape-callee",
            "fn main() {\n    let g = with_provider[Clk](F { t: 1 }, || { return || tick(); });\n    println(g());\n}\n",
            "this closure uses provider-rooted resource `Clk`, whose provider is bound by the `with_provider` at 7:13, so it cannot be returned",
        ),
        (
            "provider-escape-tail",
            "fn main() {\n    let g = with_provider[Clk](F { t: 1 }, || { || tick() });\n    println(g());\n}\n",
            "this closure uses provider-rooted resource `Clk`",
        ),
        (
            "provider-escape-fn-value",
            "fn main() {\n    let g = with_provider[Clk](F { t: 1 }, || { return tick; });\n    println(g());\n}\n",
            "function `tick`, taken as a value, uses provider-rooted resource `Clk`",
        ),
        (
            "provider-escape-push",
            "fn main() {\n    let mut v: Vec[Fn() -> i64 with reads(Clk)] = Vec.new();\n    with_provider[Clk](F { t: 1 }, || { v.push(|| tick()); });\n    println(v.len());\n}\n",
            "so it cannot be stored in a collection",
        ),
        (
            "provider-escape-field",
            "struct Holder { f: Fn() -> i64 with reads(Clk) }\nfn main() {\n    let h = with_provider[Clk](F { t: 1 }, || { Holder { f: || Clk.now() } });\n    println((h.f)());\n}\n",
            "so it cannot be stored in a struct field",
        ),
    ];
    for (tag, main, expect) in cases {
        rejected(tag, &format!("{CLK}{main}"), expect);
    }
    accepted_and_runs(
        "provider-plain-data-leaves",
        &format!(
            "{CLK}fn apply(f: Fn() -> i64 with reads(Clk)) -> i64 reads(Clk) {{ f() }}\n\
             fn main() {{\n    let x = with_provider[Clk](F {{ t: 7 }}, || {{ let c = || tick() + 1; c() }});\n    \
             let y = with_provider[Clk](F {{ t: 3 }}, || apply(|| tick() * 2));\n    println(x + y);\n}}\n"
        ),
        "14\n",
    );
}

const SHARED_CLK: &str = "trait Ticker { fn now(self) -> i64; }
effect resource Clk: Ticker;
shared struct S { t: i64 }
impl Ticker for S { fn now(self) -> i64 { self.t } }
fn tick() -> i64 { Clk.now() }
fn fan() { let mut tg = TaskGroup.new(); tg.spawn(|| println(tick())); }
";

/// core §14.9: a provider must be `CrossTask` only when a branch or a task
/// inside the scope shares it, in the body or in a function it reaches
/// (B-2026-10-20-8). A sequential scope may bind a `shared struct`.
#[test]
fn a_provider_need_be_cross_task_only_when_a_task_shares_it() {
    accepted_and_runs(
        "provider-shared-sequential",
        &format!(
            "{SHARED_CLK}fn main() {{\n    let x = with_provider[Clk](S {{ t: 1 }}, || tick() + 1);\n    \
             providers {{ Clk => S {{ t: 3 }} }} in {{ println(x + tick()); }}\n}}\n"
        ),
        "5\n",
    );
    let expect = "provider for resource `Clk` (type `S`) cannot cross a task boundary";
    rejected(
        "provider-shared-by-a-task",
        &format!("{SHARED_CLK}fn main() {{ with_provider[Clk](S {{ t: 1 }}, || fan()); }}\n"),
        expect,
    );
    rejected(
        "provider-shared-by-a-task-block",
        &format!(
            "{SHARED_CLK}fn main() {{ providers {{ Clk => S {{ t: 2 }} }} in {{ fan(); }} }}\n"
        ),
        expect,
    );
}

/// core §5.4 / §5.5: a function declared to return a reference returns a
/// borrow of something its caller lent, so a call that hands back an owned
/// value is a temporary dropped at the return; and a `let` cannot keep a
/// reference to an owned call result, since there is no temporary lifetime
/// extension (B-2026-10-22-29).
#[test]
fn a_reference_to_a_returned_temporary_is_refused() {
    let dangling = "returned borrow does not originate from a `ref` parameter";
    rejected(
        "ref-return-call",
        "fn make() -> String { String.from(\"temp\") }\nfn bad() -> ref String { make() }\nfn main() { println(bad()); }\n",
        dangling,
    );
    rejected(
        "ref-return-ctor",
        "fn bad(s: ref String) -> ref String { String.from(\"temp\") }\nfn main() { let x = String.from(\"a\"); println(bad(x)); }\n",
        dangling,
    );
    rejected(
        "ref-return-early",
        "fn bad(s: ref String, n: i64) -> ref String { if n > 0 { return String.from(\"t\"); } s }\nfn main() { let x = String.from(\"a\"); println(bad(x, 1)); }\n",
        dangling,
    );
    rejected(
        "ref-let-call",
        "fn make() -> String { String.from(\"temp\") }\nfn main() { let r: ref String = make(); println(r); }\n",
        "a reference to a temporary outlives its statement (core-semantics.md §5.5)",
    );
    accepted_and_runs(
        "ref-return-lent",
        "struct P { name: String }\n\
         impl P { fn name(ref self) -> ref String { ref self.name } }\n\
         fn first(v: ref Vec[String]) -> ref String { ref v[0] }\n\
         fn pick(a: ref String, b: ref String, n: i64) -> ref String { if n > 0 { a } else { b } }\n\
         fn echo(s: ref String) -> ref String { s }\n\
         fn via(s: ref String) -> ref String { echo(s) }\n\
         fn get(v: ref Vec[String]) -> ref String { v.get(0).unwrap() }\n\
         fn main() {\n    let v = [String.from(\"a\")];\n    let b = String.from(\"b\");\n    \
         let p = P { name: String.from(\"n\") };\n    println(first(v));\n    println(pick(v[0], b, 0));\n    \
         println(p.name());\n    println(via(b));\n    println(get(v));\n}\n",
        "a\nb\nn\nb\na\n",
    );
}

/// A `Vec` receiver finds every `Slice` method (docs/library/collections.md),
/// `to_vec` among them (B-2026-10-23-5).
#[test]
fn vec_to_vec_copies_into_a_new_vec() {
    accepted_and_runs(
        "vec-to-vec",
        "fn main() {\n    let v = [3, 1, 2];\n    let mut w = v.to_vec();\n    w.push(4);\n    \
         println(f\"{v.len()} {w.len()} {w[3]}\");\n    \
         let s = [String.from(\"a\"), String.from(\"b\")];\n    let t = s.to_vec();\n    println(t[1]);\n}\n",
        "3 4 4\nb\n",
    );
}

/// A bare name in a pattern that the scrutinee's enum declares is that
/// variant, even when the name means something else in scope (the prelude's
/// `Fn`) or nothing (a library enum's `NotFound`); a capitalised name the
/// enum does not declare is an error, not a catch-all binding
/// (B-2026-10-22-31).
#[test]
fn a_bare_variant_pattern_is_the_scrutinees_variant() {
    accepted_and_runs(
        "bare-variant-prelude-name",
        "enum Tok { Fn, Let, Plus }\n\
         fn kind(t: Tok) -> i64 {\n    match t { Fn => 1, Let => 2, _ => 3 }\n}\n\
         fn by_ref(t: ref Tok) -> i64 {\n    match t { Fn => 1, _ => 3 }\n}\n\
         fn main() {\n    \
         println(f\"{kind(Tok.Fn)} {kind(Tok.Let)} {kind(Tok.Plus)} {by_ref(Tok.Plus)}\");\n}\n",
        "1 2 3 3\n",
    );
    accepted_and_runs(
        "bare-variant-library-enum",
        "fn main() {\n    let e = IoError.PermissionDenied;\n    \
         let n = match e { NotFound => 1, PermissionDenied => 2, _ => 3 };\n    \
         match e { NotFound => println(\"nf\"), _ => println(f\"{n}\") }\n}\n",
        "2\n",
    );
    let (dir, path) = fixture(
        "bare-variant-unknown",
        "enum Color { Red, Green }\n\
         fn f(c: Color) -> i64 {\n    match c { Red => 1, Purple => 2 }\n}\n\
         fn main() {\n    println(f(Color.Green));\n}\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("`Color` has no variant `Purple`"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A variant is built with its qualified name; only `Some`, `None`, `Ok` and
/// `Err` are written bare, and a pattern may leave the enum name out
/// (design.md § Enums, B-2026-10-22-46).
#[test]
fn a_bare_variant_does_not_build_a_value() {
    rejected_then_fixed_by(
        "bare-variant-unit",
        "enum E { A(String), B }\n\
         fn main() {\n    let p = B;\n    match p { E.A(s) => println(s), B => println(\"b\") }\n}\n",
        "write `E.B`",
        "let p = E.B;",
    );
    rejected_then_fixed_by(
        "bare-variant-tuple",
        "enum E { A(String), B }\n\
         fn main() {\n    let p = A(\"a\");\n    match p { A(s) => println(s), B => println(\"b\") }\n}\n",
        "write `E.A`",
        "let p = E.A(\"a\");",
    );
    accepted_and_runs(
        "bare-prelude-variants",
        "fn main() {\n    let a: Option[i64] = None;\n    let b = Some(2);\n    \
         let c: Result[i64, String] = Ok(3);\n    \
         println(f\"{a.unwrap_or(1)} {b.unwrap_or(0)} {c.unwrap_or(0)}\");\n}\n",
        "1 2 3\n",
    );
}

/// The total-order 16-bit float wrappers print as the float they hold, as
/// `F64` and `F32` do (B-2026-10-22-32).
#[test]
fn f16_and_bf16_print_as_their_float() {
    accepted_and_runs(
        "f16-bf16-display",
        "fn main() {\n    let a = F16.from(2.5);\n    let b = Bf16.from(3.25);\n    \
         println(a);\n    println(f\"{b} {F64.from(1.5)}\");\n}\n",
        "2.5\n3.25 1.5\n",
    );
}

/// `sum`, `product`, `max` and `min` are iterator methods: a `Vec` reaches
/// them through `.iter()`, which `karac fix` writes; and `f64.from` widens an
/// `f32` (B-2026-10-22-34).
#[test]
fn collection_terminals_go_through_iter_and_f64_from_widens() {
    rejected_then_fixed_by(
        "vec-direct-max",
        "fn main() {\n    let xs: Vec[i64] = [1, 5, 3];\n    \
         println(f\"{xs.max().unwrap()} {xs.sum()}\");\n}\n",
        "write `.iter().max()`",
        "xs.iter().max().unwrap()} {xs.iter().sum()}",
    );
    accepted_and_runs(
        "f64-from-f32",
        "fn main() {\n    let x: f32 = 1.5;\n    let y: f64 = f64.from(x);\n    \
         let z: f64 = x.into();\n    println(f\"{y} {z}\");\n}\n",
        "1.5 1.5\n",
    );
}

/// A `let mut` of a `Copy` `for` element copies it (core-semantics.md §3),
/// so the copy can be passed `mut`; the collection is not written
/// (B-2026-10-23-11).
#[test]
fn a_let_mut_copy_of_a_for_element_is_its_own() {
    accepted_and_runs(
        "let-mut-copy-of-for-elem",
        "fn bump(x: mut ref i64) {\n    *x += 1;\n}\n\
         fn main() {\n    for v in vec![1, 2] {\n        let mut w = v;\n        bump(mut w);\n        \
         println(f\"{w}\");\n    }\n    let xs = vec![10];\n    for v in xs.iter() {\n        \
         let mut w = v;\n        bump(mut w);\n        println(f\"{w} {xs[0]}\");\n    }\n}\n",
        "2\n3\n11 10\n",
    );
}

/// B-2026-10-22-33: programs v2 has no meaning for are refused at check,
/// not later by the MIR builder or at run time: a pipe, the legacy
/// encoding and statistics namespaces, an impl that omits a required
/// method, `dbg` of a function value, and a run-time `Type` parameter.
#[test]
fn check_refuses_what_v2_has_no_meaning_for() {
    rejected(
        "pipe",
        "fn double(x: i64) -> i64 {\n    x * 2\n}\nfn main() {\n    let y = 21 |> double;\n    \
         println(f\"{y}\");\n}\n",
        "there is no pipe operator `|>`",
    );
    rejected(
        "base64-namespace",
        "fn main() {\n    println(Base64.encode(\"hi\".bytes()));\n}\n",
        "write `base64_encode(bytes)`",
    );
    rejected(
        "url-namespace",
        "fn main() {\n    println(Url.encode(\"a b\"));\n}\n",
        "write `percent_encode(text.bytes())`",
    );
    rejected(
        "stats-namespace",
        "fn main() {\n    let xs = vec![1.0, 2.0];\n    println(f\"{Stats.mean(xs.as_slice())}\");\n}\n",
        "statistics come with the `kara-data` package",
    );
    accepted_and_runs(
        "own-hex-namespace",
        "struct Hex {}\nimpl Hex {\n    fn encode(n: i64) -> i64 {\n        n + 1\n    }\n}\n\
         fn main() {\n    println(f\"{Hex.encode(1)}\");\n}\n",
        "2\n",
    );
    rejected(
        "index-without-index",
        "struct P { x: i64 }\nimpl Index for P { type Output = i64; }\nfn main() { }\n",
        "impl of trait 'Index' for 'P' is missing method 'index'",
    );
    rejected(
        "trait-without-method",
        "trait Shape {\n    fn area(self) -> i64;\n    fn twice(self) -> i64 {\n        self.area() * 2\n    }\n}\n\
         struct P { x: i64 }\nimpl Shape for P { }\nfn main() { }\n",
        "impl of trait 'Shape' for 'P' is missing method 'area'",
    );
    rejected(
        "dbg-of-fn",
        "fn add(a: i64) -> i64 {\n    a\n}\nfn main() {\n    let g = add;\n    dbg(g);\n}\n",
        "is a function value, which has no Debug",
    );
    rejected(
        "type-param",
        "fn describe(t: Type) -> i64 {\n    0\n}\nfn main() { }\n",
        "E_TYPE_VALUE_AT_RUNTIME",
    );
}

/// A tuple literal owns what it is built from, so passing one to a borrowed
/// parameter still moves its elements in (core-semantics.md §3), as the MIR
/// move check finds; `karac fix` clones at the move (B-2026-10-22-33).
#[test]
fn a_tuple_literal_argument_moves_its_elements() {
    rejected_then_fixed(
        "tuple-arg-move",
        "fn look(k: (String, String)) -> i64 {\n    k.0.len()\n}\nfn main() {\n    \
         let c: String = f\"cc\";\n    let d: String = f\"dd\";\n    let n = look((c, d));\n    \
         println(f\"{n} {c.len()}\");\n}\n",
        "value 'c' moved here, used again here",
    );
    rejected(
        "tuple-key-move",
        "fn main() {\n    let mut m: Map[(String, String), i64] = Map.new();\n    \
         m.insert((f\"a\", f\"b\"), 0);\n    let a: String = f\"a\";\n    let b: String = f\"b\";\n    \
         println(f\"{m.get((a, b)).is_some()} {a.len()}\");\n}\n",
        "value 'a' moved here, used again here",
    );
}

/// Under D5 a bare `a: T` borrows, so `a + b` on a generic `T: Add` names
/// the two fixes, `own T` and a `Copy` bound, both of which then run
/// (B-2026-10-22-40).
#[test]
fn a_generic_operator_on_borrowed_operands_names_the_fix() {
    rejected(
        "generic-add-borrowed",
        "fn g[T: Add](a: T, b: T) -> T {\n    a + b\n}\nfn main() {\n    println(f\"{g(1, 2)}\");\n}\n",
        "declare it `own T`, or bound `T: Copy` as well",
    );
    accepted_and_runs(
        "generic-add-own-or-copy",
        "fn h[T: Add + Copy](a: T, b: T) -> T {\n    a + b\n}\n\
         fn k[T: Add](a: own T, b: own T) -> T {\n    a + b\n}\n\
         fn main() {\n    println(f\"{h(1, 2)} {k(3, 4)}\");\n}\n",
        "3 7\n",
    );
}

/// A `let` of a reference binding copies the reference (core-semantics.md
/// §4.6): `let m = r` over a bare parameter is accepted, and `let mut b =
/// self` over a borrowed receiver is a `ref Self`, so a mutating call and a
/// part move through it are refused (B-2026-10-22-33).
#[test]
fn a_let_of_a_borrowed_receiver_is_a_ref() {
    accepted(
        "let-copies-a-ref",
        "struct R { name: String }\nfn g(r: R) -> i64 {\n    let m = r;\n    \
         m.name.len() + r.name.len()\n}\nfn main() {\n    let r = R { name: \"abc\".to_string() };\n    \
         println(g(r));\n}\n",
    );
    rejected(
        "mut-through-copied-self",
        "struct Bag { xs: Vec[i64] }\nimpl Bag {\n    fn arrange(mut ref self) {\n        \
         self.xs.reverse();\n    }\n    fn inner(self) -> i64 {\n        let mut b = self;\n        \
         b.arrange();\n        b.xs.len()\n    }\n}\nfn main() {\n    let bag = Bag { xs: [1, 2] };\n    \
         println(bag.inner());\n}\n",
        "`b` copies the receiver `ref self`",
    );
    rejected(
        "move-through-copied-self",
        "struct Bag { xs: Vec[i64] }\nimpl Bag {\n    fn inner(self) -> Vec[i64] {\n        \
         let b = self;\n        b.xs\n    }\n}\nfn main() {\n    let bag = Bag { xs: [1, 2] };\n    \
         println(bag.inner().len());\n}\n",
        "cannot move a non-`Copy` value out of a borrowed place",
    );
}

/// `flatten` declares `where Self.Item: Iterator`, so `v.iter().flatten()`
/// over a `Vec[Vec[T]]` is refused at the call wherever the result goes: a
/// `for` loop, a binding walked later, or a chain. Pre-fix only a chained
/// method call was refused (B-2026-10-22-36).
#[test]
fn flatten_over_collections_is_refused_at_the_call() {
    rejected(
        "flatten-for",
        "fn main() {\n    let v: Vec[Vec[i64]] = [[1, 2], [3]];\n    \
         for x in v.iter().flatten() {\n        println(x);\n    }\n}\n",
        "write `.flat_map(|v| v.iter())` in place of `.flatten()`",
    );
    rejected(
        "flatten-let",
        "fn main() {\n    let v: Vec[Vec[i64]] = [[1, 2], [3]];\n    \
         let it = v.iter().flatten();\n    for x in it {\n        println(x);\n    }\n}\n",
        "write `.flat_map(|v| v.iter())` in place of `.flatten()`",
    );
    rejected(
        "flatten-owned",
        "fn main() {\n    let v: Vec[Vec[i64]] = [[1, 2], [3]];\n    \
         for x in v.into_iter().flatten() {\n        println(x);\n    }\n}\n",
        "write `.flat_map(|v| v.into_iter())` in place of `.flatten()`",
    );
    // A chain says it once, with nothing cascading from the adapters on it.
    let (dir, path) = fixture(
        "flatten-chain",
        "fn main() {\n    let v: Vec[Vec[i64]] = [[1, 2], [3]];\n    \
         let s: Vec[i64] = v.iter().flatten().map(|x| *x).collect();\n    \
         println(f\"{s.len()}\");\n}\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("in place of `.flatten()`"),
        "flatten-chain: {err}"
    );
    assert!(
        err.contains("1 error(s) found"),
        "flatten-chain: one error: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    accepted_and_runs(
        "flatten-of-iterators",
        "fn main() {\n    let v: Vec[Vec[i64]] = [[1, 2], [3]];\n    \
         let n = v.iter().map(|r| r.iter()).flatten().count();\n    \
         let t: i64 = v.iter().flat_map(|r| r.iter()).map(|x| *x).sum();\n    \
         println(f\"{n} {t}\");\n}\n",
        "3 6\n",
    );
}

/// design.md § Type inference: an unsolved variable is an error at the end
/// of the body. `or_else`'s new error type, fixed by nothing when the result
/// is only unwrapped, is refused by check rather than by the MIR builder;
/// an annotated binding fixes it and runs (B-2026-10-22-36).
#[test]
fn a_call_result_no_use_fixes_is_refused() {
    rejected(
        "or-else-unwrapped",
        "fn main() {\n    let r: Result[i64, i64] = Err(9);\n    \
         println(r.or_else(|x: i64| Ok(x * 2)).unwrap_or(0));\n}\n",
        "cannot infer type parameter 'E' of `Result[i64, E]`, this call's result",
    );
    accepted_and_runs(
        "or-else-annotated",
        "fn main() {\n    let r: Result[i64, i64] = Err(9);\n    \
         let q: Result[i64, i64] = r.or_else(|x: i64| Ok(x * 2));\n    \
         println(q.unwrap_or(0));\n}\n",
        "18\n",
    );
}

/// A binding bound out of a bare parameter is a borrow of it, so giving it
/// away names the parameter's `own` fix, which `karac fix` writes: a
/// catch-all arm, a destructuring `let`, a payload returned from an arm
/// (B-2026-10-20-16).
#[test]
fn giving_away_a_part_of_a_bare_parameter_names_its_own_fix() {
    rejected_then_fixed_by(
        "catch-all-arm",
        "enum E { A, B(i64) }\nfn pass(r: Result[i64, E]) -> Result[i64, E] {\n    \
         match r {\n        Err(E.A) => Ok(0),\n        other => other,\n    }\n}\n\
         fn main() {\n    println(f\"{pass(Ok(1)).is_ok()}\");\n}\n",
        "cannot give away `other`, bound out of parameter `r`",
        "r: own Result[i64, E]",
    );
    rejected_then_fixed_by(
        "let-destructure",
        "struct R { s: String }\nstruct W { r: R, n: i64 }\n\
         fn take(w: W) -> R {\n    let W { r, n } = w;\n    r\n}\n\
         fn main() {\n    let w = W { r: R { s: f\"x\" }, n: 1 };\n    println(take(w).s);\n}\n",
        "cannot give away `r`, bound out of parameter `w`",
        "w: own W",
    );
    rejected_then_fixed_by(
        "payload-return",
        "struct R { s: String }\nenum Box2 { Empty, Full(R) }\nfn get(b: Box2) -> R {\n    \
         match b {\n        Box2.Full(r) => {\n            return r;\n        }\n        \
         Box2.Empty => R { s: f\"e\" },\n    }\n}\n\
         fn main() {\n    println(get(Box2.Full(R { s: f\"x\" })).s);\n}\n",
        "cannot give away `r`, bound out of parameter `b`",
        "b: own Box2",
    );
}

/// iterators.md: a chain can be passed down to a generic function. A
/// method on `it: I` under `I: Iterator[Item = i64]` instantiates its own
/// type parameters and reads `I.Item` as `i64`, so `fold`, `filter` and
/// `map` closures check, through an adapter chain too (B-2026-10-21-60).
#[test]
fn iterator_methods_on_a_generic_iterator_check() {
    accepted_and_runs(
        "generic-fold",
        "fn total[I: Iterator[Item = i64]](it: own I) -> i64 {\n    it.fold(0, |a, x| a + x)\n}\n\
         fn main() {\n    let v: Vec[i64] = [1, 2, 3];\n    println(f\"{total(v.into_iter())}\");\n}\n",
        "6\n",
    );
    accepted_and_runs(
        "generic-chain",
        "fn tot[I: Iterator[Item = i64]](it: own I) -> i64 {\n    \
         it.filter(|x| *x > 1).map(|x| x * 2).fold(0, |a, b| a + b)\n}\n\
         fn main() {\n    let v: Vec[i64] = [1, 2, 3];\n    println(f\"{tot(v.into_iter())}\");\n}\n",
        "10\n",
    );
    accepted(
        "generic-map-count",
        "fn tot[I: Iterator[Item = i64]](it: own I) -> i64 {\n    it.map(|x| x + 1).count()\n}\n\
         fn main() {\n    let v: Vec[i64] = [1, 2, 3];\n    println(f\"{tot(v.into_iter())}\");\n}\n",
    );
}

#[test]
fn a_for_loop_over_a_value_that_does_not_iterate_is_refused() {
    // B-2026-10-21-76: a `for` iterates a collection, an iterator or an
    // `IntoIterator`; anything else was bound whole to the pattern.
    rejected(
        "for-over-struct",
        "struct S { a: i64 }\nfn main() {\n    let s = S { a: 1 };\n    for x in s { println(f\"{x.a}\"); }\n}\n",
        "type 'S' cannot be iterated by a `for` loop",
    );
    rejected(
        "for-over-int",
        "fn main() {\n    let n = 5;\n    for x in n { println(f\"{x}\"); }\n}\n",
        "type 'i64' cannot be iterated by a `for` loop",
    );
    accepted_and_runs(
        "for-over-iterators",
        "struct Counter { n: i64 }\nimpl Iterator for Counter {\n    type Item = i64;\n    \
         fn next(mut ref self) -> Option[i64] { if self.n == 0 { None } else { self.n -= 1; Some(self.n) } }\n}\n\
         fn main() {\n    let c = Counter { n: 2 };\n    for x in c { println(f\"{x}\"); }\n    \
         let v: StringSlice = \"hé\";\n    for ch in v { let d: char = ch; println(f\"{d}\"); }\n    \
         for i in 0..2 { println(f\"{i}\"); }\n}\n",
        "1\n0\nh\né\n0\n1\n",
    );
}

#[test]
fn a_spawned_task_returns_only_a_cross_task_value() {
    // B-2026-10-21-71: concurrency.md declares `spawn[T: CrossTask]`, so the
    // result direction is checked like the capture direction.
    rejected(
        "taskgroup-spawn-shared-result",
        "shared struct Node { v: i64 }\nfn main() {\n    let mut g = TaskGroup.new();\n    \
         let h = g.spawn(|| Node { v: 1 });\n    println(h.join().v);\n}\n",
        "the result of the task passed to `TaskGroup.spawn` (type `Node`) cannot cross a task boundary",
    );
    rejected(
        "taskgroup-spawn-shared-in-vec",
        "shared struct Node { v: i64 }\nfn main() {\n    let mut g = TaskGroup.new();\n    \
         let h = g.spawn(|| [Node { v: 1 }]);\n    println(h.join().len());\n}\n",
        "E_NOT_CROSS_TASK",
    );
    accepted_and_runs(
        "taskgroup-spawn-plain-result",
        "struct P { v: i64 }\nfn main() {\n    let mut g = TaskGroup.new();\n    \
         let h = g.spawn(|| P { v: 1 });\n    let k = g.spawn(|| [1, 2, 3]);\n    \
         println(h.join().v);\n    println(k.join().len());\n}\n",
        "1\n3\n",
    );
}

#[test]
fn a_mismatch_names_the_string_view_str() {
    // B-2026-10-21-79: the view is `Str` in the source and the pages;
    // `StringSlice` is the checker's own name for it.
    rejected(
        "command-args-vec-string",
        "fn main() {\n    let a = [\"-n\", \"hi\"];\n    \
         let o = Command.new(\"echo\").args(a).output().unwrap();\n}\n",
        "expected 'Slice[Str]', found 'Vec[String]': each `String` owns its text",
    );
    rejected(
        "str-view-in-a-tuple",
        "fn main() {\n    let v: (Str, i64) = (5, 1);\n}\n",
        "expected '(Str, i64)', found '(i64, i64)'",
    );
}

#[test]
fn into_on_a_unit_variant_literal_converts() {
    // B-2026-10-21-80: `A.Y.into()` parses as a call of the path `A.Y.into`
    // and converts as `(A.Y).into()` does.
    let src = "enum A { X, Y }\nenum B { P, Q }\n\
               impl From[A] for B { fn from(a: own A) -> B { match a { A.X => B.P, A.Y => B.Q } } }\n\
               fn main() {\n    let b: B = A.Y.into();\n    \
               match b { B.P => println(\"P\"), B.Q => println(\"Q\") }\n}\n";
    accepted("unit-variant-literal-into", src);
    accepted_and_runs(
        "parenthesised-variant-into",
        &src.replace("A.Y.into()", "(A.Y).into()"),
        "Q\n",
    );
    rejected(
        "unit-variant-literal-into-no-impl",
        &src.replace(
            "let b: B = A.Y.into();",
            "let b: B = A.Y.into();\n    let c: i64 = A.X.into();",
        ),
        "no `impl From[A] for i64` is in scope; cannot `.into()`",
    );
}

#[test]
fn a_char_casts_to_every_integer_of_32_bits_or_more() {
    // B-2026-10-24-7: the Unicode scalar value fits `i128` and `u128` as it
    // fits `i64`; a narrow target is still refused with its own error.
    accepted_and_runs(
        "char-as-wide-ints",
        "fn main() {\n    let c = 'A';\n    let a = c as i64;\n    let b = c as u64;\n    \
         let d = c as i128;\n    let e = '\\u{10FFFF}' as u128;\n    \
         println(f\"{a} {b} {d} {e}\");\n}\n",
        "65 65 65 1114111\n",
    );
    rejected(
        "char-as-u16",
        "fn main() {\n    let n = 'A' as u16;\n    println(f\"{n}\");\n}\n",
        "E_CHAR_AS_NARROW_INT",
    );
}

#[test]
fn a_program_function_shadows_a_builtin_of_the_same_name() {
    // B-2026-10-23-10: the program's own `todo`, `panic` and `ref_eq` are
    // called, not the prelude builtins they shadow.
    accepted_and_runs(
        "own-todo-panic-ref-eq",
        "fn todo(a: i64, b: i64) -> i64 {\n    a + b\n}\n\
         fn panic(s: Str) -> i64 {\n    7\n}\n\
         fn ref_eq(a: i64, b: i64, c: i64) -> bool {\n    a == b and b == c\n}\n\
         fn main() {\n    println(f\"{todo(1, 2)} {panic(\"x\")} {ref_eq(1, 1, 1)}\");\n}\n",
        "3 7 true\n",
    );
    rejected(
        "builtin-todo-still-checked",
        "fn main() {\n    let x: i64 = todo(1, 2);\n    println(f\"{x}\");\n}\n",
        "todo() takes 0 or 1 argument(s), found 2",
    );
}

#[test]
fn a_numeric_type_inside_a_compound_keeps_its_sign() {
    // B-2026-10-23-12: a generic argument or a tuple element holds its value
    // in place, so `Map[i64, i64]`, `Option[i64]` and `(i64, i64)` do not
    // stand for their `u64` / `i8` versions; literals still take the slot's
    // type.
    rejected(
        "defaulted-map-into-a-u64-field",
        "struct S { m: Map[i64, u64] }\nfn main() {\n    let mut m2 = Map.new();\n    \
         let _ = m2.insert(5, 9);\n    let s = S { m: m2 };\n    println(s.m.len());\n}\n",
        "expected 'Map[i64, u64]', found 'Map[i64, i64]'",
    );
    rejected(
        "option-i64-into-option-u64",
        "fn g(x: own Option[i64]) -> Option[u64] {\n    x\n}\nfn main() {\n    \
         println(g(Some(1)).is_some());\n}\n",
        "expected 'Option[u64]', found 'Option[i64]'",
    );
    rejected(
        "tuple-value-into-a-narrower-tuple",
        "fn main() {\n    let a = (1, 2);\n    let b: (i64, i8) = a;\n    println(f\"{b.1}\");\n}\n",
        "expected '(i64, i8)', found '(i64, i64)'",
    );
    accepted_and_runs(
        "literals-take-the-slot-type",
        "fn main() {\n    let o: Option[u64] = Some(5);\n    let t: (i64, u8) = (1, 2);\n    \
         let v: Vec[(u8, i64)] = [(1, 2), (3, 4)];\n    let mut m: Map[i64, u64] = Map.new();\n    \
         m.insert(5, 9);\n    println(f\"{o.unwrap()} {t.1} {v.len()} {m.len()}\");\n}\n",
        "5 2 2 1\n",
    );
}

#[test]
fn a_list_literal_into_a_container_element_follows_the_element_type() {
    // B-2026-10-19-32: `push`, `push_back`, `insert` and a set's `insert`
    // check a list literal against the element type, as a `let` annotation
    // does.
    accepted_and_runs(
        "list-literal-element-args",
        "fn main() {\n    let mut best: Vec[Array[i64, 2]] = Vec.new();\n    best.push([0, 1]);\n    \
         let mut d: VecDeque[Array[i64, 2]] = VecDeque.new();\n    d.push_back([2, 3]);\n    \
         let mut nv: Vec[Vec[u64]] = Vec.new();\n    nv.push([18446744073709551615]);\n    \
         nv.insert(0, vec![3]);\n    let mut s: Set[Vec[u8]] = Set.new();\n    s.insert([250, 1]);\n    \
         println(f\"{best[0][1]} {d[0][1]} {nv} {s.len()}\");\n}\n",
        "1 3 [[3], [18446744073709551615]] 1\n",
    );
    rejected(
        "list-literal-element-out-of-range",
        "fn main() {\n    let mut v: Vec[Vec[u8]] = Vec.new();\n    v.push([300]);\n    println(v.len());\n}\n",
        "integer literal 300 out of range for 'u8'",
    );
}

#[test]
fn an_integer_literal_in_a_tuple_literal_must_fit_its_slot() {
    // B-2026-10-24-17: `(1, 300)` for an `(i64, u8)` is refused as
    // `let x: u8 = 300` is, nested and pushed tuples too; an in-range
    // literal still takes the slot's type.
    rejected(
        "tuple-literal-out-of-range",
        "fn main() {\n    let t: (i64, u8) = (1, 300);\n    println(f\"{t.1}\");\n}\n",
        "integer literal 300 out of range for 'u8'",
    );
    rejected(
        "nested-tuple-literal-out-of-range",
        "fn main() {\n    let n: ((i64, i8), i64) = ((1, 200), 2);\n    println(n.1);\n}\n",
        "integer literal 200 out of range for 'i8'",
    );
    rejected(
        "pushed-tuple-literal-out-of-range",
        "fn main() {\n    let mut v: Vec[(u8, i64)] = Vec.new();\n    v.push((300, 1));\n    println(v.len());\n}\n",
        "integer literal 300 out of range for 'u8'",
    );
    accepted_and_runs(
        "tuple-literals-in-range",
        "fn main() {\n    let t: (i64, u8) = (1, 255);\n    let mut v: Vec[(u8, i64)] = Vec.new();\n    \
         v.push((200, -1));\n    println(f\"{t.1} {v[0].0}\");\n}\n",
        "255 200\n",
    );
}

#[test]
fn collect_after_an_item_keeping_adaptor_on_a_generic_iterator_checks() {
    // B-2026-10-24-16: `IterFilter` / `IterTake` name their items `I.Item`,
    // which the enclosing `I: Iterator[Item = i64]` fixes.
    accepted_and_runs(
        "filter-take-collect-under-a-bound",
        "fn ev[I: Iterator[Item = i64]](it: own I) -> Vec[i64] {\n    it.filter(|x| x % 2 == 0).collect()\n}\n\
         fn tk[I](it: own I) -> i64 where I: Iterator, I.Item = i64 {\n    \
         let r: Vec[i64] = it.take(2).collect();\n    r.len()\n}\n\
         fn main() {\n    let v: Vec[i64] = [1, 2, 3, 4];\n    println(ev(v.into_iter()).len());\n    \
         let w: Vec[i64] = [1, 2, 3, 4];\n    println(tk(w.into_iter()));\n}\n",
        "2\n2\n",
    );
}

#[test]
fn a_list_literal_in_a_tuple_literal_follows_its_slot() {
    // B-2026-10-23-7: `vec![1, 2]` for a `Vec[u8]` tuple element builds a
    // `Vec[u8]`, at a `let` and in a pushed tuple.
    accepted_and_runs(
        "vec-literal-in-a-tuple-slot",
        "fn main() {\n    let b: (String, Vec[u8]) = (\"x\".to_string(), vec![1, 2]);\n    \
         let d: (Vec[u8], i64) = (vec![1], 2);\n    let mut v: Vec[(String, Vec[u8])] = Vec.new();\n    \
         v.push((\"y\".to_string(), vec![0xef, 0xbb, 0xbf]));\n    \
         println(f\"{b.1.len()} {d.1} {v[0].1}\");\n}\n",
        "2 2 [239, 187, 191]\n",
    );
    rejected(
        "vec-literal-in-a-tuple-slot-out-of-range",
        "fn main() {\n    let b: (String, Vec[u8]) = (\"x\".to_string(), vec![300]);\n    println(b.1.len());\n}\n",
        "integer literal 300 out of range for 'u8'",
    );
}

#[test]
fn a_list_literal_reads_a_ref_to_a_copy_element_as_the_value() {
    // B-2026-10-22-6, core-semantics §5.10: `[p]` with `p: ref i64` builds a
    // `Vec[i64]` where one is expected, as `[1, p]` already did.
    accepted_and_runs(
        "list-literal-of-a-copy-ref",
        "fn main() {\n    let v: Vec[i64] = [3, 4];\n    for p in v.iter() {\n        \
         let u: Vec[i64] = [p];\n        let a: Vec[i64] = [p, 1];\n        \
         let mut st: Vec[i64] = Vec.new();\n        st = [p];\n        println(f\"{u} {a} {st}\");\n    }\n}\n",
        "[3] [3, 1] [3]\n[4] [4, 1] [4]\n",
    );
}

#[test]
fn a_str_lookup_leaves_an_unannotated_key_open() {
    // B-2026-10-21-3: a `Str` lookup fits a `String` key, so it does not fix
    // an unannotated `Map.new()` / `Set.new()` key; the insert decides.
    accepted_and_runs(
        "str-lookup-then-string-insert",
        "fn main() {\n    let text = \"a b a\";\n    let mut m = Map.new();\n    \
         for w in text.split_whitespace() {\n        let n = m.get(w) ?? 0;\n        m[w.to_string()] = n + 1;\n    }\n    \
         let mut seen = Set.new();\n    for w in text.split_whitespace() {\n        \
         if not seen.contains(w) { seen.insert(w.to_string()); }\n    }\n    \
         println(f\"{m.len()} {m[\"a\"]} {seen.len()}\");\n}\n",
        "2 2 2\n",
    );
}

#[test]
fn a_block_arm_lending_among_owned_arms_has_no_unparseable_fix() {
    // B-2026-10-20-36: a block arm's copy goes on the values it yields, which
    // carry their own fixes; `.clone()` after the block does not parse.
    let src = "#[derive(Clone)]\nstruct NDef { bold: bool }\nfn main() {\n    \
               let mut nd: Map[String, NDef] = Map.new();\n    \
               nd.insert(\"\".to_string(), NDef { bold: false });\n    let parent = nd.get(\"\");\n    \
               let flag = true;\n    let mut n = match parent {\n        None => NDef { bold: false },\n        \
               Some(p) => {\n            if flag { nd.get(\"\").unwrap() } else { p }\n        }\n    };\n    \
               n.bold = true;\n    println(f\"{n.bold}\");\n}\n";
    let (dir, path) = fixture("block-arm-lends", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must reject: {err}");
    assert!(
        err.contains("Write `.clone()` on each value this arm yields"),
        "{err}"
    );
    let out = karac().arg("fix").arg(&path).output().unwrap();
    let fix_err = String::from_utf8_lossy(&out.stderr);
    assert!(!fix_err.contains("skipped a fix"), "{fix_err}");
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        fixed.contains("nd.get(\"\").unwrap().clone() } else { p.clone() }"),
        "{fixed}"
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_mut_argument_through_a_binding_into_a_shared_ref_is_refused() {
    // B-2026-10-24-21: `buf` binds a place inside `c`, a `ref` parameter, so
    // passing it `mut` writes through the `ref` (§4.6, §5.9), as `buf.push`
    // does; the fix makes the parameter `mut ref`.
    for (tag, arm) in [
        ("mut-arg-via-match-binding", "add(mut buf, 7)"),
        ("push-via-match-binding", "buf.push(7)"),
    ] {
        let src = format!(
            "enum S {{ B(Vec[u8]), N }}\nstruct C {{ s: S }}\n\
             fn add(out: mut ref Vec[u8], x: u8) {{ out.push(x); }}\n\
             fn put(c: ref C) {{\n    match c.s {{\n        S.B(buf) => {arm},\n        S.N => {{}}\n    }}\n}}\n\
             fn main() {{\n    let mut c = C {{ s: S.B(Vec.new()) }};\n    put(c);\n}}\n"
        );
        rejected_then_fixed_in(tag, &src, "bound from `c.s`", "c: mut ref C", 2);
    }
}

#[test]
fn a_mut_argument_through_a_binding_into_an_owned_place_is_accepted() {
    // B-2026-10-24-21: the same binding into a `mut ref` place writes it,
    // and a `Copy` binding is a copy of its own.
    accepted_and_runs(
        "mut-arg-via-owned-match-binding",
        "enum S { B(Vec[u8]), N(u8) }\nstruct C { s: S }\n\
         fn add(out: mut ref Vec[u8], x: u8) { out.push(x); }\n\
         fn bump(n: mut ref u8) { *n = *n + 1; }\n\
         fn put(c: mut ref C) {\n    match c.s {\n        S.B(buf) => add(mut buf, 7),\n        S.N(_) => {}\n    }\n    \
         if let S.B(buf) = c.s { buf.push(8); }\n}\n\
         fn copy(c: ref C) -> u8 {\n    match c.s {\n        S.N(n) => { let mut m = n; bump(mut m); m }\n        S.B(_) => 0,\n    }\n}\n\
         fn main() {\n    let mut c = C { s: S.B(Vec.new()) };\n    put(mut c);\n    \
         let n = match c.s { S.B(b) => b.len(), S.N(_) => 0 };\n    \
         println(f\"{n} {copy(C { s: S.N(4) })}\");\n}\n",
        "2 5\n",
    );
}

#[test]
fn an_index_let_that_is_only_read_is_fixed_by_a_borrow() {
    // B-2026-10-24-18: `t` is only read, so the repair is `ref v[i]`, with no
    // derive and no copy, whether or not `Truck` has a `.clone()`.
    for (tag, derive) in [
        ("index-let-read-no-clone", ""),
        ("index-let-read-clone", "#[derive(Clone)]\n"),
    ] {
        let src = format!(
            "{derive}struct Truck {{\n    minutes: i64,\n    trips: Vec[String],\n}}\n\
             fn main() {{\n    let mut v: Vec[Truck] = Vec.new();\n    \
             v.push(Truck {{ minutes: 3, trips: vec![\"a\".to_string()] }});\n    \
             for i in 0..v.len() {{\n        let t = v[i];\n        \
             println(f\"{{t.minutes}} {{t.trips.len()}}\");\n        v[i].minutes = 0;\n    }}\n}}\n"
        );
        rejected_then_fixed_by(tag, &src, "the fix borrows it", "let t = ref v[i];");
        let (dir, path) = fixture(&format!("{tag}-once"), &src);
        karac().arg("fix").arg(&path).output().unwrap();
        let fixed = std::fs::read_to_string(&path).unwrap();
        assert!(
            !fixed.contains("derive(Clone)]\n#[derive"),
            "{tag}: {fixed}"
        );
        assert_eq!(
            fixed.matches("Clone").count(),
            derive.matches("Clone").count(),
            "{tag}: {fixed}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn an_index_let_held_across_a_write_keeps_the_copy_fix() {
    // B-2026-10-24-18: a borrow of `v[0]` would conflict with the write to
    // `v` while `t` is used (§5.6), and a moved `t` needs a value, so both
    // keep the copy; the message says the fix derives the `.clone()`.
    for (tag, body) in [
        (
            "index-let-held-across-push",
            "    let t = v[0];\n    v.push(Truck { minutes: 9, name: String.new() });\n    println(f\"{t.minutes}\");\n",
        ),
        (
            "index-let-moved",
            "    let t = v[0];\n    let u = t;\n    println(f\"{u.minutes}\");\n",
        ),
    ] {
        let src = format!(
            "struct Truck {{\n    minutes: i64,\n    name: String,\n}}\nfn main() {{\n    \
             let mut v: Vec[Truck] = Vec.new();\n    v.push(Truck {{ minutes: 3, name: String.new() }});\n{body}}}\n"
        );
        rejected_then_fixed_in(tag, &src, "the fix derives one", "let t = v[0].clone();", 2);
    }
}

#[test]
fn a_bare_parameter_pushed_into_a_collection_gets_the_own_fix() {
    // B-2026-10-24-19: `v.push(e)` gives away the borrow a bare parameter
    // holds, as `return e` does, so it gets the same message and `own` fix,
    // for a bare parameter and for a binding out of one, Clone or not.
    for (tag, derive, f) in [
        (
            "push-bare-param",
            "",
            "fn one(e: E) -> Vec[E] {\n    let mut v: Vec[E] = Vec.new();\n    v.push(e);\n    v\n}\n",
        ),
        (
            "push-bare-param-clone",
            "#[derive(Clone)]\n",
            "fn one(e: E) -> Vec[E] {\n    let mut v: Vec[E] = Vec.new();\n    v.push(e);\n    v\n}\n",
        ),
        (
            "push-part-of-bare-param",
            "",
            "fn one(o: Option[E]) -> Vec[E] {\n    let mut v: Vec[E] = Vec.new();\n    \
             match o {\n        Some(x) => v.push(x),\n        None => {}\n    }\n    v\n}\n",
        ),
        (
            "insert-bare-param",
            "",
            "fn one(e: E) -> Vec[E] {\n    let mut v: Vec[E] = Vec.new();\n    v.insert(0, e);\n    v\n}\n",
        ),
    ] {
        let arg = if tag.contains("part") { "Some(E { v: Vec.new() })" } else { "E { v: Vec.new() }" };
        let src = format!(
            "{derive}struct E {{\n    v: Vec[u8],\n}}\n{f}fn main() {{\n    \
             let n = one({arg}).len();\n    println(f\"{{n}}\");\n}}\n"
        );
        let edit = if tag.contains("part") { "o: own Option[E]" } else { "e: own E" };
        rejected_then_fixed_by(tag, &src, "a bare parameter borrows (core-semantics §4.1)", edit);
    }
}

#[test]
fn a_str_right_operand_of_string_concat_gets_the_to_string_fix() {
    // B-2026-10-24-24: `String + Str` is refused, as `Add` on `String` takes
    // a `String`; the error now carries the `.to_string()` the call-site
    // mismatch does, for a constant, an array element and a method result.
    for (tag, rhs) in [
        ("concat-str-const", "SUF"),
        ("concat-str-array-elem", "SUFS[1]"),
        ("concat-str-method", "t.trim()"),
    ] {
        let src = format!(
            "const SUF: Str = \"ing\";\nconst SUFS: Array[Str, 2] = [\"s\", \"ed\"];\n\
             fn main() {{\n    let w = \"walk\".to_string();\n    let t: Str = \" x \";\n    \
             let a = w + {rhs};\n    println(a);\n}}\n"
        );
        rejected_then_fixed_by(
            tag,
            &src,
            "Take an owned copy with `.to_string()`",
            &format!("w + {rhs}.to_string()"),
        );
    }
}

#[test]
fn an_array_of_non_scalar_elements_has_the_slice_reads() {
    // B-2026-10-24-23: an `Array[T, N]` has every `Slice[T]` method
    // (library/collections.md), so `contains` / `get` / `first` / `last`
    // work on an array of `Str`, `String` or tuples as on `.as_slice()`.
    accepted_and_runs(
        "array-non-scalar-slice-reads",
        "const WORDS: Array[Str, 3] = [\"a\", \"b\", \"c\"];\nfn main() {\n    \
         let ls: Array[String, 2] = [\"x\".to_string(), \"y\".to_string()];\n    \
         let ps: Array[(i64, Str), 2] = [(1, \"p\"), (2, \"q\")];\n    \
         println(f\"{WORDS.contains(\"b\")} {ls.contains(\"z\".to_string())} {ps.contains((2, \"q\"))}\");\n    \
         println(f\"{WORDS.first()} {WORDS.get(1)} {ls.last()} {WORDS.get(9)} {ps.first()}\");\n}\n",
        "true false true\nSome(a) Some(b) Some(y) None Some((1, p))\n",
    );
}

#[test]
fn a_for_over_a_const_array_borrows_nothing_under_self() {
    // B-2026-10-24-25: a `const` is not a place under `self`, so writing a
    // field of `self` inside `for w in WORDS` does not conflict with the
    // loop's borrow.
    accepted_and_runs(
        "for-over-const-writes-self",
        "const WORDS: Array[Str, 3] = [\"ab\", \"cd\", \"ef\"];\n\
         struct Lex { pos: i64, hits: i64 }\nimpl Lex {\n    \
         fn bump(mut ref self) { self.hits += 1; }\n    \
         fn scan(mut ref self) -> i64 {\n        for w in WORDS {\n            \
         if w.len() > 1 {\n                self.pos += 1;\n                self.bump();\n                \
         return self.pos;\n            }\n        }\n        -1\n    }\n}\n\
         fn main() {\n    let mut l = Lex { pos: 0, hits: 0 };\n    let p = l.scan();\n    \
         println(f\"{p} {l.hits}\");\n}\n",
        "1 1\n",
    );
}

#[test]
fn a_bare_parameter_in_a_tuple_or_entry_key_gets_the_own_fix() {
    // B-2026-10-24-30: a bare parameter inside a tuple literal pushed into an
    // owned slot, or as `Map.entry`'s owned key, gives away its borrow as
    // `v.push(key)` does, with the same message and `own` fix.
    for (tag, body, edit) in [
        (
            "tuple-elem-give-away",
            "fn note(log: mut ref Vec[(String, i64)], key: String, t: i64) {\n    log.push((key, t));\n}\n",
            "key: own String",
        ),
        (
            "entry-key-give-away",
            "fn note(m: mut ref Map[String, i64], key: String, t: i64) {\n    *m.entry(key).or_insert(0) += t;\n}\n",
            "key: own String",
        ),
    ] {
        let src = format!(
            "{body}fn main() {{\n    let mut log: Vec[(String, i64)] = Vec.new();\n    \
             let mut m: Map[String, i64] = Map.new();\n    let _ = m.len();\n    let _ = log.len();\n}}\n"
        );
        let src = if tag.starts_with("tuple") {
            src.replace("let _ = log.len();", "note(mut log, \"a\".to_string(), 1);\n    let _ = log.len();")
        } else {
            src.replace("let _ = m.len();", "note(mut m, \"a\".to_string(), 1);\n    let _ = m.len();")
        };
        rejected_then_fixed_by(tag, &src, "cannot give away parameter `key`", edit);
    }
}

#[test]
fn copied_on_an_option_names_cloned_and_fixes_to_it() {
    // B-2026-10-24-26: `.copied()` is the Rust spelling of `Option[ref T]`'s
    // `.cloned()`; the error names it and the fix writes it.
    let src = "fn main() {\n    let mut m: SortedMap[String, i64] = SortedMap.new();\n    \
               m.insert(\"a\".to_string(), 1);\n    let v: Vec[i64] = [4, 5].to_vec();\n    \
               let x = m.get(\"a\").copied().unwrap_or(0);\n    \
               let y = v.get(0).copied().unwrap_or(0);\n    println(f\"{x} {y}\");\n}\n";
    rejected_then_fixed_by(
        "option-copied",
        src,
        "copies its item out with `.cloned()`",
        "v.get(0).cloned().unwrap_or(0)",
    );
}

#[test]
fn a_copy_option_field_read_twice_by_value_stays_usable() {
    // B-2026-10-24-32: `p.a.unwrap_or(0)` on a `Copy` field (`a: Option[i64]`)
    // copies the receiver out, so a second read is not a use after move, for
    // an owned parameter, a `mut ref` parameter and a local alike.
    accepted_and_runs(
        "copy-option-field-twice",
        "struct P { a: Option[i64], s: Option[String] }\n\
         fn f(p: own P) -> i64 {\n    let x = p.a.unwrap_or(0);\n    x + p.a.unwrap_or(0)\n}\n\
         fn g(p: mut ref P) -> i64 {\n    let x = p.a.unwrap_or(0);\n    p.a = Some(x + 1);\n    \
         x + p.a.unwrap_or(0) + p.a.unwrap()\n}\n\
         fn main() {\n    let p = P { a: Some(3), s: None };\n    println(f\"{f(p)}\");\n    \
         let mut q = P { a: Some(2), s: None };\n    let y = q.a.unwrap_or(0);\n    \
         println(f\"{y} {q.a.unwrap_or(1)} {q.a.is_some()} {g(mut q)}\");\n}\n",
        "6\n2 2 true 8\n",
    );
    // A non-`Copy` field still moves out on the first by-value read.
    rejected(
        "non-copy-option-field-twice",
        "struct P { a: Option[i64], s: Option[String] }\n\
         fn f(p: own P) -> String {\n    let x = p.s.unwrap_or(\"d\".to_string());\n    \
         x + p.s.unwrap_or(\"e\".to_string())\n}\n\
         fn main() {\n    let p = P { a: Some(3), s: None };\n    println(f(p));\n}\n",
        "value 'p.s' moved here",
    );
}

#[test]
fn a_primitive_type_name_cannot_be_bound_as_a_value() {
    // B-2026-10-24-22, design.md § Primitive type names: `let i64 = 3;` is an
    // error, at every binding site, and the error names the binding rather
    // than a type path at a later use.
    for (tag, src, name) in [
        ("prim-let", "fn main() {\n    let i64 = 3;\n    println(f\"{i64 + 1}\");\n}\n", "i64"),
        (
            "prim-param",
            "fn twice(u8: i64) -> i64 {\n    u8 * 2\n}\nfn main() {\n    println(f\"{twice(4)}\");\n}\n",
            "u8",
        ),
        ("prim-for", "fn main() {\n    for char in [1, 2] {\n        println(char);\n    }\n}\n", "char"),
        (
            "prim-match",
            "fn main() {\n    let o: Option[i64] = Some(1);\n    match o {\n        \
             Some(usize) => println(usize),\n        None => {}\n    }\n}\n",
            "usize",
        ),
        (
            "prim-closure",
            "fn main() {\n    let f = |bool: i64| bool + 1;\n    println(f(1));\n}\n",
            "bool",
        ),
        (
            "prim-method-use",
            "fn show(s: ref String) {\n    println(s);\n}\nfn main() {\n    \
             let u8 = \"abc\".to_string();\n    show(u8);\n    println(f\"{u8.len()}\");\n}\n",
            "u8",
        ),
    ] {
        rejected(tag, src, &format!("`{name}` is a primitive type name and cannot be bound"));
    }
    // A field named after a primitive is not a binding.
    accepted_and_runs(
        "prim-field-name",
        "struct S { usize: i64 }\nfn main() {\n    let s = S { usize: 4 };\n    \
         println(f\"{s.usize} {i64.MAX > 0}\");\n}\n",
        "4 true\n",
    );
}

#[test]
fn a_string_literal_let_used_as_a_str_names_the_binding_and_fixes_it() {
    // B-2026-10-24-33: `let mut how = "plate";` is a `String` (design.md §
    // String literals), so a use as a `Str` is refused; the error says why at
    // the use and, when the binding only ever holds literals, `karac fix`
    // declares it `: Str`.
    let src = "struct Hit {\n    how: Str,\n}\nfn pick(tagged: bool) -> Hit {\n    \
               let mut how = \"plate\";\n    if tagged {\n        how = \"tag\";\n    }\n    \
               Hit { how: how }\n}\nfn f() -> Str {\n    let b = \"x\";\n    b\n}\n\
               fn g(c: bool) -> (i64, Str) {\n    let b = if c { \"x\" } else { \"y\" };\n    (1, b)\n}\n\
               fn main() {\n    println(pick(true).how);\n    println(f());\n    println(g(false).1);\n}\n";
    for edit in [
        "let mut how: Str = \"plate\";",
        "let b: Str = \"x\";",
        "let b: Str = if c",
    ] {
        rejected_then_fixed_by(
            "str-lit-let",
            src,
            "`how` (bound at 5:13) is a `String`, because a string literal with no expected type is one",
            edit,
        );
    }
    // Assigned a value other than a literal: the reason, and no fix.
    rejected(
        "str-lit-let-assigned",
        "fn h(s: ref String) -> Str {\n    let mut b = \"x\";\n    b = s.to_string();\n    b\n}\n\
         fn main() {\n    println(h(\"a\".to_string()));\n}\n",
        "`b` (bound at 2:13) is a `String`, because a string literal with no expected type is one \
         (design.md § String literals)\n",
    );
}

#[test]
fn a_write_through_a_bare_parameter_fixes_to_mut_ref() {
    // B-2026-10-24-37: a bare parameter is D5's `ref T` with no `ref`
    // written, so its fix inserts `mut ref `, not `mut `; a local bound from
    // it names the parameter, not itself.
    let main =
        "fn main() {\n    let mut v = [\"b\".to_string(), \"a\".to_string()].to_vec();\n    \
                println(sorted_names(v));\n}\n";
    for (tag, body, expect) in [
        (
            "bare-param-write",
            "    items.sort();\n    items.join(\",\")\n",
            "`items` is a bare parameter, which borrows",
        ),
        (
            "bare-param-local-write",
            "    let mut s = items;\n    s.sort();\n    s.join(\",\")\n",
            "`s` is bound from `items`, a bare parameter, which borrows",
        ),
    ] {
        let src = format!("fn sorted_names(items: Vec[String]) -> String {{\n{body}}}\n{main}");
        rejected_then_fixed_in(tag, &src, expect, "items: mut ref Vec[String]", 2);
    }
    let src = format!(
        "fn sorted_names(items: ref Vec[String]) -> String {{\n    let mut s = items;\n    \
         s.sort();\n    s.join(\",\")\n}}\n{main}"
    );
    rejected_then_fixed_in(
        "ref-param-local-write",
        &src,
        "`s` is bound from `items`, a `ref` parameter",
        "items: mut ref Vec[String]",
        2,
    );
}

#[test]
fn find_on_a_const_str_borrows_its_argument() {
    // B-2026-10-24-40: `CODES.find(s)` parses as a path call that the
    // typechecker dispatches as a method call on the `const`; its receiver
    // now carries the path's node, so `find` reads `s` as it does on any
    // `Str` receiver, an element argument included.
    accepted_and_runs(
        "const-str-find",
        "const CODES: Str = \"LRI\";\n\
         struct Origin { name: String }\n\
         const HOME: Str = \"home\";\n\
         fn main() {\n    let ch = \"R\".to_string();\n    let a = CODES.find(ch);\n    \
         println(f\"{ch} {a.unwrap_or(9)}\");\n    let f: Vec[String] = [\"I\".to_string()].to_vec();\n    \
         let b = CODES.find(f[0]);\n    let o = Origin { name: \"me\".to_string() };\n    \
         println(f\"{b.unwrap_or(9)} {f.len()} {HOME.find(o.name).is_none()} {o.name}\");\n}\n",
        "R 1\n2 1 false me\n",
    );
}

#[test]
fn a_lone_file_package_is_checked_against_the_v2_library() {
    // B-2026-10-24-39: `karac build` builds a package whose only module is
    // `src/main.kara` as a lone file, against the v2 library it appends, so
    // `karac check` in the package (and on its `src/main.kara`) checks it
    // the same way: a v2-only library item such as `WsMessage` resolves.
    let (dir, _) = fixture("lone-file-v2-library", "");
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "fn main() {\n    let m: Option[WsMessage] = None;\n    println(f\"{m.is_none()}\");\n}\n",
    )
    .unwrap();
    for args in [vec!["check"], vec!["check", "src/main.kara"]] {
        let out = karac().args(&args).current_dir(&dir).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{args:?} must pass: {err}");
    }
    let out = karac()
        .args(["run", "src/main.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "true\n",
        "run: {:?}",
        out
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn as_bytes_on_a_string_names_bytes_and_fixes_to_it() {
    // B-2026-10-24-42: `.as_bytes()` is the Rust spelling of a string's
    // `.bytes()` (library/strings.md); the error names it and the fix writes
    // it, on a `String` and on a `Str`.
    let src = "fn main() {\n    let s = \"abc\".to_string();\n    let b = s.as_bytes();\n    \
               let t: Str = \"xy\";\n    let c = t.as_bytes();\n    println(f\"{b.len()} {c[1]}\");\n}\n";
    for edit in ["let b = s.bytes();", "let c = t.bytes();"] {
        rejected_then_fixed_by(
            "string-as-bytes",
            src,
            "no method 'as_bytes' on type 'String': a string's UTF-8 bytes are `.bytes()`",
            edit,
        );
    }
    accepted_and_runs(
        "string-bytes",
        "fn main() {\n    let t: Str = \"xy\";\n    let c = t.bytes();\n    println(f\"{c.len()} {c[1]}\");\n}\n",
        "2 121\n",
    );
}

#[test]
fn a_package_with_two_modules_is_checked_against_the_v2_library() {
    // B-2026-10-24-43: a package with more than one module is checked module
    // by module, and each module now sees the v2 library a lone file is
    // checked against, so a v2-only item (`WsMessage`, `std.process`'s
    // `Command`) resolves in `karac check` and in the check in front of
    // `karac run`, while a misspelled one is still an error in its own file.
    let (dir, _) = fixture("two-modules-v2-library", "");
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let util = "import std.process.{Command};\npub fn two() -> i64 {\n    \
                let _c = Command.new(\"echo\");\n    2\n}\n";
    std::fs::write(dir.join("src/util.kara"), util).unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import util.{two};\nfn main() {\n    let m: Option[WsMessage] = None;\n    \
         println(f\"{m.is_none()} {two()}\");\n}\n",
    )
    .unwrap();
    let out = karac().arg("check").current_dir(&dir).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "check must pass: {err}");
    let out = karac()
        .args(["run", "src/main.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "true 2\n",
        "run: {:?}",
        out
    );
    std::fs::write(
        dir.join("src/util.kara"),
        util.replace("Command.new", "Comand.new"),
    )
    .unwrap();
    let out = karac().arg("check").current_dir(&dir).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a misspelled item must fail: {err}");
    assert!(
        err.contains("util.kara:3:14"),
        "names the module's file: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_package_with_two_modules_reads_a_bare_parameter_as_a_borrow() {
    // B-2026-10-25-4: the per-module passes and the flattened late phases of
    // a package with more than one module read a bare parameter the way a
    // lone file does (D5, core-semantics §4.1): a call into another module
    // borrows its argument, so the caller uses it again, and a callee that
    // gives its bare parameter away is refused in its own file.
    let (dir, _) = fixture("two-modules-d5", "");
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let util = "pub fn len_of(s: String) -> i64 {\n    s.len()\n}\n";
    std::fs::write(dir.join("src/util.kara"), util).unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import util.{len_of};\nfn main() {\n    let v = vec![\"ab\".to_string()];\n    \
         let mut t = 0;\n    for x in v { t += len_of(x); }\n    \
         let s = \"abc\".to_string();\n    t += len_of(s);\n    println(f\"{t} {s}\");\n}\n",
    )
    .unwrap();
    let out = karac().arg("check").current_dir(&dir).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "check must pass: {err}");
    let out = karac()
        .args(["run", "src/main.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "5 abc\n",
        "run: {:?}",
        out
    );
    std::fs::write(
        dir.join("src/util.kara"),
        format!("{util}pub fn keep(s: String) -> String {{\n    s\n}}\n"),
    )
    .unwrap();
    let out = karac().arg("check").current_dir(&dir).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "giving a bare parameter away must fail: {err}"
    );
    assert!(
        err.contains("util.kara:5:5"),
        "names the callee's file: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_package_check_refuses_what_the_v2_check_of_its_combined_source_does() {
    // B-2026-10-25-5: `karac check` on a package with more than one module
    // also runs the v2 check of the combined source that `karac run` and
    // `karac build` run, so a refusal only that check makes (a library
    // method taking `own self` on a bare `for` element) fails check too,
    // named by its module's file in text and JSON alike.
    let (dir, _) = fixture("two-modules-combined-check", "");
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/util.kara"),
        "pub fn total(v: own Vec[Option[String]]) -> i64 {\n    let mut n = 0;\n    \
         for o in v {\n        n += o.unwrap().len();\n    }\n    n\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import util.{total};\nfn main() {\n    let v = vec![Some(\"ab\".to_string())];\n    \
         println(f\"{total(v)}\");\n}\n",
    )
    .unwrap();
    let out = karac().arg("check").current_dir(&dir).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr).replace('\\', "/");
    assert!(!out.status.success(), "check must fail as run does: {err}");
    assert!(
        err.contains("src/util.kara:4:14: cannot move a `for` element"),
        "{err}"
    );
    let out = karac()
        .args(["check", "--output=json"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let json = String::from_utf8_lossy(&out.stdout).replace("\\\\", "/");
    assert!(!out.status.success(), "{json}");
    assert!(
        json.contains("\"file\":\"src/util.kara\",\"line\":4,\"column\":14"),
        "{json}"
    );
    std::fs::write(
        dir.join("src/util.kara"),
        "pub fn total(v: own Vec[Option[String]]) -> i64 {\n    let mut n = 0;\n    \
         for o in v.into_iter() {\n        n += o.unwrap().len();\n    }\n    n\n}\n",
    )
    .unwrap();
    let out = karac().arg("check").current_dir(&dir).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("All checks passed."));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_own_self_method_on_a_borrowed_receiver_is_a_move_out_of_the_borrow() {
    // B-2026-10-25-25: a method taking `own self` (`unwrap`, a user `take`)
    // called on a borrowed receiver, a `ref` parameter, a bare one (D5) or
    // a bare `for` element of one, moves out of the borrow (core-semantics
    // §3.7), as the MIR builder refuses it; the fix calls it on a copy. A
    // `Copy` receiver behind the borrow is copied and stays legal.
    let shapes = [
        (
            "ref-param",
            "fn f(o: ref Option[String]) -> i64 {\n    o.unwrap().len()\n}\n",
            "o.clone().unwrap()",
        ),
        (
            "bare-param",
            "fn f(o: Option[String]) -> i64 {\n    o.unwrap().len()\n}\n",
            "o.clone().unwrap()",
        ),
        (
            "for-over-param",
            "fn f(v: Vec[Option[String]]) -> i64 {\n    let mut n = 0;\n    \
             for o in v {\n        n += o.unwrap().len();\n    }\n    n\n}\n",
            "o.clone().unwrap()",
        ),
        (
            "user-method",
            "#[derive(Clone)]\nstruct S { s: String }\nimpl S {\n    \
             fn take(own self) -> String {\n        self.s\n    }\n}\n\
             fn f(x: ref S) -> i64 {\n    x.take().len()\n}\n",
            "x.clone().take()",
        ),
    ];
    for (tag, f, edit) in shapes {
        let src = format!("{f}fn main() {{\n    println(\"x\");\n}}\n");
        rejected_then_fixed_by(
            &format!("own-self-borrowed-{tag}"),
            &src,
            "cannot move a non-`Copy` value out of a borrowed place: this method takes `self` by value",
            edit,
        );
    }
    accepted_and_runs(
        "own-self-borrowed-copy",
        "fn f(o: ref Option[i64]) -> i64 {\n    o.unwrap()\n}\nfn main() {\n    \
         let o = Some(4);\n    println(f\"{f(o)} {o.is_some()}\");\n}\n",
        "4 true\n",
    );
}

#[test]
fn an_escaping_closure_cannot_capture_a_non_copy_place_behind_a_borrow() {
    // B-2026-10-25-20: an escaping closure captures every place by move
    // (core-semantics §9.3), so a place it uses from behind a borrow (a bare
    // or `ref` parameter, a `ref` local) that is neither `Copy` nor a
    // counted handle would move out of the borrow (§3.7) and is refused:
    // the whole parameter (`size(p)`), a non-`Copy` field (`p.s.len()`).
    // A `Copy` field is copied (`p.n`), an `own` parameter moves in, and a
    // closure passed to a built-in like `Vec.from_fn` does not escape.
    let program = |op: &str, param: &str| {
        format!(
            "struct P {{\n    s: String,\n    n: i64,\n}}\nstruct Op {{\n    run: Fn(i64) -> i64,\n}}\n\
             fn new_op(run: escaping Fn(i64) -> i64) -> Op {{\n    Op {{ run: run }}\n}}\n\
             fn size(p: P) -> i64 {{\n    p.n + p.s.len()\n}}\n\
             fn adder(p: {param}) -> Op {{\n    new_op({op})\n}}\n\
             fn main() {{\n    let op = adder(P {{ s: \"a\", n: 5 }});\n    \
             println(f\"{{(op.run)(1)}} {{(op.run)(2)}}\");\n}}\n"
        )
    };
    for (tag, op, param, place) in [
        ("whole", "|x| x + size(p)", "P", "`p`"),
        ("ref", "|x| x + size(p)", "ref P", "`p`"),
        ("field", "|x| x + p.s.len()", "P", "`p.s`"),
    ] {
        rejected_with(
            &format!("escaping-borrow-{tag}"),
            &program(op, param),
            &format!("an escaping closure captures {place} by move (core-semantics §9.3), but `p` borrows its value"),
        );
    }
    accepted_and_runs(
        "escaping-borrow-copy-field",
        &program("|x| x + p.n", "P"),
        "6 7\n",
    );
    accepted_and_runs(
        "escaping-borrow-own",
        &program("|x| x + size(p)", "own P"),
        "7 8\n",
    );
    accepted_and_runs(
        "escaping-borrow-from-fn",
        "fn f(mat: Vec[i64]) -> Vec[i64] {\n    Vec.from_fn(3, |j| mat[j] * 2)\n}\n\
         fn main() {\n    let m = vec![1, 2, 3];\n    println(f\"{f(m)[2]} {m.len()}\");\n}\n",
        "6 3\n",
    );
}

#[test]
fn a_program_cannot_redeclare_a_library_resource() {
    // B-2026-10-25-24: `Clock`, `Env` and `RandomSource` are declared by the
    // library with their provider traits (core-semantics §14.1, §14.7), so a
    // program's own `effect resource Clock;` is refused rather than
    // shadowing the library's and skipping the provider checks; `karac fix`
    // deletes it, and the provider bound with `with_provider` must then
    // implement `ClockProvider`.
    let body = "struct FakeClock {\n    t: i64,\n}\n{impl}\
                fn main() {\n    with_provider[Clock](FakeClock { t: 42 }, || {\n        \
                println(Clock.now());\n    });\n}\n";
    let with_trait = body.replace(
        "{impl}",
        "impl ClockProvider for FakeClock {\n    fn now(ref self) -> i64 {\n        self.t\n    }\n}\n",
    );
    let inherent = body.replace(
        "{impl}",
        "impl FakeClock {\n    fn now(ref self) -> i64 {\n        self.t\n    }\n}\n",
    );
    let declared = "`Clock` is declared by the library, as `effect resource Clock: ClockProvider`";
    rejected_with(
        "redeclared-resource-inherent",
        &format!("effect resource Clock;\n{inherent}"),
        declared,
    );
    rejected_with(
        "redeclared-resource-inherent-provider",
        &format!("effect resource Clock;\n{inherent}"),
        "does not implement `ClockProvider`",
    );
    let (dir, path) = fixture(
        "redeclared-resource-fix",
        &format!("effect resource Clock;\n{with_trait}"),
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && err.contains(declared), "{err}");
    karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        !fixed.contains("effect resource"),
        "fix must delete it: {fixed}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    accepted_and_runs("redeclared-resource-fixed", &fixed, "42\n");
    for (name, t) in [("Env", "EnvProvider"), ("RandomSource", "RandomProvider")] {
        rejected_with(
            &format!("redeclared-resource-{name}"),
            &format!("effect resource {name};\nfn main() {{}}\n"),
            &format!("`{name}` is declared by the library, as `effect resource {name}: {t}`"),
        );
    }
}

#[test]
fn karac_fix_on_a_package_module_fixes_it_from_the_package_view() {
    // B-2026-10-25-26: `karac fix` on a module of a package reads it the way
    // `karac check` and `karac run` do, through the package, not as a lone
    // file that cannot see the enum it imports and so "corrects" its
    // variant `Enum` to the library's `Env`. A misspelt name gets the
    // package's own `did you mean`, whose edit (in `--output=json` too) is an
    // offset into the file; a fix the package's combined check offers lands
    // in the module it edits and is applied by a fix of that module only.
    let (dir, _) = fixture("fix-package-module", "");
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"fp\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/ast.kara"),
        "pub enum Item {\n    Struct(i64),\n    Enum(i64),\n}\npub struct Tag {\n    pub s: String,\n}\n\
         pub fn keep(t: Tag) -> Tag {\n    t\n}\n",
    )
    .unwrap();
    let main = "import ast.{Item, Tag, keep};\nfn f(it: Item) -> i64 {\n    match it {\n        \
                Struct(n) => n,\n        Enum(n) => n + 1,\n    }\n}\n\
                fn main() {\n    println(f(Itme.Enum(3)));\n    println(keep(Tag { s: \"a\" }).s);\n}\n";
    std::fs::write(dir.join("src/main.kara"), main).unwrap();
    let json = karac()
        .args(["check", "--output=json", "src/main.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let json = String::from_utf8_lossy(&json.stdout);
    let at = main.find("Itme").unwrap();
    assert!(
        json.contains(&format!(
            "\"replacement\":{{\"offset\":{at},\"length\":4,\"text\":\"Item\"}}"
        )),
        "{json}"
    );
    let fix = |file: &str| {
        karac()
            .args(["fix", file])
            .current_dir(&dir)
            .output()
            .unwrap()
    };
    fix("src/main.kara");
    let fixed = std::fs::read_to_string(dir.join("src/main.kara")).unwrap();
    assert_eq!(fixed, main.replace("Itme", "Item"));
    // The move out of `keep`'s borrowed parameter is `ast.kara`'s to fix.
    fix("src/main.kara");
    assert_eq!(
        std::fs::read_to_string(dir.join("src/main.kara")).unwrap(),
        fixed
    );
    fix("src/ast.kara");
    let ast = std::fs::read_to_string(dir.join("src/ast.kara")).unwrap();
    assert!(ast.contains("pub fn keep(t: own Tag) -> Tag {"), "{ast}");
    let out = karac().arg("check").current_dir(&dir).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = karac()
        .args(["run", "src/main.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "4\na\n", "{out:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bare_use_of_a_variant_named_like_a_library_item_is_the_variant() {
    // B-2026-10-25-33: a program's variant `Match` used bare (a pattern
    // `Match(n)`) is the variant, not a use of `std.regex`'s `Match` that
    // needs an import; a path on from the name still is one.
    accepted_and_runs(
        "bare-variant-named-match",
        "enum E {\n    Match(i64),\n    Other,\n}\nfn f(e: E) -> i64 {\n    match e {\n        \
         Match(n) => n,\n        Other => 0,\n    }\n}\nfn main() {\n    println(f(E.Match(2)));\n}\n",
        "2\n",
    );
    rejected_with(
        "unimported-regex-beside-a-variant",
        "enum E {\n    Regex,\n}\nfn main() {\n    let r = Regex.new(\"a\");\n    println(r.is_ok());\n}\n",
        "`Regex` is in the library module `std.regex`",
    );
}

#[test]
fn a_function_a_package_module_imports_shadows_the_library_function_of_its_name() {
    // B-2026-10-25-34: `import ast.{take}` binds the module's `take`, not
    // `std.mem`'s generic `take(mut ref T) -> T`, in the importing module's
    // own check as in the package's combined source, under its name, an
    // alias and a wildcard alike.
    let (dir, _) = fixture("imported-fn-shadows-library", "");
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"fq\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/ast.kara"),
        "pub struct Box2 {\n    pub s: String,\n}\npub fn take(b: own Box2) -> String {\n    b.s\n}\n",
    )
    .unwrap();
    for (import, call, want) in [
        ("import ast.{Box2, take};", "take", "2\n"),
        ("import ast.{Box2, take as grab};", "grab", "2\n"),
        ("import ast.*;", "take", "2\n"),
    ] {
        std::fs::write(
            dir.join("src/main.kara"),
            format!(
                "{import}\nfn main() {{\n    let b = Box2 {{ s: \"ab\" }};\n    \
                 println({call}(b).len());\n}}\n"
            ),
        )
        .unwrap();
        for args in [&["check"][..], &["check", "src/main.kara"][..]] {
            let out = karac().args(args).current_dir(&dir).output().unwrap();
            assert!(
                out.status.success(),
                "{import} {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let out = karac()
            .args(["run", "src/main.kara"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{import}: {out:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_method_on_an_associated_type_of_a_bound_takes_the_trait_declared_bounds() {
    // B-2026-10-25-32: a value of `B.Out` under `B: Mk`, where `trait Mk`
    // declares `type Out: Fin`, takes `Fin`'s methods, as a where clause's
    // `B.Out: Fin` already gave them; it checked as Error with no
    // diagnostic, and the MIR builder stopped on the Error type. A method
    // no bound declares is refused by name.
    let program = |call: &str| {
        format!(
            "trait Fin {{\n    fn fin(ref self) -> i64;\n}}\ntrait Mk {{\n    type Out: Fin;\n    \
             fn mk(ref self) -> Self.Out;\n}}\nstruct F {{\n    h: i64,\n}}\n\
             impl Fin for F {{\n    fn fin(ref self) -> i64 {{\n        self.h\n    }}\n}}\n\
             struct M {{}}\nimpl Mk for M {{\n    type Out = F;\n    fn mk(ref self) -> F {{\n        \
             F {{ h: 3 }}\n    }}\n}}\nfn word[B: Mk](b: ref B, w: i64) -> i64 {{\n    \
             let h = b.mk();\n    h.{call}() + w\n}}\nfn main() {{\n    println(word(M {{}}, 7));\n}}\n"
        )
    };
    accepted_and_runs("projection-declared-bound", &program("fin"), "10\n");
    rejected_with(
        "projection-declared-bound-missing",
        &program("nope"),
        "no method 'nope' on type parameter 'B.Out'",
    );
    accepted_and_runs(
        "projection-declared-bound-library",
        "struct Fnv {\n    h: u64,\n}\nimpl Hasher for Fnv {\n    \
         fn write(mut ref self, bytes: ref Slice[u8]) {\n        \
         for _ in bytes {\n            self.h = self.h * 31u64 + 1u64;\n        }\n    }\n    \
         fn finish(ref self) -> u64 {\n        self.h\n    }\n}\nstruct FnvBuild {}\n\
         impl BuildHasher for FnvBuild {\n    type Hasher = Fnv;\n    \
         fn build(ref self) -> Fnv {\n        Fnv { h: 1u64 }\n    }\n}\n\
         fn hasher_word[B: BuildHasher](b: ref B, w: u64) -> u64 {\n    \
         let mut h = b.build();\n    h.write_u8(2u8);\n    h.finish() + w\n}\n\
         fn main() {\n    println(hasher_word(FnvBuild {}, 7u64));\n}\n",
        "39\n",
    );
}

#[test]
fn a_shared_value_cannot_be_sent_on_a_channel_whose_element_the_send_pins() {
    // B-2026-10-25-37: `Sender.send` carries the CrossTask bound (design.md
    // § CrossTask), so a plain `shared` value is refused at a send whose
    // channel left its element open (`channel(4)`), as at one whose element
    // was written; a `sync` value and a plain owned struct are sent.
    let program = |decl: &str| {
        format!(
            "{decl} struct Counter {{\n    n: i64,\n}}\nfn main() {{\n    \
             let (tx, rx) = channel(4);\n    let _ = tx.send(Counter {{ n: 1 }});\n    \
             println(rx.recv().unwrap().n);\n}}\n"
        )
    };
    rejected_with(
        "shared-sent-on-a-channel",
        &program("shared"),
        "value sent across a channel (type `Counter`) cannot cross a task boundary",
    );
    rejected_with(
        "shared-sent-on-a-channel-help",
        &program("shared"),
        "help: declare `Counter` as a `sync` type",
    );
    accepted_and_runs("sync-sent-on-a-channel", &program("sync"), "1\n");
    accepted_and_runs("plain-sent-on-a-channel", program("").trim_start(), "1\n");
}

#[test]
fn a_package_diagnostic_names_its_places_by_file_line_and_offset() {
    // B-2026-10-25-35: a package is checked as its combined source, and a
    // diagnostic of it shows the files' places: the line a message names
    // (`moved at line 4:17`, main.kara's own, not the combined text's 10),
    // a snippet gutter of one width, and a `--output=json` edit in another
    // module carrying that module's `file` and its offset there.
    let (dir, _) = fixture("package-diagnostic-places", "");
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"fq\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/ast.kara"),
        "pub struct Box2 {\n    pub s: String,\n}\npub fn eat(b: own Box2) -> i64 {\n    b.s.len()\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import ast.{Box2, eat};\nfn main() {\n    let b = Box2 { s: \"ab\" };\n    \
         println(eat(b));\n    println(eat(b));\n}\n",
    )
    .unwrap();
    let out = karac().arg("check").current_dir(&dir).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
    .replace('\\', "/");
    assert!(
        text.contains(
            "src/main.kara:5:17: value 'b' moved here, used again here (moved at line 4:17)"
        ),
        "{text}"
    );
    assert!(
        text.contains("\n  |\n5 |     println(eat(b));\n  |"),
        "{text}"
    );
    let out = karac()
        .args(["check", "--output=json"])
        .current_dir(&dir)
        .output()
        .unwrap();
    // A Windows separator arrives JSON-escaped, as `\\`.
    let json = String::from_utf8_lossy(&out.stdout).replace("\\\\", "/");
    assert!(json.contains("(moved at line 4:17)"), "{json}");
    assert!(
        json.contains("\"replacement\":{\"offset\":0,\"length\":0,\"text\":\"#[derive(Clone)]\\n\",\"file\":\"src/ast.kara\"}"),
        "{json}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_package_function_named_like_a_library_function_leaves_other_modules_the_library_one() {
    // B-2026-10-25-38: a module's own `take` is that module's, and its
    // importers', in the package's combined source; a module that does not
    // import it still calls `std.mem`'s `take`, as a lone file does.
    let (dir, _) = fixture("package-fn-named-like-library", "");
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"fq\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/ast.kara"),
        "pub struct Box2 {\n    pub s: String,\n}\npub fn take(b: own Box2) -> String {\n    b.s\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/util.kara"),
        "import ast.{Box2, take};\npub fn size() -> i64 {\n    take(Box2 { s: \"abc\" }).len()\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import util.{size};\nfn main() {\n    let mut x = 5;\n    let y = take(mut x);\n    \
         println(f\"{x} {y} {size()}\");\n}\n",
    )
    .unwrap();
    let out = karac().arg("check").current_dir(&dir).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = karac()
        .args(["run", "src/main.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "0 5 3\n", "{out:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_sync_type_holds_only_cross_task_fields_and_counts_as_a_handle() {
    // B-2026-10-25-42: every task sees a `sync` value, so each of its
    // fields and payloads is `CrossTask` (core-semantics §6): a plain
    // `shared` one is refused at the field, a `sync` one is held.
    // B-2026-10-25-43: a `sync` type is a counted handle like a `shared`
    // one, so a self-referential `sync enum` with a base case is a finite
    // tree, not an ownership cycle.
    let holder = |decl: &str| {
        format!(
            "{decl} struct Plain {{\n    n: i64,\n}}\nsync struct Holder {{\n    p: Plain,\n}}\n\
             fn read(h: Holder) -> i64 {{\n    let q = h.p.clone();\n    q.n\n}}\n\
             fn main() {{\n    let h = Holder {{ p: Plain {{ n: 1 }} }};\n    let g = TaskGroup.new();\n    \
             let t = {{\n        let h1 = h.clone();\n        g.spawn(|| read(h1))\n    }};\n    \
             println(f\"{{t.join()}} {{h.p.n}}\");\n}}\n"
        )
    };
    rejected_with(
        "sync-field-of-a-shared-type",
        &holder("shared"),
        "field `p` of `sync struct Holder` (type `Plain`) cannot cross a task boundary",
    );
    accepted_and_runs("sync-field-of-a-sync-type", &holder("sync"), "1 1\n");
    for (tag, payload) in [("tuple", "A(Plain)"), ("struct", "A { p: Plain }")] {
        rejected_with(
            &format!("sync-enum-payload-{tag}"),
            &format!(
                "shared struct Plain {{\n    n: i64,\n}}\nsync enum E {{\n    {payload},\n    C(i64),\n}}\n\
                 fn main() {{}}\n"
            ),
            "of `sync enum E` (type `Plain`) cannot cross a task boundary",
        );
    }
    accepted_and_runs(
        "sync-enum-tree",
        "sync enum Tree {\n    Leaf(i64),\n    Pair(Tree, Tree),\n}\nfn sum(t: Tree) -> i64 {\n    \
         match t {\n        Leaf(n) => n,\n        Pair(a, b) => sum(a) + sum(b),\n    }\n}\n\
         fn main() {\n    let t = Tree.Pair(Tree.Leaf(1), Tree.Pair(Tree.Leaf(2), Tree.Leaf(3)));\n    \
         println(sum(t));\n}\n",
        "6\n",
    );
}

#[test]
fn a_ref_to_a_shared_or_sync_enum_reads_as_the_handle() {
    // B-2026-10-25-12: a `ref` to a handle reads as a counted copy of it
    // (core-semantics §6.1) in an argument, a constructor and a `return`.
    // A `shared` struct always did; a `shared` / `sync` enum is spelled
    // `Named`, not `Shared`, and was refused with "expected 'E', found
    // 'ref E'" (or, for `sync`, as a move out of a borrow).
    for decl in ["shared", "sync"] {
        accepted_and_runs(
            &format!("ref-to-a-{decl}-enum-reads-as-the-handle"),
            &format!(
                "{decl} enum E {{\n    A(i64),\n    B,\n}}\n{decl} struct S {{\n    n: i64,\n}}\n\
                 fn size(e: E) -> i64 {{\n    match e {{\n        E.A(n) => n,\n        E.B => 0,\n    }}\n}}\n\
                 fn first(v: ref Vec[E]) -> Option[E] {{\n    for e in v {{\n        return Some(e);\n    }}\n    None\n}}\n\
                 fn first_s(v: ref Vec[S]) -> Option[S] {{\n    for s in v {{\n        return Some(s);\n    }}\n    None\n}}\n\
                 fn main() {{\n    let v = Vec[E.A(4), E.B];\n    let r = ref v[0];\n    let mut t = 0;\n    \
                 for e in v.iter() {{\n        t = t + size(e);\n    }}\n    let w = Vec[S {{ n: 3 }}];\n    \
                 let k = match first(v) {{\n        Some(e) => size(e),\n        None => -1,\n    }};\n    \
                 println(f\"{{size(r)}} {{t}} {{k}} {{first_s(w).unwrap().n}}\");\n}}\n"
            ),
            "4 4 4 3\n",
        );
    }
}

#[test]
fn a_slice_where_a_vec_is_wanted_is_fixed_with_to_vec() {
    // B-2026-10-25-9: `String.bytes()` is a `Slice[u8]`; where a `Vec[u8]`
    // or a `ref Vec[u8]` is wanted, `karac fix` appends `.to_vec()`, and the
    // message names it and the `Slice[u8]` parameter that takes both.
    rejected_then_fixed_by(
        "slice-to-a-ref-vec-param",
        "fn total(b: ref Vec[u8]) -> i64 {\n    let mut t = 0;\n    for x in b {\n        t += x as i64;\n    }\n    t\n}\n\
         fn main() {\n    let s = \"abc\".to_string();\n    println(f\"{total(s.bytes())}\");\n}\n",
        "found 'Slice[u8]': `.to_vec()` copies the elements into a `Vec`; a `Slice[u8]` parameter would take both",
        "total(s.bytes().to_vec())",
    );
    rejected_then_fixed_by(
        "slice-assigned-to-a-vec",
        "fn main() {\n    let mut cur: Vec[u8] = Vec.new();\n    cur = \"!\".bytes();\n    println(f\"{cur.len()}\");\n}\n",
        "expected 'Vec[u8]', found 'Slice[u8]': `.to_vec()` copies the elements into a `Vec`",
        "cur = \"!\".bytes().to_vec();",
    );
}

#[test]
fn a_mut_inside_an_arm_or_loop_pattern_is_fixed_by_a_rebinding() {
    // B-2026-10-25-8: `mut` inside a pattern outside a `let` is fixed by
    // deleting it and opening the body with `let mut x = x;`. An expression
    // arm body becomes a block; a block body takes the `let` on its own line.
    let counter = "struct Counter {\n    n: i64,\n}\nimpl Counter {\n    fn bump(mut ref self) -> i64 {\n        \
                   self.n += 1;\n        self.n\n    }\n}\nfn make() -> Result[Counter, String] {\n    \
                   Ok(Counter { n: 0 })\n}\n";
    rejected_then_fixed_by(
        "mut-in-a-match-arm",
        &format!(
            "{counter}fn main() {{\n    match make() {{\n        Ok(mut c) => println(f\"{{c.bump()}}\"),\n        \
             Err(e) => println(e),\n    }}\n}}\n"
        ),
        "`mut c` inside a pattern is Rust syntax",
        "Ok(c) => { let mut c = c; println(f\"{c.bump()}\") },",
    );
    rejected_then_fixed_by(
        "mut-in-an-if-let",
        &format!(
            "{counter}fn main() {{\n    if let Ok(mut c) = make() {{\n        println(f\"{{c.bump()}}\");\n    }}\n}}\n"
        ),
        "`mut c` inside a pattern is Rust syntax",
        "if let Ok(c) = make() {\n        let mut c = c;\n        println(",
    );
    rejected_then_fixed_by(
        "mut-in-a-for-pair",
        "fn main() {\n    let v = Vec[(1, 2)];\n    for (mut a, mut b) in v {\n        a += 1;\n        b += 1;\n        \
         println(f\"{a} {b}\");\n    }\n}\n",
        "`mut a` inside a pattern is Rust syntax",
        "for (a, b) in v {\n        let mut a = a;\n        let mut b = b;\n        a += 1;",
    );
}

#[test]
fn a_usize_cast_of_an_i64_length_is_dropped_and_a_usize_length_needs_its_as() {
    // B-2026-10-25-10: lengths are `i64` (design.md §5), so `x as usize`
    // over an `i64`, met by an `i64` operand or slot, is Rust's habit and
    // `karac fix` deletes the cast.
    rejected_then_fixed_by(
        "usize-cast-in-arithmetic",
        "const DAYS: i64 = 7;\nfn main() {\n    let v: Vec[i64] = Vec.new();\n    let nc = v.len();\n    \
         let a = nc * DAYS as usize;\n    println(f\"{a}\");\n}\n",
        "sizes, lengths and indices are `i64` in Kāra (`usize` is for FFI), so drop the `as usize`",
        "let a = nc * DAYS;",
    );
    rejected_then_fixed_by(
        "usize-cast-as-a-length",
        "fn main() {\n    let v: Vec[i64] = Vec.new();\n    let b: Vec[i64] = Vec.filled(v.len() as usize, 0);\n    \
         println(f\"{b.len()}\");\n}\n",
        "drop the `as usize`",
        "Vec.filled(v.len(), 0)",
    );
    // B-2026-10-25-11: a `usize` length of `Vec.filled` or
    // `Vec.with_capacity` changes sign as it would at a user call.
    for (tag, call) in [
        ("filled", "let a: Vec[i64] = Vec.filled(n, 0);"),
        ("filled-synth", "let a = Vec.filled(n, 0);"),
        ("with-capacity", "let a: Vec[i64] = Vec.with_capacity(n);"),
    ] {
        let (dir, path) = fixture(
            &format!("usize-length-{tag}"),
            &format!("fn main() {{\n    let n: usize = 5;\n    {call}\n    println(f\"{{a.len()}}\");\n}}\n"),
        );
        let out = karac().arg("check").arg(&path).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success() && err.contains("implicit coercion from 'usize' to 'i64'"),
            "{tag}: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_conflict_with_an_argument_borrow_names_the_argument_and_clones_it() {
    // B-2026-10-25-7: in `self.note(self.look)`, with `note(mut ref self,
    // t: ref Tok)`, the shared borrow is the argument, so E0516 names its
    // place (14:19), not the call's, and `karac fix` clones a `Clone`
    // argument so the call borrows a temporary.
    let src = "#[derive(Clone)]\nstruct Tok {\n    v: String,\n}\nstruct P {\n    look: Tok,\n    log: Vec[String],\n}\n\
               impl P {\n    fn note(mut ref self, t: ref Tok) {\n        self.log.push(t.v.clone());\n    }\n    \
               fn run(mut ref self) {\n        self.note(self.look);\n    }\n}\n\
               fn main() {\n    let mut p = P { look: Tok { v: \"a\".to_string() }, log: Vec.new() };\n    \
               p.run();\n    println(p.log.len());\n}\n";
    rejected_then_fixed_by(
        "argument-borrow-clone",
        src,
        "still borrowed (the borrow is taken at 14:19)",
        "self.note(self.look.clone());",
    );
    // A `let` whose initializer returns the argument's borrow still gets
    // the held-borrow fix, the initializer read out.
    rejected_then_fixed_by(
        "argument-borrow-held-by-a-let",
        "fn first(v: ref Vec[i64]) -> ref i64 {\n    v.get(0).unwrap()\n}\nfn main() {\n    let mut v = Vec[1];\n    \
         let h = first(v);\n    v.push(2);\n    println(h);\n}\n",
        "`h` still holds a borrow of it, taken at 6:19",
        "let h = *first(v);",
    );
}

#[test]
fn a_package_is_refused_by_its_modules_only_at_a_module_boundary() {
    // B-2026-10-25-46: past the v2 check of a package's combined source,
    // the per-module typecheck refuses only what a module boundary carries.
    // A `ref` to an imported `shared` enum and `f64.parse(..).ok()` check and
    // run as they do in a lone file; a private field read from another
    // module is still refused.
    let (dir, _) = fixture("package-boundary-refusals", "");
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"pb\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/ast.kara"),
        "pub shared enum E {\n    A(i64),\n    B,\n}\npub struct P {\n    secret: i64,\n}\n\
         pub fn mk() -> P {\n    P { secret: 1 }\n}\n",
    )
    .unwrap();
    let main = |tail: &str| {
        format!(
            "import ast.{{E, P, mk}};\nfn size(e: E) -> i64 {{\n    match e {{\n        E.A(n) => n,\n        \
             E.B => 0,\n    }}\n}}\nfn main() {{\n    let v = Vec[E.A(4), E.B];\n    let r = ref v[0];\n    \
             let x = f64.parse(\"1.5\").ok().unwrap_or(0.0);\n    let p: P = mk();\n    \
             println(f\"{{size(r)}} {{x}}\");\n{tail}}}\n"
        )
    };
    std::fs::write(dir.join("src/main.kara"), main("")).unwrap();
    let check = karac().arg("check").current_dir(&dir).output().unwrap();
    assert!(
        check.status.success(),
        "{}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let run = karac()
        .args(["run", "src/main.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "4 1.5\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    std::fs::write(dir.join("src/main.kara"), main("    println(p.secret);\n")).unwrap();
    let check = karac().arg("check").current_dir(&dir).output().unwrap();
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(
        !check.status.success() && out.contains("private field 'secret' of struct 'P'"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_copy_part_of_a_borrowed_scrutinee_is_a_copy_in_every_pattern() {
    // B-2026-10-24-36: a plain binding of a `Copy` part copies it whatever
    // the scrutinee (core-semantics §4.6), so it is a value and `*` on it is
    // the drop-the-`*` error (§5.10) in `while let`, `if let`, `match` and a
    // `for` destructure, as it already was in a `let` destructure. The item
    // of a bare `for` is still the `ref`, and a `ref` binding still borrows.
    let shapes = [
        (
            "while let Some((s, _)) = q.front() { if *s > 2 { q.pop_front(); } else { break; } }",
            "if s > 2",
        ),
        (
            "if let Some((a, _)) = q.front() { println(f\"{*a}\"); }",
            "{a}",
        ),
        (
            "match q.front() { Some((m, _)) => println(f\"{*m + 1}\"), None => {} }",
            "{m + 1}",
        ),
        ("for (_, i) in q.iter() { println(f\"{*i}\"); }", "{i}"),
    ];
    for (body, edit) in shapes {
        let src = format!(
            "fn main() {{\n    let mut q: VecDeque[(i64, i64)] = VecDeque.new();\n    \
             q.push_back((3, 4));\n    {body}\n}}\n"
        );
        rejected_then_fixed_by(
            "copy-part-of-a-borrowed-scrutinee",
            &src,
            "a 'i64' is copied, so drop the `*`",
            edit,
        );
    }
    accepted_and_runs(
        "copy-part-ref-binding-and-for-item",
        "fn main() {\n    let v = vec![(1, 2), (3, 4)];\n    let mut t = 0;\n    \
         for (a, b) in v.iter() { t += a * b; }\n    \
         if let Some((ref x, _)) = v.first() { t += *x; }\n    \
         let w = vec![5, 6];\n    for x in w { t += *x; }\n    println(f\"{t}\");\n}\n",
        "26\n",
    );
}

#[test]
fn an_associated_function_call_checks_its_closure_kinds() {
    // B-2026-10-25-49: §9.6's kind check covers `Type.f(..)` as it covers a
    // free function: a MutFn closure where `own Fn` is declared is refused,
    // naming the capture; a closure that only reads passes.
    let prelude = "struct Tally {\n    n: i64,\n}\nstruct Runner { }\nimpl Runner {\n    \
                   fn apply(f: own Fn(i64) -> i64) -> i64 {\n        f(1) + f(2)\n    }\n}\n\
                   fn note(t: mut ref Tally, k: i64) -> i64 {\n    t.n = t.n + k;\n    t.n\n}\n";
    rejected(
        "assoc-fn-mutfn-for-fn",
        &format!(
            "{prelude}fn main() {{\n    let mut t = Tally {{ n: 0 }};\n    \
             let total = Runner.apply(|k| note(mut t, k));\n    println(f\"{{total}} {{t.n}}\");\n}}\n"
        ),
        "the closure mutates t, so it is a MutFn, but Runner.apply expects Fn (§9.6)",
    );
    accepted(
        "assoc-fn-fn-for-fn",
        &format!(
            "{prelude}fn main() {{\n    let t = Tally {{ n: 5 }};\n    \
             let total = Runner.apply(|k| k + t.n);\n    println(f\"{{total}}\");\n}}\n"
        ),
    );
    // A library associated function is checked the same way.
    rejected(
        "library-assoc-fn-mutfn-for-fn",
        "struct Store {\n    hits: i64,\n}\nfn handle(s: mut ref Store, req: Request) -> Response {\n    \
         s.hits = s.hits + 1;\n    Response { status: 200, body: f\"{s.hits}\" }\n}\nfn main() {\n    \
         let mut store = Store { hits: 0 };\n    \
         let _ = Server.serve(\"127.0.0.1:0\", |req| handle(mut store, req));\n}\n",
        "the closure mutates store, so it is a MutFn, but Server.serve expects Fn (§9.6)",
    );
}

#[test]
fn a_mut_ref_vec_passes_to_a_slice_parameter() {
    // B-2026-10-25-47: a place reached through `mut ref Vec[T]` (or
    // `mut ref Array[T, N]`) passes to a read-only `Slice[T]` parameter, as an
    // owned or `ref` one does (design.md § Slices). The run is pinned by
    // corpus/core/ok_mut_ref_vec_passes_as_a_slice.
    accepted(
        "mut-ref-vec-to-a-slice",
        "fn total(xs: Slice[u8]) -> i64 {\n    let mut t = 0;\n    for x in xs {\n        \
         t = t + x as i64;\n    }\n    t\n}\nfn grow(buf: mut ref Vec[u8], a: mut ref Array[u8, 2]) -> i64 {\n    \
         buf.push(3u8);\n    total(buf) + total(a)\n}\nfn main() {\n    let mut v: Vec[u8] = Vec.new();\n    \
         v.push(1u8);\n    let mut a: Array[u8, 2] = [1u8, 2u8];\n    println(f\"{total(v)} {grow(mut v, mut a)}\");\n}\n",
    );
}

#[test]
fn a_ref_handle_in_a_collected_tuple_reads_as_the_handle() {
    // B-2026-10-26-3: `collect` reads a `ref` handle in a tuple as the handle
    // (§6.1), as a pushed tuple does. The run is pinned by
    // corpus/core/ok_ref_handle_collects_as_the_handle.
    accepted(
        "collect-a-ref-handle-in-a-tuple",
        "shared struct S {\n    x: i64,\n}\nfn pairs(items: ref Vec[(Str, S)]) -> Vec[(String, S)] {\n    \
         items.iter().map(|(n, v)| (n.to_string(), v)).collect()\n}\nfn main() {\n    \
         let items: Vec[(Str, S)] = Vec[(\"a\", S { x: 4 }), (\"b\", S { x: 0 })];\n    \
         let p = pairs(items);\n    println(f\"{p.len()} {p[0].1.x}\");\n}\n",
    );
}

#[test]
fn a_weak_slot_upgrades_and_passes_to_a_weak_parameter() {
    // B-2026-10-26-4: `.upgrade()` on a `weak` slot is its read; on any other
    // `Option` there is no such method. B-2026-10-26-5: a `weak` field passes
    // straight to a `weak` parameter. The run is pinned by
    // corpus/core/ok_weak_slot_upgrade.
    let n = "shared struct N {\n    id: i64,\n    mut me: weak N,\n}\n\
             fn id_of(p: weak N) -> i64 {\n    match p.upgrade() {\n        Some(n) => n.id,\n        \
             None => -1,\n    }\n}\n";
    accepted(
        "weak-slot-upgrade",
        &format!(
            "{n}fn main() {{\n    let a = N {{ id: 1, me: None }};\n    a.me = a;\n    \
             println(a.me.upgrade().is_some());\n    println(id_of(a.me));\n}}\n"
        ),
    );
    rejected(
        "upgrade-on-an-option",
        &format!(
            "{n}fn main() {{\n    let a = N {{ id: 1, me: None }};\n    let o: Option[N] = Some(a);\n    \
             println(o.upgrade().is_some());\n}}\n"
        ),
        "no method 'upgrade' on type 'Option'",
    );
}

#[test]
fn a_library_index_argument_drops_an_as_usize() {
    // B-2026-10-26-2: a `Vec` method's index or count argument is an `i64`,
    // so an `as usize` on an `i64` is refused there as at `Vec.filled`, and
    // `karac fix` drops it.
    rejected_then_fixed_by(
        "vec-swap-as-usize",
        "fn main() {\n    let mut v: Vec[i64] = Vec[3, 1, 2];\n    let k: i64 = 2;\n    \
         v.swap(k as usize, 0);\n    v.truncate(k as usize);\n    println(f\"{v.len()} {v[0]}\");\n}\n",
        "implicit coercion from 'usize' to 'i64' would change sign",
        "v.swap(k, 0);\n    v.truncate(k);",
    );
}

#[test]
fn a_private_field_is_refused_through_an_imported_call() {
    // B-2026-10-26-1: a struct reached only through an imported function's
    // result keeps its fields private: `mk().secret` and `let p = mk();
    // p.secret` are refused as the read through an imported `P` is, while
    // a `pub` field and the defining module's own read pass.
    let (dir, _) = fixture("private-field-through-a-call", "");
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"pf\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/ast.kara"),
        "pub struct P {\n    secret: i64,\n    pub open: i64,\n}\npub fn mk() -> P {\n    \
         P { secret: 1, open: 2 }\n}\npub fn peek(p: P) -> i64 {\n    p.secret\n}\n",
    )
    .unwrap();
    let check_with = |body: &str| {
        std::fs::write(
            dir.join("src/main.kara"),
            format!("import ast.{{mk, peek}};\nfn main() {{\n{body}}}\n"),
        )
        .unwrap();
        let out = karac().arg("check").current_dir(&dir).output().unwrap();
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    };
    let (ok, out) = check_with("    let p = mk();\n    println(f\"{p.open} {peek(p)}\");\n");
    assert!(ok, "{out}");
    for body in [
        "    println(mk().secret);\n",
        "    let p = mk();\n    println(p.secret);\n",
    ] {
        let (ok, out) = check_with(body);
        assert!(!ok, "{body}: must be refused: {out}");
        assert!(
            out.contains(
                "private field 'secret' of struct 'P' is not visible outside its defining module"
            ),
            "{body}: {out}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_literals_open_element_types_meet_as_a_join_does() {
    // B-2026-10-25-6: elements that leave an inference variable or a
    // parameter open (`Vec.new()`, `None`) meet the others as an `if`/`else`
    // join's branches do, so the literal checks and runs; a real mismatch is
    // still refused.
    let src = "fn main() {\n    let mut a = [Vec.new(), Vec.new()];\n    a[0].push(7);\n    a[1].push(8);\n    \
               let b = [None, Some(1)];\n    let mut pts: Vec[Vec[i64]] = [Vec.new(), Vec.new()].to_vec();\n    \
               pts[1].push(5);\n    println(f\"{a[0][0]} {a[1][0]} {b[1].unwrap_or(0)} {pts[1][0]}\");\n}\n";
    accepted("literal-open-element-types", src);
    let (dir, path) = fixture("literal-open-element-types-run", src);
    let run = karac().arg("run").arg(&path).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "7 8 1 5\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    rejected(
        "literal-element-mismatch",
        "fn main() {\n    let a = [Vec.new(), Some(1)];\n    println(f\"{a.len()}\");\n}\n",
        "expected 'Vec[?T0]', found 'Option[i64]'",
    );
}

/// B-2026-10-25-13: three uses that move nothing out of a capture keep a
/// closure `Fn`: lending it to a user method's bare or `ref` parameter
/// (§4.1), using a captured `shared` handle as a value (§6.1), and a
/// `match` whose arms bind only `Copy` parts or `ref`s (§4.6). A method's
/// `own` parameter and an arm that binds a part by move still consume.
#[test]
fn a_closure_that_lends_counts_or_matches_a_capture_stays_fn() {
    accepted_and_runs(
        "closure-reads-not-consumes",
        "shared struct P {\n    n: i64,\n}\n\
         enum G {\n    Num(i64),\n    Name(String),\n}\n\
         struct H {\n    k: i64,\n}\n\
         impl H {\n    fn peek(self, p: P) -> i64 {\n        p.n + self.k\n    }\n}\n\
         fn twice(f: Fn(i64) -> i64) -> i64 {\n    f(1) + f(2)\n}\n\
         fn twice_p(f: Fn(i64) -> P) -> i64 {\n    f(1).n + f(2).n\n}\n\
         fn main() {\n\
             let a = P { n: 5 };\n\
             let h = H { k: 1 };\n\
             let x = twice(|i| i + h.peek(a));\n\
             let y = twice_p(|_| a);\n\
             let g = G.Name(\"ab\".to_string());\n\
             let z = twice(|i| match g {\n        G.Num(n) => n,\n        G.Name(_) => i,\n    });\n\
             println(f\"{x} {y} {z} {a.n}\");\n\
         }\n",
        "15 10 3 5\n",
    );
    for (tag, body, name) in [
        ("closure-own-method-arg", "i + h.eat(q)", "q"),
        (
            "closure-match-moves-part",
            "match g { G.Num(n) => n, G.Name(s) => s.len() + i }",
            "g",
        ),
    ] {
        let src = format!(
            "struct Q {{\n    n: i64,\n}}\n\
             struct H {{\n    k: i64,\n}}\n\
             impl H {{\n    fn eat(self, q: own Q) -> i64 {{\n        q.n + self.k\n    }}\n}}\n\
             enum G {{\n    Num(i64),\n    Name(String),\n}}\n\
             fn twice(f: Fn(i64) -> i64) -> i64 {{\n    f(1) + f(2)\n}}\n\
             fn main() {{\n\
                 let q = Q {{ n: 4 }};\n\
                 let h = H {{ k: 2 }};\n\
                 let g = G.Name(\"ab\".to_string());\n\
                 println(twice(|i| {body}));\n\
             }}\n"
        );
        let (dir, path) = fixture(tag, &src);
        let out = karac().arg("check").arg(&path).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{tag}: check must reject: {err}");
        assert!(
            err.contains(&format!("consumes captured binding '{name}'")),
            "{tag}: expected the consumed `{name}` in: {err}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// B-2026-10-25-14: an escaping or task closure moves a capture in once, at
/// its creation (§9.3), so reading it again in the body, twice or in a loop,
/// is no use after a move. A use after the closure is created, and a real
/// move in the body followed by a read, are still refused.
#[test]
fn an_escaping_closure_reads_its_capture_again() {
    accepted_and_runs(
        "escaping-capture-read-twice",
        "struct Prog {\n    ops: Vec[i64],\n}\n\
         fn size(p: ref Prog) -> i64 {\n    p.ops.len()\n}\n\
         fn keep(f: escaping Fn(i64) -> i64) -> Vec[Fn(i64) -> i64] {\n    Vec[f]\n}\n\
         fn main() {\n\
             let prog = Prog { ops: Vec[1, 2, 3] };\n\
             let fs = keep(|x| x + size(prog) + size(prog));\n\
             let q = Prog { ops: Vec[1] };\n\
             let gs = keep(|x| {\n        let mut t = x;\n        for _ in 0..3 {\n            t += size(q);\n        }\n        t\n    });\n\
             println(f\"{(fs[0])(1)} {(gs[0])(1)}\");\n\
         }\n",
        "7 4\n",
    );
    for (tag, body, want) in [
        (
            "escaping-capture-used-after",
            "let fs = keep(|x| x + size(prog));\n    println(size(prog));",
            "was moved into an escaping closure",
        ),
        (
            "escaping-capture-moved-then-read",
            "let r = keep1(|x| { let a = eat(prog); x + a + size(prog) });\n    println(r);",
            "moved here, used again here",
        ),
    ] {
        let src = format!(
            "struct Prog {{\n    ops: Vec[i64],\n}}\n\
             fn size(p: ref Prog) -> i64 {{\n    p.ops.len()\n}}\n\
             fn eat(p: own Prog) -> i64 {{\n    p.ops.len()\n}}\n\
             fn keep(f: escaping Fn(i64) -> i64) -> Vec[Fn(i64) -> i64] {{\n    Vec[f]\n}}\n\
             fn keep1(f: escaping OnceFn(i64) -> i64) -> i64 {{\n    f(1)\n}}\n\
             fn main() {{\n\
                 let prog = Prog {{ ops: Vec[1, 2, 3] }};\n\
                 {body}\n\
             }}\n"
        );
        let (dir, path) = fixture(tag, &src);
        let out = karac().arg("check").arg(&path).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{tag}: check must reject: {err}");
        assert!(err.contains(want), "{tag}: expected `{want}` in: {err}");
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// B-2026-10-26-9: a closure that returns a closure hands it to its caller,
/// so the returned closure escapes and moves what it captures (§9.3); the
/// outer closure then moves that capture out and is an `OnceFn` (§9.6). A
/// second call, or a use of the capture after the outer closure is made, is
/// refused; one call checks and runs.
#[test]
fn a_closure_returning_a_capturing_closure_is_once() {
    accepted_and_runs(
        "returned-closure-once-call",
        "fn main() {\n    let base = vec![1, 2, 3];\n    \
         let make = |n: i64| |x: i64| x + n + base.len();\n    \
         let f = make(1);\n    println(f\"{f(0)}\");\n}\n",
        "4\n",
    );
    for (tag, tail, want) in [
        (
            "returned-closure-second-call",
            "let f = make(1);\n    let g = make(2);\n    println(f\"{f(0)} {g(0)}\");",
            "an OnceFn is called once",
        ),
        (
            "returned-closure-capture-used-after",
            "let f = make(1);\n    println(f\"{f(0)} {base.len()}\");",
            "value 'base' was moved into an escaping closure",
        ),
    ] {
        let src = format!(
            "fn main() {{\n    let base = vec![1, 2, 3];\n    \
             let make = |n: i64| |x: i64| x + n + base.len();\n    {tail}\n}}\n"
        );
        let (dir, path) = fixture(tag, &src);
        let out = karac().arg("check").arg(&path).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{tag}: check must reject: {err}");
        assert!(err.contains(want), "{tag}: expected `{want}` in: {err}");
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// B-2026-10-26-12: a closure nested in an escaping closure's body reads the
/// escaping closure's own copy of a capture (§9.3), so a later read, or a
/// read in another `match` arm, is no use after a move.
#[test]
fn a_nested_closure_reads_an_escaping_closures_capture() {
    accepted_and_runs(
        "nested-closure-reads-escaping-capture",
        "struct Prog {\n    w: Vec[i64],\n}\n\
         enum G {\n    One(i64),\n    Many(Vec[i64]),\n}\n\
         fn wrap(run: escaping Fn(i64) -> i64) -> Vec[Fn(i64) -> i64] {\n    Vec[run]\n}\n\
         fn look(p: ref Prog, g: i64) -> i64 {\n    p.w[g]\n}\n\
         fn main() {\n\
             let p = Prog { w: Vec[10, 20, 30] };\n\
             let is: Vec[i64] = Vec[0, 2];\n\
             let fs = wrap(|x| {\n        let b: i64 = is.iter().map(|i| look(p, *i)).sum();\n        let a = look(p, 1);\n        a + b + x\n    });\n\
             let q = Prog { w: Vec[10, 20, 30] };\n\
             let g1 = G.One(1);\n\
             let gs = wrap(|x| match g1 {\n        G.One(i) => look(q, i) + x,\n        G.Many(ref js) => js.iter().map(|i| look(q, *i)).sum() + x,\n    });\n\
             println(f\"{(fs[0])(1)} {(gs[0])(1)}\");\n\
         }\n",
        "61 21\n",
    );
}

/// B-2026-10-26-13: a function value is CrossTask only when everything its
/// environment reaches is (core semantics §6.3), through nested closures
/// and erased `Fn` values. A spawned closure holding one that captures a
/// `shared` handle, the same passed to a function whose parameter is
/// spawned, and a `Server.serve` handler over a `shared` store are refused;
/// a `sync` store and a plain capture check and run.
#[test]
fn a_function_value_reaching_a_shared_handle_does_not_cross_a_task() {
    accepted_and_runs(
        "fn-env-sync-store",
        "sync struct Store {\n\
             n: Atomic[i64],\n\
         }\n\
         \n\
         fn main() {\n\
             let s = Store { n: Atomic.new(0) };\n\
             let bump: Fn() -> i64 = || s.n.load() + 1;\n\
             let g = TaskGroup.new();\n\
             let mut hs: Vec[TaskHandle[i64]] = Vec.new();\n\
             let mut i = 0;\n\
             while i < 4 {\n\
                 hs.push(g.spawn(|| bump()));\n\
                 i = i + 1;\n\
             }\n\
             let mut t = 0;\n\
             for h in hs.into_iter() {\n\
                 t = t + h.join();\n\
             }\n\
             println(t);\n\
         }\n",
        "4\n",
    );
    accepted_and_runs(
        "fn-env-plain-capture",
        "fn twice(f: Fn(i64) -> i64, x: i64) -> i64 {\n\
             f(f(x))\n\
         }\n\
         \n\
         fn main() {\n\
             let k = 3;\n\
             let add: Fn(i64) -> i64 = |x| x + k;\n\
             let g = TaskGroup.new();\n\
             let h = g.spawn(|| twice(add, 1));\n\
             println(h.join());\n\
         }\n",
        "7\n",
    );
    for (tag, src, name) in [
        (
            "fn-env-spawned-fn-value",
            "// Core semantics §6.3: a function value is CrossTask only when everything\n\
         // its environment reaches is. `bump` captures the `shared` handle `s`, so\n\
         // the spawned closure that holds `bump` cannot cross a task boundary.\n\
         shared struct Store {\n\
             mut n: i64,\n\
         }\n\
         \n\
         fn main() {\n\
             let s = Store { n: 0 };\n\
             let bump: Fn() -> i64 = || {\n\
                 let t = s.clone();\n\
                 t.n\n\
             };\n\
             let g = TaskGroup.new();\n\
             let h = g.spawn(|| bump());\n\
             println(h.join());\n\
         }\n",
            "`s`",
        ),
        (
            "fn-env-spawned-param",
            "shared struct Store {\n\
             mut n: i64,\n\
         }\n\
         \n\
         fn run_all(f: Fn() -> i64) -> i64 {\n\
             let g = TaskGroup.new();\n\
             let h = g.spawn(|| f());\n\
             h.join()\n\
         }\n\
         \n\
         fn main() {\n\
             let s = Store { n: 2 };\n\
             println(run_all(|| s.n));\n\
         }\n",
            "`s`",
        ),
        (
            "fn-env-nested-fn-value",
            "shared struct Store {\n\
             mut n: i64,\n\
         }\n\
         \n\
         fn main() {\n\
             let s = Store { n: 2 };\n\
             let inner: Fn() -> i64 = || s.n;\n\
             let outer: Fn() -> i64 = || inner() + 1;\n\
             let g = TaskGroup.new();\n\
             let h = g.spawn(|| outer());\n\
             println(h.join());\n\
         }\n",
            "`s`",
        ),
        (
            "fn-env-server-handler",
            "shared struct Hits {\n\
             mut n: i64,\n\
         }\n\
         \n\
         fn handle(h: Hits, req: Request) -> Response {\n\
             h.n = h.n + 1;\n\
             Response { status: 200, body: f\"{h.n}\" }\n\
         }\n\
         \n\
         fn main() {\n\
             let hits = Hits { n: 0 };\n\
             let _ = Server.serve(\"127.0.0.1:8080\", |req| handle(hits, req));\n\
         }\n",
            "`hits`",
        ),
    ] {
        let (dir, path) = fixture(tag, src);
        let out = karac().arg("check").arg(&path).output().unwrap();
        let err = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{tag}: check must reject: {err}");
        assert!(
            err.contains("E_NOT_CROSS_TASK") && err.contains(name),
            "{tag}: expected E_NOT_CROSS_TASK naming {name} in: {err}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// B-2026-10-26-10: writing a `mut` field through a captured `shared`
/// handle leaves the capture, the handle, as it was (§6.2), so the closure
/// is an `Fn` (§9.6), directly and through a struct field holding a handle.
/// Writing a field of an owned capture, or assigning the handle itself,
/// still makes it a `MutFn`.
#[test]
fn a_closure_writing_through_a_captured_handle_is_fn() {
    accepted_and_runs(
        "handle-write-field",
        "shared struct C {\n\
             mut stream: i64,\n\
         }\n\
         \n\
         fn twice(f: Fn(i64) -> i64) -> i64 {\n\
             f(1) + f(2)\n\
         }\n\
         \n\
         fn main() {\n\
             let cache = C { stream: 0 };\n\
             println(twice(|i| {\n\
                 cache.stream = cache.stream + i;\n\
                 cache.stream\n\
             }));\n\
             println(cache.stream);\n\
         }\n",
        "4\n3\n",
    );
    accepted_and_runs(
        "handle-write-push",
        "shared struct Log {\n\
             mut items: Vec[i64],\n\
             mut n: i64,\n\
         }\n\
         \n\
         struct Box2 {\n\
             h: Log,\n\
         }\n\
         \n\
         fn twice(f: Fn(i64) -> i64) -> i64 {\n\
             f(1) + f(2)\n\
         }\n\
         \n\
         fn main() {\n\
             let log = Log { items: Vec.new(), n: 0 };\n\
             let b = Box2 { h: log.clone() };\n\
             println(twice(|i| {\n\
                 log.items.push(i * 10);\n\
                 b.h.n = b.h.n + i;\n\
                 log.items.len() + b.h.n\n\
             }));\n\
             println(f\"{log.items.len()} {log.n}\");\n\
         }\n",
        "7\n2 3\n",
    );
    for (tag, src, name) in [
        (
            "owned-field-write",
            "struct P {\n\
             mut n: i64,\n\
         }\n\
         \n\
         fn twice(f: Fn(i64) -> i64) -> i64 {\n\
             f(1) + f(2)\n\
         }\n\
         \n\
         fn main() {\n\
             let mut p = P { n: 0 };\n\
             println(twice(|i| {\n\
                 p.n = p.n + i;\n\
                 p.n\n\
             }));\n\
         }\n",
            "mutates p",
        ),
        (
            "handle-rebind",
            "shared struct C {\n\
             mut stream: i64,\n\
         }\n\
         \n\
         fn twice(f: Fn(i64) -> i64) -> i64 {\n\
             f(1) + f(2)\n\
         }\n\
         \n\
         fn main() {\n\
             let mut cache = C { stream: 0 };\n\
             println(twice(|i| {\n\
                 cache = C { stream: i };\n\
                 cache.stream\n\
             }));\n\
         }\n",
            "mutates cache",
        ),
    ] {
        let (dir, path) = fixture(tag, src);
        let out = karac().arg("check").arg(&path).output().unwrap();
        let err = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{tag}: check must reject: {err}");
        assert!(
            err.contains(name) && err.contains("MutFn"),
            "{tag}: expected a MutFn error naming {name} in: {err}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// B-2026-10-26-11: an escaping closure moves its captures in when it is
/// created (§9.3), so one created in a loop moves an outer non-`Copy`
/// capture again each iteration (§3.3), in a `for` or a `while`, even when
/// its body only reads it in a loop of its own. A clone made inside the
/// loop, a non-escaping closure and a task closure that borrows check and
/// run.
#[test]
fn an_escaping_closure_made_in_a_loop_moves_its_capture_each_time() {
    accepted_and_runs(
        "loop-escaping-clone",
        "#[derive(Clone)]\n\
         struct Prog {\n\
             ops: Vec[i64],\n\
         }\n\
         \n\
         fn size(p: ref Prog) -> i64 {\n\
             p.ops.len()\n\
         }\n\
         \n\
         fn keep(f: escaping Fn(i64) -> i64) -> Vec[Fn(i64) -> i64] {\n\
             Vec[f]\n\
         }\n\
         \n\
         fn main() {\n\
             let prog = Prog { ops: Vec[1, 2, 3] };\n\
             for _ in 0..2 {\n\
                 let p = prog.clone();\n\
                 let fs = keep(|x| x + size(p));\n\
                 println((fs[0])(1));\n\
             }\n\
         }\n",
        "4\n4\n",
    );
    accepted_and_runs(
        "loop-nonescaping",
        "struct Prog {\n\
             ops: Vec[i64],\n\
         }\n\
         \n\
         fn size(p: ref Prog) -> i64 {\n\
             p.ops.len()\n\
         }\n\
         \n\
         fn keep(f: escaping Fn(i64) -> i64) -> Vec[Fn(i64) -> i64] {\n\
             Vec[f]\n\
         }\n\
         \n\
         fn main() {\n\
             let prog = Prog { ops: Vec[1, 2, 3] };\n\
             for _ in 0..2 {\n\
                 let f = |x: i64| x + size(prog);\n\
                 println(f(1));\n\
             }\n\
         }\n",
        "4\n4\n",
    );
    accepted_and_runs(
        "loop-spawn-borrows",
        "struct Prog {\n\
             ops: Vec[i64],\n\
         }\n\
         \n\
         fn size(p: ref Prog) -> i64 {\n\
             p.ops.len()\n\
         }\n\
         \n\
         fn keep(f: escaping Fn(i64) -> i64) -> Vec[Fn(i64) -> i64] {\n\
             Vec[f]\n\
         }\n\
         \n\
         fn main() {\n\
             let prog = Prog { ops: Vec[1, 2, 3] };\n\
             let g = TaskGroup.new();\n\
             let mut hs: Vec[TaskHandle[i64]] = Vec.new();\n\
             for _ in 0..2 {\n\
                 hs.push(g.spawn(|| size(prog)));\n\
             }\n\
             for h in hs.into_iter() {\n\
                 println(h.join());\n\
             }\n\
         }\n",
        "3\n3\n",
    );
    for (tag, src) in [
        (
            "loop-escaping-for",
            "struct Prog {\n\
             ops: Vec[i64],\n\
         }\n\
         \n\
         fn size(p: ref Prog) -> i64 {\n\
             p.ops.len()\n\
         }\n\
         \n\
         fn keep(f: escaping Fn(i64) -> i64) -> Vec[Fn(i64) -> i64] {\n\
             Vec[f]\n\
         }\n\
         \n\
         fn main() {\n\
             let prog = Prog { ops: Vec[1, 2, 3] };\n\
             for _ in 0..2 {\n\
                 let fs = keep(|x| x + size(prog));\n\
                 println((fs[0])(1));\n\
             }\n\
         }\n",
        ),
        (
            "loop-escaping-while",
            "struct Prog {\n\
             ops: Vec[i64],\n\
         }\n\
         \n\
         fn size(p: ref Prog) -> i64 {\n\
             p.ops.len()\n\
         }\n\
         \n\
         fn keep(f: escaping Fn(i64) -> i64) -> Vec[Fn(i64) -> i64] {\n\
             Vec[f]\n\
         }\n\
         \n\
         fn main() {\n\
             let prog = Prog { ops: Vec[1, 2, 3] };\n\
             let mut i = 0;\n\
             while i < 2 {\n\
                 let fs = keep(|x| {\n\
                     let mut t = x;\n\
                     for _ in 0..2 {\n\
                         t += size(prog);\n\
                     }\n\
                     t\n\
                 });\n\
                 println((fs[0])(1));\n\
                 i = i + 1;\n\
             }\n\
         }\n",
        ),
    ] {
        let (dir, path) = fixture(tag, src);
        let out = karac().arg("check").arg(&path).output().unwrap();
        let err = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{tag}: check must reject: {err}");
        assert!(
            err.contains("value 'prog' was moved into an escaping closure"),
            "{tag}: expected the escaping-capture error in: {err}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// B-2026-10-25-15: reusing a moved `Prog { ops: Vec[Op] }` takes two
/// derives, `Copy` on `Op` and then `Clone` on `Prog`. The hint calls the
/// first a first step instead of saying `prog` is then copied, and three
/// `karac fix` runs (the two derives, then the `.clone()`) give a program
/// that checks. A one-derive type still says the value is copied.
#[test]
fn a_nested_derive_fix_is_named_a_first_step() {
    let nested = "enum Op {\n\
             A(i64),\n\
             B,\n\
         }\n\
         \n\
         struct Prog {\n\
             ops: Vec[Op],\n\
         }\n\
         \n\
         fn eat(p: own Prog) -> i64 {\n\
             p.ops.len()\n\
         }\n\
         \n\
         fn main() {\n\
             let prog = Prog { ops: Vec[Op.A(1), Op.B] };\n\
             let a = eat(prog);\n\
             let b = eat(prog);\n\
             println(f\"{a} {b}\");\n\
         }\n";
    rejected_then_fixed_in(
        "nested-derive-first-step",
        nested,
        "add `#[derive(Copy)]` to 'Op' (its fields are all `Copy`), the first of the derives 'prog' needs before it can be cloned",
        "#[derive(Clone)]\nstruct Prog",
        3,
    );
    let (dir, path) = fixture(
        "one-derive-copies",
        "struct P {\n\
             n: i64,\n\
         }\n\
         \n\
         fn eat(p: own P) -> i64 {\n\
             p.n\n\
         }\n\
         \n\
         fn main() {\n\
             let p = P { n: 2 };\n\
             let a = eat(p);\n\
             let b = eat(p);\n\
             println(f\"{a} {b}\");\n\
         }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("add `#[derive(Copy)]` to 'P' (its fields are all `Copy`, so 'p' is copied instead of moved)"),
        "one-derive-copies: {err}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// B-2026-10-25-19: a string-literal `let` used as a `Str` is offered
/// `: Str`, which makes it `Copy`, so check does not also report its move
/// (whose `.clone()` fix-it would make it a `String` again). One `karac fix`
/// run gives a program that checks. A binding that also holds a `String`,
/// and a plain `String` moved twice, still report the move.
#[test]
fn a_str_retyped_literal_let_reports_no_move() {
    let retyped = "fn take(v: own Vec[(i64, Str)]) -> i64 {\n\
             v.len()\n\
         }\n\
         \n\
         fn main() {\n\
             let s = \"x\";\n\
             let a = take(vec![(0, s)]);\n\
             let b = take(vec![(1, s)]);\n\
             println(f\"{a} {b}\");\n\
         }\n";
    let (dir, path) = fixture("str-retyped-no-move", retyped);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("declare it `s: Str`") && !err.contains("moved here"),
        "str-retyped-no-move: {err}"
    );
    let _ = std::fs::remove_dir_all(dir);
    rejected_then_fixed_in(
        "str-retyped-fixed",
        retyped,
        "declare it `s: Str`",
        "let s: Str",
        1,
    );
    for (tag, src) in [
        (
            "str-reassigned-moves",
            "fn take(v: own Vec[(i64, Str)]) -> i64 {\n\
             v.len()\n\
         }\n\
         \n\
         fn name() -> String {\n\
             \"y\".to_string()\n\
         }\n\
         \n\
         fn main() {\n\
             let mut s = \"x\";\n\
             s = name();\n\
             let a = take(vec![(0, s)]);\n\
             let b = take(vec![(1, s)]);\n\
             println(f\"{a} {b}\");\n\
         }\n",
        ),
        (
            "string-moves",
            "fn eat(s: own String) -> i64 {\n\
             s.len()\n\
         }\n\
         \n\
         fn main() {\n\
             let s = \"x\";\n\
             let a = eat(s);\n\
             let b = eat(s);\n\
             println(f\"{a} {b}\");\n\
         }\n",
        ),
    ] {
        let (dir, path) = fixture(tag, src);
        let out = karac().arg("check").arg(&path).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("value 's' moved here"), "{tag}: {err}");
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// B-2026-10-24-28: a bare `None` passed to a `ref Option[T]` parameter
/// names no place, so a second `None` passed by value beside it is not a
/// move of a borrowed value.
#[test]
fn none_beside_a_ref_none_argument_is_no_borrowed_move() {
    accepted_and_runs(
        "none-ref-and-owned",
        "fn g(a: ref Option[String], b: Option[i64]) -> i64 {\n    7\n}\n\
         fn main() {\n    println(f\"{g(None, None)}\");\n}\n",
        "7\n",
    );
}

/// B-2026-10-24-38: a destructuring `let (a, b) = cases[i];` whose parts
/// are only read is fixed with the `ref cases[i]` borrow, as a plain
/// `let t = v[i];` is (B-2026-10-24-18); a part that is moved still gets
/// `.clone()`.
#[test]
fn a_read_only_destructured_index_let_is_fixed_by_a_borrow() {
    rejected_then_fixed_in(
        "index-let-tuple-borrow",
        "fn total(v: Vec[i64]) -> i64 {\n\
             let mut s = 0;\n\
             for x in v.iter() {\n\
                 s += x;\n\
             }\n\
             s\n\
         }\n\
         \n\
         fn main() {\n\
             let mut cases: Vec[(Vec[i64], Vec[i64])] = Vec.new();\n\
             cases.push((vec![1, 2], vec![3]));\n\
             cases.push((vec![4], vec![5, 6]));\n\
             let mut sum = 0;\n\
             for i in 0..2 {\n\
                 let (a, b) = cases[i];\n\
                 sum += total(a) + total(b);\n\
             }\n\
             println(f\"{sum}\");\n\
         }\n",
        "The binding is only read, so the fix borrows it",
        "let (a, b) = ref cases[i];",
        1,
    );
    rejected_then_fixed_in(
        "index-let-tuple-moved",
        "fn total(v: own Vec[i64]) -> i64 {\n\
             let mut s = 0;\n\
             for x in v.iter() {\n\
                 s += x;\n\
             }\n\
             s\n\
         }\n\
         \n\
         fn main() {\n\
             let mut cases: Vec[(Vec[i64], Vec[i64])] = Vec.new();\n\
             cases.push((vec![1, 2], vec![3]));\n\
             cases.push((vec![4], vec![5, 6]));\n\
             let mut sum = 0;\n\
             for i in 0..2 {\n\
                 let (a, b) = cases[i];\n\
                 sum += total(a) + total(b);\n\
             }\n\
             println(f\"{sum}\");\n\
         }\n",
        "E_INDEX_MOVE_NON_COPY",
        "let (a, b) = cases[i].clone();",
        3,
    );
}

/// B-2026-10-26-16: an ASCII char literal cast to `u8` is that byte exactly,
/// so E_CHAR_AS_NARROW_INT names the byte literal and `karac fix` writes it
/// (`'#' as u8` to `b'#'`, with the quote, backslash and tab escapes). A
/// non-ASCII literal keeps the truncation help and gets no edit.
#[test]
fn an_ascii_char_literal_cast_to_u8_is_fixed_to_a_byte_literal() {
    rejected_then_fixed_in(
        "char-literal-as-u8",
        "fn main() {\n\
             let s = \"a#b'c\\\\d\\te\";\n\
             let bs = s.bytes();\n\
             let mut n = 0;\n\
             for b in bs.iter() {\n\
                 if *b == '#' as u8 or *b == '\\'' as u8 or *b == '\\\\' as u8 or *b == '\\t' as u8 {\n\
                     n += 1;\n\
                 }\n\
             }\n\
             let c = 'é';\n\
             println(f\"{n} {c as u32 as u8}\");\n\
         }\n",
        "'#' is ASCII, so write the byte literal `b'#'`",
        "*b == b'#' or *b == b'\\'' or *b == b'\\\\' or *b == b'\\t'",
        1,
    );
    let (dir, path) = fixture(
        "non-ascii-char-as-u8",
        "fn main() {\n\
             let x = 'é' as u8;\n\
             println(x);\n\
         }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("`c as u32 as u8` for explicit truncation") && !err.contains("byte literal"),
        "non-ascii-char-as-u8: {err}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// B-2026-10-25-28: each binding of a destructuring `let` is its own place,
/// so a task's capture of `rx` (from `let (tx, rx) = channel(1)`) does not
/// make a later use of `tx` a use of what the task borrows (§9.5). Writing
/// the captured half while the group is open is still refused.
#[test]
fn a_task_capturing_one_half_of_a_tuple_let_leaves_the_other_free() {
    accepted_and_runs(
        "task-captures-rx-moves-tx",
        "fn count(rxs: own Vec[Receiver[String]]) -> i64 {\n\
             rxs.len()\n\
         }\n\
         \n\
         fn main() {\n\
             let (tx, rx): (Sender[String], Receiver[String]) = channel(1);\n\
             let group = TaskGroup.new();\n\
             let h = group.spawn(|| count([rx]));\n\
             println(f\"{h.join()}\");\n\
             let _ = tx;\n\
         }\n",
        "1\n",
    );
    accepted_and_runs(
        "task-captures-a-writes-b",
        "fn main() {\n\
             let mut (a, b) = (Vec[1, 2], Vec[3]);\n\
             let g = TaskGroup.new();\n\
             let h = g.spawn(|| a.len());\n\
             b.push(9);\n\
             println(f\"{h.join()} {b.len()}\");\n\
         }\n",
        "2 2\n",
    );
    let (dir, path) = fixture(
        "task-captures-a-writes-a",
        "fn main() {\n\
             let mut (a, b) = (Vec[1, 2], Vec[3]);\n\
             let g = TaskGroup.new();\n\
             let h = g.spawn(|| a.len());\n\
             a.push(9);\n\
             println(f\"{h.join()} {b.len()}\");\n\
         }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("`a` is written while a task of `g` borrows it"),
        "task-captures-a-writes-a: {err}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// B-2026-10-21-29: `karac check` follows collections.md on library borrows.
/// Set algebra borrows `other` (bare, so a borrow under D5); a `split_at_mut`
/// half is a `mut Slice` that takes an index write without `let mut`, and the
/// sequence is not read while either half is live; a set has no `iter_mut`.
#[test]
fn collections_md_library_borrows_hold_in_check() {
    accepted_and_runs(
        "set-algebra-reuses-other",
        "fn sorted(s: Set[i64]) -> Vec[i64] {\n\
             let mut v: Vec[i64] = Vec.new();\n\
             for x in s { v.push(x); }\n\
             v.sort();\n\
             v\n\
         }\n\
         fn main() {\n\
             let mut a: Set[i64] = Set.new();\n\
             let mut b: Set[i64] = Set.new();\n\
             a.insert(1); a.insert(2); a.insert(3);\n\
             b.insert(2); b.insert(3); b.insert(4);\n\
             println(f\"{sorted(a.union(b))}\");\n\
             println(f\"{sorted(a.intersection(b))}\");\n\
             println(f\"{sorted(a.difference(b))}\");\n\
             println(f\"{a.len()} {b.len()}\");\n\
         }\n",
        "[1, 2, 3, 4]\n[2, 3]\n[1]\n3 3\n",
    );
    accepted_and_runs(
        "sortedset-algebra-reuses-other",
        "fn main() {\n\
             let mut a: SortedSet[i64] = SortedSet.new();\n\
             let mut b: SortedSet[i64] = SortedSet.new();\n\
             for x in [3, 1, 2] { a.insert(x); }\n\
             for x in [4, 3, 2] { b.insert(x); }\n\
             println(f\"{a.union(b)}\");\n\
             println(f\"{a.intersection(b)}\");\n\
             println(f\"{a.difference(b)}\");\n\
             println(f\"{a} {b}\");\n\
         }\n",
        "{1, 2, 3, 4}\n{2, 3}\n{1}\n{1, 2, 3} {2, 3, 4}\n",
    );
    accepted_and_runs(
        "split-at-mut-half-index-write",
        "fn main() {\n\
             let mut v: Vec[i64] = [1, 2, 3, 4];\n\
             let (a, b) = v.split_at_mut(2);\n\
             a[0] = 10;\n\
             b[0] = 30;\n\
             println(f\"{v}\");\n\
         }\n",
        "[10, 2, 30, 4]\n",
    );
    let (dir, path) = fixture(
        "len-while-halves-live",
        "fn main() {\n\
             let mut v: Vec[i64] = [1, 2, 3, 4];\n\
             let (a, b) = v.split_at_mut(2);\n\
             println(f\"{v.len()}\");\n\
             a.fill(10);\n\
             b.fill(30);\n\
         }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("cannot borrow `v` while `b` still holds a borrow of it"),
        "len-while-halves-live: {err}"
    );
    let _ = std::fs::remove_dir_all(dir);
    let (dir, path) = fixture(
        "set-iter-mut",
        "fn main() {\n\
             let mut s: Set[i64] = Set.new();\n\
             s.insert(1);\n\
             for x in s.iter_mut() { println(f\"{x}\"); }\n\
         }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no method 'iter_mut' on type 'Set'"),
        "set-iter-mut: {err}"
    );
    let _ = std::fs::remove_dir_all(dir);
    let (dir, path) = fixture(
        "sortedset-iter-mut",
        "fn main() {\n\
             let mut s: SortedSet[i64] = SortedSet.new();\n\
             s.insert(1);\n\
             for x in s.iter_mut() { println(f\"{x}\"); }\n\
         }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no method 'iter_mut' on type 'SortedSet'"),
        "sortedset-iter-mut: {err}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// B-2026-10-26-18: a `VecDeque` has collections.md's surface (857e45825),
/// not `Vec`'s. The page's `contains`, `remove`, `swap`, `retain`,
/// `truncate` and `extend` run (written in the library, over the native
/// ends); `push` / `pop` / `first` / `last` are fixed to the deque's names,
/// and a method that needs one slice points at `Vec.from_iter(dq)`.
#[test]
fn a_vecdeque_has_the_page_surface_not_vecs() {
    accepted_and_runs(
        "deque-page-methods",
        "fn show(d: VecDeque[String]) -> String {\n\
             let mut s = \"\";\n\
             for x in d.iter() { s = s + x + \" \"; }\n\
             s\n\
         }\n\
         fn main() {\n\
             let mut d: VecDeque[String] = VecDeque.new();\n\
             for w in [\"a\", \"bb\", \"c\", \"dd\"] { d.push_back(w.to_string()); }\n\
             d.push_front(\"z\".to_string());\n\
             println(f\"{d.contains(\"c\".to_string())} {d.contains(\"q\".to_string())}\");\n\
             let r = d.remove(2);\n\
             println(f\"{r} | {show(d)}\");\n\
             d.swap(0, 2);\n\
             println(show(d));\n\
             d.retain(|x| x.len() == 1);\n\
             println(show(d));\n\
             let v: Vec[String] = [\"p\".to_string(), \"qq\".to_string()];\n\
             d.extend(v);\n\
             let mut set: SortedSet[String] = SortedSet.new();\n\
             set.insert(\"s1\".to_string());\n\
             set.insert(\"s2\".to_string());\n\
             d.extend(set);\n\
             let mut e: VecDeque[String] = VecDeque.new();\n\
             e.push_back(\"e\".to_string());\n\
             d.extend(e);\n\
             let w = [\"i1\", \"i2\"];\n\
             d.extend(w.iter().map(|x| x.to_string()));\n\
             println(show(d));\n\
             d.truncate(2);\n\
             println(f\"{show(d)} {d.len()}\");\n\
         }\n",
        "true false\nbb | z a c dd \nc a z dd \nc a z \nc a z p qq s1 s2 e i1 i2 \nc a  2\n",
    );
    rejected_then_fixed_in(
        "deque-vec-names",
        "fn main() {\n\
             let mut d: VecDeque[i64] = VecDeque.new();\n\
             d.push(3);\n\
             d.push(1);\n\
             println(f\"{d.first()}\");\n\
             let x = d.pop();\n\
             println(f\"{x}\");\n\
         }\n",
        "no method 'push' on type 'VecDeque': a `VecDeque` names it `push_back`",
        "d.pop_back()",
        1,
    );
    let (dir, path) = fixture(
        "deque-slice-methods",
        "fn main() {\n\
             let mut d: VecDeque[i64] = VecDeque.new();\n\
             d.push_back(3);\n\
             d.sort();\n\
             println(f\"{d.max()}\");\n\
         }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no method 'sort' on type 'VecDeque'")
            && err.contains("`Vec.from_iter(dq)`")
            && err.contains("`dq.iter().max()`"),
        "deque-slice-methods: {err}"
    );
    let _ = std::fs::remove_dir_all(dir);
}
