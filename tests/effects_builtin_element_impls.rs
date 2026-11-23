//! B-2026-11-23-15: a built-in operation that runs a trait impl on the
//! values it holds has that impl's effects. `xs.contains(x)` on a
//! `Vec[Big]` calls `Big`'s `eq`, `m.insert(k, v)` hashes `k`, `println(x)`
//! renders through `Display`, and a derived impl calls the same trait on
//! each field; none of them was charged, so a `Big.eq` that writes Stderr
//! was invisible behind `contains` to `#[no_effect]`, to a public clause and
//! to the auto-parallel gate. The list of methods is the one docs/library
//! bounds on the element (`where T: PartialEq`), and for the keyed
//! collections every method that files or looks up a key.

use std::path::PathBuf;
use std::process::{Command, Stdio};

const LOUD: &str = "eprintln(\"loud\");";

const EQ: &str = "impl PartialEq for Big {
    fn eq(self, other: Big) -> bool {
        eprintln(\"loud\");
        self.v == other.v
    }
}
";

const QUIET_EQ: &str = "impl PartialEq for Big {
    fn eq(self, other: Big) -> bool { self.v == other.v }
}

impl Eq for Big {}
";

const ORD: &str = "impl PartialOrd for Big {
    fn partial_cmp(self, other: Big) -> Option[Ordering] { Some(self.cmp(other)) }
}

