# mini_duration

A port of [durationpy](https://pypi.org/project/durationpy/) 0.11 (MIT; Ilia
Choly), the **Go-style duration-string codec**. Go prints and parses durations
as compact strings like `1h30m`, `1.5h`, `2w3d`, and durationpy is the Python
library that reads and writes that format — it turns up wherever a Python service
consumes a config or API value written in Go's convention (Kubernetes manifests,
Consul, Vault, Prometheus-adjacent tooling).

## What it does

`mk.py` writes `data/jobs.tsv`: one tab-separated job per line, one of

- `p <durstr>` — parse a duration string to a `timedelta`, printed as its three
  integer fields `days|seconds|microseconds`, or the token `ERR` if the library
  rejects it;
- `f <micro>` — format `timedelta(microseconds=<int>)` back to a duration string
  (default units: everything rolls up into hours);
- `fe <micro>` — the same, `extended=True`, so years / months / days are broken
  out as their own components.

`oracle.py` replays each job against the vendored `durationpy` library and writes
`expected.out`; `mini_duration.kara` reproduces it byte-for-byte on every backend.

## How the port works

`mini_duration.kara` follows `durationpy/duration.py`. `parse_dur` scans the
string into `([value][unit])` tokens, requires them to cover the whole string
with no gaps (the library asserts the same via `re.finditer` start/end), looks up
each unit's size in nanoseconds, and sums `value * size`. The library does this
in floating point (`float("1.5") * 3_600_000_000_000`); the port does it in exact
`i64` **fixed-point** — a value like `1.5` becomes the integer `15` with a scale
of `10`, and `num * size / 10^scale` is computed only when it divides exactly —
so the two agree bit-for-bit over the exercised subset. The signed total is then
normalised to a `timedelta`'s `(days, seconds, microseconds)` with floored
division, which is why a negative duration carries its sign on `days` and keeps
`seconds`/`microseconds` non-negative (e.g. `-1h30m` → `-1|81000|0`).

`to_str_dur` reverses it: it decomposes the nanosecond total into unit components
largest-first, subtracting as it goes, exactly as the library does. Python formats
each component with `{:g}`; over the subset here every component is a whole number,
so `{:g}` is plain integer printing.

## Scope

The port covers the duration grammar durationpy accepts — signed values, decimal
values, multi-unit strings, the `0`/`+0`/`-0` special cases, and all ten ASCII
units (`ns`, `us`, `ms`, `s`, `m`, `h`, `d`, `w`, `mm`, `y`) — and both format
directions. Three corners are left at the library/`datetime` boundary and are
documented rather than worked around:

- **Sub-microsecond granularity.** `datetime.timedelta` has microsecond
  resolution and rounds finer values half-to-even, so the `ns` unit and fractional
  values below a microsecond (e.g. `1.25us`) follow that rounding rather than the
  codec itself. The exercised inputs are all whole-microsecond, where floored and
  rounded results coincide.
- **Fractional format output.** `to_str`'s `{:g}` renders a non-integer component
  (e.g. `1.5s`) through CPython's float formatting; the exercised format inputs
  produce only whole-number components, so `{:g}` reduces to integer printing.
- **Error message text.** A rejected duration is reported with the canonical
  token `ERR`. durationpy's `DurationError` message embeds the original string and
  its wording depends on Python regex `finditer` classification (` 1h` is "extra
  chars at start", `abc` is "invalid duration"); reproducing that exact text is a
  library-boundary detail, not part of the codec.

The vendored `durationpy` uses only `re` and `datetime` from the standard library,
so `oracle.py` runs exactly the code the port mirrors.

## Running

```bash
karac run mini_duration.kara                 # interpret
karac build --mir mini_duration.kara         # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I oracle.py                           # regenerate expected.out
python3 -s -P -B gen_kara.py                   # regenerate the embedded jobs
```
