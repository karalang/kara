//! `#[derive(Serialize, Deserialize)]` and `Json` on the MIR pipeline
//! (design.md § Serialize and Deserialize; redesign M2, decision D8).
//!
//! The traits, the JSON format and the derives are Kāra source in
//! `runtime/stdlib/source/serde.kara`; each program here runs on the MIR
//! interpreter. `tests/mir_build.rs` holds the compiled twin.

use karac::mir::interp::Outcome;

/// Build `src` through the MIR pipeline and run it: the output and exit
/// code, or the stage that refused it.
fn run(src: &str) -> Result<(String, i32), String> {
    let r = karac::mir::lower::run_source(src)?;
    match (&r.outcome, r.exit_code()) {
        (_, Some(code)) => Ok((r.output, code)),
        (Outcome::Error(e), None) => Err(format!("run: {e}\n--- output so far\n{}", r.output)),
        _ => Err("no exit code".into()),
    }
}

fn assert_runs(src: &str, want: &str) {
    match run(src) {
        Ok((out, 0)) if out == want => {}
        other => panic!("want exit 0 and\n{want}--- got {other:?}"),
    }
}

/// A struct round-trips through JSON, its field attributes applied: a
/// renamed key, a skipped field (its `Default` on the way back), a default
/// for an absent field, and `None` for an absent `Option`.
#[test]
fn a_struct_round_trips_with_its_field_attributes() {
    let src = r#"
#[derive(Serialize, Deserialize)]
struct Config {
    #[serde(rename: "server_name")]
    name: String,
    #[serde(skip)]
    internal_cache: Vec[i64],
    #[serde(default: 8080)]
    port: i64,
    #[serde(default)]
    retries: u32,
    tags: Vec[String],
    nick: Option[String],
}

fn main() {
    let c = Config {
        name: "a \"b\"\n".to_string(),
        internal_cache: vec![1, 2],
        port: 9,
        retries: 3,
        tags: vec!["x".to_string()],
        nick: None,
    };
    let j = Json.serialize(c);
    println(j);
    let back: Result[Config, DeserializeError] = Json.deserialize(j);
    match back {
        Ok(b) => println(f"{b.name.len()} {b.internal_cache.len()} {b.port} {b.retries} {b.tags[0]} {b.nick.is_none()}"),
        Err(e) => println(f"error: {e.message}"),
    }
    let sparse: Result[Config, DeserializeError] =
        Json.deserialize("{\"server_name\": \"s\", \"extra\": [1, {\"k\": null}], \"tags\": []}");
    match sparse {
        Ok(b) => println(f"{b.name} {b.port} {b.retries} {b.tags.len()} {b.nick.is_none()}"),
        Err(e) => println(f"error: {e.message}"),
    }
    let missing: Result[Config, DeserializeError] = Json.deserialize("{\"server_name\": \"s\"}");
    match missing {
        Ok(_) => println("unexpected"),
        Err(e) => println(f"error: {e.message}"),
    }
}
"#;
    assert_runs(
        src,
        "{\"server_name\":\"a \\\"b\\\"\\n\",\"port\":9,\"retries\":3,\"tags\":[\"x\"],\"nick\":null}\n\
         6 0 9 3 x true\n\
         s 8080 0 0 true\n\
         error: missing field `tags`\n",
    );
}