impl Ord for Big {
    fn cmp(self, other: Big) -> Ordering {
        eprintln(\"loud\");
        self.v.cmp(other.v)
    }
}
";

const HASH: &str = "impl Hash for Big {
    fn hash[H: Hasher](self, state: mut ref H) {
        eprintln(\"loud\");
        self.v.hash(mut state);
    }
}
";

const DISPLAY: &str = "impl Display for Big {
    fn to_string(self) -> String {
        eprintln(\"loud\");
        self.v.to_string()
    }
}
";

const CLONE: &str = "impl Clone for Big {
    fn clone(self) -> Big {
        eprintln(\"loud\");
        Big { v: self.v }
    }
}
";

const DEFAULT: &str = "impl Default for Big {
    fn default() -> Big {
        eprintln(\"loud\");
        Big { v: 0 }
    }
}
";

/// `check --output=json` of a one-file program: its error messages, and its
/// `program_effects` when it has no error.
fn check(tag: &str, src: &str) -> (Vec<String>, Vec<String>) {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("karac-elem-fx-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "cell.kara", "--output=json"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let errors = v["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter(|d| d["severity"] == "error")
        .map(|d| d["message"].as_str().unwrap_or_default().to_string())
        .collect();
    let effects = v["program_effects"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|e| e.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    (errors, effects)
}

fn program(impls: &str, body: &str) -> String {
    format!(
        "struct Big {{ v: i64 }}\n\n{impls}\nfn main() {{\n    let a = Big {{ v: 1 }};\n    let b = \
         Big {{ v: 2 }};\n    {body}\n}}\n"
    )
}

fn writes_stderr(effects: &[String]) -> bool {
    effects.iter().any(|e| e == "writes(Stderr)")
}

/// The program reports `writes(Stderr)`, and the same program with the write
/// taken out of the impl does not: the charge is the impl's.
fn loud_then_quiet(tag: &str, src: &str) {
    let (errors, effects) = check(tag, src);
    assert!(errors.is_empty(), "{tag}: {errors:?}");
    assert!(writes_stderr(&effects), "{tag}: {effects:?}");
    let (errors, effects) = check(&format!("{tag}-quiet"), &src.replace(LOUD, ""));
    assert!(errors.is_empty(), "{tag} quiet: {errors:?}");
    assert!(!writes_stderr(&effects), "{tag} quiet: {effects:?}");
}

#[test]
fn a_built_in_that_compares_elements_is_charged_their_eq() {
    for (tag, body) in [
        ("contains", "let xs = [a];\n    let e = xs.contains(b);"),
        (
            "starts-with",
            "let xs = [a];\n    let ys = [b];\n    let e = xs.starts_with(ys.as_slice());",
        ),
        ("dedup", "let mut xs = [a, b];\n    xs.dedup();"),
        (
            "vec-eq",
            "let xs = [a];\n    let ys = [b];\n    let e = xs == ys;",
        ),
        (
            "vec-ne",
            "let xs = [a];\n    let ys = [b];\n    let e = xs != ys;",
        ),
        ("option-eq", "let e = Some(a) == Some(b);"),
        ("tuple-eq", "let e = (a, 1) == (b, 1);"),
    ] {
        loud_then_quiet(tag, &program(EQ, body));
    }
}

#[test]
fn a_built_in_that_orders_elements_is_charged_their_cmp() {
    let impls = format!("{QUIET_EQ}\n{ORD}");
    for (tag, body) in [
        ("sort", "let mut xs = [a, b];\n    xs.sort();"),
        (
            "binary-search",
            "let xs = [a];\n    let r = xs.binary_search(b);",
        ),
        ("is-sorted", "let xs = [a, b];\n    let r = xs.is_sorted();"),
        ("iter-max", "let xs = [a, b];\n    let m = xs.iter().max();"),
        ("iter-min", "let xs = [a, b];\n    let m = xs.iter().min();"),
        (
            "sort-by-key",
            "let mut xs = [1, 2];\n    xs.sort_by_key(|x| Big { v: 0 - x });",
        ),
        (
            "sorted-set",
            "let mut s: SortedSet[Big] = SortedSet.new();\n    s.insert(a);\n    s.insert(b);",
        ),
        (
            "sorted-map-get",
            "let mut m: SortedMap[Big, i64] = SortedMap.new();\n    let g = m.get(a);",
        ),
        (
            "priority-queue",
            "let mut q: PriorityQueue[Big] = PriorityQueue.new();\n    q.push(a);\n    q.push(b);",
        ),
        (
            "collect-sorted",
            "let xs = [a, b];\n    let s: SortedSet[Big] = xs.into_iter().collect();",
        ),
    ] {
        loud_then_quiet(tag, &program(&impls, body));
    }
}

#[test]
fn a_keyed_collection_is_charged_its_keys_hash() {
    let impls = format!("{QUIET_EQ}\n{HASH}");
    for (tag, body) in [
        (
            "map-insert",
            "let mut m: Map[Big, i64] = Map.new();\n    m.insert(a, 1);",
        ),
        (
            "map-get",
            "let m: Map[Big, i64] = Map.new();\n    let g = m.get(b);",
        ),
        (
            "map-index",
            "let mut m: Map[Big, i64] = Map.new();\n    m[a] = 1;",
        ),
        (
            "set-insert",
            "let mut s: Set[Big] = Set.new();\n    s.insert(a);",
        ),
        (
            "set-contains",
            "let s: Set[Big] = Set.new();\n    let e = s.contains(b);",
        ),
        ("map-from", "let m = Map.from([(a, 1), (b, 2)]);"),
        (
            "collect-set",
            "let xs = [a, b];\n    let s: Set[Big] = xs.into_iter().collect();",
        ),
    ] {
        loud_then_quiet(tag, &program(&impls, body));
    }
}

/// A `Map`'s value is never hashed, and a sequence's `len` runs no
/// element's impl. (Every method of a keyed type is charged its key's
/// traits, which the type's declaration bounds: B-2026-11-23-17.)
#[test]
fn a_keyed_collection_is_not_charged_what_it_does_not_run() {
    let impls = format!("{QUIET_EQ}\n{HASH}");
    for (tag, body) in [
        (
            "value",
            "let mut m: Map[i64, Big] = Map.new();\n    m.insert(1, a);\n    let g = m.get(2);",
        ),
        (
            "vec",
            "let xs = [a, b];\n    let n = xs.len();\n    let f = xs.first();",
        ),
    ] {
        let (errors, effects) = check(&format!("unkeyed-{tag}"), &program(&impls, body));
        assert!(errors.is_empty(), "{tag}: {errors:?}");
        assert!(!writes_stderr(&effects), "{tag}: {effects:?}");
    }
}

#[test]
fn rendering_a_value_is_charged_the_display_of_what_it_holds() {
    for (tag, body) in [
        ("println", "println(a);"),
        ("println-vec", "let xs = [a, b];\n    println(xs);"),
        ("println-option", "println(Some(a));"),
        ("print", "print(a);"),
        ("f-string", "let s = f\"{a}\";"),
        ("f-string-vec", "let xs = [a];\n    let s = f\"{xs}\";"),
        ("join", "let xs = [a, b];\n    let s = xs.join(\",\");"),
        (
            "vec-to-string",
            "let xs = [a, b];\n    let s = xs.to_string();",
        ),
    ] {
        loud_then_quiet(tag, &program(DISPLAY, body));
    }
}

#[test]
fn copying_elements_is_charged_their_clone_and_filling_in_their_default() {
    for (tag, body) in [
        ("vec-clone", "let xs = [a, b];\n    let ys = xs.clone();"),
        (
            "to-vec",
            "let xs = [a, b];\n    let ys = xs.as_slice().to_vec();",
        ),
        ("resize", "let mut xs = [a];\n    xs.resize(3, b);"),
        ("filled", "let xs = Vec.filled(3, a);"),
        ("option-clone", "let o = Some(a);\n    let p = o.clone();"),
        (
            "iter-cloned",
            "let xs = [a, b];\n    let ys: Vec[Big] = xs.iter().cloned().collect();",
        ),
    ] {
        loud_then_quiet(tag, &program(CLONE, body));
    }
    loud_then_quiet(
        "unwrap-or-default",
        &program(
            DEFAULT,
            "let o: Option[Big] = None;\n    let d = o.unwrap_or_default();",
        ),
    );
}

/// A derived impl runs the trait on each field, so it reaches a field's
/// written impl, through as many derived types as lie between.
#[test]
fn a_derived_impl_is_charged_what_its_fields_run() {
    let wrap = "#[derive(PartialEq, Clone)]\nstruct Wrap { b: Big }\n\n#[derive(PartialEq, \
                Clone)]\nstruct Outer { w: Wrap, n: i64 }\n";
    for (tag, impls, body) in [
        ("eq", EQ, "let e = Wrap { b: a } == Wrap { b: b };"),
        (
            "eq-nested",
            EQ,
            "let e = Outer { w: Wrap { b: a }, n: 1 } == Outer { w: Wrap { b: b }, n: 1 };",
        ),
        ("eq-method", EQ, "let e = Wrap { b: a }.eq(Wrap { b: b });"),
        (
            "contains",
            EQ,
            "let xs = [Wrap { b: a }];\n    let e = xs.contains(Wrap { b: b });",
        ),
        (
            "clone",
            CLONE,
            "let w = Wrap { b: a };\n    let x = w.clone();",
        ),
    ] {
        // The derive needs both traits on `Big`; the one not under test is
        // written quietly.
        let other = match impls == EQ {
            true => CLONE.replace(LOUD, ""),
            false => EQ.replace(LOUD, ""),
        };
        loud_then_quiet(
            &format!("derived-{tag}"),
            &program(&format!("{impls}\n{other}\n{wrap}"), body),
        );
    }
}

/// Inside a generic function the element is a type parameter, and each
/// call that instantiates it is charged the instance's impl: `Big`'s at
/// `Big`, none at `i64` (which before this was charged every `eq` in the
/// program, `Big`'s included).
#[test]
fn a_built_in_on_a_type_parameter_is_charged_at_each_instance() {
    let generic = "fn has[T: PartialEq](xs: Vec[T], x: T) -> bool {\n    xs.contains(x)\n}\n\nfn \
                   same[T: PartialEq](a: T, b: T) -> bool {\n    a.eq(b)\n}\n";
    let src = |call: &str| {
        format!(
            "struct Big {{ v: i64 }}\n\n{EQ}\n{generic}\nfn main() {{\n    let e = {call};\n}}\n"
        )
    };
    loud_then_quiet("generic-big", &src("has([Big { v: 1 }], Big { v: 2 })"));
    loud_then_quiet(
        "generic-nested",
        &src("same([Big { v: 1 }], [Big { v: 2 }])"),
    );
    for (tag, call) in [
        ("has", "has([1, 2], 2)"),
        ("same", "same(1, 2)"),
        ("nested", "same([1], [2])"),
    ] {
        let (errors, effects) = check(&format!("generic-int-{tag}"), &src(call));
        assert!(errors.is_empty(), "{tag}: {errors:?}");
        assert!(!writes_stderr(&effects), "{tag}: {effects:?}");
    }
}

/// What the under-report let through: `#[no_effect]` on a function that
/// writes Stderr through `contains`, and a public function that owes the
/// clause.
#[test]
fn a_built_ins_element_effects_reach_no_effect_and_a_public_clause() {
    let src = format!(
        "struct Big {{ v: i64 }}\n\n{EQ}\n#[no_effect(writes(Stderr))]\nfn has(xs: Vec[Big], x: \
         Big) -> bool {{\n    xs.contains(x)\n}}\n\nfn main() {{\n    println(has([Big {{ v: 1 \
         }}], Big {{ v: 2 }}));\n}}\n"
    );
    let (errors, _) = check("no-effect", &src);
    assert!(
        errors.len() == 1 && errors[0].contains("writes(Stderr)"),
        "{errors:?}"
    );
    let src = format!(
        "pub struct Big {{ v: i64 }}\n\n{EQ}\npub fn has(xs: Vec[Big], x: Big) -> bool {{\n    \
         xs.contains(x)\n}}\n\nfn main() {{\n    println(has([Big {{ v: 1 }}], Big {{ v: 2 \
         }}));\n}}\n"
    );
    let (errors, _) = check("pub", &src);
    assert!(
        errors.len() == 1 && errors[0].contains("'has' performs effects"),
        "{errors:?}"
    );
    assert!(errors[0].contains("writes(Stderr)"), "{errors:?}");
}
