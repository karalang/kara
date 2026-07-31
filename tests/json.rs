// tests/json.rs
//
// Slice F (`std.json`) — v1 surface tests. Covers the locked design
// invariants: round-trip on objects / arrays / scalars, parse-error
// `line`/`column` surface, insertion-order Object iteration, manual
// `ToJson` impl on a user struct.
//
// All six tests run through the tree-walk interpreter (no `--features
// llvm` gate); the codegen-side wiring lands in a sibling slice as part
// of Slice B's `Response.json[T: ToJson]` builder. The runtime-crate
// FFI exports (`karac_runtime_json_*`) are exercised separately by the
// `karac_runtime::tests::test_karac_runtime_json_*` unit tests at the
// bottom of `runtime/src/lib.rs`.

use karac::run_program;

fn run(source: &str) -> String {
    run_program(source).join("")
}

#[test]
fn test_json_parse_roundtrip_object() {
    // B-2026-07-30-15 revised locked design (i): an integer-syntax token
    // parses as `Json.Int` and stringifies WITHOUT the historical `.0`
    // (which Go's encoding/json refused into an int field); float syntax
    // still round-trips as f64. Locked design (ii): Object keys preserved
    // in input order.
    let output = run(
        "fn main() {\n\
             match Json.parse(\"{\\\"a\\\": 1, \\\"b\\\": \\\"hello\\\", \\\"c\\\": [true, null]}\") {\n\
                 Ok(j) => println(j.stringify()),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
         }",
    );
    assert_eq!(
        output, "{\"a\":1,\"b\":\"hello\",\"c\":[true,null]}\n",
        "object round-trip should preserve keys and integer syntax exactly"
    );
}

#[test]
fn test_json_parse_roundtrip_array() {
    let output = run("fn main() {\n\
             match Json.parse(\"[1, 2.5, \\\"x\\\", true, null]\") {\n\
                 Ok(j) => println(j.stringify()),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "[1,2.5,\"x\",true,null]\n");
}

#[test]
fn test_json_parse_roundtrip_primitives() {
    // One scalar at a time — number, string, bool, null. Each goes
    // through parse + stringify and must come back byte-equivalent —
    // including the bare integer, since B-2026-07-30-15 (`Json.Int`).
    let output = run("fn main() {\n\
             match Json.parse(\"42\") {\n\
                 Ok(j) => println(j.stringify()),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
             match Json.parse(\"\\\"hi\\\"\") {\n\
                 Ok(j) => println(j.stringify()),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
             match Json.parse(\"true\") {\n\
                 Ok(j) => println(j.stringify()),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
             match Json.parse(\"null\") {\n\
                 Ok(j) => println(j.stringify()),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "42\n\"hi\"\ntrue\nnull\n");
}

#[test]
fn test_json_parse_error_surfaces_line_col() {
    // Malformed input `{"a": }` — serde_json reports the error at line
    // 1, column 7 (the offending `}` byte). Locked design (iv): the
    // JsonError carries line + column from `serde_json::Error::line()` /
    // `column()`.
    let output = run("fn main() {\n\
             match Json.parse(\"{\\\"a\\\": }\") {\n\
                 Ok(j) => println(\"ok\"),\n\
                 Err(e) => {\n\
                     println(e.line);\n\
                     println(e.column);\n\
                 },\n\
             }\n\
         }");
    assert_eq!(
        output, "1\n7\n",
        "JsonError should carry serde_json's 1-indexed line + column"
    );
}

#[test]
fn test_json_object_preserves_insertion_order() {
    // Locked design (ii): Object iterates in input insertion order, NOT
    // alphabetical. Backed by `Vec[(String, Json)]` on the Kāra side
    // and `serde_json` with `preserve_order` on the Rust side. If this
    // test fails with `{"a":2,"m":3,"z":1}` (alphabetical), the
    // `preserve_order` feature was dropped from the runtime crate's
    // `serde_json` dependency.
    let output = run("fn main() {\n\
             match Json.parse(\"{\\\"z\\\": 1, \\\"a\\\": 2, \\\"m\\\": 3}\") {\n\
                 Ok(j) => println(j.stringify()),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "{\"z\":1,\"a\":2,\"m\":3}\n");
}

#[test]
fn test_to_json_manual_impl() {
    // Manual `ToJson` impl on a user struct. Locked design (v):
    // derived `#[derive(ToJson)]` ships in v1.5; v1 is hand-written.
    // The impl builds an `Object` variant via a pair-Vec literal, then
    // `stringify` produces the expected JSON.
    let output = run("struct Point { x: i32, y: i32 }\n\
         \n\
         impl ToJson for Point {\n\
             fn to_json(self) -> Json {\n\
                 let pairs = [\n\
                     (\"x\", Json.Number(self.x as f64)),\n\
                     (\"y\", Json.Number(self.y as f64)),\n\
                 ];\n\
                 Json.Object(pairs)\n\
             }\n\
         }\n\
         \n\
         fn main() {\n\
             let p = Point { x: 1, y: 2 };\n\
             println(p.to_json().stringify());\n\
         }");
    assert_eq!(output, "{\"x\":1.0,\"y\":2.0}\n");
}

#[test]
fn test_json_int_variant_exact_roundtrip() {
    // B-2026-07-30-15 — `Json.Int(i64)`. Three pins:
    //  * a constructed Int stringifies with no `.0` (Go's encoding/json
    //    refuses `1.0` into an int field, so `{"id":1.0}` was not
    //    interchangeable);
    //  * 2^53 + 1 — unrepresentable in f64 — survives a parse + stringify
    //    round-trip exactly (was silently truncated to ...992.0);
    //  * float SYNTAX (`1.0`) still round-trips as `Json.Number`, keeping its
    //    fractional form — the variant is chosen by the input token's syntax,
    //    the serde_json / Go json.Number model.
    let output = run("fn main() {\n\
             let o: Json = Json.Object(Vec[(\"id\", Json.Int(1))]);\n\
             println(o.stringify());\n\
             match Json.parse(\"{\\\"n\\\":9007199254740993}\") {\n\
                 Ok(j) => println(j.stringify()),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
             match Json.parse(\"[7, 2.5, 1.0]\") {\n\
                 Ok(j) => println(j.stringify()),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
             println(Json.Int(-42).stringify());\n\
         }");
    assert_eq!(
        output,
        "{\"id\":1}\n{\"n\":9007199254740993}\n[7,2.5,1.0]\n-42\n"
    );
}

#[test]
fn test_json_int_match_destructure() {
    // The seventh variant participates in `match` like the original six, and
    // an integer-syntax parse lands in the `Int` arm while float syntax lands
    // in `Number`.
    let output = run("fn describe(j: Json) -> String {\n\
             match j {\n\
                 Json.Null => \"null\",\n\
                 Json.Bool(b) => \"bool\",\n\
                 Json.Number(f) => \"num\",\n\
                 Json.Int(i) => \"int:\" + i.to_string(),\n\
                 Json.String(s) => \"str\",\n\
                 Json.Array(xs) => \"arr\",\n\
                 Json.Object(kv) => \"obj\",\n\
             }\n\
         }\n\
         fn main() {\n\
             match Json.parse(\"41\") {\n\
                 Ok(j) => println(describe(j)),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
             match Json.parse(\"4.5\") {\n\
                 Ok(j) => println(describe(j)),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
             println(describe(Json.Int(99)));\n\
         }");
    assert_eq!(output, "int:41\nnum\nint:99\n");
}
