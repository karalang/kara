lines: 389
build: ok
interp: same
mirror: agrees
stmt-par: L345-L346 two fs.write calls (app.conf, app.local.conf); L347-L348 two fs.read_to_string calls (read back the same two files)
shared types: none
workarounds: none (no compiler failures). Three real typecheck errors on the first build were mine, not compiler bugs: `expected 'String', found 'ref String'` where an element of a bare `for` loop over a borrowed Vec (`key`, `var`, `arg`) was stored into an owned `String` field of a ConfigError; fixed with `.to_string()` at L186, L269, L285, which is what the spec says a borrowed loop element is. Also renamed my `struct Entry` to `Setting` after a `prelude_shadow` warning (it shadowed the prelude `Map` Entry type).
ignored diagnostics: 5 (`karac check`: 3 error[ownership] at L166 key reused after move, L253 path moved inside the loop, L290 body moved into a container; 2 error[borrow_projection_copy] at L338 moving `key`/`raw` out of `for (key, raw) in defaults()`). `karac build` printed these as warnings plus 2 perf[rc-fallback] notes and built fine.
