# mini_prometheus_client

The instrumentation side of Prometheus, in the shape of the Python client
`prometheus_client`: a registry of counters, gauges, summaries, histograms,
infos and enums, each with optional labels, exposed in the Prometheus text
format and in OpenMetrics, and written to a `.prom` file the way the node
exporter's textfile collector reads it. One registry serves the whole
program. The `parallel` command updates it from four worker tasks at once,
so the registry sits behind a mutex the way the library's values sit
behind their locks.

The program writes its own script (`data/script.txt`), plays it and prints
a report. `oracle.py` plays the same script against the vendored
`prometheus_client` 0.26.0 (`vendor/`, Apache-2.0, with the BSD-2-Clause
`decorator.py` it bundles) and is the reference; `mk.py` writes the script
and `gen_kara.py` embeds it in the program.

The library runs with `_created` series off (`disable_created_metrics`)
and with the legacy name rules (`enable_legacy_validation`), so nothing
depends on the clock and no name needs the newer UTF-8 quoting.

## The script

- Each line is split into words at runs of spaces. Blank lines and lines
  whose first word starts with `#` are skipped. Every other line is echoed
  as `> ` followed by its words joined by one space, then runs.
- A problem is reported as `  error: script.txt:<line>: <message>`, and the
  line then changes nothing. The messages are the library's, except the
  ones this README gives in full.
- **Escapes.** In a label value, an info value and a help word, `\\` is a
  backslash, `\n` a newline, `\s` a space and `\,` a comma. A backslash
  before anything else, or at the end, is `bad escape in <word>`.
- **Numbers** are an optional `-`, digits, an optional `.` and digits, and
  an optional `e` or `E`, optional sign and digits; or `+Inf`, `-Inf` or
  `NaN`. Anything else is `bad number <word>`. A number is read as the
  nearest `f64`.
- **Targets.** A metric is named by its full name (below). `NAME` is the
  metric itself; `NAME@v1,v2,...` is its child with those label values,
  split at commas that are not escaped (`NAME@` is one empty value). A
  name the script has not defined is `no metric <name>`.

## Defining metrics

`<type> NAME [OPTION=VALUE ...] [: HELP ...]`, where `<type>` is `counter`,
`gauge`, `summary`, `histogram`, `info` or `enum`. The help is the words
after `:`, decoded and joined by one space, or empty. Without a name it is
`usage: <type> NAME [OPTION=VALUE ...] [: HELP]`.

| option | types | value |
|---|---|---|
| `labels` | all | label names, split at commas |
| `unit` | all | a unit |
| `ns`, `sub` | all | a namespace and a subsystem |
| `buckets` | histogram | numbers, split at commas (default `.005, .01, .025, .05, .075, .1, .25, .5, .75, 1, 2.5, 5, 7.5, 10`) |
| `states` | enum | state names, split at commas; none if the option is absent |

A word before `:` without `=` is `bad option <word>`; an option the type
does not take is `<type> has no option <key>`; an option given twice is
`repeated option <key>`. Then, in the library's order:

1. An enum with no states is `No states provided for Enum metric: <name>`;
   an enum whose name is one of its labels is
   `Overlapping labels for Enum metric: <name>`. A histogram's bucket that
   is not a number is `bad number <word>`; buckets not in ascending order
   (equal neighbours are allowed) are `Buckets not in sorted order`;
   `+Inf` is appended unless it is last, and fewer than two buckets then
   is `Must have at least two buckets`.
2. The **full name** is `ns_` + `sub_` + NAME, without a counter's final
   `_total`, with `_<unit>` appended unless it already ends that way. An
   info or enum with a unit is
   `Metric name is of a type that cannot have a unit: <full name>`.
3. Each label name must match `[a-zA-Z_][a-zA-Z0-9_]*`
   (`Invalid label metric name: <l>`), must not start with `__`
   (`Reserved label metric name: <l>`), and must not be `le` in a
   histogram or `quantile` in a summary
   (`Reserved label methe fric name: <l>`, the library's own typo).
4. The full name must match `[a-zA-Z_:][a-zA-Z0-9_:]*`
   (`invalid metric name <full name>`).
5. The registry refuses a metric whose series names meet a name already
   taken. A metric takes its full name and, by type, `_total` and
   `_created` (counter); `_sum`, `_count` and `_created` (summary);
   `_bucket`, `_sum`, `_count` and `_created` (histogram); `_info` (info).
   A set target info takes `target_info`. The message is
   `Duplicated timeseries in CollectorRegistry: {'a', 'b'}` with the
   clashing names sorted (the library prints a set, whose order is not
   fixed).

A defined metric prints `  defined <type> <full name>`.

## Updating metrics

Each command checks, in order: its word count (`usage: ...`), its target,
that the metric's type has the command (`<type> <name> has no <command>`),
its numbers, then the label values and the update.

| command | types | effect |
|---|---|---|
| `inc T [AMOUNT]` | counter, gauge | adds AMOUNT (default 1); a counter refuses a negative amount: `Counters can only be incremented by non-negative amounts.` |
| `dec T [AMOUNT]` | gauge | subtracts AMOUNT (default 1) |
| `set T VALUE` | gauge | sets the value |
| `set T K=V ...` | info | replaces the info's pairs (a later repeat of a key keeps the first one's place); a word without `=` is `bad pair <word>` |
| `observe T VALUE` | histogram, summary | adds VALUE to the sum, and one to the count (summary) or to the first bucket whose bound is at least VALUE (histogram; a NaN fits none) |
| `state T STATE` | enum | sets the state; an unknown one is `'<state>' is not in list` |
| `reset T` | counter | sets it back to 0 |

