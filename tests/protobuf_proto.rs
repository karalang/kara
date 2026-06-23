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
fn proto_nested_message_field_roundtrip() {
    // A field whose type is another message declared in the same schema is a
    // nested message — the parser maps it to that struct and the derive encodes
    // it as a length-delimited sub-message. End-to-end round trip.
    let src = r#"
#[proto_schema]
const SCHEMA: String = "
    syntax = \"proto3\";
    message Address { string city = 1; string zip = 2; }
    message Person { string name = 1; Address addr = 2; int64 age = 3; }
";

fn main() {
    let p = Person { name: "Ada", addr: Address { city: "London", zip: "NW1" }, age: 36 };
    let q = Person.decode(p.encode());
    println(q.name);
    println(q.addr.city);
    println(q.addr.zip);
    println(q.age);
}
"#;
    assert_eq!(run(src), vec!["Ada\n", "London\n", "NW1\n", "36\n"]);
}

#[test]
fn proto_nested_message_forward_reference() {
    // A message field may reference a message declared later in the schema — the
    // parser collects all message names before resolving field types.
    let src = r#"
#[proto_schema]
const SCHEMA: String = "
    message Outer { Inner child = 1; int64 id = 2; }
    message Inner { int64 v = 1; }
";

fn main() {
    let o = Outer { child: Inner { v: 99 }, id: 4 };
    let back = Outer.decode(o.encode());
    println(back.child.v);
    println(back.id);
}
"#;
    assert_eq!(run(src), vec!["99\n", "4\n"]);
}

#[test]
fn proto_repeated_fields_roundtrip() {
    // `repeated TYPE name = N;` maps to a `Vec[KaraType]` field across scalar,
    // string, and nested-message element types — end-to-end round trip.
    let src = r#"
#[proto_schema]
const SCHEMA: String = "
    syntax = \"proto3\";
    message Point { int64 x = 1; int64 y = 2; }
    message Path {
        string name = 1;
        repeated int64 marks = 2;
        repeated string labels = 3;
        repeated Point points = 4;
    }
";

fn main() {
    let p = Path {
        name: "route",
        marks: [3, -4, 5],
        labels: ["a", "b"],
        points: [Point { x: 1, y: 2 }, Point { x: 3, y: 4 }],
    };
    let back = Path.decode(p.encode());
    println(back.name);
    println(back.marks.len());
    println(back.marks[1]);
    println(back.labels[0]);
    println(back.points.len());
    println(back.points[1].y);
}
"#;
    assert_eq!(
        run(src),
        vec!["route\n", "3\n", "-4\n", "a\n", "2\n", "4\n"]
    );
}

#[test]
fn proto_repeated_bytes_roundtrip() {
    // `repeated bytes` is `Vec[Vec[u8]]` — the element maps through `bytes`.
    let src = r#"
#[proto_schema]
const SCHEMA: String = "message Blobs { repeated bytes chunks = 1; }";

fn main() {
    let b = Blobs { chunks: [[1u8, 2u8], [9u8]] };
    let back = Blobs.decode(b.encode());
    println(back.chunks.len());
    println(back.chunks[0].len());
    println(back.chunks[1][0]);
}
"#;
    assert_eq!(run(src), vec!["2\n", "2\n", "9\n"]);
}

#[test]
fn proto_float_double_roundtrip() {
    // `.proto` `double` → `f64` (fixed64), `float` → `f32` (fixed32).
    let src = r#"
#[proto_schema]
const SCHEMA: String = "message Sample { double precise = 1; float approx = 2; }";

fn main() {
    let s = Sample { precise: 1.25, approx: 0.5 };
    let back = Sample.decode(s.encode());
    println(back.precise == 1.25);
    println(back.approx == 0.5);
}
"#;
    assert_eq!(run(src), vec!["true\n", "true\n"]);
}

#[test]
fn proto_map_field_roundtrip() {
    // `map<K, V> name = N;` maps to a `Map[K, V]` field — scalar and nested-
    // message values round-trip end to end.
    let src = r#"
#[proto_schema]
const SCHEMA: String = "
    syntax = \"proto3\";
    message Point { int64 x = 1; int64 y = 2; }
    message Doc {
        string title = 1;
        map<string, int64> counts = 2;
        map<int32, Point> points = 3;
    }
";

fn main() {
    let mut counts: Map[String, i64] = Map.new();
    counts.insert("a", 10);
    counts.insert("b", 20);
    let mut points: Map[i32, Point] = Map.new();
    points.insert(1, Point { x: 5, y: 6 });
    let d = Doc { title: "t", counts: counts, points: points };
    let back = Doc.decode(d.encode());
    println(back.title);
    println(back.counts.len());
    println(match back.counts.get("a") { Option.Some(x) => x, Option.None => -1 });
    println(match back.points.get(1) { Option.Some(p) => p.x, Option.None => -1 });
}
"#;
    assert_eq!(run(src), vec!["t\n", "2\n", "10\n", "5\n"]);
}

#[test]
fn proto_enum_roundtrip() {
    // A `.proto` enum becomes a Kāra `enum` (UPPER_SNAKE variants converted to
    // PascalCase), and an enum-typed field round-trips as a varint.
    let src = r#"
#[proto_schema]
const SCHEMA: String = "
    syntax = \"proto3\";
    enum Priority {
        PRIORITY_LOW = 0;
        PRIORITY_HIGH = 1;
        PRIORITY_CRITICAL = 2;
    }
    message Ticket { string title = 1; Priority prio = 2; }
";

fn main() {
    let t = Ticket { title: "bug", prio: Priority.PriorityHigh };
    let back = Ticket.decode(t.encode());
    println(back.title);
    println(match back.prio {
        Priority.PriorityLow => "low",
        Priority.PriorityHigh => "high",
        Priority.PriorityCritical => "critical",
    });
}
"#;
    assert_eq!(run(src), vec!["bug\n", "high\n"]);
}

#[test]
fn proto_comments_are_stripped() {
    // `//` line and `/* */` block comments are ignored — including ones that
    // contain proto keywords like `message` or `enum`.
    let src = r#"
#[proto_schema]
const SCHEMA: String = "
    // this message describes a user; enum-like words here are ignored
    message User {
        string name = 1; /* the display name */
        int64 id = 2;    // a unique id
    }
";

fn main() {
    let u = User { name: "z", id: 9 };
    let back = User.decode(u.encode());
    println(back.name);
    println(back.id);
}
"#;
    assert_eq!(run(src), vec!["z\n", "9\n"]);
}

#[test]
fn proto_enum_noncontiguous_values_error() {
    // proto3 enums must start at 0 and the v1 mapping requires contiguous
    // values (declaration order is the wire numbering).
    let src = r#"
#[proto_schema]
const SCHEMA: String = "enum E { A = 0; B = 2; }";
fn main() {}
"#;
    let diags = schema_diags(src);
    assert!(
        diags
            .iter()
            .any(|d| d.contains("E_COMPTIME_ERROR") && d.contains("contiguous")),
        "expected a contiguous-enum-value diagnostic; got: {diags:?}"
    );
}

#[test]
fn proto_unsupported_type_errors() {
    // `sint32` is not in the v1 scalar set (zigzag types are unsupported); the
    // pure-Kāra parser reports it.
    let src = r#"
#[proto_schema]
const SCHEMA: String = "message M { sint32 x = 1; }";
fn main() {}
"#;
    let diags = schema_diags(src);
    assert!(
        diags.iter().any(|d| d.contains("E_COMPTIME_ERROR")
            && d.contains("unsupported field type")
            && d.contains("sint32")),
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
