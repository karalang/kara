//! The typed JSON reader (`Json.deserialize`) reads JSON's grammar, as
//! `Json.parse` does: surrogate-pair escapes (B-2026-11-17-40); no leading
//! zero, bare fraction, raw control character, out-of-range number, `-0`
//! integer or loose integer map key (B-2026-11-17-41). A derived
//! `Serialize` on a struct variant with a field named `s` builds
//! (B-2026-11-17-42), and `\b`, `\f`, a BOM's column and a bare
//! newtype variant's name read as serde_json has them (B-2026-11-17-43).

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-json-grammar-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

fn run_stdout(path: &std::path::Path) -> String {
    let check = karac(&["check"], path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], path);
    assert!(run.status.success(), "{}", text(&run));
    String::from_utf8_lossy(&run.stdout).into_owned()
}

const TYPES: &str = "import std.json.{Json};
#[derive(Serialize, Deserialize)] struct G { v: String }
#[derive(Serialize, Deserialize)] struct N { #[allow(float_in_serialized_type)] b: f64, d: Vec[i64] }
#[derive(Serialize, Deserialize)] struct I { i: i64 }
#[derive(Serialize, Deserialize)] struct W { u: u64 }
#[derive(Serialize, Deserialize)] struct M { m: SortedMap[i64, String] }
#[derive(Serialize, Deserialize)] enum E { Unit, New(i64), Tup(i64, bool), St { s: i64 } }
#[derive(Serialize, Deserialize)] #[serde(untagged)] enum Un { I { i: i64 }, S { s: String } }
fn show[T: Serialize](r: Result[T, DeserializeError]) -> String {
    match r { Ok(v) => f\"OK {Json.serialize(v)}\", Err(e) => f\"ERR {e}\" }
}
";

fn program(main: &str) -> String {
    format!("{TYPES}fn main() {{\n{main}}}\n")
}

#[test]
fn a_surrogate_pair_escape_reads_as_one_character() {
    let path = scratch(
        "pair.kara",
        &program(
            r#"    let a: Result[G, DeserializeError] = Json.deserialize("{\"v\":\"\\u00e9\\ud83d\\ude00\"}");
    println(show(a));
    let b: Result[G, DeserializeError] = Json.deserialize("{\"v\":\"\\uDBFF\\uDFFF\"}");
    println(f"{b.is_ok()}");
    let c: Result[G, DeserializeError] = Json.deserialize("{\"v\":\"\\ud800\\u0041\"}");
    println(f"{c.is_ok()}");
"#,
        ),
    );
    assert_eq!(run_stdout(&path), "OK {\"v\":\"é😀\"}\ntrue\nfalse\n");
}

#[test]
fn the_typed_reader_refuses_what_json_parse_refuses() {
    let path = scratch(
        "grammar.kara",
        &program(
            r#"    let a: Result[N, DeserializeError] = Json.deserialize("{\"b\":1,\"d\":[01]}");
    println(show(a));
    let b: Result[N, DeserializeError] = Json.deserialize("{\"b\":.5,\"d\":[]}");
    println(show(b));
    let c: Result[N, DeserializeError] = Json.deserialize("{\"b\":1e400,\"d\":[]}");
    println(show(c));
    let d: Result[G, DeserializeError] = Json.deserialize("{\"v\":\"a\tb\"}");
    println(show(d));
    let e: Result[I, DeserializeError] = Json.deserialize("{\"i\":-0}");
    println(show(e));
    let f: Result[W, DeserializeError] = Json.deserialize("{\"u\":-0}");
    println(show(f));
    let g: Result[M, DeserializeError] = Json.deserialize("{\"m\":{\"01\":\"a\"}}");
    println(f"{g.is_ok()}");
    let h: Result[M, DeserializeError] = Json.deserialize("{\"m\":{\" 1\":\"a\"}}");
    println(f"{h.is_ok()}");
    let k: Result[M, DeserializeError] = Json.deserialize("{\"m\":{\"-1\":\"a\",\"10\":\"b\"}}");
    println(show(k));
    let n: Result[N, DeserializeError] = Json.deserialize("{\"b\":-0.0,\"d\":[0,-1,10]}");
    println(show(n));
"#,
        ),
    );
    assert_eq!(
        run_stdout(&path),
        "ERR invalid number at line 1 column 14\n\
         ERR expected value at line 1 column 6\n\
         ERR number out of range at line 1 column 10\n\
         ERR control character (\\u0000-\\u001F) found while parsing a string at line 1 column 8\n\
         ERR i: invalid type: floating point `-0.0`, expected i64 at line 1 column 8\n\
         ERR u: invalid type: floating point `-0.0`, expected u64 at line 1 column 8\n\
         false\nfalse\n\
         OK {\"m\":{\"-1\":\"a\",\"10\":\"b\"}}\n\
         OK {\"b\":-0.0,\"d\":[0,-1,10]}\n"
    );
}

#[test]
fn a_struct_variant_field_named_s_serializes() {
    let path = scratch(
        "hygiene.kara",
        &program(
            r#"    println(Json.serialize(E.St { s: 4 }));
    println(Json.serialize(Un.S { s: "q".to_string() }));
    let e: Result[E, DeserializeError] = Json.deserialize("{\"St\":{\"s\":5}}");
    println(show(e));
"#,
        ),
    );
    assert_eq!(
        run_stdout(&path),
        "{\"St\":{\"s\":4}}\n{\"s\":\"q\"}\nOK {\"St\":{\"s\":5}}\n"
    );
}

#[test]
fn escapes_bom_and_variant_shape_read_as_serde_json_has_them() {
    let path = scratch(
        "small.kara",
        &program(
            r#"    println(Json.serialize(G { v: "\u{8}\u{c}\r".to_string() }));
    match Json.parse("\u{feff}[1]") {
        Ok(_) => println("ok"),
        Err(e) => println(e.message),
    }
    match Json.parse("[\u{e9}]") {
        Ok(_) => println("ok"),
        Err(e) => println(e.message),
    }
    let a: Result[E, DeserializeError] = Json.deserialize("\"New\"");
    println(show(a));
    let b: Result[E, DeserializeError] = Json.deserialize("\"Tup\"");
    println(show(b));
    let c: Result[E, DeserializeError] = Json.deserialize("\"St\"");
    println(show(c));
"#,
        ),
    );
    assert_eq!(
        run_stdout(&path),
        "{\"v\":\"\\b\\f\\r\"}\n\
         expected value at line 1 column 1\n\
         expected value at line 1 column 2\n\
         ERR invalid type: unit variant, expected newtype variant\n\
         ERR invalid type: unit variant, expected tuple variant\n\
         ERR invalid type: unit variant, expected struct variant\n"
    );
}
