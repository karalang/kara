# mini_parsy

A port of [parsy](https://pypi.org/project/parsy/) 2.2 (MIT; Jeanine
Adkisson), the parser combinator library for Python, with the grammars a
program writes with it. A parser wraps a function from a stream and an
index to a result; combinators build new parsers from old ones, and the
primitives match strings, regular expressions, characters and the items of
a token list. Each result carries the furthest failure seen and what was
expected there, which is how parsy words its errors. The vendored copy in
`vendor/parsy/` is the package, unchanged.

```
== 2 string parse 'y'
  ! ParseError: expected 'x' at 0:0
== 347 json parse <153 characters>
  {'int': 1.0, 'string': 'hello', 'a list': [1.0, 2.0, 3.0], 'escapes': '\n ⓒ', 'nested': {'x': 'y'}, 'other': [True, False, None]}
== 392 eval pipe '2 * (3 + 4)'
  tokens [2, '*', '(', 3, '+', 4, ')']
  14
== 422 http_request parse @get.http
  Request(method='GET', target='/api/orders?status=open&limit=20', version=(1, 1), headers=[('host', 'kds.example'), ...])
== 431 semver_range parse '^1.2.3 || ~2.4'
  [[(<Op.CARET: '^'>, (1, 2, 3))], [(<Op.TILDE: '~'>, (2, 4, 'x'))]]
== 445 query pipe 'NOT (label:"needs review" OR label:wip) -draft'
  tokens [('NOT',), ('(',), ('WORD', 'label'), (':',), ('PHRASE', 'needs review'), ...]
  ('and', [('not', ('or', [('field', 'label', ':', 'needs review'), ('field', 'label', ':', 'wip')])), ('not', ('term', 'draft'))])
== 448 query pipe '(a OR b'
  ! ParseError: expected one of '(', ')', '-', ':', '<', '<=', '>', '>=', 'AND', 'NOT', 'NUM', 'OR', 'PHRASE', 'WORD' at 4
```

## What it does

`mk.py` writes `data/jobs.txt`, 453 jobs over about a hundred parsers, and
17 documents under `data/docs/`. `grammars.py` builds the parsers with the
vendored parsy and `oracle.py` runs the jobs through them; `mini_parsy.kara`
builds the same parsers with the port. Both print each result as Python's
repr prints it, and each error as `! ParseError: ...` (or `ValueError`,
`AttributeError`). A job parses a whole input, parses a prefix
(`parse_partial`, value and rest), parses a list of tokens, parses each
line of a document, or runs a lexer and then a parser over its tokens.

- **The library, from parsy's own tests**: `string` with and without a
  transform, `regex` with groups by number, name and tuple, an optional
  group, and flags; `then`, `skip`, `bind`, `map`, `combine`,
  `combine_dict` (dropping `None` keys and `_` names), `concat`,
  `@generate` with and without a description, `mark`, `tag`, `result`,
  `desc`; `|` with backtracking, `+` on strings and lists, `*` by a count
  and by a range, `>>` and `<<`; `many`, `times`, `at_most`, `at_least`,
  `optional`, `until` with `min`, `max` and `consume_other`, `sep_by` with
  ranges; `alt` and `seq`, positional and by keyword; `test_char`,
  `char_from`, `string_from` (longest first), `from_enum` over `Enum`
  classes, `peek`, `any_char`, `whitespace`, `letter`, `digit`, `decimal`,
  `index`, `line_info` and `line_info_at`, `should_fail`, `eof`; parsers
  over lists of tokens (`test_item`, `match_item`), and
  `forward_declaration` with `become`, including becoming twice and
  calling one that never became anything.
- **parsy's examples**: JSON, S-expressions with quote, a SQL `SELECT`
  into dataclasses, a Logo lexer and parser, and a calculator that lexes
  to tokens and parses them with generators.
- **Grammars of this app's own**, the same in the oracle and the port: an
  INI file with sections, comments and CRLF lines (each setting marked with
  its line), nginx's access log, RFC 4180 CSV with quoted fields, an
  HTTP/1.1 request head with folded headers, npm version ranges (`^`, `~`,
  `x`, hyphen ranges, `||`), and a search query language with a lexer to
  tuples and a parser for `AND`, `OR`, `NOT`, `-`, fields, comparisons,
  phrases and parentheses.

