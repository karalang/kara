//! `#[derive(Message)]` comptime codegen (protobuf slice 2).
//!
//! The stdlib-provided `derive_message` comptime fn reads a struct's fields via
//! reflection and emits `encode` / `decode` / `merge` methods over the slice-1
//! wire codec. Field numbers are 1-based declaration order; supported field
//! types are the proto3 scalars i32 / i64 / u32 / u64 / bool / String / Vec[u8].
//! These tests drive whole programs end-to-end through the interpreter.

fn run(src: &str) -> Vec<String> {
    karac::run_program(src)
}

/// Parse → desugar → resolve → typecheck → lower → comptime; return comptime
/// diagnostics (for asserting on `derive_message`'s `compiler.error` output).
fn comptime_diags(source: &str) -> Vec<String> {
    let mut parsed = karac::parse(source);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::desugar_program(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "resolve errors: {:?}",
        resolved.errors
    );
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    karac::comptime_eval(&mut parsed.program, &typed)
        .iter()
        .map(|e| e.message.clone())
        .collect()
}

// ── round trips ─────────────────────────────────────────────────

#[test]
fn derive_roundtrip_scalars() {
    let src = r#"
#[derive(Message)]
struct Person {
    name: String,
    age: i64,
    active: bool,
    score: u32,
}

fn main() {
    let p = Person { name: "Ada", age: 36, active: true, score: 99u32 };
    let q = Person.decode(p.encode());
    println(q.name);
    println(q.age);
    println(q.active);
    println(q.score);
}
"#;
    assert_eq!(run(src), vec!["Ada\n", "36\n", "true\n", "99\n"]);
}

#[test]
fn derive_roundtrip_wide_ints_and_bytes() {
    // i32 / u64 / negative i64 / Vec[u8] all survive a round trip. A negative
    // int64 encodes as a full-width varint (two's complement) and decodes back.
    let src = r#"
#[derive(Message)]
struct Wide {
    a: i32,
    b: u64,
    c: i64,
    d: Vec[u8],
}

fn main() {
    let mut bytes = Vec.new();
    bytes.push(7u8);
    bytes.push(8u8);
    bytes.push(255u8);
    let w = Wide { a: 0 - 5, b: 18000000000u64, c: 0 - 123456789, d: bytes };
    let r = Wide.decode(w.encode());
    println(r.a);
    println(r.b);
    println(r.c);
    println(r.d.len());
    println(r.d[2]);
}
"#;
    assert_eq!(
        run(src),
        vec!["-5\n", "18000000000\n", "-123456789\n", "3\n", "255\n"]
    );
}

#[test]
fn derive_roundtrip_empty_string_and_bytes() {
    let src = r#"
#[derive(Message)]
struct Blob { tag: String, data: Vec[u8] }

fn main() {
    let b = Blob { tag: "", data: Vec.new() };
    let r = Blob.decode(b.encode());
    println(r.tag.len());
    println(r.data.len());
}
"#;
    assert_eq!(run(src), vec!["0\n", "0\n"]);
}

// ── proto3 defaults ─────────────────────────────────────────────

#[test]
fn derive_decode_empty_yields_defaults() {
    // Decoding an empty buffer yields every field at its proto3 zero value.
    let src = r#"
#[derive(Message)]
struct Person { name: String, age: i64, active: bool, score: u32 }

fn main() {
    let p = Person.decode(Vec.new());
    println(p.name.len());
    println(p.age);
    println(p.active);
    println(p.score);
}
"#;
    assert_eq!(run(src), vec!["0\n", "0\n", "false\n", "0\n"]);
}

// ── decode is order-independent + forward-compatible ────────────

#[test]
fn derive_decode_skips_unknown_fields() {
    // A buffer carrying an unknown field number (here 9) interleaved with known
    // fields decodes correctly — the unknown field is skipped by wire type.
    let src = r#"
#[derive(Message)]
struct Person { name: String, age: i64 }

fn main() {
    let mut buf = Vec.new();
    buf.extend_from_slice(ProtoBuf.encode_tag(9, 2));
    buf.extend_from_slice(ProtoBuf.encode_string("ignore-me"));
    buf.extend_from_slice(ProtoBuf.encode_tag(2, 0));
    buf.extend_from_slice(ProtoBuf.encode_varint(42u64));
    buf.extend_from_slice(ProtoBuf.encode_tag(1, 2));
    buf.extend_from_slice(ProtoBuf.encode_string("Grace"));
    let p = Person.decode(buf);
    println(p.name);
    println(p.age);
}
"#;
    assert_eq!(run(src), vec!["Grace\n", "42\n"]);
}

