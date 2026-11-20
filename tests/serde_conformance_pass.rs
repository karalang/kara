//! The Serialize/Json conformance pass (B-2026-11-20-23..30), against
//! serde_json 1.0.149 with `preserve_order`:
//! - B-2026-11-20-24: an unknown `#[serde(...)]` key did nothing, so a
//!   misspelt `skip_serializing` wrote the field it was meant to hide. It is
//!   an error now, with a did-you-mean rename `karac fix` applies.
//! - B-2026-11-20-25: `Json.serialize` wrote a float in Kāra's own text
//!   (`1e-07`) and widened an `f32` (`0.10000000149011612`); it writes what
//!   `Json.stringify` and serde_json write.
//! - B-2026-11-20-26: `Str` had no `Serialize`, though log.md lists it, and
//!   a trait impl on `Str` never resolved at all.
//! - B-2026-11-20-29: an `f64` field read `null` as NaN; it is the
//!   wrong-shape error, as in serde_json (review-thread ruling 2026-11-20).

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
        "karac-serde-pass-{}-{}",
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
fn serde_misspelt_skip_serializing_is_refused_with_a_rename() {
    let src = "import std.json.{Json};\n#[derive(Serialize)]\nstruct Login {\n    user: String,\n    #[serde(skip_serialising)]\n    password: String,\n}\nfn main() {\n    println(Json.serialize(Login { user: \"a\".to_string(), password: \"p\".to_string() }));\n}\n";
    let path = scratch(src);
    let check = karac(&["check"], &path);
    let out = text(&check);
    assert!(!check.status.success(), "{out}");
    assert!(
        out.contains("unknown `serde` key `skip_serialising`; did you mean `skip_serializing`?"),
        "{out}"
    );
    let fix = karac(&["fix"], &path);
    assert!(fix.status.success(), "{}", text(&fix));
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(fixed.contains("#[serde(skip_serializing)]"), "{fixed}");
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "{\"user\":\"a\"}\n",
        "{}",
        text(&run)
    );
}

#[test]
fn serde_unknown_enum_and_bare_valued_keys_are_refused() {
    let src = "#[derive(Serialize, Deserialize)]\n#[serde(tagg: \"kind\")]\nenum E { A { x: i64 } }\n#[derive(Serialize, Deserialize)]\nstruct T {\n    #[serde(rename)]\n    a: i64,\n    #[serde(skip: true)]\n    b: i64,\n    #[serde(zzzzzz)]\n    c: i64,\n}\n#[derive(Serialize)]\nenum V { #[serde(skip)] One, Two { #[serde(rename: \"y\")] x: i64 } }\nfn main() { }\n";
    let path = scratch(src);
    let check = karac(&["check"], &path);
    let out = text(&check);
    assert!(!check.status.success(), "{out}");
    for want in [
        "unknown `serde` key `tagg`; did you mean `tag`? On an enum the derives read `tag` and `untagged`",
        "`serde(rename: ...)` needs a value",
        "`serde(skip)` takes no value; write `skip`",
        "unknown `serde` key `zzzzzz`. On a field the derives read",
        "unknown `serde` key `skip`. On a variant the derives read `rename`",
        "unknown `serde` key `rename`. The derives read no `serde` key on a variant's field",
    ] {
        assert!(out.contains(want), "missing {want:?} in {out}");
    }
}

#[test]
fn serde_known_keys_still_check_clean() {
    runs(
        "import std.json.{Json};\n#[derive(Serialize, Deserialize, Debug)]\n#[serde(tag: \"type\")]\nenum M { #[serde(rename: \"p\")] Ping, D { n: i64 } }\n#[derive(Serialize, Deserialize, Debug)]\n#[serde(untagged)]\nenum U { I(i64), S(String) }\n#[derive(Serialize, Deserialize, Debug)]\nstruct C {\n    #[serde(rename: \"server_name\")]\n    name: String,\n    #[serde(skip)]\n    cache: Vec[i64],\n    #[serde(default: 8080)]\n    port: i64,\n    #[serde(default)]\n    note: String,\n    #[serde(skip_serializing, default)]\n    hidden: i64,\n    #[serde(skip_deserializing)]\n    shown: i64,\n}\nfn main() {\n    println(Json.serialize(M.Ping));\n    println(Json.serialize(U.S(\"s\".to_string())));\n    println(Json.serialize(C { name: \"x\".to_string(), cache: Vec[1], port: 1, note: \"n\".to_string(), hidden: 2, shown: 3 }));\n}\n",
        "{\"type\":\"p\"}\n\"s\"\n{\"server_name\":\"x\",\"port\":1,\"note\":\"n\",\"shown\":3}\n",
    );
}

