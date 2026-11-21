# Library conformance probes

Each probe here checks one page of `docs/library/` against an oracle that
does not come from karac. Wherever possible the oracle is a reference
implementation: Python's `base64`, `binascii`, `urllib.parse`, `hashlib`
and `hmac`, FIPS 180-4's example digests, and RFC 4231's HMAC test cases.
Where none exists, the oracle is the page itself, as a fixed case. Every
program runs on the MIR interpreter and, when karac has the LLVM backend,
compiled. Both must give the expected output.

| Script | Page | Oracle |
|---|---|---|
| `encoding.py` | encoding.md | Python's `base64`, `urllib.parse`, and the page's own rules where Python is laxer (non-canonical Base64, whitespace in hex) |
| `crypto.py` | crypto.md | FIPS 180-4, RFC 4231, `hashlib`, `hmac`; the streaming types against the one-shot functions for random splits |
| `json_suite.py` | json.md | JSONTestSuite's `y_`/`n_`/`i_` files (`JSONTESTSUITE=<checkout>`); `stringify` against Python's parse laid out as serde_json 1.0.149 writes it; random `f64` bit patterns round-tripped |
| `ct_timing.py` | crypto.md, secret.md | `constant_time_eq` and `Secret.ct_eq` timed on a first-byte vs last-byte difference, compiled only, with `==` as the control that shows the instrument sees an early exit |
| `channel_model.py` | concurrency.md | Random single-task channel scripts against a model of the page's rules, the model itself checked against Go's buffered channels (`go run`) on every script Go can express |
| `precedence.py` | design.md §6 | Every pair of binary operators, alone, around each comparison and under the logical words, against a parser of the page's table, that parser checked against Python's `ast.parse` wherever the two grammars agree (all but `not`); a pair counts only where a tuple tells its two groupings apart |
| `overflow.py` | design.md §5 Integer overflow, Division, remainder and shifts | Every integer type: the edges that must not trap, computed from the page's rules, and each trap the page lists exiting 101 with its message |
| `cases.py <dir>...` | design.md §3 and §6 (`design/lexical`, `design/expressions`), log.md, secret.md, pool.md, concurrency.md, the random and env half of time-random-env.md, and the examples on encoding.md and crypto.md | The page, as fixed cases (below) |

A fixed case is `<name>.kara` with files beside it, each written from the page:

| File | Meaning |
|---|---|
| `.out`, `.err` | The exact stdout and stderr; with a nonzero `.exit`, `.err` holds lines stderr must contain |
| `.exit` | The exit status, when the page promises a panic |
| `.unordered` | The page leaves the order of lines open, so they are compared sorted |
| `.repeat` | A schedule-sensitive case: N runs (20 if empty) on each backend, and every run must meet the expectation |
| `.refused` | `karac check` must refuse it, printing each line of the file |
| `.buildrefused` | The page refuses it when the program is built: check passes, and both the interpreter and the build fail with each line on stderr |
| `.fixed` | `karac fix` must turn the program into exactly this text |
| `.env`, `.args` | A Python literal: variables to add, and command-line arguments (bytes for a non-UTF-8 one). With `.args` the interpreter runs through `karac run --interp`, since `__mir-run` takes none |

A concurrency case holds output to an order only where concurrency.md
promises one (FIFO on a channel, a `par` block's source order, a group that
joins before the next line). Where it does not, the program checks the
guarantee itself and prints the verdict, as `taskgroup_fan_in` checks that
each producer's messages arrive in its own order.

```bash
python3 conformance/encoding.py --seed 1      # random cases; any seed
python3 conformance/crypto.py --seed 1
JSONTESTSUITE=../JSONTestSuite python3 conformance/json_suite.py --seed 1
python3 conformance/ct_timing.py
python3 conformance/channel_model.py --seed 1
python3 conformance/precedence.py --seed 1
python3 conformance/overflow.py
python3 conformance/cases.py conformance/log conformance/secret conformance/pool conformance/encoding conformance/crypto \
    conformance/concurrency conformance/random_env conformance/data \
    conformance/design/lexical conformance/design/expressions
```

`CONF_LANES=interp,build,asan,tsan` adds the AddressSanitizer and
ThreadSanitizer builds to `cases.py`, `channel_model.py`, `precedence.py` and
`overflow.py` (`karac
__mir-llvm-asan` / `__mir-llvm-tsan`; TSan on four workers, at least three
runs, against `libkarac_runtime_tsan.a` built over an instrumented std as
CLAUDE.md § Runtime archives says), and any sanitizer report fails the run.
`KARAC=<path>` picks the binary (default `target/release/karac`), and
`CONF_BUILD=0` skips the compiled run. A script exits 1 on any failure and 2
when it compared nothing.

data.md describes the post-v1 `kara-data` package. The page says nothing on
it runs yet, so `data/` holds only that refusal: `karac check` refuses
`Column` and `DataFrame` naming the package and the page. Its API gets a
pandas-oracled probe when the package ships.