// ── merge ───────────────────────────────────────────────────────

#[test]
fn derive_merge_overwrites_only_present_fields() {
    // proto3 merge: fields present on the wire overwrite the receiver; absent
    // fields keep their prior value. The delta buffer carries only field 2.
    let src = r#"
#[derive(Message)]
struct Person { name: String, age: i64, active: bool }

fn main() {
    let mut p = Person { name: "Ada", age: 36, active: true };
    let mut delta = Vec.new();
    delta.extend_from_slice(ProtoBuf.encode_tag(2, 0));
    delta.extend_from_slice(ProtoBuf.encode_varint(99u64));
    p.merge(delta);
    println(p.name);
    println(p.age);
    println(p.active);
}
"#;
    assert_eq!(run(src), vec!["Ada\n", "99\n", "true\n"]);
}

#[test]
fn derive_merge_from_full_encode() {
    // Merging a fully-encoded message overwrites every field.
    let src = r#"
#[derive(Message)]
struct Person { name: String, age: i64, active: bool }

fn main() {
    let mut p = Person { name: "old", age: 1, active: false };
    let fresh = Person { name: "new", age: 2, active: true };
    p.merge(fresh.encode());
    println(p.name);
    println(p.age);
    println(p.active);
}
"#;
    assert_eq!(run(src), vec!["new\n", "2\n", "true\n"]);
}

// ── encode is pure (non-consuming) ──────────────────────────────

#[test]
fn derive_encode_does_not_consume_self() {
    // `encode(ref self)` borrows — the message is still usable afterward, and
    // re-encoding yields an identical buffer.
    let src = r#"
#[derive(Message)]
struct Person { name: String, age: i64 }

fn main() {
    let p = Person { name: "Ada", age: 36 };
    let b1 = p.encode();
    let b2 = p.encode();
    println(b1.len() == b2.len());
    println(p.name);
}
"#;
    assert_eq!(run(src), vec!["true\n", "Ada\n"]);
}

// ── wire compatibility with the slice-1 codec ───────────────────

#[test]
fn derive_encode_is_readable_by_proto_reader() {
    // The bytes a derived `encode` emits are plain proto3 — a hand-driven
    // `ProtoReader` reads them field by field.
    let src = r#"
#[derive(Message)]
struct Pair { x: i64, y: i64 }

fn main() {
    let p = Pair { x: 10, y: 20 };
    let mut reader = ProtoReader.new(p.encode());
    let (f1, w1) = reader.read_tag();
    let v1 = reader.read_varint();
    let (f2, w2) = reader.read_tag();
    let v2 = reader.read_varint();
    println(f1);
    println(w1);
    println(v1);
    println(f2);
    println(v2);
    println(reader.at_end());
}
"#;
    assert_eq!(
        run(src),
        vec!["1\n", "0\n", "10\n", "2\n", "20\n", "true\n"]
    );
}

// ── diagnostics ─────────────────────────────────────────────────

#[test]
fn derive_unsupported_field_type_errors() {
    // A field whose type isn't a supported proto3 scalar (here a nested struct)
    // raises a `compiler.error` from `derive_message`.
    let src = r#"
struct Inner { v: i64 }

#[derive(Message)]
struct Outer { id: i64, inner: Inner }

fn main() {}
"#;
    let diags = comptime_diags(src);
    assert!(
        diags.iter().any(|d| d.contains("E_COMPTIME_ERROR")
            && d.contains("unsupported type")
            && d.contains("inner")),
        "expected an unsupported-field diagnostic; got: {diags:?}"
    );
}

#[test]
fn derive_supported_struct_has_no_diagnostics() {
    let src = r#"
#[derive(Message)]
struct Person { name: String, age: i64, active: bool, score: u32 }

fn main() {}
"#;
    assert!(
        comptime_diags(src).is_empty(),
        "unexpected diagnostics: {:?}",
        comptime_diags(src)
    );
}
