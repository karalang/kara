# JSON values (`Json`)

`Json` is a JSON value as a tree, for programs that read or build JSON whose shape is not a Kāra type: a request body checked field by field, a document passed through, a response built by hand. It is in the prelude. To turn a Kāra type into JSON text and back, derive `Serialize` and `Deserialize` and use `Json.serialize` / `Json.deserialize` ([design.md § Serialize and Deserialize](../design.md#serialize-and-deserialize)).

```kara
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
| `Json` | `enum Json { Null, Bool(bool), Number(f64), String(String), Array(Vec[Json]), Object(Vec[(String, Json)]), Int(i64) }` | An object keeps its keys in order |
| `parse` | `fn parse(text: Str) -> Result[Json, JsonError]` | One value, with whitespace around it. A number with no fraction or exponent that fits an `i64` is `Int`; any other number is `Number` (`-0` is `Number(-0.0)`). A repeated key keeps its first place and takes the last value. Nesting deeper than 127 arrays and objects is an error |
| `stringify` | `fn stringify(own self) -> String` | Compact text, keys in order. A repeated key is written once, at its first place, with its last value. A `Number` is written in the shortest form that reads back to the same `f64` (`1.0`, `0.1`, `1e+21`, `1e-7`); NaN and the infinities are written `null`. `"`, `\` and the control characters are escaped; other text is written as it is |
| `JsonError` | `struct JsonError { line: u32, column: u32, message: String }` | Where `parse` stopped: the 1-based line and the byte column on it. `message` says what was wrong and where: `expected `:` at line 1 column 6` |

The text `stringify` writes and the errors `parse` reports are those of the Rust `serde_json` crate built with `preserve_order`, which is what the legacy pipeline runs.
