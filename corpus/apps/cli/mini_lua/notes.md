# mini_lua

An interpreter for a subset of Lua 5.4, used the way an application
embeds a scripting language. The run writes seven scripts and a web
server's access log to disk, then runs each script from its file:
- `basics.lua`: integer and float arithmetic (floor division and modulo
  on negatives, wrapping integers, `2^53`, `-0.0`, `inf`), string
  coercion, comparisons, `tostring`/`tonumber` with bases, `math` and
  `string.format`;
- `closures.lua`: counters with private upvalues, a fresh loop variable
  per iteration, two closures sharing one local, recursion,
  map/filter/reduce, multiple results and their truncation, `while`,
  `repeat`, numeric `for` with integer and float steps, `break`;
- `tables.lua`: arrays, records, `table.insert/remove/concat/sort` with
  and without a comparator, holes, `t[1.0]` as `t[1]`, matrices, sets,
  `next`;
- `oop.lua`: classes with metatables, `__index` chains for inheritance,
  `__index` functions, `__tostring`, `__add`, `__eq`, `__lt`, method
  chaining, `pcall` of a method;
- `strings.lua`: Lua patterns: `find` (plain and pattern), `match`,
  `gmatch`, `gsub` with string, table and function replacements, anchors,
  sets, lazy `-`, empty matches;
- `errors.lua`: `error` with levels 0, 1 and 2, non-string error values,
  `assert`, nested `pcall`, a retry helper;
- `logs.lua`: an access-log report. Each line is parsed with one pattern,
  then grouped by status, path and client, ranked with comparators, and
  summarised; slow requests are listed. The report is written with
  `io.open(.., "w")` and read back with `io.lines`.

**The interpreter.** A lexer, a recursive-descent parser with Lua's
precedence table into expression and statement arenas, and a tree-walk
evaluator. Tables have an array part and an insertion-ordered hash part.
Locals are shared cells, so closures see later writes; a closure records
how many names of its scope it can see. Errors carry Lua values and get
Lua's `chunk:line:` prefix from a call stack of frames. Floats print with
C's `%.14g`, computed from the double's exact decimal expansion, so ties
round to even as printf does (`%.1f` of 435.25 is `435.2`). The pattern
matcher is a port of `lstrlib.c`, including `%b`, `%f`, back-references
and position captures.

Not supported: varargs, goto, coroutines, integer bit operators, long
strings, `__newindex` and `__call`, and the `utf8` and `os` libraries.
Strings are Kāra Strings, so bytes that are not valid UTF-8 become `?`.

**Oracle.** `expected.out` is real Lua 5.4.6's output on the same scripts,
made by `make_expected.sh` (`lua5.4 scripts/<name>.lua` for each, from
this directory, so error positions name the same chunk). The run's
`scripts/` and `data/` files are byte-identical to the ones here.

## Edits from mini_lua.orig.kara to mini_lua.kara

Author errors (the diagnostics were right):
- `s.bytes()` is a `Slice[u8]`, not a `Vec[u8]`. Helpers that took
  `ref Vec[u8]` take `Slice[u8]`, and the matcher stores
  `s.bytes().to_vec()`.

Compiler gaps, worked around and cited in the source:
- B-2026-10-14-70: `karac check` refuses chained field receivers
  (`self.ast.exprs.push(e)`), so the `Ast` has its own `add_*` methods.
- B-2026-10-15-31 (new): `karac check` keeps a `ref t` borrow live past a
  `return`. The parser read the token once with `let t = ref
  self.toks[self.p]` and returned from each branch. It now reads the
  token's fields by index before the branches.
- B-2026-10-15-32 (new): `karac check` treats a pattern binding as moving
  an owned scrutinee on every path, so `if let Value.Text(s) = v { .. }
  return Err(v);` is refused. The three sites match a `ref` binding and
  clone the string.
- B-2026-10-06-86: the MIR front end refuses a `Value` payload inside
  another enum, though `karac check` now accepts it (a note on that row).
  The assignment target `Place` is a struct.
- B-2026-10-15-24: matching `f.state[i]` binds a Copy payload by value on
  MIR. Iterator state is read through `ref`.
- B-2026-10-15-33 (new): some method calls on a borrowed handle of a
  `shared struct` do not build on MIR ("a type that failed to check is
  not lowered yet"). Those calls go through a cloned handle (`let h =
  t.clone(); h.get(key)`), which copies the pointer.
- B-2026-10-15-34 (new): on MIR, an f-string can call a user function
  named `format`. The `string.format` implementation is `str_format`.

Written 2026-10-15 by the real-world apps thread; karac at 2cec444448.
