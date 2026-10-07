lines: 400

build: ok
interp: same
mirror: agrees

stmt-par: none

shared types: none

workarounds:
- E_INDEX_MOVE_NON_COPY is a hard error that stops the build. Passing a split field straight into an owned-`String` helper, e.g. `parse_int(line, parts[1])?` and `parse_region(line_no, f[3])?`, failed with `error[typecheck]: ... error[E_INDEX_MOVE_NON_COPY]: cannot move out of an index expression: v[i] evaluates to ref T, and this element type is not Copy` (9 sites). Workaround: the parse helpers (`parse_category`, `parse_region`, `parse_currency`, `parse_int`, `parse_discount`) take `s: ref String` and build their error payloads with `s.clone()`. `BadRecord(line_no, f[0])` became `f[0].clone()`. The same index reads used as struct-literal fields (`Order { id: f[1], customer: f[2], .. }`, `LineItem { sku: f[1], .. }`) were NOT rejected, so the rule fires on call arguments only, which is inconsistent.
- A multi-line string literal (`"""..."""`) fails in codegen: `error: codegen failed: ...: codegen: no handler for expression kind MultiStringLit; this is a codegen bug`. `karac run --interp` handles it fine. I wanted the input order file as one `"""` literal (it also kept the program under 400 lines). Workaround: `input_text()` builds the file with one `s.push_str("...\n")` per record.
- (minor, spec mismatch rather than a workaround) The spec types `SortedMap.get` as `Option[ref V]`, so I wrote `Some(v) => *v`. The compiler rejected it (`unary '*' requires 'ref T' ... found 'i64'`), so the code uses `Some(v) => v`.

ignored diagnostics: 13 (`karac check` exits 1 with 7 `error[borrow_projection_copy]` and 6 `error[ownership]`; `karac build` still succeeds)
