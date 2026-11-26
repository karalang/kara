//! Diagnostics whose help names a mechanical edit and which carried no
//! machine-applicable fix: the fix coverage audit (2026-11-21) found each
//! one by triggering it and reading its JSON record. Each now carries the
//! edit its help names, and each pin applies it with `karac fix` and checks
//! the result clean: the first `check --output=json` record has a
//! `replacement` or `fix_diff`, the fixed text is what the help describes,
//! and it checks with no diagnostic (and, where it runs, prints what the
//! original meant).

use std::path::{Path, PathBuf};
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

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("karac-fix-coverage-{name}-{}", std::process::id()))
}

struct Mended {
    /// Whether the first check's record for `code` carried an edit.
    fixable: bool,
    /// The first check's JSON, for failure messages.
    first: String,
    /// The text `karac fix` leaves.
    text: String,
    /// Whether that text checks with no diagnostic at all.
    clean: bool,
    /// The fixed check's output, for failure messages.
    after: String,
    /// What the fixed text prints under the MIR interpreter.
    out: String,
}

fn mend(name: &str, code: &str, src: &str) -> Mended {
    let dir = scratch(name);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, src).unwrap();
    let (_, first, _) = karac(&dir, &["check", "--output=json", "cell.kara"]);
    let json: serde_json::Value = serde_json::from_str(&first).expect("one JSON document");
    let fixable = json["diagnostics"].as_array().unwrap().iter().any(|d| {
        d["code"] == code
            && (d.get("replacement").is_some_and(|r| !r.is_null())
                || d.get("fix_diff").is_some_and(|r| !r.is_null()))
    });
    karac(&dir, &["fix", "cell.kara"]);
    let text = std::fs::read_to_string(&file).unwrap();
    let (_, after, after_err) = karac(&dir, &["check", "--output=json", "cell.kara"]);
    let clean = serde_json::from_str::<serde_json::Value>(&after)
        .ok()
        .and_then(|j| j["diagnostics"].as_array().map(|d| d.is_empty()))
        .unwrap_or(false);
    let (_, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    Mended {
        fixable,
        first,
        text,
        clean,
        after: format!("{after}{after_err}"),
        out,
    }
}

fn assert_mended(m: &Mended, fixed: &str, out: Option<&str>) {
    assert!(m.fixable, "no edit on the first record:\n{}", m.first);
    assert_eq!(m.text, fixed);
    assert!(m.clean, "the fixed text does not check clean:\n{}", m.after);
    if let Some(out) = out {
        assert_eq!(m.out, out);
    }
}

#[test]
fn fix_coverage_an_irrefutable_if_let_becomes_a_block_and_its_else_goes() {
    let m = mend(
        "iflet",
        "W0302",
        "fn main() {\n    let t = (1, 2);\n    if let (a, b) = t {\n        println(a + b);\n    } else {\n        println(0);\n    }\n}\n",
    );
    assert_mended(
        &m,
        "fn main() {\n    let t = (1, 2);\n    { let (a, b) = t;\n        println(a + b);\n    }\n}\n",
        Some("3\n"),
    );
}

#[test]
fn fix_coverage_a_doc_comment_on_a_statement_becomes_a_plain_comment() {
    let m = mend(
        "doc",
        "W0296",
        "fn main() {\n    /// counts things\n    let n = 3;\n    println(n);\n}\n",
    );
    assert_mended(
        &m,
        "fn main() {\n    // counts things\n    let n = 3;\n    println(n);\n}\n",
        Some("3\n"),
    );
}

#[test]
fn fix_coverage_a_bound_an_alias_cannot_check_is_removed() {
    let m = mend(
        "alias",
        "W0293",
        "type Ranked[T: Ord] = SortedMap[T, i32];\n\nfn main() {\n    let r: Ranked[i64] = SortedMap.new();\n    println(r.len());\n}\n",
    );
    assert_mended(
        &m,
        "type Ranked[T] = SortedMap[T, i32];\n\nfn main() {\n    let r: Ranked[i64] = SortedMap.new();\n    println(r.len());\n}\n",
        Some("0\n"),
    );
}

