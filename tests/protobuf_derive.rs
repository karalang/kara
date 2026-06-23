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

/// Typecheck error messages (through desugar + resolve), as `karac check` sees
/// them — used to assert that calls to derive-generated methods don't trip the
/// "no method" / "no associated function" diagnostic before the comptime pass
/// has synthesized them.
fn typecheck_errors(source: &str) -> Vec<String> {
    let mut parsed = karac::parse(source);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::desugar_program(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    typed.errors.iter().map(|e| e.message.clone()).collect()
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

// ── repeated fields ─────────────────────────────────────────────

#[test]
fn derive_repeated_scalar_roundtrip() {
    // Packed numeric repeated field round-trips, including a negative element.
    let src = r#"
#[derive(Message)]
struct Bag { nums: Vec[i64] }

fn main() {
    let b = Bag { nums: [1, -2, 300, 0] };
    let back = Bag.decode(b.encode());
    println(back.nums.len());
    println(back.nums[0]);
    println(back.nums[1]);
    println(back.nums[2]);
    println(back.nums[3]);
}
"#;
    assert_eq!(run(src), vec!["4\n", "1\n", "-2\n", "300\n", "0\n"]);
}

#[test]
fn derive_repeated_bool_and_string_and_bytes_roundtrip() {
    let src = r#"
#[derive(Message)]
struct Bag { flags: Vec[bool], tags: Vec[String], blobs: Vec[Vec[u8]] }

fn main() {
    let b = Bag { flags: [true, false, true], tags: ["a", "bb"], blobs: [[1u8, 2u8], [9u8]] };
    let back = Bag.decode(b.encode());
    println(back.flags[0]);
    println(back.flags[2]);
    println(back.tags[0]);
    println(back.tags[1]);
    println(back.blobs.len());
    println(back.blobs[0].len());
    println(back.blobs[1][0]);
}
"#;
    assert_eq!(
        run(src),
        vec!["true\n", "true\n", "a\n", "bb\n", "2\n", "2\n", "9\n"]
    );
}

#[test]
fn derive_repeated_message_roundtrip() {
    let src = r#"
#[derive(Message)]
struct Item { v: i64 }

#[derive(Message)]
struct Bag { items: Vec[Item] }

fn main() {
    let b = Bag { items: [Item { v: 7 }, Item { v: -8 }] };
    let back = Bag.decode(b.encode());
    println(back.items.len());
    println(back.items[0].v);
    println(back.items[1].v);
}
"#;
    assert_eq!(run(src), vec!["2\n", "7\n", "-8\n"]);
}

#[test]
fn derive_repeated_empty_is_omitted() {
    // proto3 omits an empty repeated field entirely; decode restores an empty
    // vector.
    let src = r#"
#[derive(Message)]
struct Bag { nums: Vec[i64], tags: Vec[String] }

fn main() {
    let b = Bag { nums: [], tags: [] };
    println(b.encode().len());
    let back = Bag.decode(b.encode());
    println(back.nums.len());
    println(back.tags.len());
}
"#;
    assert_eq!(run(src), vec!["0\n", "0\n", "0\n"]);
}

#[test]
fn derive_repeated_merge_appends() {
    // Merging concatenates repeated elements onto the existing ones.
    let src = r#"
#[derive(Message)]
struct Bag { nums: Vec[i64], tags: Vec[String] }

fn main() {
    let mut acc = Bag { nums: [1, 2], tags: ["x"] };
    let more = Bag { nums: [3], tags: ["y", "z"] };
    acc.merge(more.encode());
    println(acc.nums.len());
    println(acc.nums[2]);
    println(acc.tags.len());
    println(acc.tags[2]);
}
"#;
    assert_eq!(run(src), vec!["3\n", "3\n", "3\n", "z\n"]);
}

#[test]
fn derive_repeated_scalar_is_packed_single_field() {
    // A packed repeated numeric occupies ONE length-delimited field (wire type
    // 2), while a repeated string repeats its tag per element.
    let src = r#"
#[derive(Message)]
struct M { nums: Vec[i64], tags: Vec[String] }

fn main() {
    let m = M { nums: [10, 20, 30], tags: ["p", "q"] };
    let mut r = ProtoReader.new(m.encode());
    let mut desc = "";
    while not r.at_end() {
        let (fld, wire) = r.read_tag();
        desc = desc + f"({fld},{wire})";
        let _ = r.skip_field(wire);
    }
    println(desc);
}
"#;
    assert_eq!(run(src), vec!["(1,2)(2,2)(2,2)\n"]);
}

#[test]
fn derive_repeated_numeric_decodes_unpacked_form() {
    // proto3 readers must accept the non-packed wire form for numeric repeated
    // fields too (each element its own wire-type-0 field).
    let src = r#"
#[derive(Message)]
struct M { nums: Vec[i64] }

fn main() {
    let mut bytes = Vec.new();
    bytes.extend_from_slice(ProtoBuf.encode_tag(1, 0));
    bytes.extend_from_slice(ProtoBuf.encode_varint_i64(5));
    bytes.extend_from_slice(ProtoBuf.encode_tag(1, 0));
    bytes.extend_from_slice(ProtoBuf.encode_varint_i64(-6));
    let back = M.decode(bytes);
    println(back.nums.len());
    println(back.nums[0]);
    println(back.nums[1]);
}
"#;
    assert_eq!(run(src), vec!["2\n", "5\n", "-6\n"]);
}

#[test]
fn derive_repeated_typecheck_clean() {
    let src = r#"
#[derive(Message)]
struct Item { v: i64 }

#[derive(Message)]
struct Bag { nums: Vec[i64], items: Vec[Item] }

fn main() {
    let b = Bag { nums: [1], items: [Item { v: 2 }] };
    let back = Bag.decode(b.encode());
    println(back.nums.len());
}
"#;
    let errs = typecheck_errors(src);
    assert!(
        !errs
            .iter()
            .any(|e| e.contains("no method") || e.contains("no associated function")),
        "repeated derive methods must typecheck clean; got: {errs:?}"
    );
}

#[test]
fn derive_repeated_non_message_element_errors() {
    // A repeated field of structs that don't derive `Message` is rejected with a
    // clear diagnostic.
    let src = r#"
struct Plain { v: i64 }

#[derive(Message)]
struct Bag { items: Vec[Plain] }

fn main() {}
"#;
    let diags = comptime_diags(src);
    assert!(
        diags.iter().any(|d| d.contains("E_COMPTIME_ERROR")
            && d.contains("does not derive(Message)")
            && d.contains("items")),
        "expected a repeated-non-message diagnostic; got: {diags:?}"
    );
}

// ── enums ───────────────────────────────────────────────────────

#[test]
fn derive_enum_field_roundtrip() {
    // An enum field is a proto3 enum: a varint of the variant's 0-based
    // declaration index.
    let src = r#"
enum Status { Pending, Active, Closed }

#[derive(Message)]
struct Task { id: i64, status: Status }

fn main() {
    let t = Task { id: 5, status: Status.Active };
    let back = Task.decode(t.encode());
    println(back.id);
    println(match back.status { Status.Pending => "p", Status.Active => "a", Status.Closed => "c" });
}
"#;
    assert_eq!(run(src), vec!["5\n", "a\n"]);
}

#[test]
fn derive_enum_zero_default() {
    // proto3 enum fields default to the zero (first) variant.
    let src = r#"
enum Status { Pending, Active, Closed }

#[derive(Message)]
struct Task { status: Status }

fn main() {
    let d = Task.decode(Vec.new());
    println(match d.status { Status.Pending => "p", _ => "?" });
}
"#;
    assert_eq!(run(src), vec!["p\n"]);
}

#[test]
fn derive_enum_wire_is_varint() {
    // The enum field occupies a wire-type-0 (varint) field carrying the index.
    let src = r#"
enum Status { Pending, Active, Closed }

#[derive(Message)]
struct Task { status: Status }

fn main() {
    let t = Task { status: Status.Closed };
    let mut r = ProtoReader.new(t.encode());
    let (fld, wire) = r.read_tag();
    println(fld);
    println(wire);
    println(r.read_varint());
}
"#;
    assert_eq!(run(src), vec!["1\n", "0\n", "2\n"]);
}

#[test]
fn derive_enum_typecheck_clean() {
    let src = r#"
enum Status { Pending, Active }

#[derive(Message)]
struct Task { status: Status }

fn main() {
    let t = Task { status: Status.Active };
    let back = Task.decode(t.encode());
    println(match back.status { Status.Active => "a", _ => "?" });
}
"#;
    let errs = typecheck_errors(src);
    assert!(
        !errs
            .iter()
            .any(|e| e.contains("no method") || e.contains("no associated function")),
        "enum derive methods must typecheck clean; got: {errs:?}"
    );
}

// ── enum in composite positions (repeated / map) ────────────────

#[test]
fn derive_repeated_enum_roundtrip() {
    // `repeated MyEnum` packs each variant index as a varint and round-trips,
    // including the zero variant.
    let src = r#"
enum Color { Red, Green, Blue }

#[derive(Message)]
struct Pal { colors: Vec[Color] }

fn main() {
    let p = Pal { colors: [Color.Red, Color.Blue, Color.Green, Color.Red] };
    let back = Pal.decode(p.encode());
    println(back.colors.len());
    println(match back.colors[1] { Color.Blue => "blue", _ => "?" });
    println(match back.colors[2] { Color.Green => "green", _ => "?" });
    println(match back.colors[3] { Color.Red => "red", _ => "?" });
}
"#;
    assert_eq!(run(src), vec!["4\n", "blue\n", "green\n", "red\n"]);
}

#[test]
fn derive_repeated_enum_is_packed_single_field() {
    // Repeated enums pack into ONE length-delimited (wire 2) field: 3 small
    // indices → 3 single-byte varints.
    let src = r#"
enum Color { Red, Green, Blue }

#[derive(Message)]
struct Pal { colors: Vec[Color] }

fn main() {
    let mut r = ProtoReader.new(Pal { colors: [Color.Red, Color.Green, Color.Blue] }.encode());
    let (f, w) = r.read_tag();
    let blob = r.read_len_delim();
    println(f);
    println(w);
    println(blob.len());
    println(r.at_end());
}
"#;
    assert_eq!(run(src), vec!["1\n", "2\n", "3\n", "true\n"]);
}

#[test]
fn derive_repeated_enum_unknown_index_falls_back_to_zero() {
    // proto3 maps an unknown enum number to the zero variant; an out-of-range
    // packed index decodes to variant 0.
    let src = r#"
enum Color { Red, Green, Blue }

#[derive(Message)]
struct Pal { colors: Vec[Color] }

fn main() {
    let mut blob = Vec.new();
    blob.extend_from_slice(ProtoBuf.encode_varint(1u64));
    blob.extend_from_slice(ProtoBuf.encode_varint(99u64));
    let mut buf = Vec.new();
    buf.extend_from_slice(ProtoBuf.encode_tag(1, 2));
    buf.extend_from_slice(ProtoBuf.encode_varint(blob.len() as u64));
    buf.extend_from_slice(blob);
    let back = Pal.decode(buf);
    println(back.colors.len());
    println(match back.colors[0] { Color.Green => "green", _ => "?" });
    println(match back.colors[1] { Color.Red => "red(fallback)", _ => "?" });
}
"#;
    assert_eq!(run(src), vec!["2\n", "green\n", "red(fallback)\n"]);
}

#[test]
fn derive_map_enum_value_roundtrip() {
    // A `Map[K, MyEnum]` value rides the varint-index path through the entry.
    let src = r#"
enum Color { Red, Green, Blue }

#[derive(Message)]
struct Doc { tints: Map[String, Color] }

fn main() {
    let mut tints: Map[String, Color] = Map.new();
    tints.insert("sky", Color.Blue);
    tints.insert("grass", Color.Green);
    let back = Doc.decode(Doc { tints: tints }.encode());
    println(back.tints.len());
    println(match back.tints.get("sky") { Option.Some(c) => match c { Color.Blue => "blue", _ => "?" }, Option.None => "none" });
    println(match back.tints.get("grass") { Option.Some(c) => match c { Color.Green => "green", _ => "?" }, Option.None => "none" });
}
"#;
    assert_eq!(run(src), vec!["2\n", "blue\n", "green\n"]);
}

// ── float / double ──────────────────────────────────────────────

#[test]
fn derive_float_double_roundtrip() {
    // `f64` (double, fixed64) and `f32` (float, fixed32) round-trip exactly,
    // including negatives; absent fields default to 0.0.
    let src = r#"
#[derive(Message)]
struct Meas { d: f64, f: f32, label: String }

fn main() {
    let m = Meas { d: 3.141592653589793, f: 2.5, label: "x" };
    let back = Meas.decode(m.encode());
    println(back.d == 3.141592653589793);
    println(back.f == 2.5);
    println(back.label);
    let n = Meas { d: -1.5, f: -0.25, label: "" };
    let bn = Meas.decode(n.encode());
    println(bn.d == -1.5);
    println(bn.f == -0.25);
    println(Meas.decode(Vec.new()).d == 0.0);
}
"#;
    assert_eq!(
        run(src),
        vec!["true\n", "true\n", "x\n", "true\n", "true\n", "true\n"]
    );
}

#[test]
fn derive_double_is_fixed64_float_is_fixed32() {
    // double occupies a wire-type-1 (fixed64) field, float a wire-type-5
    // (fixed32) field.
    let src = r#"
#[derive(Message)]
struct M { d: f64, f: f32 }

fn main() {
    let m = M { d: 1.25, f: 0.5 };
    let mut r = ProtoReader.new(m.encode());
    let (f1, w1) = r.read_tag();
    let _ = r.read_fixed64();
    let (f2, w2) = r.read_tag();
    let _ = r.read_fixed32();
    println(f1);
    println(w1);
    println(f2);
    println(w2);
    println(r.at_end());
}
"#;
    assert_eq!(run(src), vec!["1\n", "1\n", "2\n", "5\n", "true\n"]);
}

// ── float / double in composite positions (repeated / map / oneof) ──

#[test]
fn derive_repeated_float_roundtrip() {
    // Repeated `double` / `float` round-trip (packed), including negatives.
    let src = r#"
#[derive(Message)]
struct Bag { ds: Vec[f64], fs: Vec[f32] }

fn main() {
    let b = Bag { ds: [1.5, -2.25, 0.0], fs: [0.5, -0.25] };
    let back = Bag.decode(b.encode());
    println(back.ds.len());
    println(back.ds[0] == 1.5);
    println(back.ds[1] == -2.25);
    println(back.fs.len());
    println(back.fs[1] == -0.25);
}
"#;
    assert_eq!(run(src), vec!["3\n", "true\n", "true\n", "2\n", "true\n"]);
}

#[test]
fn derive_repeated_float_is_packed_single_field() {
    // Repeated doubles pack into ONE length-delimited (wire 2) field: 3 elements
    // × 8 bytes = 24 payload bytes.
    let src = r#"
#[derive(Message)]
struct Bag { ds: Vec[f64] }

fn main() {
    let mut r = ProtoReader.new(Bag { ds: [1.0, 2.0, 3.0] }.encode());
    let (f, w) = r.read_tag();
    let blob = r.read_len_delim();
    println(f);
    println(w);
    println(blob.len());
    println(r.at_end());
}
"#;
    assert_eq!(run(src), vec!["1\n", "2\n", "24\n", "true\n"]);
}

#[test]
fn derive_repeated_float_decodes_unpacked_form() {
    // proto3 readers must accept the non-packed wire form: one wire-1 field per
    // double. Hand-build two unpacked entries for field 1.
    let src = r#"
#[derive(Message)]
struct Bag { ds: Vec[f64] }

fn main() {
    let mut buf = Vec.new();
    buf.extend_from_slice(ProtoBuf.encode_tag(1, 1));
    buf.extend_from_slice(ProtoBuf.encode_fixed64((1.5f64).to_bits()));
    buf.extend_from_slice(ProtoBuf.encode_tag(1, 1));
    buf.extend_from_slice(ProtoBuf.encode_fixed64((2.5f64).to_bits()));
    let back = Bag.decode(buf);
    println(back.ds.len());
    println(back.ds[0] == 1.5);
    println(back.ds[1] == 2.5);
}
"#;
    assert_eq!(run(src), vec!["2\n", "true\n", "true\n"]);
}

#[test]
fn derive_map_float_value_roundtrip() {
    // A `Map[K, f64]` value rides the fixed64 path through the entry message.
    let src = r#"
#[derive(Message)]
struct Doc { scores: Map[String, f64] }

fn main() {
    let mut scores: Map[String, f64] = Map.new();
    scores.insert("a", 1.5);
    scores.insert("b", -2.25);
    let back = Doc.decode(Doc { scores: scores }.encode());
    println(back.scores.len());
    println(match back.scores.get("a") { Option.Some(x) => x == 1.5, Option.None => false });
    println(match back.scores.get("b") { Option.Some(x) => x == -2.25, Option.None => false });
}
"#;
    assert_eq!(run(src), vec!["2\n", "true\n", "true\n"]);
}

#[test]
fn derive_oneof_float_payload_roundtrip() {
    // A oneof case may carry an `f64` / `f32` payload (fixed64 / fixed32 wire).
    let src = r#"
enum Num { NotSet, D(f64), F(f32) }

#[derive(Message)]
struct Event { body: Num }

fn main() {
    let b1 = Event.decode(Event { body: Num.D(3.5) }.encode());
    println(match b1.body { Num.D(x) => x == 3.5, _ => false });
    let b2 = Event.decode(Event { body: Num.F(-1.25) }.encode());
    println(match b2.body { Num.F(x) => x == -1.25, _ => false });
}
"#;
    assert_eq!(run(src), vec!["true\n", "true\n"]);
}

// ── wire-type overrides (sint / fixed / sfixed) ─────────────────

#[test]
fn derive_wire_overrides_roundtrip() {
    // `#[karac::proto(...)]` selects ZigZag (`sint*`) or fixed-width
    // (`fixed*`/`sfixed*`) encodings that the Kāra int type can't express.
    let src = r#"
#[derive(Message)]
struct M {
    #[karac::proto(sint64)] a: i64,
    #[karac::proto(sint32)] b: i32,
    #[karac::proto(fixed64)] c: u64,
    #[karac::proto(sfixed32)] d: i32,
    plain: i64,
}

fn main() {
    let m = M { a: -5, b: -100000, c: 18000000000u64, d: -7, plain: -5 };
    let back = M.decode(m.encode());
    println(back.a);
    println(back.b);
    println(back.c);
    println(back.d);
    println(back.plain);
}
"#;
    assert_eq!(
        run(src),
        vec!["-5\n", "-100000\n", "18000000000\n", "-7\n", "-5\n"]
    );
}

#[test]
fn derive_sint_shrinks_small_negatives() {
    // ZigZag encodes a small negative in 1 payload byte, vs 10 for plain int64.
    let src = r#"
#[derive(Message)]
struct Plain { v: i64 }

#[derive(Message)]
struct Zig { #[karac::proto(sint64)] v: i64 }

fn main() {
    println(Plain { v: -5 }.encode().len());
    println(Zig { v: -5 }.encode().len());
}
"#;
    // Plain int64 -5 → tag(1) + 10-byte varint = 11; sint64 -5 → tag(1) + 1 = 2.
    assert_eq!(run(src), vec!["11\n", "2\n"]);
}

#[test]
fn derive_unknown_wire_override_errors() {
    let src = r#"
#[derive(Message)]
struct M { #[karac::proto(varint7)] x: i64 }

fn main() {}
"#;
    let diags = comptime_diags(src);
    assert!(
        diags.iter().any(|d| d.contains("E_COMPTIME_ERROR")
            && d.contains("unknown wire override")
            && d.contains("varint7")),
        "expected an unknown-wire-override diagnostic; got: {diags:?}"
    );
}

// ── per-field number override (#[karac::field(N)]) ──────────────

#[test]
fn derive_field_number_override_roundtrip() {
    // `#[karac::field(N)]` sets a field's wire number; an unmarked field keeps
    // its positional number. Numbers may be sparse / out of order.
    let src = r#"
#[derive(Message)]
struct Sparse {
    #[karac::field(5)] name: String,
    #[karac::field(17)] id: i64,
    active: bool,
}

fn main() {
    let s = Sparse { name: "Ada", id: 99, active: true };
    let mut r = ProtoReader.new(s.encode());
    let mut desc = "";
    while not r.at_end() {
        let (f, w) = r.read_tag();
        desc = desc + f"({f})";
        let _ = r.skip_field(w);
    }
    println(desc);
    let back = Sparse.decode(s.encode());
    println(back.name);
    println(back.id);
    println(back.active);
}
"#;
    // name → 5, id → 17, active → positional 3.
    assert_eq!(run(src), vec!["(5)(17)(3)\n", "Ada\n", "99\n", "true\n"]);
}

#[test]
fn derive_field_number_override_decodes_by_number() {
    // Decode keys off the overridden number, not declaration position: a buffer
    // carrying field 17 populates the `#[karac::field(17)]` field.
    let src = r#"
#[derive(Message)]
struct Sparse {
    #[karac::field(5)] name: String,
    #[karac::field(17)] id: i64,
}

fn main() {
    let mut buf = Vec.new();
    buf.extend_from_slice(ProtoBuf.encode_tag(17, 0));
    buf.extend_from_slice(ProtoBuf.encode_varint(42u64));
    let back = Sparse.decode(buf);
    println(back.id);
    println(back.name.len());
}
"#;
    assert_eq!(run(src), vec!["42\n", "0\n"]);
}

#[test]
fn derive_field_number_override_composes_with_wire_override() {
    // A field may carry both a number override and a wire override.
    let src = r#"
#[derive(Message)]
struct M { #[karac::field(9)] #[karac::proto(sint64)] v: i64 }

fn main() {
    let back = M.decode(M { v: 0 - 5 }.encode());
    println(back.v);
    let mut r = ProtoReader.new(M { v: 0 - 5 }.encode());
    let (f, w) = r.read_tag();
    println(f);
    println(w);
}
"#;
    // field 9, wire 0 (varint); sint64 zigzag round-trips -5.
    assert_eq!(run(src), vec!["-5\n", "9\n", "0\n"]);
}

// ── maps ────────────────────────────────────────────────────────

#[test]
fn derive_map_scalar_roundtrip() {
    // A `Map[K, V]` field is a proto3 map: repeated key/value entry messages.
    let src = r#"
#[derive(Message)]
struct Doc { counts: Map[String, i64] }

fn main() {
    let mut counts: Map[String, i64] = Map.new();
    counts.insert("a", 1);
    counts.insert("b", -2);
    let d = Doc { counts: counts };
    let back = Doc.decode(d.encode());
    println(back.counts.len());
    println(match back.counts.get("a") { Option.Some(x) => x, Option.None => -100 });
    println(match back.counts.get("b") { Option.Some(x) => x, Option.None => -100 });
}
"#;
    assert_eq!(run(src), vec!["2\n", "1\n", "-2\n"]);
}

#[test]
fn derive_map_message_value_roundtrip() {
    let src = r#"
#[derive(Message)]
struct Inner { v: i64 }

#[derive(Message)]
struct Doc { parts: Map[String, Inner] }

fn main() {
    let mut parts: Map[String, Inner] = Map.new();
    parts.insert("x", Inner { v: 99 });
    let d = Doc { parts: parts };
    let back = Doc.decode(d.encode());
    println(back.parts.len());
    println(match back.parts.get("x") { Option.Some(p) => p.v, Option.None => -1 });
}
"#;
    assert_eq!(run(src), vec!["1\n", "99\n"]);
}

#[test]
fn derive_map_merge_last_write_wins() {
    // Merge inserts each entry; a repeated key takes the merged-in value.
    let src = r#"
#[derive(Message)]
struct D { m: Map[String, i64] }

fn main() {
    let mut base: Map[String, i64] = Map.new();
    base.insert("a", 1);
    let mut acc = D { m: base };
    let mut other: Map[String, i64] = Map.new();
    other.insert("a", 9);
    other.insert("b", 2);
    acc.merge(D { m: other }.encode());
    println(acc.m.len());
    println(match acc.m.get("a") { Option.Some(x) => x, Option.None => -1 });
    println(match acc.m.get("b") { Option.Some(x) => x, Option.None => -1 });
}
"#;
    assert_eq!(run(src), vec!["2\n", "9\n", "2\n"]);
}

#[test]
fn derive_map_empty_is_omitted() {
    let src = r#"
#[derive(Message)]
struct D { m: Map[String, i64] }

fn main() {
    let d = D { m: Map.new() };
    println(d.encode().len());
    let back = D.decode(d.encode());
    println(back.m.len());
}
"#;
    assert_eq!(run(src), vec!["0\n", "0\n"]);
}

#[test]
fn derive_map_non_message_value_errors() {
    let src = r#"
struct Plain { v: i64 }

#[derive(Message)]
struct D { m: Map[String, Plain] }

fn main() {}
"#;
    let diags = comptime_diags(src);
    assert!(
        diags.iter().any(|d| d.contains("E_COMPTIME_ERROR")
            && d.contains("does not derive(Message)")
            && d.contains("m")),
        "expected a map-value-non-message diagnostic; got: {diags:?}"
    );
}

// ── oneof ───────────────────────────────────────────────────────

#[test]
fn derive_oneof_roundtrip() {
    // An enum field with payload variants is a oneof: each case has its own
    // field number (assigned from the field's position) and round-trips.
    let src = r#"
enum Payload { NotSet, Num(i64), Text(String), Flag(bool) }

#[derive(Message)]
struct Event { id: i64, body: Payload }

fn main() {
    let e1 = Event { id: 1, body: Payload.Num(42) };
    let b1 = Event.decode(e1.encode());
    println(b1.id);
    println(match b1.body { Payload.Num(x) => x, _ => -1 });

    let e2 = Event { id: 2, body: Payload.Text("hi") };
    let b2 = Event.decode(e2.encode());
    println(match b2.body { Payload.Text(s) => s, _ => "?" });

    let e3 = Event { id: 3, body: Payload.Flag(true) };
    let b3 = Event.decode(e3.encode());
    println(match b3.body { Payload.Flag(f) => f, _ => false });
}
"#;
    assert_eq!(run(src), vec!["1\n", "42\n", "hi\n", "true\n"]);
}

#[test]
fn derive_oneof_unset_default() {
    let src = r#"
enum Payload { NotSet, Num(i64) }

#[derive(Message)]
struct Event { body: Payload }

fn main() {
    let d = Event.decode(Vec.new());
    println(match d.body { Payload.NotSet => "unset", _ => "?" });
    // Unset encodes nothing.
    println(Event { body: Payload.NotSet }.encode().len());
}
"#;
    assert_eq!(run(src), vec!["unset\n", "0\n"]);
}

#[test]
fn derive_oneof_cases_have_distinct_field_numbers() {
    // The oneof cases occupy field numbers continuing the message's numbering:
    // `id` is 1, so `Num`/`Text` are 2/3.
    let src = r#"
enum P { NotSet, Num(i64), Text(String) }

#[derive(Message)]
struct E { id: i64, body: P }

fn main() {
    let mut r = ProtoReader.new(E { id: 9, body: P.Text("x") }.encode());
    let mut d = "";
    while not r.at_end() {
        let (f, w) = r.read_tag();
        d = d + f"({f},{w})";
        let _ = r.skip_field(w);
    }
    println(d);
}
"#;
    // id → field 1 wire 0; Text → field 3 wire 2.
    assert_eq!(run(src), vec!["(1,0)(3,2)\n"]);
}

#[test]
fn derive_oneof_message_payload_not_deriving_message_errors() {
    // A struct oneof payload that does not itself derive `Message` is rejected
    // with the message-payload diagnostic.
    let src = r#"
struct Inner { v: i64 }

#[derive(Message)]
struct E { body: Choice }

enum Choice { NotSet, Msg(Inner) }

fn main() {}
"#;
    let diags = comptime_diags(src);
    assert!(
        diags.iter().any(|d| d.contains("E_COMPTIME_ERROR")
            && d.contains("does not derive(Message)")
            && d.contains("Msg")),
        "expected a message-payload-non-derive diagnostic; got: {diags:?}"
    );
}

#[test]
fn derive_oneof_unsupported_payload_errors() {
    // A payload that is neither scalar/float, message, nor enum (here a `Map`) is
    // rejected as unsupported.
    let src = r#"
#[derive(Message)]
struct E { body: Choice }

enum Choice { NotSet, M(Map[String, i64]) }

fn main() {}
"#;
    let diags = comptime_diags(src);
    assert!(
        diags
            .iter()
            .any(|d| d.contains("E_COMPTIME_ERROR") && d.contains("unsupported payload type")),
        "expected an unsupported-oneof-payload diagnostic; got: {diags:?}"
    );
}

#[test]
fn derive_oneof_message_payload_roundtrip() {
    // A oneof case may carry a nested message (length-delimited sub-message).
    let src = r#"
#[derive(Message)]
struct Addr { city: String, zip: i64 }

enum Body { NotSet, Loc(Addr), Note(String) }

#[derive(Message)]
struct Evt { id: i64, body: Body }

fn main() {
    let e = Evt { id: 7, body: Body.Loc(Addr { city: "London", zip: 9 }) };
    let back = Evt.decode(e.encode());
    println(back.id);
    println(match back.body { Body.Loc(a) => f"{a.city}/{a.zip}", _ => "?" });
    let e2 = Evt { id: 8, body: Body.Note("hi") };
    println(match Evt.decode(e2.encode()).body { Body.Note(s) => s, _ => "?" });
}
"#;
    assert_eq!(run(src), vec!["7\n", "London/9\n", "hi\n"]);
}

#[test]
fn derive_oneof_enum_payload_roundtrip() {
    // A oneof case may carry an enum payload (varint index). The inner enum must
    // be `shared` to satisfy the one-level nested-enum-payload rule.
    let src = r#"
shared enum Color { Red, Green, Blue }

enum Body { NotSet, Tint(Color), Note(String) }

#[derive(Message)]
struct Evt { body: Body }

fn main() {
    let back = Evt.decode(Evt { body: Body.Tint(Color.Blue) }.encode());
    println(match back.body { Body.Tint(c) => match c { Color.Blue => "blue", Color.Green => "green", Color.Red => "red" }, _ => "?" });
    let n = Evt.decode(Evt { body: Body.Note("x") }.encode());
    println(match n.body { Body.Note(s) => s, _ => "?" });
}
"#;
    assert_eq!(run(src), vec!["blue\n", "x\n"]);
}

// ── nested messages ─────────────────────────────────────────────

#[test]
fn derive_nested_message_roundtrip() {
    // A struct-typed field whose type also derives `Message` is encoded as a
    // length-delimited sub-message and round-trips, including a negative scalar
    // inside the nested value.
    let src = r#"
#[derive(Message)]
struct Inner { label: String, score: i64 }

#[derive(Message)]
struct Outer { id: i64, child: Inner, tag: String }

fn main() {
    let o = Outer { id: 7, child: Inner { label: "hi", score: -3 }, tag: "z" };
    let back = Outer.decode(o.encode());
    println(back.id);
    println(back.child.label);
    println(back.child.score);
    println(back.tag);
}
"#;
    assert_eq!(run(src), vec!["7\n", "hi\n", "-3\n", "z\n"]);
}

#[test]
fn derive_nested_message_multi_level_roundtrip() {
    // Nesting composes: a message field can itself contain a message field.
    let src = r#"
#[derive(Message)]
struct A { v: i64 }

#[derive(Message)]
struct B { a: A, name: String }

#[derive(Message)]
struct C { b: B, k: i64 }

fn main() {
    let c = C { b: B { a: A { v: 42 }, name: "deep" }, k: 9 };
    let back = C.decode(c.encode());
    println(back.k);
    println(back.b.name);
    println(back.b.a.v);
}
"#;
    assert_eq!(run(src), vec!["9\n", "deep\n", "42\n"]);
}

#[test]
fn derive_nested_decode_empty_yields_default_nested() {
    // Decoding empty bytes leaves a nested field at its proto3 zero value — the
    // decode of empty bytes, i.e. every sub-field defaulted.
    let src = r#"
#[derive(Message)]
struct Inner { label: String, score: i64 }

#[derive(Message)]
struct Outer { id: i64, child: Inner }

fn main() {
    let o = Outer.decode(Vec.new());
    println(o.id);
    println(o.child.label == "");
    println(o.child.score);
}
"#;
    assert_eq!(run(src), vec!["0\n", "true\n", "0\n"]);
}

#[test]
fn derive_nested_wire_is_length_delimited_submessage() {
    // The nested field is written with wire type 2 (length-delimited), and the
    // payload is exactly the nested message's own encoding — a `ProtoReader`
    // peels the outer frame, then decodes the inner blob.
    let src = r#"
#[derive(Message)]
struct Inner { score: i64 }

#[derive(Message)]
struct Outer { child: Inner }

fn main() {
    let o = Outer { child: Inner { score: 5 } };
    let mut reader = ProtoReader.new(o.encode());
    let (field, wire) = reader.read_tag();
    println(field);
    println(wire);
    let blob = reader.read_len_delim();
    println(reader.at_end());
    let inner = Inner.decode(blob);
    println(inner.score);
}
"#;
    assert_eq!(run(src), vec!["1\n", "2\n", "true\n", "5\n"]);
}

#[test]
fn derive_nested_typecheck_clean() {
    let src = r#"
#[derive(Message)]
struct Inner { v: i64 }

#[derive(Message)]
struct Outer { inner: Inner }

fn main() {
    let o = Outer { inner: Inner { v: 1 } };
    let back = Outer.decode(o.encode());
    println(back.inner.v);
}
"#;
    let errs = typecheck_errors(src);
    assert!(
        !errs
            .iter()
            .any(|e| e.contains("no method") || e.contains("no associated function")),
        "nested derive methods must typecheck clean; got: {errs:?}"
    );
}

// ── diagnostics ─────────────────────────────────────────────────

#[test]
fn derive_unsupported_field_type_errors() {
    // A field whose type isn't a supported proto3 scalar (here `u8`, which has
    // no proto3 wire type) and isn't a nested message raises a `compiler.error`
    // from `derive_message`.
    let src = r#"
#[derive(Message)]
struct Outer { id: i64, flag: u8 }

fn main() {}
"#;
    let diags = comptime_diags(src);
    assert!(
        diags.iter().any(|d| d.contains("E_COMPTIME_ERROR")
            && d.contains("unsupported type")
            && d.contains("flag")),
        "expected an unsupported-field diagnostic; got: {diags:?}"
    );
}

#[test]
fn derive_nested_non_message_struct_errors() {
    // A struct-typed field is treated as a nested message, but only a struct
    // that itself derives `Message` has the codec to delegate to. Nesting a
    // plain struct must raise a clear `compiler.error` (not the generic
    // unsupported-type one).
    let src = r#"
struct Plain { v: i64 }

#[derive(Message)]
struct Outer { id: i64, inner: Plain }

fn main() {}
"#;
    let diags = comptime_diags(src);
    assert!(
        diags.iter().any(|d| d.contains("E_COMPTIME_ERROR")
            && d.contains("does not derive(Message)")
            && d.contains("inner")),
        "expected a nested-non-message diagnostic; got: {diags:?}"
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

// ── typecheck visibility (karac check / build) ──────────────────

#[test]
fn derive_methods_typecheck_clean() {
    // The derive's `encode`/`decode`/`merge` are synthesized after typecheck, so
    // calling them must not trip "no method" / "no associated function" — that
    // would make `karac check`/`build` reject a correct program. A comptime-
    // backed `#[derive]` marks the type's method set open.
    let src = r#"
#[derive(Message)]
struct Person { name: String, age: i64 }

fn main() {
    let p = Person { name: "Ada", age: 36 };
    let bytes = p.encode();
    let mut q = Person.decode(bytes);
    q.merge(p.encode());
}
"#;
    let errs = typecheck_errors(src);
    assert!(
        !errs
            .iter()
            .any(|e| e.contains("no method") || e.contains("no associated function")),
        "derive methods must typecheck clean; got: {errs:?}"
    );
}

#[test]
fn non_derived_type_still_reports_missing_method() {
    // The suppression is scoped to comptime-derived types — a plain struct still
    // reports a genuinely missing method.
    let src = r#"
struct Plain { x: i64 }
fn main() {
    let p = Plain { x: 1 };
    let _ = p.encode();
}
"#;
    let errs = typecheck_errors(src);
    assert!(
        errs.iter()
            .any(|e| e.contains("no method") && e.contains("encode")),
        "a missing method on a non-derived type must still be reported; got: {errs:?}"
    );
}
