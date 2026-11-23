//! Diagnostics whose help names one edit carry it (B-2026-11-21-74): a
//! reassigned `let x: T;` gets `mut`, a misspelled `break` label its one
//! suggestion, `fn drop[T]` loses its generic list, an argument-position
//! `impl Trait` a trait method or a nested type cannot take becomes the
//! explicit generic parameter, and the acronym rename `karac fix` applies is
//! in the JSON record.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// What `karac check --output=json` says of `src` before any fix, then the
/// text `karac fix` leaves, whether that checks clean, and its output under
/// the MIR interpreter.
fn mend(name: &str, src: &str) -> (Vec<serde_json::Value>, String, bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-row-74-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let (_, json, _) = karac(&dir, &["check", "--output=json", "cell.kara"]);
    let doc: serde_json::Value =
        serde_json::from_str(json.trim()).unwrap_or_else(|e| panic!("{e}: {json}"));
    let diags = doc["diagnostics"].as_array().cloned().unwrap_or_default();
    karac(&dir, &["fix", "cell.kara"]);
    let text = std::fs::read_to_string(dir.join("cell.kara")).unwrap();
    let (clean, _, _) = karac(&dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (diags, text, clean, out)
}

fn carries_a_fix(d: &serde_json::Value) -> bool {
    d["replacement"].is_object() || d["fix_diff"].is_array()
}

/// `let x: i64;` assigned twice: each reassignment's error carries `mut` at
/// the declaration, and the one edit clears them all.
#[test]
fn a_reassigned_uninitialised_let_gets_mut_at_its_declaration() {
    let (diags, text, clean, out) = mend(
        "uninit",
        "fn main() {\n    let x: i64;\n    x = 5;\n    x = 6;\n    x = x + 1;\n    println(x);\n}\n",
    );
    let reassigns: Vec<_> = diags.iter().filter(|d| d["code"] == "E0506").collect();
    assert_eq!(reassigns.len(), 2, "{diags:?}");
    for d in &reassigns {
        assert_eq!(d["replacement"]["text"], "mut ", "{d}");
    }
    assert_eq!(
        text,
        "fn main() {\n    let mut x: i64;\n    x = 5;\n    x = 6;\n    x = x + 1;\n    println(x);\n}\n"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "7\n");
}

/// A shadowing `let x: i64;` is its own declaration: the `mut` goes on the
/// one the reassignment writes.
#[test]
fn the_mut_goes_on_the_declaration_the_reassignment_writes() {
    let (_, text, clean, out) = mend(
        "uninit-shadow",
        "fn main() {\n    let x: i64;\n    x = 1;\n    println(x);\n    let x: i64;\n    x = 2;\n    x = 3;\n    println(x);\n}\n",
    );
    assert_eq!(
        text,
        "fn main() {\n    let x: i64;\n    x = 1;\n    println(x);\n    let mut x: i64;\n    x = 2;\n    x = 3;\n    println(x);\n}\n"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "1\n3\n");
}

/// `continue 'outr` carried the rename (B-2026-07-07-3); `break 'outr` did
/// not.
#[test]
fn a_misspelled_break_label_takes_its_one_suggestion() {
    let (diags, text, clean, out) = mend(
        "label",
        "fn main() {\n    let mut n = 0;\n    'outer: for i in 0..3 {\n        for j in 0..3 {\n            if j == 2 {\n                break 'outr;\n            }\n            n += i + j;\n        }\n    }\n    println(n);\n}\n",
    );
    let d = diags
        .iter()
        .find(|d| d["code"] == "E0107")
        .unwrap_or_else(|| panic!("{diags:?}"));
    assert_eq!(d["replacement"]["text"], "outer", "{d}");
    assert!(text.contains("break 'outer;"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "1\n");
}

/// A `break` with a value names its label the same way.
#[test]
fn a_misspelled_label_on_a_break_with_a_value_is_renamed() {
    let (_, text, clean, out) = mend(
        "label-value",
        "fn main() {\n    let n = 'find: loop {\n        break 'fnd 4;\n    };\n    println(n);\n}\n",
    );
    assert!(text.contains("break 'find 4;"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "4\n");
}

#[test]
fn a_drop_methods_generic_list_is_deleted() {
    let (diags, text, clean, out) = mend(
        "drop-generics",
        "struct Conn { id: i64 }\n\nimpl Drop for Conn {\n    fn drop[T](mut ref self) {\n        println(self.id);\n    }\n}\n\nfn main() {\n    let c = Conn { id: 7 };\n    println(c.id + 1);\n}\n",
    );
    let d = diags
        .iter()
        .find(|d| d["code"] == "E_DROP_SIGNATURE_INVALID")
        .unwrap_or_else(|| panic!("{diags:?}"));
    assert_eq!(d["replacement"]["text"], "", "{d}");
    assert!(text.contains("fn drop(mut ref self) {"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "8\n7\n");
}

/// The body names the parameter, so deleting the list would leave `T`
/// undefined: the diagnostic keeps its message and carries no edit.
#[test]
fn a_drop_generic_list_the_body_uses_is_left_to_the_author() {
    let src = "struct Conn { id: i64 }\n\nimpl Drop for Conn {\n    fn drop[T](mut ref self) {\n        let n: Option[T] = None;\n        println(self.id);\n    }\n}\n\nfn main() {\n    let c = Conn { id: 7 };\n    println(c.id);\n}\n";
    let (diags, text, clean, _) = mend("drop-generics-used", src);
    let d = diags
        .iter()
        .find(|d| d["code"] == "E_DROP_SIGNATURE_INVALID")
        .unwrap_or_else(|| panic!("{diags:?}"));
    assert!(!carries_a_fix(d), "{d}");
    assert_eq!(text, src);
    assert!(!clean);
}

/// One `impl Trait` argument: the method gains `[T: Trait]`. Two, in a
/// method that has a generic list already: both join it, in the first
/// diagnostic's fix.
#[test]
fn an_impl_trait_argument_of_a_trait_method_becomes_a_generic_parameter() {
    let (diags, text, clean, out) = mend(
        "impl-arg",
        "trait Logger {\n    fn log(self, msg: impl Display) -> i64;\n    fn both[A](self, a: A, b: impl Display, c: impl Debug) -> i64;\n}\n\nstruct L { n: i64 }\n\nimpl Logger for L {\n    fn log[M: Display](self, msg: M) -> i64 {\n        println(msg);\n        self.n\n    }\n    fn both[A, B: Display, C: Debug](self, a: A, b: B, c: C) -> i64 {\n        println(b);\n        self.n + 1\n    }\n}\n\nfn main() {\n    let l = L { n: 2 };\n    println(l.log(\"hi\"));\n    println(l.both(1, 2, 3));\n}\n",
    );
    let named: Vec<_> = diags
        .iter()
        .filter(|d| d["code"] == "E_IMPL_TRAIT_IN_TRAIT_METHOD_ARG")
        .collect();
    assert_eq!(named.len(), 3, "{diags:?}");
    assert!(named[0]["fix_diff"].is_array(), "{}", named[0]);
    assert!(named[1]["fix_diff"].is_array(), "{}", named[1]);
    assert!(!carries_a_fix(named[2]), "{}", named[2]);
    assert!(
        text.contains("    fn log[T: Display](self, msg: T) -> i64;\n"),
        "{text}"
    );
    assert!(
        text.contains("    fn both[A, U: Display, V: Debug](self, a: A, b: U, c: V) -> i64;\n"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "hi\n2\n2\n3\n");
}

/// `Vec[impl Display]` in a function's parameter: the same rewrite, on the
/// enclosing function.
#[test]
fn an_impl_trait_nested_in_a_parameters_type_becomes_a_generic_parameter() {
    let (diags, text, clean, out) = mend(
        "impl-nested",
        "fn show(xs: Vec[impl Display]) {\n    for x in xs {\n        println(x);\n    }\n}\n\nfn main() {\n    let mut v = Vec.new();\n    v.push(1);\n    v.push(2);\n    show(v);\n}\n",
    );
    let d = diags
        .iter()
        .find(|d| d["code"] == "E_IMPL_TRAIT_IN_NESTED_POSITION")
        .unwrap_or_else(|| panic!("{diags:?}"));
    assert!(d["fix_diff"].is_array(), "{d}");
    assert!(
        text.starts_with("fn show[T: Display](xs: Vec[T]) {\n"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "1\n2\n");
}

/// The fresh parameter's name is one the file does not use.
#[test]
fn the_generic_parameter_takes_a_name_the_file_does_not_use() {
    let (_, text, clean, _) = mend(
        "impl-arg-name",
        "struct T { n: i64 }\n\ntrait Logger {\n    fn log(self, msg: impl Display) -> i64;\n}\n\nfn main() {\n    let t = T { n: 1 };\n    println(t.n);\n}\n",
    );
    assert!(
        text.contains("    fn log[U: Display](self, msg: U) -> i64;\n"),
        "{text}"
    );
    assert!(clean, "{text}");
}

/// W0298's rename, which `karac fix` applied already, is in the record.
#[test]
fn an_acronym_type_names_rename_is_in_the_json_record() {
    let (diags, text, clean, out) = mend(
        "acronym",
        "struct HTTPClient { port: i64 }\n\nfn main() {\n    let c = HTTPClient { port: 80 };\n    println(c.port);\n}\n",
    );
    let d = diags
        .iter()
        .find(|d| d["code"] == "W0298")
        .unwrap_or_else(|| panic!("{diags:?}"));
    let edits = d["fix_diff"].as_array().unwrap_or_else(|| panic!("{d}"));
    assert_eq!(edits.len(), 2, "{d}");
    assert!(edits.iter().all(|e| e["text"] == "HttpClient"), "{d}");
    assert!(text.contains("struct HttpClient {"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "80\n");
}

/// A package's check carries the same edits, as offsets into the module's
/// own file.
#[test]
fn a_package_modules_acronym_rename_is_in_its_json_record() {
    let dir = std::env::temp_dir().join(format!("karac-row-74-pkg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import util.{port};\n\nfn main() {\n    println(port());\n}\n",
    )
    .unwrap();
    let util = "struct HTTPClient { port: i64 }\n\npub fn port() -> i64 {\n    let c = HTTPClient { port: 80 };\n    c.port\n}\n";
    std::fs::write(dir.join("src/util.kara"), util).unwrap();
    let (_, out, _) = karac(&dir, &["check", "--output=json"]);
    let doc: serde_json::Value =
        serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{e}: {out}"));
    let d = doc["diagnostics"]
        .as_array()
        .and_then(|a| a.iter().find(|d| d["code"] == "W0298"))
        .unwrap_or_else(|| panic!("{out}"));
    let edits = d["fix_diff"].as_array().unwrap_or_else(|| panic!("{d}"));
    assert_eq!(edits.len(), 2, "{d}");
    for e in edits {
        let at = e["offset"].as_u64().unwrap() as usize;
        let len = e["length"].as_u64().unwrap() as usize;
        assert_eq!(&util[at..at + len], "HTTPClient", "{d}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn find<'a>(diags: &'a [serde_json::Value], code: &str) -> &'a serde_json::Value {
    diags
        .iter()
        .find(|d| d["code"] == code)
        .unwrap_or_else(|| panic!("no {code} in {diags:?}"))
}

#[test]
fn a_valued_serde_flag_is_written_bare() {
    let (diags, text, clean, out) = mend(
        "serde-flag",
        "#[derive(Serialize)]\nstruct U {\n    name: String,\n    #[serde(skip: true)]\n    age: i64,\n}\n\nfn main() {\n    let u = U { name: \"a\".to_string(), age: 3 };\n    println(u.age);\n}\n",
    );
    assert_eq!(
        find(&diags, "E_UNKNOWN_ATTRIBUTE")["replacement"]["text"],
        "skip"
    );
    assert!(text.contains("    #[serde(skip)]\n"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "3\n");
}

#[test]
fn size_of_and_align_of_with_a_type_argument_take_their_method_form() {
    let (diags, text, clean, out) = mend(
        "type-arg-call",
        "struct E { a: i64 }\n\nfn main() {\n    let n = align_of[E]();\n    let m = size_of[ E ]( );\n    println(n + m);\n}\n",
    );
    assert_eq!(find(&diags, "E0200")["replacement"]["text"], "E.align_of()");
    assert!(
        text.contains("    let n = E.align_of();\n    let m = E.size_of();\n"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "16\n");
}

/// Both arguments are reported, with the one edit; under `not` the
/// comparison keeps parentheses, and an operand that is itself an operator
/// expression keeps its own.
#[test]
fn ref_eq_on_values_that_are_not_shared_becomes_structural_equality() {
    let (diags, text, clean, out) = mend(
        "ref-eq",
        "#[derive(PartialEq)]\nstruct P { x: i64 }\n\nfn main() {\n    let a = P { x: 1 };\n    let b = P { x: 1 };\n    if ref_eq(a, b) {\n        println(1);\n    }\n    let same = not ref_eq(a, b);\n    println(same);\n    println(ref_eq(a.x + 1, b.x));\n}\n",
    );
    assert!(
        diags
            .iter()
            .filter(|d| d["code"] == "E0232")
            .all(carries_a_fix),
        "{diags:?}"
    );
    assert!(text.contains("    if a == b {\n"), "{text}");
    assert!(text.contains("    let same = not (a == b);\n"), "{text}");
    assert!(text.contains("    println((a.x + 1) == b.x);\n"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "1\nfalse\nfalse\n");
}

/// The closure writes a capture, so the annotated slot becomes `MutFn`, and
/// the binding `mut` by the fix the call's own error already carried.
#[test]
fn an_fn_slot_holding_a_closure_that_writes_becomes_mut_fn() {
    let (diags, text, clean, out) = mend(
        "fn-slot",
        "fn main() {\n    let mut n = 0;\n    let bump: Fn() -> i64 = || {\n        n += 1;\n        n\n    };\n    println(bump());\n}\n",
    );
    assert!(
        diags
            .iter()
            .filter(|d| d["code"] == "E0518")
            .all(carries_a_fix),
        "{diags:?}"
    );
    assert!(
        text.contains("    let mut bump: MutFn() -> i64 = || {\n"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "1\n");
}

/// A bare `for y in ys` had `.iter_mut()` appended already; `ys.iter()`
/// now has its call replaced.
#[test]
fn a_for_over_iter_that_writes_its_element_iterates_with_iter_mut() {
    let (diags, text, clean, out) = mend(
        "iter-mut",
        "fn main() {\n    let mut ys = Vec.new();\n    ys.push(1);\n    ys.push(2);\n    for y in ys.iter() {\n        y += 1;\n    }\n    println(ys[0] + ys[1]);\n}\n",
    );
    assert_eq!(find(&diags, "E0517")["replacement"]["text"], ".iter_mut()");
    assert!(text.contains("    for y in ys.iter_mut() {\n"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "5\n");
}

#[test]
fn a_trait_alias_is_written_out_at_each_bound_and_deleted() {
    let (diags, text, clean, out) = mend(
        "trait-alias",
        "trait Numeric = Copy + Clone;\n\nfn twice[T: Numeric](x: T) -> (T, T) {\n    (x, x)\n}\n\nfn pair[A: Display + Numeric, B](a: A, b: B) -> (A, A)\nwhere B: Numeric {\n    (a, a)\n}\n\nfn main() {\n    let (a, b) = twice(3);\n    let (c, d) = pair(1, 2);\n    println(a + b + c + d);\n}\n",
    );
    let edits = find(&diags, "E_TRAIT_ALIAS_NOT_IMPLEMENTED_YET")["fix_diff"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(edits.len(), 4, "{diags:?}");
    assert!(!text.contains("Numeric"), "{text}");
    assert!(
        text.starts_with("\nfn twice[T: Copy + Clone](x: T) -> (T, T) {\n"),
        "{text}"
    );
    assert!(
        text.contains("fn pair[A: Display + Copy + Clone, B](a: A, b: B) -> (A, A)\nwhere B: Copy + Clone {\n"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "8\n");
}

/// The alias is also named where no bound list can stand, so writing the
/// list out is not one edit: nothing is offered.
#[test]
fn a_trait_alias_named_outside_a_bound_is_left_to_the_author() {
    let src = "trait Numeric = Copy + Clone;\n\nstruct W { n: i64 }\n\nimpl Numeric for W {}\n\nfn main() {\n    println(1);\n}\n";
    let (diags, text, _, _) = mend("trait-alias-impl", src);
    assert!(
        !carries_a_fix(find(&diags, "E_TRAIT_ALIAS_NOT_IMPLEMENTED_YET")),
        "{diags:?}"
    );
    assert!(
        text.starts_with("trait Numeric = Copy + Clone;\n"),
        "{text}"
    );
}

#[test]
fn an_effect_group_is_written_out_in_each_with_clause_and_deleted() {
    let (diags, text, clean, out) = mend(
        "effect-group",
        "effect resource Log;\neffect resource Db;\neffect group logging = writes(Log) + reads(Db);\n\nfn f() with logging {\n    g();\n}\n\nfn g() with reads(Db) logging {\n}\n\nfn main() {\n    f();\n    println(1);\n}\n",
    );
    assert!(
        find(&diags, "E_DEFERRED_EFFECT_GROUP")["fix_diff"].is_array(),
        "{diags:?}"
    );
    assert!(!text.contains("logging"), "{text}");
    assert!(
        text.contains("fn f() with writes(Log) reads(Db) {\n"),
        "{text}"
    );
    assert!(
        text.contains("fn g() with reads(Db) writes(Log) reads(Db) {\n"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "1\n");
}

/// A group another group's list names is not in a `with` clause there.
#[test]
fn an_effect_group_named_by_another_group_is_left_to_the_author() {
    let src = "effect resource Log;\neffect group logging = writes(Log);\neffect group all = logging + panics;\n\nfn main() {\n    println(1);\n}\n";
    let (diags, _, _, _) = mend("effect-group-nested", src);
    let logging = diags
        .iter()
        .find(|d| {
            d["code"] == "E_DEFERRED_EFFECT_GROUP"
                && d["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("`effect group logging`"))
        })
        .unwrap_or_else(|| panic!("{diags:?}"));
    assert!(!carries_a_fix(logging), "{logging}");
}

/// The method the trait does not declare leaves the trait impl for an
/// inherent one after it, and the call that could not find it resolves.
#[test]
fn a_method_the_trait_does_not_declare_moves_to_an_inherent_impl() {
    let (diags, text, clean, out) = mend(
        "inherent-move",
        "trait Named {\n    fn name(self) -> i64;\n}\n\nstruct Resource { id: i64 }\n\nimpl Named for Resource {\n    fn name(self) -> i64 {\n        self.id\n    }\n    fn destroy(self) -> i64 {\n        self.id + 1\n    }\n}\n\nfn main() {\n    let r = Resource { id: 1 };\n    println(r.name());\n    println(r.destroy());\n}\n",
    );
    assert!(find(&diags, "E0204")["fix_diff"].is_array(), "{diags:?}");
    assert!(
        text.contains("impl Named for Resource {\n    fn name(self) -> i64 {\n        self.id\n    }\n}\n\nimpl Resource {\n    fn destroy(self) -> i64 {\n        self.id + 1\n    }\n}\n"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "1\n2\n");
}

/// A generic impl's header has parameters the inherent block would need
/// too: the move is not offered.
#[test]
fn a_stray_method_in_a_generic_trait_impl_is_left_to_the_author() {
    let src = "trait Named {\n    fn name(self) -> i64;\n}\n\nstruct Box2[T] { v: T }\n\nimpl[T] Named for Box2[T] {\n    fn name(self) -> i64 {\n        1\n    }\n    fn extra(self) -> i64 {\n        2\n    }\n}\n\nfn main() {\n    let b = Box2 { v: 1 };\n    println(b.name());\n}\n";
    let (diags, text, _, _) = mend("inherent-move-generic", src);
    assert!(!carries_a_fix(find(&diags, "E0204")), "{diags:?}");
    assert_eq!(text, src);
}

/// `-n as i64` casts `-n`, and a suffixed literal and a cast end where
/// their operand's last token does: the lints' deletion is offered there
/// too, and leaves each operand whole.
#[test]
fn an_unneeded_cast_of_a_prefix_a_literal_or_a_cast_is_deleted() {
    let (diags, text, clean, out) = mend(
        "casts",
        "fn main() {\n    let a: u32 = 3;\n    let n: i64 = 4;\n    let b: i64 = -n as i64;\n    let c: i64 = 5u8 as i64;\n    let d: i64 = n as i64 as i64;\n    let e: i64 = a as u64 as u64 as i64;\n    println(b + c + d + e);\n}\n",
    );
    let casts: Vec<_> = diags
        .iter()
        .filter(|d| d["code"] == "W0309" || d["code"] == "W0310")
        .collect();
    assert!(casts.len() >= 4, "{diags:?}");
    assert!(casts.iter().all(|d| carries_a_fix(d)), "{diags:?}");
    assert!(
        text.contains("    let b: i64 = -n;\n    let c: i64 = 5u8;\n    let d: i64 = n;\n"),
        "{text}"
    );
    assert!(
        text.contains("    let e: i64 = a as u64 as i64;\n"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "8\n");
}

#[test]
fn a_codegen_hint_on_an_impl_block_moves_onto_each_method() {
    let (diags, text, clean, out) = mend(
        "hint",
        "struct S { n: i64 }\n\n#[cold]\nimpl S {\n    fn a(self) -> i64 { self.n }\n\n    /// One more.\n    pub fn b(self) -> i64 {\n        let f = || self.n + 1;\n        f()\n    }\n}\n\nfn main() {\n    let s = S { n: 1 };\n    println(s.a() + s.b());\n}\n",
    );
    assert!(
        find(&diags, "E_CODEGEN_HINT_INVALID_POSITION")["fix_diff"].is_array(),
        "{diags:?}"
    );
    assert!(
        text.contains("\nimpl S {\n    #[cold]\n    fn a(self) -> i64 { self.n }\n\n    /// One more.\n    #[cold]\n    pub fn b(self) -> i64 {\n"),
        "{text}"
    );
    assert_eq!(text.matches("#[cold]").count(), 2, "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "3\n");
}

/// On a struct no edit is named: the hint has nowhere to go.
#[test]
fn a_codegen_hint_on_a_struct_is_left_to_the_author() {
    let src = "#[cold]\nstruct S { n: i64 }\n\nfn main() {\n    let s = S { n: 1 };\n    println(s.n);\n}\n";
    let (diags, text, _, _) = mend("hint-struct", src);
    assert!(
        !carries_a_fix(find(&diags, "E_CODEGEN_HINT_INVALID_POSITION")),
        "{diags:?}"
    );
    assert_eq!(text, src);
}

/// Each temporary gets a local of its own, named apart, on the line before
/// the `let` that sliced it.
#[test]
fn a_sliced_temporary_is_bound_to_a_local_first() {
    let (diags, text, clean, out) = mend(
        "slice-temporary",
        "fn make() -> Vec[i64] {\n    let mut v = Vec.new();\n    v.push(4);\n    v.push(5);\n    v\n}\n\nfn main() {\n    let x = [10u8, 20u8, 30u8, 40u8][1..3];\n    let y = make().as_slice();\n    println(x.len() + y.len());\n}\n",
    );
    let slices: Vec<_> = diags
        .iter()
        .filter(|d| d["code"] == "E_SLICE_FROM_TEMPORARY_ESCAPES")
        .collect();
    assert_eq!(slices.len(), 2, "{diags:?}");
    assert!(slices.iter().all(|d| d["fix_diff"].is_array()), "{diags:?}");
    assert!(
        text.contains("    let backing = [10u8, 20u8, 30u8, 40u8];\n    let x = backing[1..3];\n    let backing_2 = make();\n    let y = backing_2.as_slice();\n"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "4\n");
}

/// The removed `Base64` / `Hex` / `Url` namespaces become the `std.encoding`
/// functions their messages spell, and the file gains the import.
#[test]
fn a_removed_encoding_namespace_call_becomes_the_std_encoding_function() {
    let (diags, text, clean, out) = mend(
        "encoding",
        "fn main() {\n    let b = \"hi\".bytes();\n    println(Base64.encode(b));\n    println(Hex.encode_upper(b));\n    println(Url.encode(\"a b\"));\n    println(Base64.encode(\"yo\".bytes()));\n}\n",
    );
    let removed: Vec<_> = diags.iter().filter(|d| d["code"] == "E0520").collect();
    assert_eq!(removed.len(), 4, "{diags:?}");
    // Each carries the import it needs, so any one alone leaves a program
    // that resolves.
    assert!(
        removed.iter().all(|d| d["fix_diff"].is_array()),
        "{diags:?}"
    );
    for name in ["base64_encode", "hex_encode", "percent_encode"] {
        assert!(
            text.lines().next().unwrap().contains(name),
            "{name}: {text}"
        );
    }
    assert!(text.starts_with("import std.encoding.{"), "{text}");
    assert_eq!(text.matches("import ").count(), 1, "{text}");
    assert!(
        text.contains("    println(base64_encode(b));\n    println(hex_encode(b).to_uppercase());\n    println(percent_encode(\"a b\".bytes()));\n    println(base64_encode(\"yo\".bytes()));\n"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "aGk=\n6869\na%20b\neW8=\n");
}

/// A file that imports from `std.encoding` already has the name join that
/// list, once.
#[test]
fn the_encoding_function_joins_the_files_std_encoding_import() {
    let (_, text, clean, out) = mend(
        "encoding-import",
        "import std.encoding.{hex_decode};\n\nfn main() {\n    println(Hex.encode(hex_decode(\"6869\").unwrap()));\n    println(Hex.encode(\"a\".bytes()));\n}\n",
    );
    assert!(
        text.starts_with("import std.encoding.{hex_decode, hex_encode};\n"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "6869\n61\n");
}

/// A package's parse errors were E0001 with the symbolic code left in the
/// message and no edit: its JSON now names the code and carries the parse's
/// fixes, as offsets into the module's file. A `!`'s edit depends on its
/// operand's type, which only the checked file gives, so it is left out.
#[test]
fn a_package_modules_parse_errors_carry_their_code_and_fixes() {
    let dir = std::env::temp_dir().join(format!("karac-row-74-parse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import util.{f};\n\nfn main() {\n    println(f());\n}\n",
    )
    .unwrap();
    let util = "trait Logger {\n    fn log(self, msg: impl Display) -> i64;\n}\n\npub fn f() -> i64 {\n    let n = 2;\n    outer: for i in 0..3 {\n        if !(i == 1) {\n            break outer;\n        }\n    }\n    n\n}\n";
    std::fs::write(dir.join("src/util.kara"), util).unwrap();
    let (_, out, _) = karac(&dir, &["check", "--output=json"]);
    let doc: serde_json::Value =
        serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{e}: {out}"));
    let diags = doc["diagnostics"].as_array().cloned().unwrap_or_default();
    let named = find(&diags, "E_IMPL_TRAIT_IN_TRAIT_METHOD_ARG");
    assert_eq!(named["parent_code"], "E0001", "{named}");
    assert!(
        named["message"]
            .as_str()
            .unwrap()
            .starts_with("`impl Trait` is not permitted"),
        "{named}"
    );
    let edits = named["fix_diff"]
        .as_array()
        .unwrap_or_else(|| panic!("{named}"));
    assert_eq!(edits[0]["text"], "[T: Display]", "{named}");
    let at = edits[1]["offset"].as_u64().unwrap() as usize;
    let len = edits[1]["length"].as_u64().unwrap() as usize;
    assert_eq!(&util[at..at + len], "impl Display", "{named}");
    let labels: Vec<_> = diags
        .iter()
        .filter(|d| {
            d["message"]
                .as_str()
                .is_some_and(|m| m.starts_with("labels are written"))
        })
        .collect();
    assert_eq!(labels.len(), 2, "{out}");
    for d in &labels {
        let at = d["replacement"]["offset"].as_u64().unwrap() as usize;
        assert_eq!(&util[at..at + 5], "outer", "{d}");
        assert_eq!(d["replacement"]["text"], "'", "{d}");
    }
    let bang = diags
        .iter()
        .find(|d| {
            d["message"]
                .as_str()
                .is_some_and(|m| m.starts_with("the `!` operator"))
        })
        .unwrap_or_else(|| panic!("{out}"));
    assert!(!carries_a_fix(bang), "{bang}");
    // The text render names the code once.
    let (_, _, text) = karac(&dir, &["check"]);
    assert!(
        text.contains("error[E_IMPL_TRAIT_IN_TRAIT_METHOD_ARG]: src/util.kara:2:23: `impl Trait`")
            || text.contains(
                "error[E_IMPL_TRAIT_IN_TRAIT_METHOD_ARG]: src\\util.kara:2:23: `impl Trait`"
            ),
        "{text}"
    );
    // `karac fix` on the member applies them all, the `!` as `not`.
    karac(&dir, &["fix", "src/util.kara"]);
    let fixed = std::fs::read_to_string(dir.join("src/util.kara")).unwrap();
    assert!(
        fixed.contains("    fn log[T: Display](self, msg: T) -> i64;\n"),
        "{fixed}"
    );
    assert!(fixed.contains("    'outer: for i in 0..3 {\n"), "{fixed}");
    assert!(fixed.contains("        if not (i == 1) {\n"), "{fixed}");
    let (_, out, _) = karac(&dir, &["check", "--output=json"]);
    let doc: serde_json::Value =
        serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{e}: {out}"));
    let errors: Vec<_> = doc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["severity"] == "error")
        .collect();
    assert!(errors.is_empty(), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A foreign import's `ref CStr` or `String` parameter becomes `*const u8`,
/// and each call hands over `.as_ptr()`, parenthesizing an operand that
/// needs it.
#[test]
fn a_foreign_parameter_that_cannot_cross_becomes_a_pointer_at_every_call() {
    let (diags, text, clean, out) = mend(
        "ffi-pointer",
        "/// # Safety\n/// The pointers are NUL-terminated C strings.\nunsafe extern \"C\" {\n    fn strlen(s: ref CStr) -> usize;\n    fn atoi(s: String) -> i32;\n}\n\nfn main() {\n    let text = \"42\".to_string();\n    // Safety: both arguments end in a NUL.\n    let n = unsafe { strlen(c\"hello\") + strlen(c\"a\") };\n    // Safety: as above.\n    let v = unsafe { atoi(text + \"\\0\") };\n    println(f\"{n} {v}\");\n}\n",
    );
    assert!(
        find(&diags, "E_EXTERN_REF_PARAM")["fix_diff"].is_array(),
        "{diags:?}"
    );
    assert!(
        find(&diags, "E_EXTERN_TYPE_NOT_FFI_SAFE")["fix_diff"].is_array(),
        "{diags:?}"
    );
    assert!(
        text.contains("    fn strlen(s: *const u8) -> usize;\n    fn atoi(s: *const u8) -> i32;\n"),
        "{text}"
    );
    assert!(
        text.contains("strlen(c\"hello\".as_ptr()) + strlen(c\"a\".as_ptr())"),
        "{text}"
    );
    assert!(text.contains("atoi((text + \"\\0\").as_ptr())"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "6 42\n");
}

#[test]
fn the_free_function_receiving_a_frozen_handle_takes_it_frozen() {
    let (diags, text, clean, out) = mend(
        "frozen-slot",
        "shared struct G { n: i64 }\n\nfn take(k: i64, g: G) -> i64 {\n    g.n + k\n}\n\nfn pass(g: frozen G) -> i64 {\n    take(1, g)\n}\n\nfn main() {\n    let g = G { n: 1 };\n    println(pass(g));\n}\n",
    );
    assert_eq!(find(&diags, "E0511")["replacement"]["text"], "frozen ");
    assert!(
        text.contains("fn take(k: i64, g: frozen G) -> i64 {\n"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "2\n");
}

/// Two `fn take` in the file: which one the call reaches is not read from
/// the text, so nothing is offered.
#[test]
fn a_frozen_handle_passed_to_a_name_declared_twice_is_left_to_the_author() {
    let src = "shared struct G { n: i64 }\n\nstruct B { k: i64 }\n\nimpl B {\n    fn take(self, g: G) -> i64 {\n        g.n + self.k\n    }\n}\n\nfn take(g: G) -> i64 {\n    g.n\n}\n\nfn pass(g: frozen G) -> i64 {\n    take(g)\n}\n\nfn main() {\n    let g = G { n: 1 };\n    println(pass(g));\n}\n";
    let (diags, text, _, _) = mend("frozen-slot-twice", src);
    assert!(!carries_a_fix(find(&diags, "E0511")), "{diags:?}");
    assert_eq!(text, src);
}
