//! A `Json` read through `Json.deserialize` is read as `Json.parse` reads
//! it: a repeated key keeps its first place and takes the last value, so the
//! object holds it once (json.md § A `Json` inside a typed value;
//! B-2026-11-25-37). Serializing it back already wrote the key once, which
//! hid the second entry from every test that compared text.

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
fn a_deserialized_json_object_holds_a_repeated_key_once() {
    let src = r#"import std.json.{Json};
#[derive(Serialize, Deserialize)]
struct Envelope { kind: String, data: Json }
fn show(j: Json) {
    match j {
        Json.Object(o) => {
            println(f"{o.len()} keys");
            for (k, v) in o.iter() {
                println(f"{k} {v.clone().stringify()}");
            }
        }
        _ => println("not an object"),
    }
}
fn main() {
    let e: Envelope = Json.deserialize("{\"kind\":\"e\",\"data\":{\"id\":7,\"x\":1,\"id\":8}}").unwrap();
    show(e.data);
    let j: Json = Json.deserialize("{\"a\":1,\"b\":{\"c\":1,\"c\":[2]},\"a\":3}").unwrap();
    show(j.clone());
    match j {
        Json.Object(o) => show(o[1].1.clone()),
        _ => {}
    }
    show(Json.parse("{\"id\":7,\"x\":1,\"id\":8}").unwrap());
}
"#;
    runs(
        src,
        "2 keys\nid 8\nx 1\n2 keys\na 3\nb {\"c\":[2]}\n1 keys\nc [2]\n2 keys\nid 8\nx 1\n",
    );
}
