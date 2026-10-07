lines: 398

build: ok
interp: same (byte-identical to the `./prog` output)
mirror: agrees (`python3 csv_report.py` output is byte-identical to `./prog` and `karac run --interp`)

stmt-par: none. design.md does not mention stmt-par. The only waiting statements are L371-L372: an fs.write followed by an fs.read_to_string of the SAME file, so they depend on each other and are not independent.

shared types: none

workarounds:
- L210 `Some(p) => p,` in `aggregate`: the natural spec form `Some(p) => *p` (design.md says `Map.get` returns `Option[ref V]`) fails to build with `error[typecheck]: unary '*' requires 'ref T', 'mut ref T', or a raw pointer ..., found 'i64'`. The compiler types `Map[String, i64].get` as `Option[i64]`, so I dropped the deref.
- L271-L272 `pad_left(orders.to_string(), 6)` / `pad_left(units.to_string(), 8)` in `row`: the natural form `pad_left(f"{orders}", 6)` builds, but the binary aborts with `free(): double free detected in tcache 2` (exit 134) and loses all buffered stdout. The interpreter is correct. Minimal repro: `fn show(s: ref String) -> String { let mut o = String.new(); o.push(' '); o.push_str(s); o }` with `fn main() { println(show(f"{5}")); }`. That aborts under `karac build`. Binding the f-string to a `let` first, or passing `"5".to_string()`, works. This looks like a codegen bug: an f-string temporary passed straight to a `ref String` parameter gets a double free.
- L297 `current = t.region.clone();` in `print_report`: the natural spec form `current = t.region;` (design.md: moving out of a borrowed `for` element is an implicit copy, with only a warning) builds, but the binary aborts with `double free or corruption (fasttop)` (exit 134) once `print_report` returns. The interpreter is correct. The compiled code apparently does not perform the implicit copy, so `current` and the Vec element free the same buffer. This is a crash, not just a diagnostic, so I added `.clone()`. Both crash workarounds were needed: the binary still aborted with only one of them applied.

ignored diagnostics: 3 (`karac check` prints 3 `error[` lines: two `error[borrow_projection_copy]` at L216 for `MonthTotal.new(sale.region, sale.month)`, and one `error[ownership]` at L372 for `path` reused after `fs.write`. All were left as written, and the build succeeds.)