/// An enum is externally tagged by default: a unit variant is its name, a
/// one-field tuple its payload, a longer tuple a sequence, a struct variant
/// a struct. `rename` on a variant renames it.
#[test]
fn an_enum_is_externally_tagged() {
    let src = r#"
#[derive(Serialize, Deserialize)]
enum Shape {
    Dot,
    #[serde(rename: "circle")]
    Circle(f64),
    Rect(i64, i64),
    Named { label: String, sides: u32 },
}

fn show(s: ref Shape) -> String {
    match s {
        Shape.Dot => "Dot".to_string(),
        Shape.Circle(r) => f"Circle({r})",
        Shape.Rect(a, b) => f"Rect({a}, {b})",
        Shape.Named { label, sides } => f"Named({label}, {sides})",
    }
}

fn main() {
    let v = vec![Shape.Dot, Shape.Circle(2.5), Shape.Rect(3, 4), Shape.Named { label: "tri".to_string(), sides: 3 }];
    let j = Json.serialize(v);
    println(j);
    let back: Result[Vec[Shape], DeserializeError] = Json.deserialize(j);
    match back {
        Ok(v) => { for s in v.iter() { println(show(s)); } }
        Err(e) => println(f"error: {e.message}"),
    }
    let bad: Result[Shape, DeserializeError] = Json.deserialize("\"Hexagon\"");
    match bad { Ok(s) => println(show(s)), Err(e) => println(f"error: {e.message}") }
    let long: Result[Shape, DeserializeError] = Json.deserialize("{\"Rect\": [1, 2, 3]}");
    match long { Ok(s) => println(show(s)), Err(e) => println(f"error: {e.message}") }
}
"#;
    assert_runs(
        src,
        "[\"Dot\",{\"circle\":2.5},{\"Rect\":[3,4]},{\"Named\":{\"label\":\"tri\",\"sides\":3}}]\n\
         Dot\nCircle(2.5)\nRect(3, 4)\nNamed(tri, 3)\n\
         error: unknown variant `Hexagon` of Shape\n\
         error: too many values in Shape.Rect\n",
    );
}