#[test]
fn fix_coverage_an_int_as_bool_cast_becomes_a_non_zero_test() {
    let m = mend(
        "asbool",
        "E_INT_AS_BOOL",
        "fn main() {\n    let n = 3;\n    if n as bool {\n        println(1);\n    }\n    let z = 0;\n    let b = z as bool == false;\n    println(b);\n}\n",
    );
    assert_mended(
        &m,
        "fn main() {\n    let n = 3;\n    if n != 0 {\n        println(1);\n    }\n    let z = 0;\n    let b = (z != 0) == false;\n    println(b);\n}\n",
        Some("1\ntrue\n"),
    );
}

#[test]
fn fix_coverage_a_pub_main_loses_its_pub() {
    let m = mend(
        "pubmain",
        "E_MAIN_SIGNATURE",
        "pub fn main() {\n    println(1);\n}\n",
    );
    assert_mended(&m, "fn main() {\n    println(1);\n}\n", Some("1\n"));
}

#[test]
fn fix_coverage_a_spread_with_nothing_left_to_copy_is_dropped() {
    let m = mend(
        "spread",
        "E_STRUCT_LITERAL_SPREAD_UNSUPPORTED",
        "struct P {\n    x: i64,\n}\n\nfn main() {\n    let b = P { x: 1 };\n    let p = P { x: 2, ..b };\n    println(p.x);\n}\n",
    );
    assert_mended(
        &m,
        "struct P {\n    x: i64,\n}\n\nfn main() {\n    let b = P { x: 1 };\n    let p = P { x: 2 };\n    println(p.x);\n}\n",
        None,
    );
}

#[test]
fn fix_coverage_a_sync_types_mut_ref_receiver_becomes_bare() {
    let m = mend(
        "sync",
        "E0258",
        "import std.sync.{Atomic};\nsync struct Counter {\n    n: Atomic[i64],\n}\n\nimpl Counter {\n    fn reset(mut ref self) {\n        self.n.store(0);\n    }\n}\n\nfn main() {\n    let c = Counter { n: Atomic.new(1) };\n    c.reset();\n    println(c.n.load());\n}\n",
    );
    assert_mended(
        &m,
        "import std.sync.{Atomic};\nsync struct Counter {\n    n: Atomic[i64],\n}\n\nimpl Counter {\n    fn reset(self) {\n        self.n.store(0);\n    }\n}\n\nfn main() {\n    let c = Counter { n: Atomic.new(1) };\n    c.reset();\n    println(c.n.load());\n}\n",
        Some("0\n"),
    );
}

#[test]
fn fix_coverage_a_hand_written_copy_impl_becomes_a_derive() {
    let m = mend(
        "copyimpl",
        "E0108",
        "struct P {\n    x: i64,\n}\n\nimpl Copy for P {}\n\nfn main() {\n    let p = P { x: 1 };\n    let q = p;\n    println(p.x + q.x);\n}\n",
    );
    assert_mended(
        &m,
        "#[derive(Copy, Clone)]\nstruct P {\n    x: i64,\n}\n\nfn main() {\n    let p = P { x: 1 };\n    let q = p;\n    println(p.x + q.x);\n}\n",
        Some("2\n"),
    );
}

#[test]
fn fix_coverage_a_default_variant_gets_its_enums_derive() {
    let m = mend(
        "defattr",
        "E_DEFAULT_ATTRIBUTE_WITHOUT_DERIVE",
        "enum Mode {\n    #[default]\n    Off,\n    On,\n}\n\nfn main() {\n    let m = Mode.On;\n    match m {\n        Mode.Off => println(0),\n        Mode.On => println(1),\n    }\n}\n",
    );
    assert_mended(
        &m,
        "#[derive(Default)]\nenum Mode {\n    #[default]\n    Off,\n    On,\n}\n\nfn main() {\n    let m = Mode.On;\n    match m {\n        Mode.Off => println(0),\n        Mode.On => println(1),\n    }\n}\n",
        Some("1\n"),
    );
}