#[test]
fn json_serialize_writes_floats_as_stringify_and_serde_json_do() {
    runs(
        "import std.json.{Json};\nfn main() {\n    let xs: Vec[f64] = Vec[1e-7, 1e-5, 0.00001234, 1e16, 1e15, 1e21, 0.1, -0.0, 1.0 / 0.0];\n    println(Json.serialize(xs));\n    let fs: Vec[f32] = Vec[0.1, 1.5, 3.4e38, 1e-7];\n    println(Json.serialize(fs));\n}\n",
        "[1e-7,0.00001,0.00001234,1e+16,1000000000000000.0,1e+21,0.1,-0.0,null]\n[0.1,1.5,3.4e+38,1e-7]\n",
    );
}

#[test]
fn a_str_serializes_and_logs_as_its_text() {
    runs(
        "import std.json.{Json};\nimport std.log;\nfn main() {\n    log.init(log.json_backend(log.Stream.Stdout));\n    let line = \"user=ada id=7\".to_string();\n    let parts: Vec[Str] = line.split(\" \").collect();\n    let who: Str = parts[0];\n    log.info(\"seen\", who: who, f: 0.1 as f32);\n    println(Json.serialize(who));\n    println(Json.serialize(parts));\n}\n",
        "{\"level\":\"info\",\"msg\":\"seen\",\"who\":\"user=ada\",\"f\":0.1,\"task_id\":null}\n\"user=ada\"\n[\"user=ada\",\"id=7\"]\n",
    );
}

#[test]
fn a_user_trait_impl_on_str_resolves_directly_and_through_a_bound() {
    runs(
        "trait Shout { fn shout(ref self) -> String; }\nfn up(v: Str) -> String { v.to_uppercase() }\nimpl Shout for Str { fn shout(ref self) -> String { up(self) + self.to_uppercase() } }\nfn loud[T: Shout](x: ref T) -> String { x.shout() }\nfn main() {\n    let s: Str = \"ab\";\n    println(s.shout());\n    println(loud(s));\n    println(s.len());\n}\n",
        "ABAB\nABAB\n2\n",
    );
}

#[test]
fn null_into_an_f64_field_is_the_wrong_shape() {
    runs(
        "import std.json.{Json};\n#[derive(Serialize, Deserialize, Debug)]\nstruct F { f: f64 }\n#[derive(Serialize, Deserialize, Debug)]\nstruct G { f: Option[f64] }\nfn main() {\n    for t in [\"{\\\"f\\\":null}\", \"{\\\"f\\\":1}\"] {\n        let r: Result[F, DeserializeError] = Json.deserialize(t);\n        match r { Ok(v) => println(v.fmt_debug()), Err(e) => println(f\"err {e}\") }\n    }\n    let g: Result[G, DeserializeError] = Json.deserialize(\"{\\\"f\\\":null}\");\n    match g { Ok(v) => println(v.fmt_debug()), Err(e) => println(f\"err {e}\") }\n    println(Json.serialize(F { f: 0.0 / 0.0 }));\n}\n",
        "err f: expected a number, found null at line 1 column 6\nF { f: 1.0 }\nG { f: None }\n{\"f\":null}\n",
    );
}