/// `#[serde(tag: "type")]` tags internally, wherever the tag sits in the
/// object; `#[serde(untagged)]` writes the payload alone and reads the first
/// variant the value fits.
#[test]
fn internal_and_untagged_enums() {
    let src = r#"
#[derive(Serialize, Deserialize)]
#[serde(tag: "type")]
enum Event {
    Ping,
    #[serde(rename: "msg")]
    Message { from: String, body: String },
    Move { x: i64, y: i64 },
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Value {
    Nothing,
    Num(i64),
    Text(String),
    Pair(i64, String),
    Point { x: i64, y: i64 },
}

fn show_e(e: ref Event) -> String {
    match e {
        Event.Ping => "Ping".to_string(),
        Event.Message { from, body } => f"Message({from}, {body})",
        Event.Move { x, y } => f"Move({x}, {y})",
    }
}

fn show(v: ref Value) -> String {
    match v {
        Value.Nothing => "Nothing".to_string(),
        Value.Num(n) => f"Num({n})",
        Value.Text(t) => f"Text({t})",
        Value.Pair(a, b) => f"Pair({a}, {b})",
        Value.Point { x, y } => f"Point({x}, {y})",
    }
}

fn main() {
    let evs = vec![Event.Ping, Event.Message { from: "a".to_string(), body: "hi".to_string() }, Event.Move { x: 1, y: -2 }];
    let j = Json.serialize(evs);
    println(j);
    let back: Result[Vec[Event], DeserializeError] = Json.deserialize(j);
    match back {
        Ok(es) => { for e in es.iter() { println(show_e(e)); } }
        Err(e) => println(f"error: {e.message}"),
    }
    let late: Result[Event, DeserializeError] = Json.deserialize("{\"x\": 5, \"y\": 6, \"type\": \"Move\"}");
    match late { Ok(e) => println(show_e(e)), Err(e) => println(f"error: {e.message}") }
    let untagged: Result[Event, DeserializeError] = Json.deserialize("{\"x\": 5}");
    match untagged { Ok(e) => println(show_e(e)), Err(e) => println(f"error: {e.message}") }

    let vs = vec![Value.Nothing, Value.Num(3), Value.Text("t".to_string()), Value.Pair(1, "p".to_string()), Value.Point { x: 7, y: 8 }];
    let j2 = Json.serialize(vs);
    println(j2);
    let back2: Result[Vec[Value], DeserializeError] = Json.deserialize(j2);
    match back2 {
        Ok(vs) => { for v in vs.iter() { println(show(v)); } }
        Err(e) => println(f"error: {e.message}"),
    }
    let none: Result[Value, DeserializeError] = Json.deserialize("[true]");
    match none { Ok(v) => println(show(v)), Err(e) => println(f"error: {e.message}") }
}
"#;
    assert_runs(
        src,
        "[{\"type\":\"Ping\"},{\"type\":\"msg\",\"from\":\"a\",\"body\":\"hi\"},{\"type\":\"Move\",\"x\":1,\"y\":-2}]\n\
         Ping\nMessage(a, hi)\nMove(1, -2)\nMove(5, 6)\n\
         error: missing field `type`\n\
         [null,3,\"t\",[1,\"p\"],{\"x\":7,\"y\":8}]\n\
         Nothing\nNum(3)\nText(t)\nPair(1, p)\nPoint(7, 8)\n\
         error: the value fits no variant of untagged Value\n",
    );
}

/// The library's collections and scalars: map keys that are not strings
/// are quoted, sets and deques are sequences, a sorted map writes in key
/// order, an out-of-range integer is an error, and a non-finite float is
/// `null`.
#[test]
fn collections_and_scalars() {
    let src = r#"
#[derive(Serialize, Deserialize)]
struct Bag {
    q: VecDeque[i64],
    s: Set[String],
    ss: SortedSet[u8],
    sm: SortedMap[i64, String],
    m: Map[char, bool],
    nested: Vec[Option[Vec[i32]]],
    #[allow(float_in_serialized_type)]
    f: f32,
}

fn main() {
    let mut q: VecDeque[i64] = VecDeque.new();
    q.push_back(1);
    q.push_front(0);
    let mut s: Set[String] = Set.new();
    s.insert("only".to_string());
    let mut ss: SortedSet[u8] = SortedSet.new();
    ss.insert(9);
    ss.insert(2);
    let mut sm: SortedMap[i64, String] = SortedMap.new();
    sm.insert(2, "two".to_string());
    sm.insert(-1, "neg".to_string());
    let mut m: Map[char, bool] = Map.new();
    m.insert('k', true);
    let b = Bag { q: q, s: s, ss: ss, sm: sm, m: m, nested: vec![None, Some(vec![1, 2])], f: 0.5 };
    let j = Json.serialize(b);
    println(j);
    let back: Result[Bag, DeserializeError] = Json.deserialize(j);
    match back {
        Ok(b) => println(f"{b.q.len()} {b.s.len()} {b.ss.len()} {b.sm.len()} {b.m.len()} {b.nested.len()} {b.f}"),
        Err(e) => println(f"error: {e.message}"),
    }
    let big: Result[i8, DeserializeError] = Json.deserialize("300");
    println(f"{big.is_err()}");
    println(Json.serialize(0.0 / 0.0));
    let text: Result[String, DeserializeError] = Json.deserialize("\"a\\tb\\u0041\"");
    println(text.unwrap());
    let trailing: Result[i64, DeserializeError] = Json.deserialize("12 x");
    match trailing { Ok(_) => println("unexpected"), Err(e) => println(e.message) }
}
"#;
    assert_runs(
        src,
        "{\"q\":[0,1],\"s\":[\"only\"],\"ss\":[2,9],\"sm\":{\"-1\":\"neg\",\"2\":\"two\"},\"m\":{\"k\":true},\"nested\":[null,[1,2]],\"f\":0.5}\n\
         2 1 2 2 1 2 0.5\n\
         true\n\
         null\n\
         a\tbA\n\
         trailing characters at character 3\n",
    );
}

/// `let u: User = Json.deserialize(text)?` — the annotation selects the
/// type through the `?`, as design.md writes it.
#[test]
fn the_annotation_selects_the_type_through_question_mark() {
    let src = r#"
#[derive(Serialize, Deserialize)]
struct User { name: String, age: i64, active: bool }

fn load(json: String) -> Result[User, DeserializeError] {
    let back: User = Json.deserialize(json)?;
    Ok(back)
}

fn main() {
    let user = User { name: "ann".to_string(), age: 41, active: true };
    let json: String = Json.serialize(user);
    match load(json) {
        Ok(u) => println(f"{u.name} {u.age} {u.active}"),
        Err(e) => println(e.message),
    }
    match load("{\"name\": 3}".to_string()) {
        Ok(_) => println("unexpected"),
        Err(e) => println(e.message),
    }
}
"#;
    assert_runs(src, "ann 41 true\nexpected '\"' at character 9\n");
}

/// The derived code names no format: a format of the program's own, here a
/// one-line outline, serializes a derived type.
#[test]
fn a_program_defines_its_own_format() {
    let src = r#"
struct Outline { out: String }

impl Serializer for Outline {
    fn serialize_bool(mut ref self, v: bool) { self.out.push_str(f"{v}"); }
    fn serialize_i64(mut ref self, v: i64) { self.out.push_str(f"{v}"); }
    fn serialize_u64(mut ref self, v: u64) { self.out.push_str(f"{v}"); }
    fn serialize_f64(mut ref self, v: f64) { self.out.push_str(f"{v}"); }
    fn serialize_string(mut ref self, v: String) { self.out.push_str(v); }
    fn serialize_none(mut ref self) { self.out.push_str("-"); }
    fn serialize_seq_begin(mut ref self, len: i64) { self.out.push_str(f"<{len}>"); }
    fn serialize_element(mut ref self) { self.out.push_str(" "); }
    fn serialize_seq_end(mut ref self) { self.out.push_str(";"); }
    fn serialize_map_begin(mut ref self, len: i64) { self.out.push_str(f"map{len}"); }
    fn serialize_key(mut ref self) { self.out.push_str(" "); }
    fn serialize_value(mut ref self) { self.out.push_str("="); }
    fn serialize_map_end(mut ref self) { self.out.push_str(";"); }
    fn serialize_struct_begin(mut ref self, name: String, len: i64) { self.out.push_str(f"{name}/{len}"); }
    fn serialize_field(mut ref self, name: String) { self.out.push_str(f" {name}:"); }
    fn serialize_struct_end(mut ref self) { self.out.push_str(";"); }
    fn serialize_unit_variant(mut ref self, name: String, variant: String) { self.out.push_str(f"{name}.{variant}"); }
    fn serialize_variant_begin(mut ref self, name: String, variant: String) { self.out.push_str(f"{name}.{variant}("); }
    fn serialize_variant_end(mut ref self) { self.out.push_str(")"); }
}

#[derive(Serialize)]
enum Kind { Small, Big(i64) }

#[derive(Serialize)]
struct Item { id: u32, kind: Kind, parts: Vec[Kind] }

fn main() {
    let item = Item { id: 7, kind: Kind.Big(9), parts: vec![Kind.Small] };
    let mut o = Outline { out: "".to_string() };
    item.serialize(o);
    println(o.out);
}
"#;
    assert_runs(src, "Item/3 id:7 kind:Kind.Big(9) parts:<1> Kind.Small;;\n");
}

/// An internally tagged enum cannot hold a tuple variant: the derive says
/// so at compile time.
#[test]
fn an_internally_tagged_tuple_variant_is_a_compile_error() {
    let src = r#"
#[derive(Serialize)]
#[serde(tag: "type")]
enum Bad { A(i64) }
fn main() { println(Json.serialize(Bad.A(1))); }
"#;
    match run(src) {
        Err(e) => assert!(e.contains("`Bad.A` is a tuple variant"), "{e}"),
        other => panic!("want a compile error, got {other:?}"),
    }
}
