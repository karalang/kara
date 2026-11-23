//! B-2026-11-22-40: `karac check` of a program with a comptime-backed
//! derive type-checked the library's bodies three times: in check's first
//! pass, in the comptime evaluation, and in the re-check after the derived
//! items were spliced. Only the evaluation runs them, so they are now set
//! aside for it and the other two passes check the program against the
//! library's signatures, as they check a program with no derive.
//! `KARAC_CHECK_LIBRARY_BODIES=1` keeps the old path, which is the control
//! arm here: it must report the same diagnostics.
//!
//! The passes after the type check read the library as they read it for a
//! program with no derive, so a derive no longer changes what the program's
//! own code is found to do: under the old path a function that returned
//! `s.split(",").map(..).collect()` was found to perform `panics` once any
//! type in the file derived `Serialize`, and not before (measured at
//! c9243e3303; `KARAC_CHECK_LIBRARY_BODIES=1` still shows it).
//!
//! And an evaluation whose derives are all the library's is not run again
//! in the same process while the program's types stay as they were: an
//! editor checks the buffer after every edit, and most edits change no type.

use std::path::PathBuf;
use std::process::{Command, Stdio};

const PROGRAM: &str = r#"import std.json.{Json};

#[derive(Serialize, Deserialize)]
struct Tag { name: String, weight: i64 }

shared struct Node {
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
    let t = Tag { name: "a".to_string(), weight: 2 };
    println(Json.serialize(t));
}
"#;

const SET_ASIDE: &str = "library bodies set aside for the comptime evaluation";
const REFUSAL: &str = "cannot mutably borrow `self` while `node` still holds a borrow of it";

fn cell(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-derive-bodies-once-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    dir
}

/// Whether the check passed, what it reported, and whether it set the
/// library's bodies aside.
fn check(dir: &PathBuf, bodies: Option<&str>) -> (bool, String, bool) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.args(["check", "cell.kara"])
        .current_dir(dir)
        .env("KARAC_MIR_TIME", "1")
        .env_remove("KARAC_CHECK_LIBRARY_BODIES")
        .env_remove("KARAC_CHECK_MIR")
        .stdin(Stdio::null());
    if let Some(v) = bodies {
        cmd.env("KARAC_CHECK_LIBRARY_BODIES", v);
    }
    let out = cmd.output().expect("karac");
    let all =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    let aside = all.contains(SET_ASIDE);
    let reported: String = all
        .lines()
        .filter(|l| !l.starts_with("[mir-time]"))
        .map(|l| format!("{l}\n"))
        .collect();
    (out.status.success(), reported, aside)
}

