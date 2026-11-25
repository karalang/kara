//! Codes whose every emission carried neither a fix nor help (the 11-21
//! fix-coverage table's kind "none") now carry one: help naming the cause
//! and the edit where the edit is the author's choice, and a machine fix
//! where it is determined (an attribute that does nothing where it is
//! written, a repeated lint name, a variance marker).

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

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

fn scratch(name: &str) -> std::path::PathBuf {
    let n = CALL.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("karac-help-none-{name}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The JSON diagnostics of `src`, and the human `karac check` output.
fn check(name: &str, src: &str) -> (Vec<serde_json::Value>, String) {
    let dir = scratch(name);
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let (_, json, _) = karac(&dir, &["check", "--output=json", "cell.kara"]);
    let (_, out, err) = karac(&dir, &["check", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    let doc: serde_json::Value =
        serde_json::from_str(json.trim()).unwrap_or_else(|e| panic!("{e}: {json}"));
    (
        doc["diagnostics"].as_array().cloned().unwrap_or_default(),
        format!("{out}{err}"),
    )
}

/// The text `karac fix` leaves, and whether that checks clean.
fn fix(name: &str, src: &str) -> (String, bool, String) {
    let dir = scratch(name);
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    karac(&dir, &["fix", "cell.kara"]);
    let text = std::fs::read_to_string(dir.join("cell.kara")).unwrap();
    let (clean, out, err) = karac(&dir, &["check", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (text, clean, format!("{out}{err}"))
}

fn hint(d: &serde_json::Value) -> String {
    d["hints"][0]["description"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Each emission of `code` in `src` carries help containing `want`, in
/// JSON as a hint and in the human output as a `help:` line.
fn assert_help(code: &str, src: &str, want: &str) {
    let (diags, human) = check(code, src);
    let mine: Vec<_> = diags.iter().filter(|d| d["code"] == code).collect();
    assert!(!mine.is_empty(), "{code} not emitted: {diags:?}");
    for d in &mine {
        assert!(hint(d).contains(want), "{code}: {d}");
    }
    assert!(
        human.contains("help: ") && human.contains(want),
        "{code}: {human}"
    );
}

#[test]
fn undocumented_unsafe_names_the_comment_to_add() {
    assert_help(
        "W0317",
        "fn f() { unsafe { } }\nfn main() { f(); }\n",
        "`// Safety:",
    );
}

#[test]
fn prelude_shadow_names_the_rename_and_the_allow() {
    assert_help(
        "W0282",
        "struct Span { x: i64 }\nfn main() { println(Span { x: 1 }.x); }\n",
        "#[allow(prelude_shadow)]",
    );
}

#[test]
fn f16_emulation_moves_its_advice_to_help() {
    let src = "fn f(a: f16, b: f16) -> f16 { a * b - b }\nfn main() { }\n";
    let (diags, _) = check("w0311", src);
    let d = diags.iter().find(|d| d["code"] == "W0311").expect("W0311");
    assert!(hint(d).contains("compute in `f32`"), "{d}");
    assert!(!d["message"].as_str().unwrap().contains("Store in"), "{d}");
}

#[test]
fn a_slot_violation_names_the_widened_clause() {
    assert_help(
        "E0404",
        "fn act() with blocks { }\nfn apply(g: Fn() with suspends) with blocks { g(); }\nfn main() { apply(act); }\n",
        "add `blocks` to the slot's clause (`with suspends blocks`)",
    );
}

#[test]
fn a_struct_invariant_points_at_the_constructor() {
    assert_help(
        "E_DEFERRED_TRACK",
        "struct Bad { x: i64, invariant self.x > 0 }\nfn main() { }\n",
        "remove the `invariant` and check the property where a `Bad` is built",
    );
}

#[test]
fn a_non_bool_condition_names_the_test_its_type_means() {
    assert_help("E0211", "fn main() { if 42 { } }\n", "`n != 0`");
    assert_help(
        "E0211",
        "fn main() { let o: Option[i64] = None; while o { } }\n",
        "`.is_some()`",
    );
}

#[test]
fn a_duplicate_definition_says_rename_or_remove() {
    assert_help(
        "E0101",
        "fn area() -> i64 { 1 }\nfn area() -> i64 { 2 }\nfn main() { println(area()); }\n",
        "rename one of the two 'area' definitions",
    );
}

#[test]
fn a_profile_violation_names_where_the_effect_comes_from() {
    assert_help(
        "E_PROFILE_INCOMPATIBLE_EFFECT",
        "#[profile(embedded)]\nfn build() -> Vec[i64] { Vec.new() }\nfn main() { }\n",
        "drop `embedded` from its `#[profile(...)]`",
    );
}

#[test]
fn a_capitalised_binding_offers_the_variant_or_the_rename() {
    assert_help(
        "E0128",
        "fn f(x: i64) -> i64 { match x { Big => 0 } }\nfn main() { println(f(1)); }\n",
        "rename it to `big`",
    );
}

#[test]
fn an_unknown_effect_name_says_to_write_the_effects_out() {
    assert_help(
        "E0403",
        "fn process(n: i64) with OrderProcessing { }\nfn main() { process(1); }\n",
        "write out the effects `OrderProcessing` stands for",
    );
}

#[test]
fn a_non_constant_discriminant_names_the_const() {
    assert_help(
        "E_NON_CONSTANT_DISCRIMINANT",
        "fn two() -> i64 { 2 }\n#[repr(i64)]\nenum E { A = two() }\nfn main() { }\n",
        "module-level",
    );
}

#[test]
fn a_derive_default_gap_names_both_impls() {
    assert_help(
        "E_DERIVE_DEFAULT_MISSING_FIELD_DEFAULT",
        "struct Plain { x: i64 }\n#[derive(Default)]\nstruct Holder { p: Plain }\nfn main() { }\n",
        "`impl Default for Holder`",
    );
}

#[test]
fn a_comptime_module_at_run_time_names_the_comptime_block() {
    assert_help(
        "E_COMPTIME_MODULE_AT_RUNTIME",
        "fn main() { compiler.error(\"nope\"); }\n",
        "`comptime { ... }`",
    );
}

/// An attribute that does nothing where it is written is deleted by
/// `karac fix`, and the result checks clean.
#[test]
fn attributes_that_do_nothing_where_written_are_deleted() {
    for (attr, item) in [
        ("#[track_caller]", "struct P { x: i64 }"),
        ("#[gpu]", "struct P { x: i64 }"),
        ("#[profile(embedded)]", "enum P { A, B }"),
        ("#[no_effect(panics)]", "struct P { x: i64 }"),
        ("#[non_exhaustive]", "fn frob() { }"),
        ("#[compiler_builtin]", "struct P { x: i64 }"),
        ("#[serde(rename: \"x\")]", "fn frob() { }"),
    ] {
        let src = format!("{attr}\n{item}\nfn main() {{ }}\n");
        let (diags, _) = check("attr", &src);
        assert!(
            diags.iter().any(|d| d["replacement"].is_object()),
            "{attr}: {diags:?}"
        );
        let (text, clean, out) = fix("attr", &src);
        assert_eq!(text, format!("\n{item}\nfn main() {{ }}\n"), "{attr}");
        assert!(clean, "{attr}: {out}");
    }
}

#[test]
fn a_deprecated_field_attribute_is_deleted() {
    let (text, clean, out) = fix(
        "dep-field",
        "pub struct Foo { #[deprecated] x: i64, }\nfn main() { }\n",
    );
    assert_eq!(text, "pub struct Foo {  x: i64, }\nfn main() { }\n");
    assert!(clean, "{out}");
}

/// A private type could be made `pub` instead, so `#[non_exhaustive]` on
/// one gets help and no fix.
#[test]
fn non_exhaustive_on_a_private_struct_offers_pub_without_a_fix() {
    let (diags, _) = check(
        "ne-private",
        "#[non_exhaustive]\nstruct P { x: i64 }\nfn main() { }\n",
    );
    let d = diags
        .iter()
        .find(|d| d["code"] == "E_NON_EXHAUSTIVE_INVALID_TARGET")
        .expect("E_NON_EXHAUSTIVE_INVALID_TARGET");
    assert!(hint(d).contains("make the type `pub`"), "{d}");
    assert!(d["replacement"].is_null(), "{d}");
}

#[test]
fn a_repeated_lint_name_is_deleted_with_its_comma() {
    let (text, clean, out) = fix(
        "dup-lint",
        "#[allow(deprecated, unused_variables, deprecated)]\nfn f() { }\nfn main() { f(); }\n",
    );
    assert_eq!(
        text,
        "#[allow(deprecated, unused_variables)]\nfn f() { }\nfn main() { f(); }\n"
    );
    assert!(clean, "{out}");
}

#[test]
fn a_user_variance_marker_is_deleted() {
    let (text, clean, out) = fix("variance", "trait Tr[+T] { }\nfn main() { }\n");
    assert_eq!(text, "trait Tr[T] { }\nfn main() { }\n");
    assert!(clean, "{out}");
}