#[test]
fn fix_coverage_a_mains_with_clause_takes_the_missing_effect() {
    let m = mend(
        "mainwith",
        "E0400",
        "fn main() with reads(Env) {\n    let _ = env.var(\"HOME\");\n    println(1);\n}\n",
    );
    assert_mended(
        &m,
        "fn main() with reads(Env) writes(Stdout) {\n    let _ = env.var(\"HOME\");\n    println(1);\n}\n",
        Some("1\n"),
    );
}

#[test]
fn fix_coverage_a_closure_prefix_its_body_contradicts_is_dropped() {
    let m = mend(
        "closure",
        "E0504",
        "struct Owned {\n    x: i64,\n}\n\nfn main() {\n    let o = Owned { x: 1 };\n    let f = mut ref || {\n        let _ = o;\n    };\n    f();\n}\n",
    );
    assert_mended(
        &m,
        "struct Owned {\n    x: i64,\n}\n\nfn main() {\n    let o = Owned { x: 1 };\n    let f = || {\n        let _ = o;\n    };\n    f();\n}\n",
        None,
    );
}

#[test]
fn fix_coverage_frozen_on_a_sync_struct_receiver_is_dropped() {
    let m = mend(
        "frozen",
        "E0512",
        "sync struct Q {\n    v: i64,\n}\n\nimpl Q {\n    fn m(frozen self) -> i64 {\n        self.v\n    }\n}\n\nfn main() {\n    let q = Q { v: 1 };\n    println(q.m());\n}\n",
    );
    assert_mended(
        &m,
        "sync struct Q {\n    v: i64,\n}\n\nimpl Q {\n    fn m(self) -> i64 {\n        self.v\n    }\n}\n\nfn main() {\n    let q = Q { v: 1 };\n    println(q.m());\n}\n",
        Some("1\n"),
    );
}

#[test]
fn fix_coverage_an_irrefutable_while_let_becomes_a_loop() {
    let m = mend(
        "whilelet",
        "W0302",
        "fn main() {\n    let mut n = 0;\n    while let (a, b) = (n, 1) {\n        n = a + b;\n        if n > 2 {\n            break;\n        }\n    }\n    println(n);\n}\n",
    );
    assert_mended(
        &m,
        "fn main() {\n    let mut n = 0;\n    loop { let (a, b) = (n, 1);\n        n = a + b;\n        if n > 2 {\n            break;\n        }\n    }\n    println(n);\n}\n",
        Some("3\n"),
    );
}

#[test]
fn fix_coverage_an_irrefutable_let_else_loses_its_else() {
    let m = mend(
        "letelse",
        "W0302",
        "fn main() {\n    let (a, b) = (1, 2) else {\n        return;\n    };\n    println(a + b);\n}\n",
    );
    assert_mended(
        &m,
        "fn main() {\n    let (a, b) = (1, 2);\n    println(a + b);\n}\n",
        Some("3\n"),
    );
}

#[test]
fn fix_coverage_a_hand_written_cross_task_impl_is_removed() {
    let m = mend(
        "crosstask",
        "E0108",
        "struct P {\n    x: i64,\n}\n\nimpl CrossTask for P {}\n\nfn main() {\n    let p = P { x: 1 };\n    println(p.x);\n}\n",
    );
    assert_mended(
        &m,
        "struct P {\n    x: i64,\n}\n\nfn main() {\n    let p = P { x: 1 };\n    println(p.x);\n}\n",
        Some("1\n"),
    );
}

#[test]
fn fix_coverage_a_default_marker_with_arguments_becomes_bare() {
    let m = mend(
        "defargs",
        "E_MALFORMED_ATTRIBUTE_ARGS",
        "#[derive(Default)]\nenum Mode {\n    #[default(1)]\n    Off,\n    On,\n}\n\nfn main() {\n    let m = Mode.On;\n    match m {\n        Mode.Off => println(0),\n        Mode.On => println(1),\n    }\n}\n",
    );
    assert_mended(
        &m,
        "#[derive(Default)]\nenum Mode {\n    #[default]\n    Off,\n    On,\n}\n\nfn main() {\n    let m = Mode.On;\n    match m {\n        Mode.Off => println(0),\n        Mode.On => println(1),\n    }\n}\n",
        Some("1\n"),
    );
}