Values are a `Val`, a `shared enum` for Python's objects (None, bool, int,
float, str, list, tuple, dict, frozenset, records for namedtuples and
dataclasses, enum members), with a repr that matches Python's. Regular
expressions are compiled and matched by mini_re, the port of Python's
`re`, whose engine this program includes; text is code points, as in
Python's `str`. A regex parser keeps its matcher from one call to the next
on the same stream, so the text is copied in once. A forward declaration
holds the parser it became, which closes a cycle of handles; each grammar
breaks it when it is dropped, and the ASan build reports no leaks.

Not ported: parsing `bytes`, regexes given as compiled `re` objects, and
the `AttributeError` that `letter` and friends raise on a list of ints.

## Edits from mini_parsy.orig.kara to mini_parsy.kara

`karac check` gave 28 errors. `karac fix` applied four: `ref` on two
pattern bindings of the regex parser's group (kept), a `.clone()` on a
month name that did not clear the error under it, and `#[derive(Copy)]` on
the regex engine's `Op` enum, which changed nothing (B-2026-10-25-15,
open). Both of those were dropped. The rest were by hand:

- Mine: `for parser in parsers` inside the `alt`, `seq` and `seq_kw`
  closures moved the captured list (now `.iter()`), and the month tables
  were `String` arrays (`let names = [...]` makes `String`s; now
  `Array[Str, 12]`).
- A `ref` to a `shared enum` is refused where a handle is wanted
  (B-2026-10-25-12): seven `.clone()`s on a `Val`.
- The closure-kind check calls three things a consume that move nothing
  (B-2026-10-25-13): a captured parser passed to `g.take`, a
  captured `Val` returned, and `match group` on a captured value. These
  are `.clone()`s and `match ref group`.
- The regex parser's closure used the compiled program four times, which
  check called a move (B-2026-10-25-14, since fixed), and wrote the cache's
  fields, which made it `MutFn`. The program and the matcher now live in
  the cache, and a `search` method on it does the update.

Then check was clean, and the MIR interpreter stopped: a captured forward
declaration was moved by the first of two closures (B-2026-10-25-16), so
each closure captured its own `.clone()`. After that every job crashed on
the compiled backends and the interpreter called a reference dangling: a
closure stored in a parser captured the borrowed parameter it read by
reference (B-2026-10-25-17, high). Ten parameters that the parsers keep
became `own`, and six `let me = self;` became `self.clone()`. Then it
matched the oracle under the MIR interpreter, the LLVM backend, the ASan
build and the native binary, with nothing on stderr.

Kara fixed both rows in f6b4ec231b, so those edits are reverted: the two
closures capture the forward declaration itself, the ten parameters
borrow again and the six are `let me = self;`. It still matches the
oracle on all four backends with nothing on stderr (a debug `karac`
built at 11956dad75).

B-2026-10-25-12 was fixed in 92a0f23874, so six of its seven `.clone()`s
are gone too. The seventh, in `enum_class`'s
`members.iter().map(|(n, v)| (n.to_string(), v)).collect()`, needed the
tuple built in a closure to read the `ref Val` as the handle
(B-2026-10-26-3), fixed in 99e5f3ff2f, so it is gone as well. It still
matches the oracle on all four backends (`karac` at 98f1efe3fb).

B-2026-10-25-13 was fixed in 2928490eee, so the sixteen `.clone()`s inside
closures and the `match ref group` are reverted too. Check is clean and
the output still matches the oracle on all four backends (`karac` at
8f312bbd5c).

B-2026-10-25-14 was fixed in 1b9edf0601. Put back, the closure still
failed check twice: writing the cache's fields makes it `MutFn`
(B-2026-10-26-10, open), and the nested
`gs.iter().map(|g| group_val(prog, ...))` in one `match` arm was called a
move of the program that the other arms read (B-2026-10-26-12). Kara
fixed B-2026-10-26-12 in 6f363a1aa2, so the closure captures the compiled
program again and passes it to `search`; only the matcher and its stream
id stay in the cache, until B-2026-10-26-10 is fixed. It still matches
the oracle on all four backends with nothing on stderr (`karac` at
b5230d6642).

Later `karac fix` dropped 95 `*`s on `Copy` parts bound out of borrowed
values, which are copies rather than views (kara B-2026-10-24-36); the
output is unchanged.

## Running

```
python3 -s -P -B mk.py            # writes data/jobs.txt and data/docs/
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py         # embeds data/ in mini_parsy.kara
karac run mini_parsy.kara
```

The program writes `data/` from what it carries and reads the jobs back.
About 2.5 s on the MIR interpreter (a debug build of `karac`) and 0.01 s
built.

## License

parsy is under the MIT license in `vendor/LICENSE`:

```
Copyright (c) 2013 Jeanine Adkisson

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```

The grammars, the oracle, the job files and the Kāra port are this repo's.
