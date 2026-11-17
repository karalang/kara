//! B-2026-11-17-74: `Json.deserialize`'s errors for a sequence of the wrong
//! length (a tuple variant's payload, a tuple field) carry a line and
//! column, as every other typed-reader error does. "Too few" is placed at
//! the `]` that closed the sequence early, where serde_json places an
//! invalid length; "too many" at the first value past the end. Before,
//! the four messages carried no position at all.

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

const SRC: &str = r#"import std.json.{Json};
#[derive(Serialize, Deserialize)] enum E { Unit, New(i64), Tup(i64, bool), St { s: i64 } }
#[derive(Serialize, Deserialize)] struct P { p: (i64, i64) }
fn show[T: Serialize](r: Result[T, DeserializeError]) -> String {
    match r { Ok(v) => f"OK {Json.serialize(v)}", Err(e) => f"ERR {e}" }
}
fn main() {
    let a: Result[E, DeserializeError] = Json.deserialize("{\"Tup\":[1]}");
    println(show(a));
    let b: Result[E, DeserializeError] = Json.deserialize("{\"Tup\":[1,true,3]}");
    println(show(b));
    let c: Result[P, DeserializeError] = Json.deserialize("{\"p\":[1]}");
    println(show(c));
    let d: Result[P, DeserializeError] = Json.deserialize("{\"p\":\n[1,2,3]}");
    println(show(d));
    let e: Result[Vec[E], DeserializeError] = Json.deserialize("[{\"Tup\":[ ]}]");
    println(show(e));
    let f: Result[E, DeserializeError] = Json.deserialize("{\"Tup\":[1,true]}");
    println(show(f));
}
"#;

#[test]
fn json_payload_length_errors_carry_a_position() {
    let dir = std::env::temp_dir().join(format!("karac-json-length-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("length.kara");
    std::fs::write(&path, SRC).unwrap();
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert!(run.status.success(), "{}", text(&run));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "ERR too few values in a variant's payload at line 1 column 10\n\
         ERR too many values in E.Tup at line 1 column 16\n\
         ERR p: too few values in a tuple at line 1 column 8\n\
         ERR p: too many values in a tuple at line 2 column 6\n\
         ERR too few values in a variant's payload at line 1 column 11\n\
         OK {\"Tup\":[1,true]}\n"
    );
}
