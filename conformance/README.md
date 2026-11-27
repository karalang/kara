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
| `regex.py` | regex.md | The Rust `regex` crate the page names, at the version Cargo.lock pins (conformance/regex_oracle, built offline on first use): random patterns with groups, names, classes, repetition, anchors and flags, and invalid ones, through every method (compile, is_match, find, find_all, split, replace and replace_all with every `$` form, captures, captures_all, get, name, len, escape) on multibyte text |
| `http_server_fuzz.py` | io.md HTTP server | h11 0.16.0 (pinned; offline) as a strict RFC 9112 reader: raw requests (malformed request lines, header folding, duplicate and malformed `Content-Length`, transfer codings, chunked bodies with extensions and trailers, an over-long body, pipelining) and random mutations of them, each request the handler is given echoed and compared with h11's parse, every refusal required to be a 4xx/5xx that closes; a health request after each input catches a crash; build and ASan, the server ended through `exit(0)` so LSan reports. Differences RFC 9112 or the page leave to the server are reported, not failed; open rows are listed in `KNOWN` |
| `log_format.py` | log.md formats | A model of the page's JSON and text formats on Python's `json` (compact, quoting as serde_json does): random records (messages and string fields with spaces, quotes, `=`, controls, DEL, C1, non-ASCII; i64 limits; bools; Options; Vecs) at random levels through both backends on both streams under a random `init` level |
| `json_suite.py` | json.md | JSONTestSuite's `y_`/`n_`/`i_` files (`JSONTESTSUITE=<checkout>`); `stringify` against Python's parse laid out as serde_json 1.0.149 writes it; random `f64` bit patterns round-tripped |
| `ct_timing.py` | crypto.md, secret.md | `constant_time_eq` and `Secret.ct_eq` timed on a first-byte vs last-byte difference, compiled only, with `==` as the control that shows the instrument sees an early exit |
| `channel_model.py` | concurrency.md | Random single-task channel scripts against a model of the page's rules, the model itself checked against Go's buffered channels (`go run`) on every script Go can express |
| `atomics_model.py` | concurrency.md atomics | Random scripts of every `Atomic[T]` operation for `bool`, `i32`, `i64`, `u32`, `u64`, operands at each type's limits, against a model: previous values returned, wrapping `fetch_add`/`fetch_sub`, `compare_exchange`'s `Ok`/`Err`, logical bitwise ops on `bool` |
| `select_model.py` | concurrency.md `select` | Random single-task scripts of `select` (recv and send arms, `default`, a far `after`), `try_send`, `try_recv` and dropping either end, on a `Drop` type, against a model of the page's rules: first ready arm in source order wins, only its operation happens, a losing send's value drops, closed channels make arms ready, a dropped receiver drops its buffer |
| `pool_model.py` | pool.md | Random single-task scripts over two pools of a `Drop` type (acquire, release into its own pool or the other, drop a loan, close, a failing factory, a health check), the reused idle value read from the output and held to the page's rules: passing idle values reused first, only failing ones dropped, `CreateFailed` and `Timeout` at the right counts, one return per loan, `close` and pool drop dropping idle values and later returns |
| `secret_model.py` | secret.md access | Python's `==`: random `Secret[String]` and `Secret[Vec[u8]]` pairs (equal, one byte off at the start, middle or end, a prefix, empty, multibyte) through `ct_eq` both ways and against itself, then after an `expose_mut` push and a clear |
| `precedence.py` | design.md §6 | Every pair of binary operators, alone, around each comparison and under the logical words, against a parser of the page's table, that parser checked against Python's `ast.parse` wherever the two grammars agree (all but `not`); a pair counts only where a tuple tells its two groupings apart |
| `literals.py` | design.md §3 Numeric literals | Random integer literals in every base and float literals in every notation, with `_` grouping, against Python's `int(text, 0)` and `float(text)`; floats compared by bits, `f32` ones at single precision |
| `tls_client.py` | io.md HTTP client, HTTPS | A local test PKI (expired, not-yet-valid, wrong-host, CN-only, wildcard and IP-SAN, self-signed, untrusted-root, incomplete, bad-signature, non-CA issuer, clientAuth-only, RSA-1024 and SHA-1 certificates) against `Client.get` through a CONNECT proxy: each verdict against Python `ssl`, each refusal's message in words; SNI and ALPN off the ClientHello, h2 and HTTP/1.1, TLS 1.1 refused, `close_notify` against truncation, `KARAC_HTTP_EXTRA_CA` errors |
| `overflow.py` | design.md §5 Integer overflow, Division, remainder and shifts | Every integer type: the edges that must not trap, computed from the page's rules, and each trap the page lists exiting 101 with its message |
| `layout.py` | design.md §15 #[repr], extern "C" | Random `#[repr(C)]` structs over every type that crosses (integers to 128 bits, floats, `bool`, `char`, pointers, `#[repr]` enums, arrays, nested structs): `size_of` / `align_of` against the C compiler, each struct to C by pointer and by value and back by value and through a pointer, scalar calls past the argument registers, enums as their C integer; on interp, build and `karac run` |
| `cases.py <dir>...` | design.md §3, §6, §11 and §12 (`design/lexical`, `design/expressions`, `design/ownership`, `design/effects`; §11's package cells run with `packages.py conformance/design/ownership`), collections.md, strings.md, iterators.md, core-types.md, io.md, path.md, regex.md and json.md (`library/<page>`), and log.md, secret.md, pool.md, concurrency.md, encoding.md, crypto.md, data.md (`<page>`) and time-random-env.md (`random_env`); each of these directories maps every sentence of its page to a cell or script in its SENTENCE_MAP.md | The page, as fixed cases (below); deferred.md entries (`deferred`: a deferred construct is refused, an experimental one runs, a stated 0.8 subset holds; SENTENCE_MAP.md rows every entry) |

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
| `.effects` | The effect sets `karac query effects` must infer: `f = a, b` exactly, `f has a`, `f lacks a` |
| `.cmd`, `.json` | A karac command's output must hold each further line; for `.json`, each further line is a Python predicate over the parsed output `d` |
| `.notes`, `.absent` | Text `karac check` must print while accepting the program, or must not print |
| `.env`, `.args` | A Python literal: variables to add, and command-line arguments (bytes for a non-UTF-8 one). With `.args` the interpreter runs through `karac run --interp`, since `__mir-run` takes none |

A concurrency case holds output to an order only where concurrency.md
promises one (FIFO on a channel, a `par` block's source order, a group that
joins before the next line). Where it does not, the program checks the
guarantee itself and prints the verdict, as `taskgroup_fan_in` checks that
each producer's messages arrive in its own order.

```bash
python3 conformance/encoding.py --seed 1      # random cases; any seed
python3 conformance/crypto.py --seed 1
python3 conformance/regex.py --seed 1        # needs cargo for its oracle
python3 conformance/http_server_fuzz.py --seed 1   # needs h11 0.16.0 (pip install h11==0.16.0)
python3 conformance/log_format.py --seed 1
JSONTESTSUITE=../JSONTestSuite python3 conformance/json_suite.py --seed 1
python3 conformance/ct_timing.py
python3 conformance/channel_model.py --seed 1
python3 conformance/atomics_model.py --seed 1
python3 conformance/select_model.py --seed 1
python3 conformance/pool_model.py --seed 1
python3 conformance/secret_model.py --seed 1
python3 conformance/precedence.py --seed 1
python3 conformance/overflow.py
python3 conformance/layout.py --seed 1
python3 conformance/literals.py --seed 1
python3 conformance/tls_client.py
python3 conformance/cases.py conformance/log conformance/secret conformance/pool conformance/encoding conformance/crypto \
    conformance/concurrency conformance/random_env conformance/data \
    conformance/design/lexical conformance/design/expressions conformance/design/ownership conformance/design/effects \
    conformance/library/collections conformance/library/strings conformance/library/iterators conformance/library/core-types conformance/library/io conformance/library/path conformance/library/regex \
    conformance/library/json conformance/deferred
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
