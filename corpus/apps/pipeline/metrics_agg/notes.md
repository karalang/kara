# metrics_agg

A metrics aggregator over Prometheus text-format scrapes. It does what a
small recording-rule engine does between two scrapes of each target.

The program writes two scrapes 60 s apart for each of two API targets and
a worker to disk, then reads them back and parses them. The parser handles
`# HELP` / `# TYPE`, label sets with the format's escapes (`\\`, `\"`,
`\n`), `+Inf` / `-Inf` / `NaN`, exponents and optional millisecond
timestamps. The worker's scrape is deliberately broken. Each bad line is
reported with its line number and skipped:
- a duplicate series;
- an unterminated label set;
- a missing value;
- a non-numeric value;
- a TYPE line after the family's samples;
- a `summary` family, which is kept as untyped;
- a label name that starts with a digit.

From the scrape pairs it computes:
- per-series counter rates, with reset detection (one target restarted);
- request rates summed by status class, and the error ratio;
- quantiles from summed histogram bucket rates, the same way PromQL's
  `histogram_quantile` does, including its edge cases: q < 0, q > 1, and
  no `+Inf` bucket;
- mean latency from `_sum` and `_count`;
- gauge max / min / mean over resident memory, and a `build_info` count.

It writes recording-rule results as `aggregated.prom` in canonical form,
then parses that file again and checks that the escaped label survives.

Oracle: `metrics_agg.py`. It is a port, and its parser was cross-checked
against `prometheus_client`'s text parser on the same scrapes.
`expected.out` is its output. Float sums run in the mirror's insertion
order, so the printed digits match exactly.

## Edits from metrics_agg.orig.kara to metrics_agg.kara

Author errors (the diagnostics were right):
- A label name was pushed into the series and then used in an error
  message. It is now pushed as `name.clone()`.

There were no compiler gaps. The draft passed every MIR surface once the
author error was fixed.

Written 2026-10-15 by the real-world apps thread; karac at fee58fc7e2.