/// The field's type is renamed, as the help says; a constructor that
/// names `OnceCell` elsewhere is not, and reports its own mismatch.
#[test]
fn fix_coverage_a_once_cell_field_of_a_sync_struct_becomes_a_once_lock() {
    let m = mend(
        "oncecell",
        "E_ONCE_CELL_IN_PAR_TYPE",
        "import std.sync.{OnceCell};\nsync struct S {\n    c: OnceCell[i64],\n}\n\nfn main() {\n    println(1);\n}\n",
    );
    assert_mended(
        &m,
        // The rename leaves the `OnceCell` import behind; the import fix adds
        // `OnceLock`'s.
        "import std.sync.{OnceCell};\nimport std.sync.{OnceLock};\nsync struct S {\n    c: OnceLock[i64],\n}\n\nfn main() {\n    println(1);\n}\n",
        Some("1\n"),
    );
}

#[test]
fn fix_coverage_a_fallback_on_an_unwrapped_value_is_dropped() {
    let m = mend(
        "nilc",
        "E0276",
        "fn main() {\n    let n = 3;\n    let m = n ?? 0;\n    println(m);\n}\n",
    );
    assert_mended(
        &m,
        "fn main() {\n    let n = 3;\n    let m = n;\n    println(m);\n}\n",
        Some("3\n"),
    );
}

#[test]
fn fix_coverage_an_errdefer_that_never_runs_becomes_a_defer() {
    let m = mend(
        "errdefer",
        "W0303",
        "fn f() -> i64 {\n    errdefer println(\"x\");\n    1\n}\n\nfn main() {\n    println(f());\n}\n",
    );
    assert_mended(
        &m,
        "fn f() -> i64 {\n    defer println(\"x\");\n    1\n}\n\nfn main() {\n    println(f());\n}\n",
        Some("x\n1\n"),
    );
}

/// Denied, the same lint is E0303 and carries the same edit.
#[test]
fn fix_coverage_a_denied_errdefer_that_never_runs_becomes_a_defer() {
    let m = mend(
        "errdeferdeny",
        "E0303",
        "#[deny(unreachable_errdefer)]\nfn work() {\n    errdefer println(\"cleanup\");\n    println(\"work\");\n}\n\nfn main() {\n    work();\n}\n",
    );
    assert_mended(
        &m,
        "#[deny(unreachable_errdefer)]\nfn work() {\n    defer println(\"cleanup\");\n    println(\"work\");\n}\n\nfn main() {\n    work();\n}\n",
        Some("work\ncleanup\n"),
    );
}

#[test]
fn fix_coverage_a_unions_drop_impl_is_removed() {
    let m = mend(
        "uniondrop",
        "E_UNION_DROP_FORBIDDEN",
        "#[repr(C)]\nunion FloatBits {\n    f: f32,\n    bits: u32,\n}\n\nimpl Drop for FloatBits {\n    fn drop(mut ref self) {}\n}\n\nfn main() {\n    println(1);\n}\n",
    );
    assert_mended(
        &m,
        "#[repr(C)]\nunion FloatBits {\n    f: f32,\n    bits: u32,\n}\n\nfn main() {\n    println(1);\n}\n",
        Some("1\n"),
    );
}

/// The impl ends the file, which has no newline after it.
#[test]
fn fix_coverage_a_unions_drop_impl_at_the_end_of_the_file_is_removed() {
    let m = mend(
        "uniondropeof",
        "E_UNION_DROP_FORBIDDEN",
        "#[repr(C)]\nunion FloatBits { f: f32, bits: u32 }\nimpl Drop for FloatBits { fn drop(mut ref self) {} }",
    );
    assert_mended(
        &m,
        "#[repr(C)]\nunion FloatBits { f: f32, bits: u32 }\n",
        None,
    );
}

#[test]
fn fix_coverage_a_union_without_repr_c_gets_it() {
    let m = mend(
        "unionrepr",
        "E_UNION_REQUIRES_REPR",
        "union FloatBits {\n    f: f32,\n    bits: u32,\n}\n\nfn main() {\n    println(1);\n}\n",
    );
    assert_mended(
        &m,
        "#[repr(C)]\nunion FloatBits {\n    f: f32,\n    bits: u32,\n}\n\nfn main() {\n    println(1);\n}\n",
        Some("1\n"),
    );
}

