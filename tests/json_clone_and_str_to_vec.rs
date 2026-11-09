//! Found building the webhook_relay app.
//!
//! B-2026-11-09-48: `Json` had no `.clone()`, so a request body's field
//! could not be copied out of a borrowed tree, and the refusal said "add
//! `#[derive(Clone)]` to its declaration", which is the library's. `Json`
//! now derives `Clone`, and a library type without the trait is named as
//! such.
//!
//! B-2026-11-09-49: `["a", "b"].to_vec()` handed to a `Vec[Str]` parameter
//! (or `[("k", v)].to_vec()` to a `Vec[(Str, Json)]` one) was refused as
//! "expected 'Vec[Str]', found 'Vec[String]'": the literals defaulted to
//! `String`. The elements now take the wanted `Str`.

mod common;

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn check_err(src: &str) -> String {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let path =
        std::env::temp_dir().join(format!("karac-json-clone-{}-{n}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    if out.status.success() {
        String::new()
    } else {
        String::from_utf8_lossy(&out.stderr).into_owned()
    }
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[cfg(feature = "llvm")]
fn compiled(src: &str) -> Option<String> {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!("karac-json-clone-b-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 0).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    let out = linked.map(|()| Command::new(&exe).output().expect("run"));
    let _ = std::fs::remove_dir_all(&dir);
    let out = out?;
    assert!(out.status.success(), "{:?}", out.status);
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

const JSON_CLONE: &str = "import std.json.{Json};

fn field(doc: Json, name: Str) -> Option[Json] {
    match doc {
        Json.Object(fields) => {
            for (k, v) in fields {
                if k == name {
                    return Some(v.clone());
                }
            }
            None
        }
        _ => None,
    }
}

fn main() {
    let doc = Json.parse(\"{\\\"event\\\": \\\"push\\\", \\\"commits\\\": [{\\\"id\\\": 1}, 2.5, null]}\").unwrap();
    let copy = doc.clone();
    let commits = field(doc, \"commits\") ?? Json.Null;
    println(commits.stringify());
    println(copy.stringify());
    println(doc.stringify());
}
";

const JSON_CLONE_OUT: &str = "[{\"id\":1},2.5,null]
{\"event\":\"push\",\"commits\":[{\"id\":1},2.5,null]}
{\"event\":\"push\",\"commits\":[{\"id\":1},2.5,null]}
";

#[test]
fn json_values_clone() {
    assert_eq!(check_err(JSON_CLONE), "");
    assert_eq!(run(JSON_CLONE), JSON_CLONE_OUT);
}

#[cfg(feature = "llvm")]
#[test]
fn json_values_clone_compiled() {
    if let Some(out) = compiled(JSON_CLONE) {
        assert_eq!(out, JSON_CLONE_OUT);
    }
}

#[test]
fn a_library_type_without_clone_is_named_as_the_librarys() {
    let err = check_err(
        "import std.json.{Json};

fn main() {
    match Json.parse(\"{x\") {
        Ok(_) => println(\"ok\"),
        Err(e) => {
            let f = e.clone();
            println(f.message);
        }
    }
}
",
    );
    assert!(
        err.contains("no method 'clone' on type 'JsonError': the library's `JsonError` does not implement `Clone`"),
        "{err}"
    );
    assert!(!err.contains("#[derive(Clone)]"), "{err}");
}

const STR_TO_VEC: &str = "import std.json.{Json};

fn total(xs: Vec[Str]) -> i64 {
    xs.len()
}

fn obj(pairs: Vec[(Str, Json)]) -> String {
    let fields: Vec[(String, Json)] = pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    Json.Object(fields).stringify()
}

fn main() {
    println(total([\"a\", \"b\", \"c\"].to_vec()));
    println(obj([(\"event\", Json.String(\"push\".to_string())), (\"n\", Json.Int(7))].to_vec()));
}
";

const STR_TO_VEC_OUT: &str = "3\n{\"event\":\"push\",\"n\":7}\n";

#[test]
fn string_literals_in_a_to_vec_take_the_wanted_str() {
    assert_eq!(check_err(STR_TO_VEC), "");
    assert_eq!(run(STR_TO_VEC), STR_TO_VEC_OUT);
}

#[cfg(feature = "llvm")]
#[test]
fn string_literals_in_a_to_vec_take_the_wanted_str_compiled() {
    if let Some(out) = compiled(STR_TO_VEC) {
        assert_eq!(out, STR_TO_VEC_OUT);
    }
}
