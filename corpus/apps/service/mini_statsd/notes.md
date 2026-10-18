# mini_statsd

Etsy's statsd 0.9.0, the metrics daemon behind a great many Graphite
dashboards, with its graphite backend and its management console.
Applications fire counters, timers, gauges and sets at it over UDP without
waiting for an answer; every flush interval it turns them into per-second
rates, percentiles and histograms, writes them to Graphite over TCP, and
resets them. Operators query and prune its state through a TCP console.
This program is the daemon, its clients and the Graphite collector, all
talking over real 127.0.0.1 sockets.

The inputs in `data/` were made by `mk.py`:
- `statsd-classic.js`, a config in statsd's own format (a JS object
  literal, with comments): the legacy Graphite namespace, three percentile
  thresholds including a bottom one (`-10`), and histogram bins for
  `api.latency`.
- `statsd-modern.js`: `deleteIdleStats`, a gauge TTL of 2 flushes,
  `calculatedTimerMetrics`, `keyNameSanitize: false`, a non-default
  `prefixStats`, and the new Graphite namespace with a global prefix, an
  empty counter prefix, a timer prefix, a suffix and no raw counts.
- `jobs.txt`: packets to send (several metrics per packet, sample rates,
  DogStatsD tags, keys with spaces, slashes and junk, an integer key, six
  malformed lines, and every quirk of JS's `Number()` that the validity
  check lets through: hex, padding, exponents, a leading `.`, an
  unknown type, a type with a trailing space and a sample rate of 0),
  console commands, and flushes.

The program runs the jobs in order. `config` starts a daemon from a
config file, `send` sends one UDP packet, `admin` sends one console
command and prints the reply, and `flush` runs a flush and prints what the
collector received. It shows statsd's behaviour exactly, including JS's
number formatting and object key order (`404` is listed before every
other counter), that a counter deleted from the console comes back as
`NaN` the next time statsd increments it, that `@0` makes a counter
`Infinity`, that an empty timer reports `count_ps` before `count`, that
`calculatedTimerMetrics: ['upper_99_9']` never matches anything, and that
`flush_length` is the length of the previous payload.

Not supported: the `stats` and `config` console commands, the pickle
protocol, `keyFlush`, other backends and servers, and config reloading.
The daemon binds the ports its config names; there is no real clock, so
flushes happen when a job asks for one.

**Oracle.** `expected.out` is the output of `real.py`, which runs the
jobs against the real statsd 0.9.0 (node 22) with its ports rewritten to
free ones, and normalizes what depends on the wall clock: timestamps
become a virtual clock (startup at 1760000000, flush k at 1760000000 +
5(k+1)), the millisecond timings and `timestamp_lag` become 0,
`last_flush` becomes the previous virtual flush time, and `flush_length`
the length of the previous normalized payload (`npm install --prefix
realnode statsd@0.9.0` puts statsd where `real.py` looks for it, and a run
takes about 40 seconds of real flushes). `oracle.py` is the same
program as this one in Python, and its output is byte-identical.

## Edits from mini_statsd.orig.kara to mini_statsd.kara

Compiler gaps:
- B-2026-10-15-68: `f64.partial_cmp` runs on no v2 surface, so the timer
  sort compares with `<` and `>`;
- B-2026-10-15-69: binding a `TcpListener` to port 0 prints a
  `BOUND_PORT=<n>` line to the program's stdout on every v2 surface, so
  the daemon binds the ports its config names instead of letting the OS
  choose.

Making that last edit, I first wrote the job loop as `match (daemon,
net)`, which moves both into the tuple on every iteration. Check passed it
and every v2 surface refused it (B-2026-10-15-70), so it is two nested
matches.

After that, the output matched `expected.out` on `__mir-run`,
`__mir-llvm-run` and `__mir-llvm-asan` on the first run.

The B-2026-10-14-70 workaround (`ref` rebinds and the `add_at`, `push_at`
and `insert_at` helpers) was dropped after the fix in 0bacd9912b. The
original `self.counters.vals[i]` reads and writes need no `mut ref place`
binding, so nothing of B-2026-10-15-14 remains either.

Written 2026-10-15 by the real-world apps thread; karac at 7e52125b4c.

Note 2026-10-15, karac at eae83f9d88: B-2026-10-15-69 is fixed, so
binding port 0 no longer prints anything. The daemon still binds the
ports its config names, since that is what the real statsd does and the
config reload depends on it; the original's port-0 binding would only
come back as a partial, mixed revert.

Reverted 2026-10-15, karac at 3385ce51b1: B-2026-10-15-68 is fixed, so
the timer sort is the natural `sort_by(|a, b|
a.partial_cmp(b).unwrap())` again, and the app still matches on all four
MIR surfaces.

## Back to the original, 2026-10-18

The rows behind the edits above are fixed, so `mini_statsd.kara` is now exactly what `karac fix` makes of `mini_statsd.orig.kara`. It matches expected.out on all four MIR backends (karac at 4ea06e27b9).
