lines: 379

build: ok
interp: same (byte-identical to ./prog; the interpreter also printed correct output for the original generic-dedupe version that miscompiled under build)
mirror: agrees

stmt-par: L293-L294 two fs.write calls (separate files); L300-L301 two fs.read_to_string calls (separate files)

shared types: none

workarounds:
- L335-L342 (main): natural `let (crm, crm_merged) = dedupe(contacts);` fails `karac build` with
  `error: codegen failed: ... codegen: no handler for method 'len' on variable 'crm' in main (method dispatch fell through; this is a codegen bug ...)`.
  Any tuple-destructured SortedMap binding hits it (minimal: `let (m, n) = mk(); m.len()` where mk returns `(SortedMap[String, i64], i64)`); a type annotation on the let does not help. Worked around by binding the tuple and reading `.0` / `.1`. Interpreter accepted the original.
- L207-L248 (dedupe): originally one generic `fn dedupe[T: Record](rows: Vec[T]) -> (SortedMap[String, T], i64)` over a `Record` trait (`key()`/`stamp()`). It built cleanly but MISCOMPILED under `karac build`: inside a generic fn, a local `SortedMap[String, T]` (or `Map[String, T]`) after `insert` returns a wrong value from `get` — `old.stamp()` read 16 or a pointer-sized garbage number instead of the stored date — so newest-wins kept the wrong record (#5 instead of #4 for carol, #10 instead of #9 for grace). `--interp` was correct. Minimal repro: `fn f[T: Record](row: T) { let mut m: SortedMap[String, T] = SortedMap.new(); m.insert("a", row); match m.get("a") { Some(o) => println(f"{o.stamp()}"), None => {} } }` prints 16 for a stamp of 5; the same with a concrete `C`, or with the map passed in by `ref` to the generic fn, prints 5. Worked around by removing the trait and writing two concrete functions, dedupe_contacts and dedupe_signups.
- L17 (minor, warning not a failure): first named the enum `Channel`; got `warning[prelude_shadow]` (shadows the prelude Channel type), renamed to `Source`.
- L141 (spec gap, not compiler): `match c.as_str()` -> `no method 'as_str' on type 'String'`; matching a String directly against string-literal patterns works.

ignored diagnostics: 1 (`error[borrow_projection_copy]` at L200, moving the `for` element `line` in data_lines; plus several `warning[ownership]` lines not counted)