#[test]
fn derive_program_checks_against_library_signatures_outside_the_evaluation() {
    for (tag, src) in [
        ("refused", PROGRAM.to_string()),
        (
            "accepted",
            PROGRAM.replace("self.index.get(key);", "self.index.get(key).cloned();"),
        ),
        (
            "misuse",
            PROGRAM.replace(
                "println(Json.serialize(t));",
                "let n: i64 = Json.serialize(t);",
            ),
        ),
    ] {
        let dir = cell(tag, &src);
        let (ok, reported, aside) = check(&dir, None);
        assert!(aside, "{tag}: the bodies stayed in: {reported}");
        // The control arm: every pass checks the bodies, and reports the same.
        let (ok_old, reported_old, aside_old) = check(&dir, Some("1"));
        assert!(!aside_old, "{tag}: control arm is void: {reported_old}");
        assert_eq!(
            ok, ok_old,
            "{tag}: {reported}\n--- control ---\n{reported_old}"
        );
        assert_eq!(reported, reported_old, "{tag}");
        match tag {
            "refused" => assert!(!ok && reported.matches(REFUSAL).count() == 1, "{reported}"),
            "accepted" => assert!(ok, "{reported}"),
            _ => assert!(!ok && reported.contains("cell.kara:"), "{reported}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A package of one file, `src/main.kara`.
fn package(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-derive-bodies-once-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/main.kara"), src).unwrap();
    dir
}

/// What `karac check` prints for the package, as text or as JSON.
fn check_output(dir: &PathBuf, json: bool) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.arg("check");
    if json {
        cmd.arg("--output=json");
    }
    let out = cmd
        .current_dir(dir)
        .env("KARAC_HASH_SEED", "1")
        .env_remove("KARAC_CHECK_LIBRARY_BODIES")
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

/// The same file with and without a derive on a type nothing uses gives the
/// same check, text and JSON: the public function's effects, the program's,
/// the warnings. The derive is the file's last item, so no line of the rest
/// moves. Under the old path the JSON gave `parts` one more effect, `panics`,
/// in the file with the derive, and the program `receives(Channel)` for its
/// `String.find`, which the old path took for `Iterator.find` over every
/// `next` in the library, a channel's among them.
#[test]
fn a_derive_on_an_unused_type_leaves_the_rest_of_the_check_as_it_was() {
    let rest = "pub fn parts(s: String) -> Vec[String] {\n    s.split(\",\").map(|p| \
                p.to_string()).collect()\n}\n\nfn has_comma(s: String) -> bool {\n    \
                s.find(\",\").is_some()\n}\n\nfn main() {\n    let n: i64 = 1;\n    let m = n \
                as i64;\n    println(parts(\"a,b\".to_string()).len() + m);\n    \
                println(has_comma(\"a,b\".to_string()));\n}\n";
    let plain = package("unused-plain", rest);
    let derived = package(
        "unused-derive",
        &format!("{rest}\n#[derive(Serialize)]\nstruct Unused {{ x: i64 }}\n"),
    );
    for json in [false, true] {
        let (without, with) = (check_output(&plain, json), check_output(&derived, json));
        assert!(without.contains("redundant cast"), "{without}");
        assert_eq!(without, with, "json: {json}");
        if json {
            let v: serde_json::Value = serde_json::from_str(&with).expect("json");
            let listed = |key: &str| v[key].as_array().is_some_and(|e| !e.is_empty());
            assert!(listed("program_effects"), "{v}");
            let program = v["program_effects"].to_string();
            assert!(!program.contains("Channel"), "{program}");
            assert!(
                v["public_function_effects"]["parts"]
                    .as_array()
                    .is_some_and(|e| !e.is_empty()),
                "{v}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&plain);
    let _ = std::fs::remove_dir_all(&derived);
}

/// A program whose one derive is the library's, with `field` in its type
/// and `body` in its `main`.
fn probe(field: &str, body: &str) -> String {
    format!(
        "import std.json.{{Json}};\n\n#[derive(Serialize)]\nstruct DeriveCacheProbe {{ {field}: i64 \
         }}\n\nfn main() {{\n    let p = DeriveCacheProbe {{ {field}: 1 }};\n    {body}\n}}\n"
    )
}

#[test]
fn an_edit_that_changes_no_type_reuses_the_derives_answers() {
    let errors = |src: &str| -> Vec<String> {
        karac::check_source(src)
            .into_iter()
            .filter(|d| d.severity == "error")
            .map(|d| format!("{}:{}: {}", d.line, d.column, d.message))
            .collect()
    };
    let stats = karac::comptime::derive_cache_stats;
    let (hits, runs) = stats();
    // The first check of these types evaluates the derive.
    let found = errors(&probe("x", "println(Json.serialize(p));"));
    assert!(found.is_empty(), "{found:?}");
    assert_eq!(stats(), (hits, runs + 1));
    // An edit to a body: the answer is reused, and the derived impl is there.
    let found = errors(&probe(
        "x",
        "println(Json.serialize(p));\n    println(\"again\");",
    ));
    assert!(found.is_empty(), "{found:?}");
    assert_eq!(stats(), (hits + 1, runs + 1));
    // Reused, and still checked against: a misuse of the derived impl is an
    // error at its own line.
    let found = errors(&probe("x", "let n: i64 = Json.serialize(p);"));
    assert!(found.len() == 1 && found[0].starts_with("8:"), "{found:?}");
    assert_eq!(stats(), (hits + 2, runs + 1));
    // An edit to the type: the derive is evaluated again, for the new field.
    let found = errors(&probe("y", "println(Json.serialize(p));"));
    assert!(found.is_empty(), "{found:?}");
    assert_eq!(stats(), (hits + 2, runs + 2));
    // A derive of the program's own is evaluated every time: what it returns
    // can depend on any of the program's code.
    let own = "comptime fn derive_describe(comptime T: Type) -> Vec[Item] {\n    let n = \
               T.name();\n    [ast.item(\"impl \" + n + \" { fn describe(self) -> String { \\\"a \" \
               + n + \"\\\" } }\")]\n}\n\n#[derive(Describe)]\nstruct OwnDeriveProbe { x: i64 \
               }\n\nfn main() {\n    let w = OwnDeriveProbe { x: 7 };\n    \
               println(w.describe());\n}\n";
    let before = stats();
    let first = errors(own);
    let second = errors(own);
    assert!(
        first.is_empty() && second.is_empty(),
        "{first:?} {second:?}"
    );
    assert_eq!(stats(), before);
}

/// A file's `program_effects` under `karac check --output=json`, sorted.
fn program_effects(tag: &str, src: &str) -> Vec<String> {
    let dir = cell(tag, src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "--output=json", "cell.kara"])
        .current_dir(&dir)
        .env_remove("KARAC_CHECK_LIBRARY_BODIES")
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert!(
        text.contains("E_HELPER_ATTRIBUTE_INVALID_POSITION"),
        "{tag}: {text}"
    );
    let mut effects: Vec<String> = v["program_effects"]
        .as_array()
        .unwrap_or_else(|| panic!("{tag}: {text}"))
        .iter()
        .map(|e| e.as_str().unwrap().to_string())
        .collect();
    effects.sort();
    effects
}

/// Name resolution refuses the program, so nothing is lowered and the
/// evaluation never runs. The effects the check still reports are the ones
/// it reports for the same program with no derive: the bodies set aside for
/// the evaluation go back for the effect pass. Before that the derive
/// program reported `writes(Stdout)` alone, with no `allocates(Heap)` or
/// `panics` for its `Json.serialize`.
#[test]
fn a_derive_program_refused_at_name_resolution_reports_a_plain_programs_effects() {
    let plain = "import std.json.{Json};\n\nstruct Point { x: i64, y: i64 }\n\n#[serde(rename: \
                 \"make\")]\nfn make_point() -> Point { Point { x: 1, y: 2 } }\n\nfn main() {\n    \
                 println(Json.serialize(make_point()));\n}\n";
    let derived = plain.replace("struct Point", "#[derive(Serialize)]\nstruct Point");
    let without = program_effects("refused-plain", plain);
    let with = program_effects("refused-derive", &derived);
    assert!(
        without.iter().any(|e| e == "allocates(Heap)"),
        "{without:?}"
    );
    assert_eq!(without, with);
}
