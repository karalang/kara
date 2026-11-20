# JSON values (`Json`)

`Json` is a JSON value as a tree, for programs that read or build JSON whose shape is not a Kāra type: a request body checked field by field, a document passed through, a response built by hand. It is in `std.json`: import it with `import std.json.{Json};`, which `Json.serialize` and `Json.deserialize` need as well. A use without the import is a `karac check` error, and `karac fix` adds the import. To turn a Kāra type into JSON text and back, derive `Serialize` and `Deserialize` and use `Json.serialize` / `Json.deserialize` ([design.md § Serialize and Deserialize](../design.md#serialize-and-deserialize)).

```kara
import std.json.{Json};

let v = Json.parse("{\"name\": \"ada\", \"tags\": [1, 2.5]}")?;
match v {
    Json.Object(fields) => println(f"{fields.len()} fields"),   // 2 fields
    _ => println("not an object"),
}
let out = Json.Object(Vec[("ok", Json.Bool(true)), ("n", Json.Int(3))]);
println(out.stringify());                                        // {"ok":true,"n":3}
```

## Items

| Item | Signature | Notes |
|---|---|---|
| `Json` | `enum Json { Null, Bool(bool), Number(f64), String(String), Array(Vec[Json]), Object(Vec[(String, Json)]), Int(i64) }` | An object keeps its keys in order; `Json` implements `Clone` |
| `parse` | `fn parse(text: Str) -> Result[Json, JsonError]` | One value, with whitespace around it. A number with no fraction or exponent that fits an `i64` is `Int`; any other number is `Number` (`-0` is `Number(-0.0)`). A repeated key keeps its first place and takes the last value. Nesting deeper than 127 arrays and objects is an error |
| `stringify` | `fn stringify(own self) -> String` | Compact text, keys in order. A repeated key is written once, at its first place, with its last value. A `Number` is written in the shortest form that reads back to the same `f64` (`1.0`, `0.1`, `1e+21`, `1e-7`); NaN and the infinities are written `null`. `"`, `\` and the control characters are escaped; other text is written as it is |
| `JsonError` | `struct JsonError { line: u32, column: u32, message: String }` | Where `parse` stopped: the 1-based line and the byte column on it. `message` says what was wrong and where: `expected `:` at line 1 column 6`. It implements `Display` (the message) and `Error`, so `?` converts it into `AnyError` |

The text `stringify` writes and the errors `parse` reports are those of the Rust `serde_json` crate built with `preserve_order`, which is what the legacy pipeline runs, except that an exponent carries its sign (`1e+21`, where `serde_json` writes `1e21`) (review-thread ruling 2026-11-16).

**Repeated keys.** `parse` reads JSON into a dynamic value, and a repeated key keeps its first place and takes the last value. `Json.deserialize` into a type with a derived `Deserialize` refuses a repeated field instead, with `serde_json`'s text at the end of the repeated name: `{"a":1,"a":2}` is ``duplicate field `a` at line 1 column 10``. A typed reader that took either copy would let text pass a validator that read the other one (review-thread ruling 2026-11-16, B-2026-11-16-175). A repeated key the type has no field for is skipped, as any unknown key is.

**Floats.** `Json.serialize` writes a float as `stringify` writes a `Number`, and an `f32` by its own shortest digits (`0.1`). NaN and the infinities are written `null`, as serde_json writes them, and `null` does not read back into an `f64` or `f32`: `Json.deserialize` refuses it as the wrong shape (`f: expected a number, found null at line 1 column 6`). They do not round-trip, so a field that must carry them is an `Option[f64]`, or a type with its own impl (review-thread ruling 2026-11-20, B-2026-11-20-29).

**`Json.deserialize` errors.** Text that is not JSON is reported as `parse` reports it, with the same message and position (`EOF while parsing an object at line 1 column 7`). JSON of the wrong shape is reported with the path of the field being read: `a: expected an integer, found a string at line 1 column 6`, ``missing field `active` `` (review-thread ruling 2026-11-16, B-2026-11-16-174).
