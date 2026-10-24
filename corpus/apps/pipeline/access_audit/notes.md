# pipeline/access_audit

Four web servers each keep an access log. The program generates the four
logs (8000 requests each, from a seeded generator), writes them to their
own files, reads them back, parses each one, and runs five reports over
the combined 32000 requests:

- requests per status code and the 5xx error rate;
- the six busiest routes, with numeric path segments folded to `{id}`;
- latency percentiles (nearest rank p50, p90, p99, and the maximum) per
  method;
- the number of distinct clients and the three that pulled the most bytes;
- the three busiest minutes, per server.

Every report sorts or keys its output (`SortedMap`, `sort_by`), so nothing
depends on hash order. The Python mirror `access_audit.py` uses the same
generator and wrote `expected.out`.

stmt-par: four groups of statements that do not depend on each other, in
`run()` (`access_audit.orig.kara` lines):
- L286-L289 four `fs.write` calls, one per log file, each with the
  `generate` call that builds its text;
- L295-L298 four `fs.read_to_string` calls, one per file;
- L303-L306 four `parse` calls, one per log. Each can fail with `?`, so a
  parallel run has to report the first failure in source order;
- L321-L325 five reports over the same `hits`, which each only read.

The reports and the parses are where the time goes, so these are the
groups that can show a speedup.

## Edits from access_audit.orig.kara to access_audit.kara

Author errors (the diagnostics were right, and `karac fix` applied the
machine-applicable ones):
- `split` is a lazy iterator: `[0]` became `.nth(0).unwrap()`, `.iter()`
  before `.map` went, and the pieces that are indexed became
  `Vec[Str]` through `.collect()`.
- `line.substring(..).split(" ").collect()` keeps borrows of a temporary
  past its statement; slicing `line[a..b]` borrows `line` itself instead.
- `for h in log { hits.push(h) }` moves out of a borrowed element; the
  loops became `.into_iter()`. `karac fix` had also added
  `#[derive(Clone)]` to `Hit` for the `.clone()` alternative the
  diagnostic offers; it is not needed once the loop moves, so it went.

Compiler gaps: none found.

## Running

```bash
karac run access_audit.kara
karac build access_audit.kara && ./access_audit
python3 -I -B access_audit.py > expected.out
```
