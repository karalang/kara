# mini_humanhash

A port of [humanhash3](https://github.com/blag/humanhash) 0.0.6 (Public Domain,
the Unlicense), which renders a hexadecimal digest as a short, memorable phrase
like `equal-monkey-lake-beryllium`. People compare and remember a four-word
phrase far more reliably than 32 hex characters, so humanhash is used to display
fingerprints — SSH or GPG key fingerprints, commit ids, API-key checksums — in a
form a human can eyeball. The mapping is deterministic: the same digest always
produces the same phrase (given the same wordlist).

## What it does

`mk.py` writes `data/jobs.tsv`: one `h <words> <hexdigest>` job per line
(tab-separated), asking for the human-readable rendering of a digest with that
many words. `oracle.py` replays each against the vendored `humanhash` library
and writes `expected.out`, one hyphen-joined phrase per job;
`mini_humanhash.kara` reproduces it byte-for-byte on every backend.

The jobs cover digests of several lengths (the library's own docstring digest,
SHA-1, SHA-256, all-zero and all-ones), word counts from 1 to 8, the
*passthrough* case where the requested word count is at least the byte length
(each byte becomes its own word), and — deliberately — digest/word-count pairs
that land on the segment boundaries where the library's float arithmetic rounds
differently from plain integer division (see below).

## How the port works

`mini_humanhash.kara` follows `humanhash.py`. The hex digest is read in pairs
into bytes; `compress` folds those bytes into `words` segments with an XOR
checksum and each segment byte indexes the 256-word list. The one subtle part is
which segment a byte lands in: the library computes
`floor(i / (length / words))` in floating point, and that is **not** always
equal to the integer `(i * words) / length` — the two disagree on certain
lengths (e.g. an 18-byte digest into 14 words). The port therefore does the same
float computation in `f64`. Kāra's `f64` is IEEE-754, bit-identical to Python's
`float`, and its `f64`-to-`i64` cast truncates toward zero (which is `floor` for
the non-negative quotients here), so the result matches the library on every
input, the rounding-boundary cases included — verified against the library.

## Scope

The port covers the deterministic rendering path (`humanize` / `humanize_list` /
`compress`) with the default 256-word list and a `-` separator. The library's
`uuid()` helper, which draws a random UUID, is not a deterministic surface and
is out of scope. The vendored `humanhash.py` is pure Python (it imports only
`operator`, `math`, `uuid` and `sys`), so `oracle.py` runs exactly the code the
port mirrors.

## Running

```bash
karac run mini_humanhash.kara                  # interpret
karac build --mir mini_humanhash.kara          # native binary
python3 -s -P -B mk.py                              # regenerate data/jobs.tsv
python3 -I oracle.py                                 # regenerate expected.out
python3 -s -P -B gen_kara.py                         # regenerate the embedded jobs
```
