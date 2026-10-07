lines: 397

build: ok
interp: same (stdout byte-identical to ./prog; both backends also print an "Error return trace" block on stderr, which is not part of stdout)
mirror: agrees (`python3 schema_transform.py` stdout == `./prog` stdout, byte for byte)

stmt-par: none (the program does no file I/O and no sleep_ms; all input is embedded)

shared types:
- `shared enum JsonValue` (L3). A JSON value is recursive (`Arr(Vec[JsonValue])`, `Obj(SortedMap[String, JsonValue])`), and the spec names `shared enum` as the idiomatic shape for tree ADTs, "JSON values" among them. Objects use `SortedMap`, so serialization with sorted keys falls out of iteration order.

workarounds:
1. Enum name. I first named the type `Json`, which shadows a prelude type (warning[prelude_shadow]). `karac build` then panicked: `src/codegen/stmts.rs:8188:39: Found StructValue(... { i64, i64, i64, i64 }) but expected PointerValue variant`. Renaming it to `JsonValue` fixes the build, and the interpreter worked either way. Renaming `Parser` to `JsonParser` made no difference to the build. I renamed both.
2. `transform` originally took `record: ref JsonValue` and did `let obj = match record { JsonValue.Obj(fields) => fields, _ => return Err(...) };` followed by `obj.get("id")`. Codegen failed with `no handler for method 'get' on variable 'obj' in transform (method dispatch fell through; this is a codegen bug ...)`. Annotating `obj: ref SortedMap[String, JsonValue]` did not help. Workaround: I split it into `migrate(record)` (L273), which matches the variant, and `transform(obj: ref SortedMap[...])`, which takes the map as a parameter.
3. `for (i, raw) in v1_records().iter().enumerate()` failed in codegen: `for-loop over the .enumerate() iterator adaptor is not yet lowered`, even though the pattern is already a 2-tuple. Workaround: bind `let records = v1_records();` first (L381).
4. `contact.insert("email", email.map(|e| JsonValue.Str(e.to_lowercase())) ?? JsonValue.Null)` (a closure returning a shared-enum value) failed LLVM module verification: `Function return type does not match operand type of return inst! ret ptr %rc_alloc { i64, i64, i64, i64 }`. Workaround: an inline `match email { Some(e) => ..., None => JsonValue.Null }` (L301-302).
5. `out.push(self.parse_unicode_escape()?)`, where the helper returns `Result[char, ParseError]`, failed LLVM verification. The char is loaded as i64 and passed to `karac_string_encode_char(i32)`. Minimal repro: `?` on any `Result[char, SomeStruct]`, then `s.push(c)`. Binding the char with `let` first also fails. Workaround: the helper takes `out: mut ref String`, pushes the char itself, and returns `Result[(), ParseError]` (L124).
6. Miscompile (wrong output, no build error). `for t in tags { tag_values.push(JsonValue.Str(t)); }` over a `SortedSet[String]` printed garbage tag strings under `karac build`. A standalone repro aborts with `free(): double free detected in tcache 2`. `for t in tags.into_iter()` behaves the same way. The interpreter was correct. Workaround: `for t in tags.iter() { tag_values.push(JsonValue.Str(t.clone())); }` (L333). The `.clone()` is there because the by-value walk miscompiles, not to satisfy a diagnostic.

ignored diagnostics: 0 (`karac check source.orig.kara` prints no `error[` lines; it prints only one effect note about mutual recursion and "All checks passed.". An earlier draft drew one warning[borrow_projection_copy] on the SortedSet loop, which went away with workaround 6.)