// B-2026-11-20-27: a field `#[serde(skip)]` leaves off the wire needs
// neither trait in that direction; read back, it takes its `default:
// expr` or its type's `Default`, which it must then have.
#[test]
fn a_skipped_field_needs_no_serde_impl() {
    runs(
        "import std.json.{Json};\nimport std.secret.{Secret};\n#[derive(Default)]\nstruct Cache { hits: i64 }\nstruct Handle { fd: i64 }\n#[derive(Serialize, Deserialize)]\nstruct T {\n    a: i64,\n    #[serde(skip)]\n    c: Cache,\n    #[serde(skip_deserializing)]\n    s: i64,\n    #[serde(skip_serializing, default)]\n    w: String,\n    #[serde(skip, default: 3)]\n    k: i64,\n}\n#[derive(Serialize)]\nstruct OnlyOut { a: i64, #[serde(skip)] h: Handle, #[serde(skip)] tok: Secret[String] }\nfn main() {\n    let t = T { a: 1, c: Cache { hits: 2 }, s: 5, w: \"w\".to_string(), k: 9 };\n    println(Json.serialize(t));\n    let r: Result[T, DeserializeError] = Json.deserialize(\"{\\\"a\\\":4,\\\"c\\\":{\\\"x\\\":1},\\\"s\\\":6,\\\"w\\\":\\\"in\\\"}\");\n    match r { Ok(v) => println(f\"{v.a} {v.c.hits} {v.s} {v.w} {v.k}\"), Err(e) => println(f\"err {e}\") }\n    println(Json.serialize(OnlyOut { a: 1, h: Handle { fd: 3 }, tok: Secret.new(\"x\".to_string()) }));\n}\n",
        "{\"a\":1,\"s\":5}\n4 0 0 in 3\n{\"a\":1}\n",
    );
}

#[test]
fn a_field_skipped_when_read_needs_a_default() {
    let path = scratch(
        "struct Handle { fd: i64 }\n#[derive(Deserialize)]\nstruct T { a: i64, #[serde(skip)] h: Handle }\nfn main() { }\n",
    );
    let check = karac(&["check"], &path);
    let out = text(&check);
    assert!(!check.status.success(), "{out}");
    assert!(
        out.contains("skips field 'h', which is then built as its type's `Default`, but 'Handle' does not implement Default"),
        "{out}"
    );
    assert!(!out.contains("does not implement Deserialize"), "{out}");
}

// B-2026-11-20-23: a value the type refuses names the field and the place;
// a field missing from a nested struct names that struct.
#[test]
fn a_refused_value_names_its_field_and_place() {
    runs(
        "import std.json.{Json};\n#[derive(Deserialize, Debug)]\nstruct In { n: i64, c: char, small: u8, o: Option[i32] }\n#[derive(Deserialize, Debug)]\nstruct Out { inner: In }\nfn show(t: Str) {\n    let r: Result[Out, DeserializeError] = Json.deserialize(t);\n    match r { Ok(v) => println(v.fmt_debug()), Err(e) => println(f\"err {e}\") }\n}\nfn main() {\n    show(\"{\\\"inner\\\":{\\\"n\\\":1,\\\"c\\\":\\\"ab\\\",\\\"small\\\":1}}\");\n    show(\"{\\\"inner\\\":{\\\"n\\\":1,\\\"c\\\":\\\"a\\\",\\\"small\\\":300}}\");\n    show(\"{\\\"inner\\\":{\\\"n\\\":1,\\\"c\\\":\\\"a\\\",\\\"small\\\":1,\\\"o\\\":3000000000}}\");\n    show(\"{\\\"inner\\\":{\\\"n\\\":1,\\\"c\\\":\\\"a\\\"}}\");\n    show(\"{}\");\n    let x: Result[i8, DeserializeError] = Json.deserialize(\"200\");\n    match x { Ok(v) => println(f\"{v}\"), Err(e) => println(f\"err {e}\") }\n}\n",
        "err inner.c: expected one character, found \"ab\" at line 1 column 24\nerr inner.small: 300 is out of range for u8 at line 1 column 35\nerr inner.o: 3000000000 is out of range for i32 at line 1 column 48\nerr inner: missing field `small`\nerr missing field `inner`\nerr 200 is out of range for i8 at line 1 column 3\n",
    );
}
