//! `.proto` schema → message types (protobuf slice 3).
//!
//! A `#[proto_schema]` module-level `const` whose value is proto3 schema text is
//! expanded, before name resolution, into the `struct` types it declares — each
//! carrying `#[derive(Message)]` so slice 2 supplies encode/decode/merge. The
//! `.proto` parser is pure-Kāra comptime code (`proto_parse_schema` in
//! `runtime/stdlib/protobuf.kara`); this exercises the whole path end to end.

fn run(src: &str) -> Vec<String> {
    karac::run_program(src)
}

/// Parse + run the pre-resolve `#[proto_schema]` expansion, returning its
/// diagnostics (the `.proto` parser surfaces malformed schemas via
/// `compiler.error`, which `expand_proto_schemas` returns).
fn schema_diags(src: &str) -> Vec<String> {
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::desugar_program(&mut parsed.program)
        .iter()
        .map(|e| e.message.clone())
        .collect()
}

// ── basic generation + round trip ───────────────────────────────

#[test]
fn proto_roundtrip_scalars() {
    let src = r#"
#[proto_schema]
const SCHEMA: String = "
    syntax = \"proto3\";
    message Person {
        string name = 1;
        int64 age = 2;
        bool active = 3;
    }
";

fn main() {
    let p = Person { name: "Ada", age: 36, active: true };
    let q = Person.decode(p.encode());
    println(q.name);
    println(q.age);
    println(q.active);
}
"#;
    assert_eq!(run(src), vec!["Ada\n", "36\n", "true\n"]);
}

#[test]
fn proto_all_supported_scalar_types() {
    let src = r#"
#[proto_schema]
const SCHEMA: String = "
    message Wide {
        int32 a = 1;
        int64 b = 2;
        uint32 c = 3;
        uint64 d = 4;
        bool e = 5;
        string f = 6;
        bytes g = 7;
    }
";

fn main() {
    let mut raw = Vec.new();
    raw.push(9u8);
    let w = Wide { a: 0 - 7, b: 5000000000, c: 42u32, d: 18000000000u64, e: true, f: "hi", g: raw };
    let r = Wide.decode(w.encode());
    println(r.a);
    println(r.b);
    println(r.c);
    println(r.d);
    println(r.e);
    println(r.f);
    println(r.g.len());
}
"#;
    assert_eq!(
        run(src),
        vec![
            "-7\n",
            "5000000000\n",
            "42\n",
            "18000000000\n",
            "true\n",
            "hi\n",
            "1\n"
        ]
    );
}

// ── multiple messages in one schema ─────────────────────────────

#[test]
fn proto_multiple_messages() {
    let src = r#"
#[proto_schema]
const SCHEMA: String = "
    message Point { int64 x = 1; int64 y = 2; }
    message Tag { string label = 1; }
";

fn main() {
    let p = Point { x: 3, y: 4 };
    let rp = Point.decode(p.encode());
    println(rp.x);
    println(rp.y);
    let t = Tag { label: "hi" };
    let rt = Tag.decode(t.encode());
    println(rt.label);
}
"#;
    assert_eq!(run(src), vec!["3\n", "4\n", "hi\n"]);
}

// ── field numbers order fields (not declaration order) ──────────

#[test]
fn proto_field_numbers_determine_order() {
    // Fields declared out of number order: the generated struct orders them by
    // field number, so the wire tags match the schema's numbers on round trip.
    let src = r#"
#[proto_schema]
const SCHEMA: String = "
    message M {
        int64 second = 2;
        string first = 1;
    }
";

fn main() {
    let m = M { first: "a", second: 9 };
    let mut reader = ProtoReader.new(m.encode());
    let (f1, w1) = reader.read_tag();
    println(f1);
    println(w1);
    let v = reader.read_string();
    println(v);
    let r = M.decode(m.encode());
    println(r.first);
    println(r.second);
}
"#;
    // Field 1 (string `first`, wire 2) is emitted first; then field 2.
    assert_eq!(run(src), vec!["1\n", "2\n", "a\n", "a\n", "9\n"]);
}

// ── statements that are skipped ─────────────────────────────────

#[test]
fn proto_skips_non_message_statements() {
    let src = r#"
#[proto_schema]
const SCHEMA: String = "
    syntax = \"proto3\";
    package demo.v1;
    option go_package = \"demo\";
    import \"other.proto\";
    message Only { int64 n = 1; }
";

fn main() {
    let o = Only { n: 5 };
    println(Only.decode(o.encode()).n);
}
"#;
    assert_eq!(run(src), vec!["5\n"]);
}

// ── diagnostics ─────────────────────────────────────────────────

#[test]
fn proto_unsupported_type_errors() {
    // `float` is not in the v1 scalar set; the pure-Kāra parser reports it.
    let src = r#"
#[proto_schema]
const SCHEMA: String = "message M { float x = 1; }";
fn main() {}
"#;
    let diags = schema_diags(src);
    assert!(
        diags.iter().any(|d| d.contains("E_COMPTIME_ERROR")
            && d.contains("unsupported field type")
            && d.contains("float")),
        "expected an unsupported-type diagnostic; got: {diags:?}"
    );
}

#[test]
fn proto_noncontiguous_field_numbers_error() {
    // Field numbers 1 and 3 (gap at 2) are rejected — declaration-order tagging
    // requires contiguous 1..N in v1.
    let src = r#"
#[proto_schema]
const SCHEMA: String = "message M { int64 a = 1; int64 b = 3; }";
fn main() {}
"#;
    let diags = schema_diags(src);
    assert!(
        diags
            .iter()
            .any(|d| d.contains("E_COMPTIME_ERROR") && d.contains("contiguous")),
        "expected a contiguous-field-number diagnostic; got: {diags:?}"
    );
}

#[test]
fn no_proto_schema_no_diagnostics() {
    // A program without any `#[proto_schema]` const expands to nothing.
    let src = r#"
struct Plain { x: i64 }
fn main() { println(42); }
"#;
    assert!(schema_diags(src).is_empty());
    assert_eq!(run(src), vec!["42\n"]);
}
