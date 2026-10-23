# mini_ijson

A port of [ijson](https://pypi.org/project/ijson/) 3.3.0 (BSD-3-Clause;
Ivan Sagalaev, Rodrigo Tobar): the iterative JSON parser that ETL jobs,
log shippers and API clients use when a document is too large to load
at once. It reads its input a chunk at a time and hands out events, or
the objects found under a prefix, as it goes. This is the library's pure
Python backend, the one every installation has.

```
> items data/shop/events_bad.jsonl  multi=1 buf=256
  {'ts': '2024-03-04T10:00:31Z', 'type': 'click', 'user': 103, 'ms': Decimal('127.25')}
  ...
  {'ts': '2024-03-04T10:24:19Z', 'type': 'view', 'user': 100, 'ms': Decimal('228.0')}
  error ijson.backends.python.UnexpectedSymbol: Unexpected symbol 'out' at 1217
```

## What it does

`mk.py` writes 15 files to `data/` and a script of 184 jobs to
`data/jobs.tsv`. The files are a shop's order export, its product
catalog, a JSON-lines event log with a good and a corrupt copy, sensor
readings with exponents, and files with a byte-order mark, Latin-1 bytes,
a cut-off UTF-8 character and raw control characters. `oracle.py` replays
the script against the vendored library and `mini_ijson.kara` replays it
against the port. An exception prints as `error <class>: <message>`; 80
jobs end in one.

- **The four interfaces**: `basic_parse`, `parse` with its dotted
  prefixes (including the ambiguous ones a key with a dot makes),
  `items` and `kvitems`, with `multiple_values`, `use_float`,
  `map_type=OrderedDict`, `buf_size` and `allow_comments`.
- **Numbers**: `int` for plain digits of any length, `Decimal` otherwise
  with its exact `repr` (`Decimal('1E+5')`, `Decimal('-0E+1')`), or
  `float` with `use_float`. The leading-zero and fraction checks, and the
  `+7` and `+007` that Python's `int()` lets through.
- **Strings**, decoded by `json.decoder.scanstring`, so its errors and
  their positions within the string are CPython's.
- **Errors** from every layer: the UTF-8 decoder, the lexer
  (`Incomplete string lexeme`), and the parser (`Unexpected symbol`,
  `Additional data found`, `Incomplete JSON content` for a `nul` cut
  off at the end of the input).
- **Chunking**: which events come out before an error depends on the
  chunk size, because the events of the chunk that fails are lost with
  it. The same document runs at chunk sizes from 1 to 64 KiB.
- **The push interface** (`items_coro` and the others), driven by
  `send()` and `close()` as a network reader would, with cuts inside a
  multi-byte character, an empty chunk that ends the parse, and a
  character still cut off at `close()`.
- **The `dump` command-line tool**, with each method.
- **Four programs**: a revenue report over the order export in exact
  decimals, with each order's lines checked against its total and
  discounts rounded half-even; a latency summary of the event log; a
  schema survey of every prefix; and a JSON-lines extractor that writes
  with `json.dumps`.

## How the port works

- **Each coroutine is a state machine.** The library chains Python
  generators: a UTF-8 decoder feeds a lexer, which feeds a parser, which
  feeds the prefix tracker and the object builders. Each state machine
  remembers where its generator was waiting, so a number split across
  two chunks waits for the rest, a string lexeme spans as many chunks as
  it needs, and an error stops the chain at the same place.
- **The generator interface collects events a chunk at a time** and
  drops them when the chunk raises, as `coros2gen` does. The push
  interface keeps them, since its sink is a list.
- **Values keep Python's types.** Integers are kept as digits, decimals
  as sign, coefficient and exponent, and maps as ordered key lists, so
  `repr()`, `str()` and `json.dumps` print what Python prints.
- **The object builder closes containers instead of opening them.**
  ijson puts a new container into its parent when it starts; the port
  puts it there when it ends, under the key that was current when it
  started, which builds the same object.

## Compiler gaps

None. `karac check` refused `final` as a parameter name (it is reserved)
and asked for one `.clone()` on a borrowed `for` element; both are
language rules. The one wrong result on the first run was a port bug: the
`items` builder never put the outermost container in place.

## Scope

- The pure Python backend, as the library uses it when no yajl backend is
  built. The async interfaces are not covered.
- A string with a lone surrogate (`"\ud83d"` alone) cannot be held in a
  Kāra `String`, so the port gives U+FFFD there; no input has one.

## Running

```bash
karac run mini_ijson.kara                   # interpret
karac build --mir mini_ijson.kara           # native binary
python3 -s -P -B mk.py                      # regenerate data/
python3 -I -B oracle.py > expected.out      # regenerate expected.out
python3 -I -B gen_kara.py                   # regenerate the embedded inputs
```
