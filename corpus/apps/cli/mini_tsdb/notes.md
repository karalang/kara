# mini_tsdb

A time-series database in the shape of Prometheus: it reads an hour of
scrapes from three hosts in the text exposition format, stores each series
in chunks compressed the Gorilla way (as Prometheus, VictoriaMetrics and
InfluxDB do), writes the block to disk and reads it back, and answers a
small PromQL. Each scrape file is parsed in a task of its own.

```
> instant 1800 sum by (code) (rate(http_requests_total[5m]))
  {code="200"} 17.5991
  {code="500"} 0.2186
```

## Scrapes and ingestion

`data/h1.prom`, `h2.prom`, `h3.prom` and `zz_late.prom` hold one sample a
line, `name{label="value",...} value timestamp`, with the timestamp in
milliseconds; `#` starts a comment. Every host exports two request
counters, a CPU temperature and a queue depth, scraped every 15 s with a
few milliseconds of jitter. h2 misses three minutes of scrapes, and h3
restarts halfway, so its counters start again from zero.

A series is its labels, the metric name included. The files are appended
in name order, under Prometheus's rules: a sample older than its series'
newest is refused as out of order, one at the same time with the same
value is ignored, and one at the same time with a different value is
refused. A line that does not parse is refused with its reason. The late
file exercises each of these.

## Chunks

Each series is cut into chunks of up to 120 samples. A chunk is a 16-bit
sample count, then a bit stream, most significant bit first:

- The first sample's timestamp and value bits, 64 bits each.
- For each later sample, the timestamp's delta of delta (the change in the
  gap since the previous one, the first gap counting from zero): a `0` bit
  for none, then prefixes `10`, `110` and `1110` for values that fit in 14,
  17 and 20 bits (from -(2^(n-1) - 1) to 2^(n-1), in two's complement), and
  `1111` with all 64 bits otherwise.
- The value XOR'd with the previous one: a `0` bit when equal; otherwise
  `1`, then `0` and the meaningful bits when they fit inside the previous
  window of leading and trailing zeros, or `1`, five bits of leading zeros
  (at most 31), six bits of meaningful bits (64 written as 0) and the bits.

The block is two files under `data/run/`: `chunks`, every chunk end to end,
and `index`, a `series <labels> <chunks>` line per series followed by a
`chunk <offset> <bytes> <first ts> <last ts> <samples>` line per chunk:

```
series http_requests_total{code="200",instance="h1",job="api"} 2
chunk 0 475 1699999999999 1700001785002 120
chunk 475 500 1700001800003 1700003584999 120
```

The program reads both files back, decodes every chunk, checks it against
its index line and the series it came from, and answers the queries from
the decoded block.

## Queries

`data/queries.promql` holds `instant <t> <expr>` and
`range <start> <end> <step> <expr>`, times in seconds from the first scrape.

- A selector, `name{label="v",other!="w"}`, gives each matching series'
  newest sample in the five minutes up to the query time. A missing label
  matches as the empty string.
- A range vector, `selector[5m]` (s, m or h), is the samples in
  (t - range, t]. It is only an argument to a function.
- `rate` and `increase` follow Prometheus's extrapolatedRate: the rise
  across the window, a counter reset adding back the value before it, then
  stretched towards the window's edges when the first and last samples lie
  within 1.1 average intervals of them, but never back past the point
  where the counter would have been zero;
  `rate` divides by the range in seconds. They need two samples.
- `avg_over_time`, `max_over_time`, `min_over_time`, `count_over_time`.
- `sum`, `avg`, `max`, `min`, `count`, optionally `by (labels)`, over the
  series in label order.
- `* n` and `/ n` scale every value. A function or an arithmetic result
  drops the metric name.

Results print in label order with four decimals; a range query prints each
series' values at the steps where it had one. Parse errors name the
column. The last queries are an empty lookback, a metric that does not
exist, `rate` without a range, `by` without parentheses and a range that
ends before it starts.

## Running it

The program writes its own `data/` inputs first (embedded at the end of
`mini_tsdb.kara` by `gen_kara.py`):

```
karac run mini_tsdb.kara
karac build mini_tsdb.kara && ./mini_tsdb
```

Both print `expected.out`, and `data/run/chunks` and `data/run/index`
match the oracle's byte for byte.

## The oracle

`oracle.py` is the same database written plainly in Python from the rules
above. To regenerate:

```
python3 -s -P -B mk.py
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py
```

## Gaps found

Three `karac fix` gaps, all fixed in the compiler (the ids are in kara's
ledger, `docs/bugs-v2.jsonl`, under source `example:mini_tsdb`):

- `let samples = mut place;` was reported as "'mut' is a reserved keyword"
  with no fix; the fix now writes `mut ref place`.
- `parsed.into_iter()` on a bare `Vec` parameter whose elements have no
  `.clone()` got `parsed.clone().into_iter()`, which asked for
  `#[derive(Clone)]` next; the fix now takes the parameter `own`.
- `let samples = self.series[key].samples;`, only read afterwards, got a
  `.clone()` of the field, where `let s = v[i];` already got `ref`; a field
  of an element now borrows too, and under `self` a write to another field
  no longer rules the borrow out.
