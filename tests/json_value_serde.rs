//! `Json` implements `Serialize` and `Deserialize` (B-2026-11-20-28), as
//! serde_json's `Value` does: a typed envelope carries an untyped part, and
//! a `Json` is a log field. Its own `Json.serialize(v)` and
//! `Json.deserialize(text)` stay what `Json.serialize` and
//! `Json.deserialize` call, beside the trait functions of the same names,
//! and `Serialize.serialize(..)` reaches the trait's (B-2026-11-20-65).

use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

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

fn scratch(src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-json-value-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

/// Checks clean, then prints `want` on the interpreter and, with the
/// backend, compiled.
fn runs(src: &str, want: &str) {
    let path = scratch(src);
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(String::from_utf8_lossy(&run.stdout), want, "{}", text(&run));
    if cfg!(feature = "llvm") {
        let exe = path.with_extension("bin");
        let build = karac(&["build", "-o", exe.to_str().unwrap()], &path);
        assert!(build.status.success(), "{}", text(&build));
        let ran = Command::new(&exe).output().unwrap();
        assert!(ran.status.success(), "{}", text(&ran));
        assert_eq!(String::from_utf8_lossy(&ran.stdout), want, "{}", text(&ran));
    }
}

#[test]
fn a_typed_envelope_carries_a_json_part() {
    let src = r#"import std.json.{Json};
#[derive(Serialize, Deserialize)]
struct Envelope { kind: String, data: Json }
fn main() {
    let text = "{\"kind\":\"event\",\"data\":{\"id\":7,\"tags\":[\"a\",true,null,1.5,-0],\"id\":8,\"nested\":{}}}";
    let e: Envelope = Json.deserialize(text).unwrap();
    println(e.kind);
    println(e.data.clone().stringify());
    println(Json.serialize(e));
    let j = Json.parse("[1,{\"a\":2,\"a\":3}]").unwrap();
    println(Json.serialize(j));
    let r: Result[Envelope, DeserializeError] = Json.deserialize("{\"kind\":\"e\",\"data\":tru}");
    match r {
        Ok(_) => println("read"),
        Err(err) => println(err),
    }
}
"#;
    runs(src, "event\n{\"id\":8,\"tags\":[\"a\",true,null,1.5,-0.0],\"nested\":{}}\n{\"kind\":\"event\",\"data\":{\"id\":8,\"tags\":[\"a\",true,null,1.5,-0.0],\"nested\":{}}}\n[1,{\"a\":3}]\nexpected ident at line 1 column 23\n");
}

#[test]
fn json_serialize_and_deserialize_stay_inherent_beside_the_trait_fns() {
    let src = r#"import std.json.{Json};
struct Wrapped { data: Json }
impl Serialize for Wrapped {
    fn serialize[S: Serializer](ref self, s: mut ref S) {
        s.serialize_seq_begin(1);
        s.serialize_element();
        Serialize.serialize(self.data, s);
        s.serialize_seq_end();
    }
}
#[derive(Serialize, Deserialize)]
struct User { name: String, age: i64 }
fn main() {
    let u: User = Json.deserialize("{\"name\":\"ada\",\"age\":36}").unwrap();
    println(Json.serialize(u));
    let w = Wrapped { data: Json.parse("{\"k\":[true]}").unwrap() };
    println(Json.serialize(w));
    let j: Json = Json.deserialize("[1, \"two\"]").unwrap();
    println(Json.serialize(j));
}
"#;
    runs(
        src,
        "{\"name\":\"ada\",\"age\":36}\n[{\"k\":[true]}]\n[1,\"two\"]\n",
    );
}

#[test]
fn a_json_value_is_a_log_field() {
    let src = r#"import std.log;
import std.json.{Json};
fn main() {
    log.init(log.json_backend(log.Stream.Stdout));
    let j = Json.parse("{\"a\":[1,2.5,null],\"b\":\"x y\",\"a\":7}").unwrap();
    log.info("got", body: j, n: 3);
    log.init(log.text_backend(log.Stream.Stdout));
    log.info("got", body: j, s: Json.String("plain"));
    println(j.stringify());
}
"#;
    runs(src, "{\"level\":\"info\",\"msg\":\"got\",\"body\":{\"a\":7,\"b\":\"x y\"},\"n\":3,\"task_id\":null}\nINFO  got body={\"a\":7,\"b\":\"x y\"} s=plain\n{\"a\":7,\"b\":\"x y\"}\n");
}
