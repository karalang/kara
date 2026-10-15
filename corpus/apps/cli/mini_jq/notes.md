# mini_jq

A command-line JSON processor for a subset of jq 1.7, run the way shell
scripts run jq over API responses, logs and config files. The run writes
four input files into `data/` and a `jobs.tsv` of 47 jq invocations (one
per line: flags, file, program), then runs each job and prints what jq
prints: a `$ jq ...` line, stdout, then stderr, and `[exit N]` when the
status is not 0.

The inputs:
- `pods.json`, a Kubernetes pod list (one pod Pending with no container
  statuses and no start time, one Failed);
- `releases.json`, a GitHub-style release list with assets and download
  counts (one prerelease, one release with no assets);
- `events.ndjson`, seven log events, one per line, one of them with a
  string where the latency should be a number;
- `config.json`, a service config with environments, services, feature
  flags and number literals like `0.10` and `1.50`.

The jobs cover what such scripts do: counting and filtering, string
interpolation into report lines, `group_by` summaries, `@tsv`/`@csv`
exports, ISO dates (`fromdateiso8601`, `todate`), `try`/`catch` with
`error`, `limit`/`first`, `-s` slurping and `-n` with `inputs`, `reduce`
and `foreach` counters, updates (`+=`, `|=` through `map_values` and
`walk`, `del`, `setpath`, `delpaths`), deep merge with `*`, `paths`,
`to_entries`/`from_entries`, `-S`, `--arg`/`--argjson`, destructuring
(`as [$first, $second]`), user `def`s with recursion and filter
arguments, the formats `@base64 @uri @html @sh @csv @json`, mixed-type
`sort`, and jq's number printing (`0.30000000000000004`, `4e+17`,
`0.10` kept as written). Six jobs fail on purpose: a runtime error on
the fifth NDJSON line (the other lines still print), adding a number to
an object, indexing a string, an undefined variable, an undefined
function, and two syntax errors.

**The processor.** A lexer and a recursive-descent parser with jq's
precedence (`|`, `,`, `//`, assignments, `or`, `and`, comparisons,
`+ -`, `* / %`, unary minus, postfix terms; `as` bindings, `def`,
`reduce`, `foreach`, `if`, `try`, string interpolation with nested
strings, object construction with shorthand keys). A compile-time pass
reports undefined functions and variables with jq's message and column.
The evaluator runs a stream eagerly: each expression appends its outputs
to a buffer and returns the error that ends the stream, so outputs
before an error are kept, as jq keeps them. Products iterate in jq's
order (a binary operator's right operand slowest, an object's first key
slowest, a string's first interpolation fastest). Path expressions run
on the same evaluator with each output's path alongside it, which is
what `path`, `paths`, assignment and `del` use. Scopes are a chain of
frames: variables, definitions (a call runs in its definition's frame)
and filter arguments (closures over the caller). About forty of jq's
builtins are jq definitions in a prelude, as in jq itself; the rest are
natives. Numbers keep the text they were written with, so a literal
from the input or the program prints as written, and a computed number
prints jq 1.7's shortest 17-digit form, read off the double's shortest
round-trip digits.

Not supported: regular expressions (`test`, `match`, `sub`, ...),
`label`/`break`, `?//` alternative patterns, `limit` and `first` on
infinite streams (streams are eager), `getpath`/`paths` streaming
(`tostream` is defined, `fromstream` is not), `$__prog_args`,
`input_line_number`, `@base32`, `strftime`/`strptime` beyond the ISO
format, the SQL-style builtins, and math builtins beyond `floor ceil
round sqrt fabs log exp pow`. Case mapping
is ASCII, as jq's `ascii_*` are.

**Oracle.** `expected.out` is jq 1.7's output on the same files, made by
`make_expected.py` (run from this directory), which strips `data/` from
stderr so the file names match. The `data/` files and `jobs.tsv` the run
writes are byte-identical to the ones in this directory.

## Edits from mini_jq.orig.kara to mini_jq.kara

Author errors (the diagnostics were right):
- The base64 alphabet constant was named `B64`; Kāra's case classes read
  a name with one letter as a type name, so it is `BASE64_ALPHABET`.

Compiler gaps, worked around and cited in the source:
- B-2026-10-15-32: `karac check` takes a pattern binding as moving its
  owned scrutinee on every path, so the error value, matched with
  `J.Str(s)` and dumped in the other arm, is matched through a `ref`
  binding.
- B-2026-10-15-24: in `match items[i]`, `karac check` binds a payload
  `x: f64` as a reference and accepts `*x`, while the MIR front end (and
  `karac run --interp`) bind it by value and reject `*x`. `implode`
  matches through a `ref` binding.
- B-2026-10-15-37: avoided from the start. The syntax-tree parameter is
  `tree`, not `ast`.

Written 2026-10-15 by the real-world apps thread; karac at 2cec444448.

Reverted 2026-10-15, karac at aec4d15cc0: B-2026-10-15-24 is fixed, so
the codepoint is matched as `match items[i]` again. This is the original
line again, and the app still matches on all four MIR surfaces.
