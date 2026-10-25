# mini_jsbeautifier

[jsbeautifier](https://github.com/beautifier/js-beautify) 2.0.3, the Python
package of beautifier.io's JavaScript formatter, ported to Kāra. It is the
tool behind "format document" in many editors and the standard way to read
a minified bundle. The port takes the formatter's own test suite, replays
every call it makes and prints what each one returns.

```
== 2565 (set 201, 382 chars in, 559 out)
| ! function(e, t) {
|     "object" == typeof exports && "undefined" != typeof module ? module.exports = t() : ...
| }(this, function() {
|     "use strict";
|     var e = function(e, t) {
|         return e.qty * e.price + (t || 0)
|     };
| 
|     function t(t, n) {
|         var r = 0;
|         for (var o = 0; o < t.length; o++) r += e(t[o]);
|         return n ? Math.round(r * (1 + n.rate) * 100) / 100 : r
|     }
...
```

## What it does

`data/jobs.txt` holds 205 option sets and 2584 jobs. `mk.py` runs the
upstream test suite (`vendor/jsbeautifier/tests/generated/tests.py`) with `jsbeautifier.beautify` wrapped, and records each
call's source and options: 18,947 calls, 16,772 of them distinct. It keeps
one in seven of the distinct calls and the first call under every option
set, which puts each option the suite sets in the jobs. Four hand-written
programs follow, each under five option sets: a minified UMD module, an
event handler with template literals and a regexp, a class with getters,
a generator, a switch and a labelled loop, and a file with every kind of
comment and a `preserve:start` block.

The options cover indent size and character, tabs, a base indent level,
kept and capped blank lines, line wrapping, every brace style including
`preserve-inline`, chained-method breaking, space in and after parens,
`jslint_happy`, kept array indentation, comma-first, the three operator
positions, unescaped strings, E4X literals, the template languages
(Django, ERB, Handlebars, PHP), `end_with_newline`, each end-of-line
setting, `disabled`, and option names written with dashes or under a
`js` child.

`oracle.py` builds each job's options object the way the test suite
hands one over, calls the vendored library (`vendor/`, MIT, see
`vendor/LICENSE`) and wrote `expected.out`. Each result is printed line by
line, with control characters and Unicode separators shown as `\u{hex}`.

## How the port works

The tokenizer is the library's `InputScanner` and its patterns written as
matchers over the source's code points, in the order the regular
expressions try their alternatives: numbers, punctuation, the template
starts, acorn's identifier tables (`ID_START` and `ID_PART` as ranges,
with `\u` escapes), and the E4X pattern, whose backtracking order decides
which `<` starts a tag. Tokens are an arena with `-1` for None, and
`parent`, `previous`, `opened` and `closed` are indices into it.

The beautifier keeps its frames (`BeautifierFlags`) in an arena too, with
the frame stack and `_previous_flags` as indices, because Python mutates
a frame through every reference to it: `handle_end_expr` sets the mode of
the frame it just left. `Output` and `OutputLine` are ported with the
indent string cache, wrap points and the comma-first pull-back.

The port keeps the library's surprises, since the output depends on them:

- A javadoc check skips empty lines, because `any()` runs over the lines
  themselves.
- `}* drain()`: a generator method after a getter stays on the closing
  brace's line.
- A `</tag>` where an E4X literal could start is skipped: the matcher
  consumes it and no token comes of it.
- `#{` (a record) opens a block that no `}` is matched to.

The unpackers (eval-packed and obfuscated scripts) are not vendored. With
no unpacker matching, `unpack()` returns the source, so the port passes it
through and the recorder and oracle stub the module the same way.

## Compiler gaps

One, filed as B-2026-10-24-25: inside a method that writes `self`, a bare
`for p in PUNCT` over a global `const` array is taken to borrow `self`, so
`karac check` refuses `self.pos += …` in the loop body (E0516). The
checker resolves a name it cannot find among the locals to `self`
(`root_of` in `src/typechecker/core_ref_write.rs`). The same loop in a
free function, or over `PUNCT.iter()`, passes. The port used `.iter()`
until Thread A fixed the row (kara f0ada4a3b1); it is the bare loop again.

`karac check` also found two of my mistakes: `for bs in
get_array(...)` borrows each element, so moving one out takes
`.into_iter()`, and a dropped `Option` from `pop()` wants `let _ =`. After
one wrong nesting of mine in the comma-first branch, the MIR interpreter,
the LLVM backend, the ASan build and the native binary each matched the
oracle byte for byte, with nothing on stderr.

## Scope

The port covers `jsbeautifier.beautify` for JavaScript. It leaves out the
command line, `beautify_file`, the unpackers, and the CSS and HTML
beautifiers, which live in other packages. Sources with lone surrogates
are not in the jobs, since a Kāra `String` cannot hold them.

## Running

```bash
python3 -s -P -B mk.py                 # writes data/jobs.txt
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py              # the jobs_text() function at the end of the .kara
karac run mini_jsbeautifier.kara
karac build --mir mini_jsbeautifier.kara && ./mini_jsbeautifier
```

About 45 s on the MIR interpreter (a debug build of `karac`) and 0.06 s
built. The program writes `data/jobs.txt` and reads it back.
