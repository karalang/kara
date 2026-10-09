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
        "fn take(s: String) { println(s); }\n\
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
        "fn take(s: String) { println(s); }\n\
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
        "fn take(s: String) { println(s); }\n\
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
         impl Bag { fn add(mut ref self, s: String) { self.items.push(s); } }\n\
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
        "fn take(s: String) { println(s); }\n\
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
/// one. `karac fix` rewrites the impl to the trait's mode.
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
    for expect in [
        "impl method 'P.grab' takes its receiver as `self`, but 'Grab.grab' declares `ref self`",
        "impl method 'P.grab' takes 'v' by value, but 'Grab.grab' takes it as `ref`",
        "impl method 'P.grab' takes 'w' as `mut ref`, but 'Grab.grab' takes it as `ref`",
    ] {
        assert!(err.contains(expect), "expected `{expect}` in: {err}");
    }
    karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        fixed.contains("fn grab(ref self, v: ref Vec[i64], w: ref Vec[i64]) -> i64 {"),
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
        "fn take(s: String) { println(s); }\n\
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
         fn render(t: T) -> String { t.name }\n\
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
         fn render(s: S) -> String { s.name }\n\
         fn main() {\n\
             let v = vec![S { name: \"a\".to_string() }];\n\
             for s in v.into_iter() {\n\
                 println(render(s));\n\
             }\n\
         }\n",
    );
}

#[test]
fn a_result_holding_a_handle_moves() {
    // §6.1: only `Option`s and tuples of handles copy by counting; a
    // `Result` holding one is move-only, so reusing it is a use after move.
    // (`Result` has no `.clone()` yet, so there is no fix to apply.)
    let (dir, path) = fixture(
        "result-handle",
        "shared struct Node { v: i64 }\n\
         fn take(r: Result[Node, i64]) -> i64 { match r { Ok(n) => n.v, Err(e) => e } }\n\
         fn main() {\n\
             let r: Result[Node, i64] = Ok(Node { v: 1 });\n\
             let a = take(r);\n\
             println(a + take(r));\n\
         }\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must reject: {err}");
    assert!(err.contains("value 'r' moved here"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
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
         fn eat(v: Vec[String]) -> i64 { v.len() }\n\
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
         fn eat(s: String) -> i64 { s.len() }\n\
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
        "fn take(s: String) -> i64 { s.len() }\n\
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
        "fn take(s: String) -> i64 { s.len() }\n\
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
         fn take(s: String) -> i64 { s.len() }\n\
         fn main() { for s in mk() { println(take(s)); } }\n",
        "cannot move 's'",
    );
    rejected_then_fixed(
        "for-map-pair",
        "fn take(s: String) -> i64 { s.len() }\n\
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
         fn take(s: String) -> i64 { s.len() }\n\
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
         fn eat(v: Vec[String]) -> i64 { v.len() }\n\
         fn f(h: H) -> i64 { match h { H.Y { v } => eat(v), H.N => 0 } }\n\
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
        "fn take(s: String) { println(s); }\n\
         fn main() {\n\
             let mut t = \"a\".to_string();\n\
             let mut i = 0;\n\
             while i < 2 { t = \"b\".to_string(); take(t); i = i + 1; }\n\
         }\n",
        "not assigned again",
    );
    accepted(
        "loop-reassign-after",
        "fn take(s: String) { println(s); }\n\
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
    // The fixed form, `w.inner.s.clone()`, trips the legacy backend's
    // `chained_field_receiver` deferral, so this case checks the rejection
    // only; the direct case above covers the `.clone()` fix.
    rejected(
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
        "fn take(x: Vec[i64]) -> i64 { return x.len() }\n\
         fn main() {\n\
             let v: Vec[(Vec[i64], i64)] = [([1], 1)];\n\
             for pair in v.iter() { match pair { (a, j) => println(take(a)) } }\n\
         }\n",
        "it borrows its value",
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
    rejected(
        "taskgroup-origin-declared-after",
        &format!(
            "{total}fn main() {{\n\
                 let mut g = TaskGroup.new();\n\
                 let data: Vec[i64] = [1, 2, 3];\n\
                 let h = g.spawn(|| total(data));\n\
                 println(h.join());\n\
             }}\n"
        ),
        "`data` drops before `g`",
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
             fn take(w: W) -> R {{ return w.r; }}\n\
             fn main() {{ let r = take(W {{ r: R {{ name: \"a\" }}, n: 1 }}); println(r.name); }}\n"
        ),
        "`#[allow(partial_move_of_drop_struct)]` here has no effect any more",
    );
    rejected(
        "allow-pattern-move",
        &format!(
            "{drop_w}#[allow(partial_move_of_drop_struct)]\n\
             fn take(w: W) -> R {{ let W {{ r, n }} = w; r }}\n\
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
        "fn take(s: String) -> i64 { s.len() }\n\
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
         fn delete(k: String) -> bool { k.len() > 0 }\n\
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
         fn run(j: Job) { println(j.id) }\n\
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
         fn take(r: Res) { println(r.id) }\n\
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
         fn run(j: Job) { println(j.id) }\n\
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
         fn take(p: P) -> i64 { p.x + p.y }\n\
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
         fn show(d: Dir) -> i64 { match d { Dir.North => 0, Dir.South(n) => n } }\n\
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
             fn take(n: N) {}\n\
             fn main() { let n = N { s: \"a\" }; take(n); take(n); }\n",
        ),
        (
            "derive-copy-drop",
            "struct D { x: i64 }\n\
             impl Drop for D { fn drop(mut ref self) { println(self.x) } }\n\
             fn take(d: D) {}\n\
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
/// payload binding. A non-`Copy` value is read in place instead.
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
    rejected_then_fixed_by(
        "map-get-deref-string",
        "fn main() {\n\
             let mut m: Map[String, String] = Map.new();\n\
             m.insert(\"a\", \"x\");\n\
             match m.get(\"a\") {\n\
                 Some(c) => println(f\"{*c}\"),\n\
                 None => {},\n\
             }\n\
         }\n",
        "out of a borrow with `*`",
        "println(f\"{c}\")",
    );
}

/// The `#[derive(Copy)]` fix covers a move inside a loop and a moved field.
#[test]
fn derive_copy_fixes_loop_and_field_moves() {
    rejected_then_fixed_by(
        "derive-copy-loop",
        "enum Reason { Capacity, Deleted }\n\
         fn note(r: Reason) -> i64 { match r { Reason.Capacity => 1, Reason.Deleted => 2 } }\n\
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
        "`Option.cloned` needs a payload with `.clone()`",
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
