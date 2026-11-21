//! Rows found by the conformance pass over design.md §8 (generics, type
//! inference) and §9 (traits, impls). Each test is one sentence of the page
//! the compiler did not keep, or a diagnostic the review thread ruled on;
//! the case each row cites lives under `conformance/design/`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// `karac <args> cell.kara` in `dir`: success, stdout + stderr, and the
/// file as it stands afterwards (for `fix`).
fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let file = std::fs::read_to_string(dir.join("cell.kara")).unwrap_or_default();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        file,
    )
}

fn cell(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-design-conformance-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    dir
}

fn modes() -> Vec<&'static str> {
    let mut m = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        m.push("__mir-llvm-run");
    }
    m
}

/// Clean under `check`, and printing `expected` under every backend.
fn runs_to(tag: &str, src: &str, expected: &str) {
    let dir = cell(tag, src);
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(ok, "{tag}: check: {out}");
    for mode in modes() {
        let (ok, out, _) = karac(&dir, &[mode]);
        assert!(ok, "{tag}: {mode}: {out}");
        assert_eq!(out, expected, "{tag}: {mode}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Refused by `check`; returns the diagnostics.
fn refused(tag: &str, src: &str) -> String {
    let dir = cell(tag, src);
    let (ok, out, _) = karac(&dir, &["check"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ok, "{tag}: accepted, expected a refusal");
    out
}

/// `check`'s output for a program that is accepted (warnings included).
fn accepted(tag: &str, src: &str) -> String {
    let dir = cell(tag, src);
    let (ok, out, _) = karac(&dir, &["check"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{tag}: refused: {out}");
    out
}

/// B-2026-11-20-110: a join of an integer and a float literal is the
/// existing mismatch, not an `i64` binding holding an `f64`.
#[test]
fn design_join_of_int_and_float_literals_is_a_mismatch() {
    let out = refused(
        "int-float-join",
        "fn main() {\n    let c = true;\n    let x = if c { 1 } else { 2.5 };\n    println(f\"{x}\");\n}\n",
    );
    assert!(out.contains("type mismatch in branches"), "{out}");
}

/// B-2026-11-20-111: a type parameter named only in another parameter's
/// bound is solved from the argument's impl, and the call lowers.
#[test]
fn design_param_named_only_in_another_bound_is_solved() {
    runs_to(
        "param-in-bound",
        "trait Source[T] { fn get(self) -> T; }
struct Ten {}
impl Source[i64] for Ten { fn get(self) -> i64 { 10 } }
struct Name {}
impl Source[String] for Name { fn get(self) -> String { \"n\" } }
fn count[T, S: Source[T]](s: S) -> i64 { let _ = s.get(); 1 }
fn shown[T: Display, S: Source[T]](s: S) -> String { f\"<{s.get()}>\" }
fn main() { println(f\"{count(Ten {})} {count(Name {})} {shown(Ten {})} {shown(Name {})}\"); }
",
        "1 1 <10> <n>\n",
    );
}

/// B-2026-11-20-113: a negative const argument to a signed const parameter
/// is its value; `Array[i64, -1]` says why it is refused.
#[test]
fn design_negative_const_argument_reaches_the_body() {
    runs_to(
        "neg-const",
        "struct Small[const S: i8] { v: i64 }
impl[const S: i8] Small[S] { fn s(self) -> i8 { S } }
fn main() {
    let s: Small[-3] = Small { v: 4 };
    let t: Small[5] = Small { v: 4 };
    println(f\"{s.s()} {t.s()}\");
}
",
        "-3 5\n",
    );
    let out = refused(
        "neg-array-len",
        "fn main() { let a: Array[i64, -1] = [1]; }\n",
    );
    assert!(out.contains("literal -1 does not fit in usize"), "{out}");
}

/// B-2026-11-20-114: `Self` in a trait's default associated fn, called on a
/// primitive (`i64.again()`), is the primitive, and the call dispatches to
/// the primitive's impl.
#[test]
fn design_default_assoc_fn_self_on_a_primitive() {
    runs_to(
        "self-prim",
        "trait Unit {
    fn one() -> Self;
    fn again() -> Self { Self.one() }
    fn two() -> (Self, Self) { (Self.one(), Self.one()) }
}
impl Unit for i64 { fn one() -> i64 { 1 } }
struct P { v: i64 }
impl Unit for P { fn one() -> P { P { v: 7 } } }
fn main() {
    let a = i64.again();
    let (c, d) = i64.two();
    let p = P.again();
    println(f\"{a + 1} {c} {d} {p.v}\");
}
",
        "2 1 1 7\n",
    );
}

/// B-2026-11-20-115: a gated std trait supplies its methods only where its
/// name is imported; `import std.log.{Backend}` or `std.log.*` brings it in.
#[test]
fn design_gated_std_trait_methods_need_the_trait_in_scope() {
    let call = "    let mut b = log.json_backend(log.Stream.Stdout);
    b.emit(Record { level: Level.Info, msg: \"hi\", fields: Vec.new(), trace_id: None, span_id: None, task_id: None });
}
";
    let out = refused(
        "trait-out-of-scope",
        &format!("import std.log;\nimport std.log.{{Record, Level}};\nfn main() {{\n{call}"),
    );
    assert!(
        out.contains("a trait supplies methods only where its name is in scope"),
        "{out}"
    );
    assert!(out.contains("Backend"), "{out}");
    accepted(
        "trait-named",
        &format!(
            "import std.log;\nimport std.log.{{Record, Level, Backend}};\nfn main() {{\n{call}"
        ),
    );
}

/// B-2026-11-20-116: the ambiguity diagnostic lists each candidate's
/// effects, the axis on which the two impls differ.
#[test]
fn design_ambiguity_diagnostic_lists_each_candidates_effects() {
    let out = refused(
        "ambiguity-effects",
        "effect resource Db;
trait Reader { fn access(self, key: i64) -> i64 with reads(Db); }
trait Writer { fn access(self, key: i64) -> i64 with writes(Db); }
struct Connection {}
impl Reader for Connection { fn access(self, key: i64) -> i64 with reads(Db) { key } }
impl Writer for Connection { fn access(self, key: i64) -> i64 with writes(Db) { key + 1 } }
fn use_conn(conn: Connection, key: i64) -> i64 { conn.access(key) }
fn main() { println(f\"{use_conn(Connection {}, 1)}\"); }
",
    );
    assert!(out.contains("with reads(Db)"), "{out}");
    assert!(out.contains("with writes(Db)"), "{out}");
}

/// B-2026-11-20-117: `Pod` is a nameable std marker: an empty impl makes a
/// struct of `Pod` fields one, and the impl is checked.
#[test]
fn design_pod_is_a_nameable_marker() {
    runs_to(
        "pod",
        "#[derive(Copy, Clone)]
struct Px { r: u8, g: u8 }
impl Pod for Px { }
fn zero[T: Pod]() -> T { mem.zeroed() }
fn main() {
    let p: Px = zero();
    let n: i64 = zero();
    println(f\"{p.r} {p.g} {n}\");
}
",
        "0 0 0\n",
    );
    let out = refused(
        "pod-bad",
        "struct Bad { s: String }\nimpl Pod for Bad { }\nfn main() { }\n",
    );
    assert!(out.contains("field 's' has non-Pod type"), "{out}");
    let out = refused(
        "pod-enum",
        "enum E { A, B }\nimpl Pod for E { }\nfn main() { }\n",
    );
    assert!(out.contains("an enum is not `Pod`"), "{out}");
}

/// B-2026-11-20-118: one equality constraint per associated type.
#[test]
fn design_where_clause_holds_one_equality_per_assoc_type() {
    let out = refused(
        "two-equalities",
        "fn f[I: Iterator](iter: own I) -> i64 where I.Item = i64, I.Item = String { 0 }\nfn main() {}\n",
    );
    assert!(
        out.contains("a second equality constraint on `I.Item`"),
        "{out}"
    );
}

/// B-2026-11-20-125: a generic impl overlapping a standard one is refused,
/// as the concrete form already was, and says why without "0.8".
#[test]
fn design_generic_impl_overlapping_std_is_refused() {
    let out = refused(
        "overlap-std-display",
        "impl[T: Display] Display for Vec[T] {\n    fn to_string(self) -> String { \"mine\" }\n}\nfn main() { let v: Vec[i64] = [1]; println(f\"{v}\"); }\n",
    );
    assert!(out.contains("conflicting impl"), "{out}");
    let out = refused(
        "overlap-std-eq",
        "impl[T: PartialEq] PartialEq for Vec[T] {\n    fn eq(self, other: Vec[T]) -> bool { true }\n}\nfn main() { let a: Vec[i64] = [1]; let b: Vec[i64] = [2]; println(f\"{a == b}\"); }\n",
    );
    assert!(out.contains("conflicting impl"), "{out}");
}

/// Review ruling 7 (B-2026-11-21-28): the overlap message states the rule and
/// points at the other impl; it does not read as a 0.8 limitation.
#[test]
fn design_overlap_message_names_the_rule_and_the_other_impl() {
    let out = refused(
        "overlap-wording",
        "trait Show { fn show(self) -> String; }
struct W[T] { v: T }
impl[T] Show for W[T] { fn show(self) -> String { \"g\" } }
impl Show for W[i64] { fn show(self) -> String { \"s\" } }
fn main() {}
",
    );
    assert!(out.contains("overlapping impls are not allowed"), "{out}");
    assert!(out.contains("at line 3"), "{out}");
    assert!(!out.contains("0.8 does not support"), "{out}");
}

/// B-2026-11-20-126: `.into()` as the argument to a bare (borrowed)
/// parameter is checked against the parameter's type.
#[test]
fn design_into_argument_to_a_borrowed_parameter() {
    runs_to(
        "into-borrowed",
        "struct Celsius { c: i64 }
struct Fahrenheit { f: i64 }
impl From[Celsius] for Fahrenheit {
    fn from(v: own Celsius) -> Fahrenheit { Fahrenheit { f: v.c * 9 / 5 + 32 } }
}
fn takes(f: Fahrenheit) -> i64 { f.f }
fn main() { println(takes(Celsius { c: 10 }.into())); }
",
        "50\n",
    );
}

/// B-2026-11-20-127: the fix for `#[default]` without `derive(Default)`
/// adds the derive, before `pub` when the enum has it.
#[test]
fn design_default_attribute_fix_adds_the_derive() {
    for (tag, head) in [("plain", "enum"), ("pub", "pub enum")] {
        let src = format!(
            "{head} Status {{ #[default] Pending, Active }}\nfn main() {{ let s = Status.Active; match s {{ Status.Pending => println(\"p\"), Status.Active => println(\"a\") }} }}\n"
        );
        let dir = cell(&format!("default-attr-{tag}"), &src);
        let (ok, _, _) = karac(&dir, &["check"]);
        assert!(!ok, "{tag}");
        let (_, out, fixed) = karac(&dir, &["fix"]);
        assert!(
            fixed.starts_with(&format!("#[derive(Default)]\n{head} Status")),
            "{tag}: {out}\n{fixed}"
        );
        let (ok, out, _) = karac(&dir, &["check"]);
        assert!(ok, "{tag}: {out}\n{fixed}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// B-2026-11-20-128: a `#[default]` variant with a payload points at a
/// manual `impl Default` skeleton.
#[test]
fn design_default_variant_payload_names_the_manual_impl() {
    let out = refused(
        "default-payload",
        "#[derive(Default)]\nenum Status { #[default] Pending(i64), Active }\nfn main() { let _ = Status.default(); }\n",
    );
    assert!(out.contains("E_DEFAULT_VARIANT_HAS_PAYLOAD"), "{out}");
    assert!(
        out.contains("impl Default for Status { fn default() -> Status"),
        "{out}"
    );
}

/// Review ruling 1 (B-2026-11-21-25): a branch mismatch reads "type mismatch in
/// branches: expected X, found Y" with a note listing each branch's type.
#[test]
fn design_branch_mismatch_lists_every_branch_type() {
    let out = refused(
        "lub-match",
        "fn f(n: i64) -> i64 {\n    let v = match n { 0 => 1, 1 => \"one\", _ => 3 };\n    0\n}\nfn main() { println(f(0)); }\n",
    );
    assert!(
        out.contains("type mismatch in branches: expected 'i64', found 'String'"),
        "{out}"
    );
    assert!(out.contains("note: the branches' types are"), "{out}");
}

/// Review ruling 5 (B-2026-11-21-27): `derive(Hash)` alone implies `PartialEq`
/// and `Eq`, and the warning's note names them.
#[test]
fn design_derive_hash_alone_notes_the_implied_derives() {
    let out = accepted(
        "derive-hash",
        "#[derive(Hash)]\nstruct K { a: i64 }\nfn main() { let _ = K { a: 1 }; }\n",
    );
    assert!(out.contains("implied derive on `K`"), "{out}");
    assert!(
        out.contains("note: the derive implies `PartialEq`, `Eq`"),
        "{out}"
    );
}

/// Review ruling 8 (B-2026-11-21-29): E0400 names `Connection.access`, never the
/// compiler's internal `#Writer` impl label.
#[test]
fn design_e0400_names_the_method_without_its_impl_label() {
    let out = refused(
        "e0400-label",
        "pub effect resource Db;
trait Reader { fn access(self, key: i64) -> i64 with reads(Db); }
pub trait Writer { fn access(self, key: i64) -> i64 with writes(Db); }
pub struct Connection {}
impl Reader for Connection { fn access(self, key: i64) -> i64 with reads(Db) { key } }
impl Writer for Connection { fn access(self, key: i64) -> i64 with writes(Db) { key + 1 } }
pub fn read_it(conn: Connection) -> i64 with reads(Db) { Writer.access(conn, 5) }
fn main() { println(f\"{read_it(Connection {})}\"); }
",
    );
    assert!(out.contains("E0400"), "{out}");
    assert!(out.contains("'Connection.access'"), "{out}");
    assert!(!out.contains('#'), "{out}");
}

/// Review ruling 4 (B-2026-11-21-26): `karac query monomorphization` counts the
/// generic calls inside an f-string hole, and a bounded generic's.
#[test]
fn design_monomorphization_query_counts_calls_in_fstrings() {
    let dir = cell(
        "mono-query",
        "trait Sh { fn sh(self) -> i64; }
impl Sh for i64 { fn sh(self) -> i64 { 1 } }
impl Sh for bool { fn sh(self) -> i64 { 2 } }
fn g[T: Sh](x: T) -> i64 { x.sh() }
fn main() { println(f\"{g(1)} {g(true)} {g(2)}\"); }
",
    );
    let (ok, out, _) = karac(&dir, &["query", "monomorphization"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{out}");
    assert!(
        out.contains("\"generic\":\"g\",\"instance_count\":2"),
        "{out}"
    );
}

/// B-2026-11-20-122: a user `Display` for a tuple overlaps the library's
/// and is refused; a tuple's `to_string()` is the library's rendering. A
/// user trait's tuple impl is refused until tuple impls land
/// (B-2026-11-21-30), where it was accepted and then never found.
#[test]
fn design_tuple_display_is_the_librarys() {
    let out = refused(
        "tuple-display",
        "struct H {}
impl Display for H { fn to_string(self) -> String { \"h\" } }
impl Display for (i64, H) { fn to_string(self) -> String { f\"pair {self.0}\" } }
fn main() { let p = (3, H {}); println(f\"{p}\"); }
",
    );
    assert!(out.contains("conflicting impl"), "{out}");
    runs_to(
        "tuple-to-string",
        "fn main() {\n    let p = (3, \"h\");\n    println(f\"{p}\");\n    println(p.to_string());\n}\n",
        "(3, h)\n(3, h)\n",
    );
    let out = refused(
        "tuple-user-trait",
        "trait Show { fn show(self) -> String; }
impl Show for (i64, i64) { fn show(self) -> String { f\"{self.0}-{self.1}\" } }
fn main() { let p = (1, 2); println(p.show()); }
",
    );
    assert!(
        out.contains("a trait impl for a tuple type is not supported yet"),
        "{out}"
    );
}

/// design.md § Const generic parameters: `i128` is a permitted const
/// parameter type, a negative `i128` argument keeps its sign, and the
/// refusal's list of permitted types names it.
#[test]
fn design_i128_const_parameter() {
    runs_to(
        "i128-const",
        "struct A[const N: i128] { v: i64 }
impl[const N: i128] A[N] { fn n(self) -> i128 { N } }
fn main() {
    let a: A[9223372036854775807] = A { v: 1 };
    let b: A[-5] = A { v: 1 };
    println(f\"{a.n()} {b.n()}\");
}
",
        "9223372036854775807 -5\n",
    );
    let out = refused(
        "f64-const",
        "struct C[const N: f64] { v: i64 }\nfn main() {}\n",
    );
    assert!(
        out.contains("allowed types are i8, i16, i32, i64, i128, bool, char, and fieldless enums"),
        "{out}"
    );
}

/// design.md § Const generic parameters: a failed const bound names the
/// predicate as written and the argument values.
#[test]
fn design_const_bound_failure_names_the_predicate() {
    let out = refused(
        "const-bound",
        "fn half[const N: i64](a: Array[i64, N]) -> i64 where N > 2 { N / 2 }
fn main() { let a: Array[i64, 2] = [1, 2]; println(half(a)); }
",
    );
    assert!(
        out.contains("const constraint violated: `N > 2` evaluated to false with N=2"),
        "{out}"
    );
}

/// B-2026-11-20-118's check keys on the trait too: `Add[Output = T]` and
/// `Mul[Output = i64]` fix two associated types that share a name.
#[test]
fn design_same_assoc_name_on_two_traits_is_two_equalities() {
    runs_to(
        "two-traits-output",
        "fn f[T: Add[Output = T] + Mul[Output = i64] + Copy](a: T, b: T) -> (T, i64) { (a + b, a * b) }
fn main() { let (s, p) = f(3, 4); println(f\"{s} {p}\"); }
",
        "7 12\n",
    );
}