#[test]
fn fix_coverage_a_refinement_assert_from_another_type_goes_through_the_base() {
    let m = mend(
        "refine",
        "E_REFINEMENT_CAST_SOURCE_MISMATCH",
        "type Even = i64 where self % 2 == 0;\n\nfn half(x: i32) -> i64 {\n    let e = x as Even;\n    e / 2\n}\n\nfn main() {\n    println(half(4));\n}\n",
    );
    assert_mended(
        &m,
        "type Even = i64 where self % 2 == 0;\n\nfn half(x: i32) -> i64 {\n    let e = (x as i64) as Even;\n    e / 2\n}\n\nfn main() {\n    println(half(4));\n}\n",
        Some("2\n"),
    );
}

/// Every module-level `let` is refused on its own (E_MODULE_LET, whose
/// remedy is a design choice), so this one cannot check clean: the pin is
/// that the `OnceCell` refusal is gone and that one is all that is left.
#[test]
fn fix_coverage_a_once_cell_at_module_scope_becomes_a_once_lock() {
    let m = mend(
        "oncemod",
        "E_ONCE_CELL_AT_MODULE_SCOPE",
        "let CFG: Map[String, OnceCell[i64]] = Map.new();\n\nfn main() {\n    println(1);\n}\n",
    );
    assert!(m.fixable, "no edit on the first record:\n{}", m.first);
    assert_eq!(
        m.text,
        // The bare `OnceCell` gets its import, then the rename leaves it
        // behind and `OnceLock` gets its own.
        "import std.sync.{OnceCell};\nimport std.sync.{OnceLock};\n\nlet CFG: Map[String, OnceLock[i64]] = Map.new();\n\nfn main() {\n    println(1);\n}\n"
    );
    let after: serde_json::Value =
        serde_json::from_str(m.after.lines().next().unwrap_or("")).expect("JSON");
    let codes: Vec<&str> = after["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["code"].as_str())
        .collect();
    assert_eq!(codes, ["E_MODULE_LET"], "{}", m.after);
}

/// Denied, the irrefutable-pattern lint is E0302 and carries the same edit.
#[test]
fn fix_coverage_a_denied_irrefutable_if_let_becomes_a_block() {
    let m = mend(
        "ifletdeny",
        "E0302",
        "#[deny(irrefutable_let_pattern)]\nfn main() {\n    let t = (1, 2);\n    if let (a, b) = t {\n        println(a + b);\n    }\n}\n",
    );
    assert_mended(
        &m,
        "#[deny(irrefutable_let_pattern)]\nfn main() {\n    let t = (1, 2);\n    { let (a, b) = t;\n        println(a + b);\n    }\n}\n",
        Some("3\n"),
    );
}

/// Denied, the stray doc comment is E0296 and carries the same edit.
#[test]
fn fix_coverage_a_denied_doc_comment_on_a_statement_becomes_a_plain_comment() {
    let m = mend(
        "docdeny",
        "E0296",
        "#![deny(unused_doc_comment)]\nfn main() {\n    /// the answer\n    let x = 42;\n    println(x);\n}\n",
    );
    assert_mended(
        &m,
        "#![deny(unused_doc_comment)]\nfn main() {\n    // the answer\n    let x = 42;\n    println(x);\n}\n",
        Some("42\n"),
    );
}

/// The derive line goes, and with it E_DERIVE_TARGET's refusal of the same
/// attribute, which carries no edit of its own.
#[test]
fn fix_coverage_a_derive_on_a_union_is_removed() {
    let m = mend(
        "unionderive",
        "E_UNION_DERIVE_FORBIDDEN",
        "#[repr(C)]\n#[derive(Debug)]\nunion U {\n    a: i32,\n    b: f32,\n}\n\nfn main() {\n    println(\"hi\")\n}\n",
    );
    assert_mended(
        &m,
        "#[repr(C)]\nunion U {\n    a: i32,\n    b: f32,\n}\n\nfn main() {\n    println(\"hi\")\n}\n",
        Some("hi\n"),
    );
}