- Label values: a metric with labels needs `@` values
  (`<library type> metric is missing label values`, where an enum's
  library type is `stateset`), and the count must match
  (`Incorrect label count`); `@` on a metric without labels is
  `No label names were set when constructing <library type>:<name>`. The
  first use of a set of values creates the child, which starts at zero
  (an enum at its first state, an info empty) even if the update then
  fails.
- An info's keys must not be its label names:
  `Overlapping labels for Info metric, metric: ('a',) child: {'k': 'v', ...}`,
  Python-shaped, pairs in the order given. The library does not check the
  keys otherwise.
- The port's own messages: `set` on an info and `reset` on a counter that
  needs label values are `info metric is missing label values` and
  `counter metric is missing label values` (the library raises
  AttributeError there).

## Removing

- `remove T` drops the child with those values (nothing if there is none);
  without values or with the wrong count it is
  `Incorrect label count (expected <n>, got ('v', ...))`, Python-shaped.
- `clear NAME` drops every child; `unregister NAME` takes the metric out of
  the registry and the script, freeing its names.
- `target K=V ...` sets the registry's target info; `target` alone clears
  it. Setting it when it is not set and a metric owns `target_info` is
  `CollectorRegistry already contains a target_info metric`.

## Exposing

`expose` prints the text format, `expose openmetrics` prints OpenMetrics,
each line indented by two spaces. The target info comes first, then the
metrics in the order they were registered, each child in the order it was
created.

- **Values** print as `+Inf`, `-Inf` and `NaN`, and otherwise as Python's
  `repr` of the float (the shortest round-trip digits, `1e-07`, `-0.0`),
  except that a positive value with more than six digits before the point
  prints Go-style: `1234567.0` prints `1.234567e+06`.
- **Samples.** A counter `c` has `c_total`; a gauge `g` has `g`; a summary
  `s` has `s_count` and `s_sum`; a histogram `h` has `h_bucket` for each
  bound with label `le` (the bound printed as a value) and the count of
  observations at or below it, then `h_count` and, when the first bound is
  not negative, `h_sum`; an info `i` has `i_info` with value 1 and its pairs
  as labels; an enum `e` has one `e` sample per state with label `e` set to
  the state and value 1 for the current state, else 0.
- **Labels** are the child's label values plus the sample's own, sorted by
  name, written `name="value"` and joined by `,` inside `{}`. A label name
  that does not match the label rule (with a colon, a dash, a leading digit,
  a leading `__`) is written with each character outside `a-zA-Z_` (and a
  digit in first place) replaced by `_`. In a value, `\` is `\\`, a newline
  is `\n` and `"` is `\"`.
- **Text format.** `# HELP <name> <help>` (help with `\` as `\\` and a
  newline as `\n`) and `# TYPE <name> <type>` before each metric's samples.
  A counter's name there is `c_total`; an info's is `i_info`, typed
  `gauge`; an enum is typed `gauge`.
- **OpenMetrics.** The names are the full names, the types are the
  library's (`stateset` for an enum), the help also escapes `"`, a metric
  with a unit adds `# UNIT <name> <unit>`, and the output ends with
  `# EOF`. A sample's name has every `:` replaced by `_` (the HELP and TYPE
  names keep it).
- `write FILE` writes the text format to `data/out/FILE` through a
  temporary file and a rename, as `write_to_textfile` does. FILE is
  `[a-z0-9_]+\.prom` (else `bad file name <word>`). The file differs from
  `expose` in one way: a label name that breaks the label rule is written
  in double quotes, escaped like a value, instead of with underscores. It
  prints `  wrote out/FILE: <n> lines, <m> bytes`, then the file it read
  back.

## Many updates at once

`parallel N COMMAND ...` runs an update command (`inc`, `dec`, `set`,
`observe`, `state` or `reset`) N times (1 to 999999, else
`bad count <word>`), split over four worker tasks that share the registry.
Every run of one command has the same outcome, so the result does not
depend on which worker ran what: it prints
`  ran N times on 4 workers`, or the error with ` (N times)` appended.
Another command is `usage: parallel N COMMAND ...`.

`unknown command <word>` ends the list. The report ends with `== end` and
`  <n> metrics registered`.

## Scope

- Exemplars, native histograms, `_created` series, the `time()` and
  `count_exceptions()` helpers, multiprocess mode, push gateways, HTTP
  serving, the UTF-8 name rules and the `restricted_registry` view (which
  collects in an unspecified order) are out of scope.
- Label values are given positionally only, and the script's buckets are
  never NaN.
- The script's error cases keep quotes, backslashes and newlines out of
  the values the Python-shaped messages print.

## Running

```bash
python3 -s -P -B mk.py            # writes data/script.txt
python3 -I -B gen_kara.py         # embeds data/ in the program
python3 -I -B oracle.py > expected.out
karac run mini_prometheus_client.kara
karac build mini_prometheus_client.kara && ./mini_prometheus_client
```
