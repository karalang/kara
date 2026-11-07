# mini_mrjob

MapReduce jobs in the shape of mrjob's. A job is a list of steps, and each
step is a mapper, an optional combiner and a reducer. A run splits the
step's input into map tasks. Each map task maps its records, combines its
own pairs and partitions them by a hash of the key. Each reduce task then
sorts one partition by key and reduces each key's values, and the next
step reads what the last one wrote. The tasks of each stage go to a pool
of four worker tasks. The workers take tasks from one channel, which they
share behind a mutex, count what they do in atomics and a locked table,
and send each result back on a second channel.

The program writes its own inputs (`data/input/*.txt`) and script
(`data/script.txt`), plays the script and prints a report. `oracle.py` is
the reference: it runs every task in order on one thread, and the
program's output must not depend on which worker ran what or in which
order the tasks finished. `mk.py` writes the data, and `gen_kara.py`
embeds it in the program.

## Common rules

- A **name** is 1 to 32 of `a-z 0-9 _`, starting with a letter.
- A **number** is an optional `-` and 1 to 9 digits.
- `#` starts a comment, and words are separated by spaces. Blank lines are
  skipped. Every other line is echoed as `> ` followed by its words joined
  by one space. A problem with a line is reported as
  `  error: script.txt:<line>: <message>`, and the line then does nothing.

## Values and records

- A **value** is an int (64 bits), a text, or a list of values. Values are
  ordered with ints before texts before lists. Ints compare by value, texts
  by their UTF-8 bytes, and lists item by item, so a list that is a prefix
  of another comes first.
- A value's **JSON** is its digits, a text in double quotes with `"` and
  `\` escaped by a backslash, or `[` and the items' JSON joined by `,`
  and `]`, with no spaces.
- A **record** is a line of an input file (its path, its 1-based line
  number and its text) or a key and value pair that a step wrote.
- The **tokens** of a line are its runs of ASCII letters, lowercased.

## Mappers

| mapper | reads | writes |
|---|---|---|
| words | a line | `(token, 1)` for each token |
| bigrams | a line | `("<a> <b>", 1)` for each two tokens in a row |
| lengths | a line | `(length, 1)` for each token |
| index | a line | `(token, "<stem>:<line>")` for each token; the stem is the file's name without its directory and last extension |
| pack | a pair `(k, v)` | `("all", [v, k])` |
| total | a pair `(k, v)` | `("total", v)` |
| first_letter | a pair with a text key `(k, v)` | `(<k's first letter>, v)` |

A line mapper given a pair fails with `TypeError: <m> expects a line, got
a pair`, and a pair mapper given a line fails with `TypeError: <m> expects
a pair, got a line`. `first_letter` given a key that is not a text fails
with `TypeError: first_letter expects a text key, got an int` (or
`got a list`).

## Reducers

A reducer gets a key and its values, in the order the map tasks wrote them
(task by task), and writes one value for the key.

| reducer | writes |
|---|---|
| sum | the sum of the values, which must be ints |
| max | the largest value, which must be ints |
| count | how many values there are |
| collect | the distinct values, sorted, as a list |
| top3 | the three largest values, largest first, as a list |

`sum` and `max` fail with `TypeError: <r> expects ints, got text` (or
`got a list`) at the first value that is not an int. A sum outside 64
bits fails with `OverflowError: sum does not fit in 64 bits`. Only `sum`
and `max` can be combiners.

## Commands

- `input <name> <path>...` (`usage: input <name> <path>...`; `bad name
  '<n>'`; `input <n> already exists`; `cannot read <path>`) reads each
  file under `data/` in turn and keeps its lines as records. A file's
  lines are its text split at `\n`, without the empty piece after a final
  newline. It prints `  <name>: <n> file(s), <k> lines`.
- `job <name> <step>...` (`usage: job <name> <step>...`; `bad name '<n>'`;
  `job <n> already exists`) defines a job. A step is `mapper|reducer` or
  `mapper|combiner|reducer`, and is checked in this order: its shape
  (`bad step '<s>': want mapper|reducer or mapper|combiner|reducer`), the
  mapper (`unknown mapper '<m>'`), the combiner (`bad combiner '<c>': only
  sum and max can combine`) and the reducer (`unknown reducer '<r>'`).
  The steps are checked left to right. It prints `  <name>: <n> step(s)`.
  Whether a mapper fits what it will read is only found out when the job
  runs.
- `run <job> <input> [maps=<n>] [reduces=<n>] [show=<n>]` (`usage: run
  <job> <input> [maps=<n>] [reduces=<n>] [show=<n>]` when there are fewer
  than three words; `no job '<j>'`; `no input '<i>'`; then each option in
  turn: `unknown option '<o>'`, `option <k> given twice`, and
  `bad value for <k>: '<v>'` unless the value is a number from 1 to 64,
  or from 0 to 1000 for `show`). `maps` defaults to 4, `reduces` to 3 and
  `show` to 10. It prints `  run <job> on <input> (maps <m>, reduces
  <r>)`, then, for each step in turn:
  - The step's records are split into `maps` contiguous chunks, the first
    `n % maps` of them one record longer than the rest. Each chunk is a
    map task: it maps its records in order, then, when the step has a
    combiner, groups its pairs by key and combines each key's values into
    one pair. Each pair goes to reduce task `fnv(json(key)) % reduces`,
    where `fnv` is 32-bit FNV-1a over the JSON's UTF-8 bytes.
  - Reduce task `p` takes the pairs sent to it, from map task 0 first,
    sorts them by key (keeping the order of each key's values), and writes
    one pair for each key. The step's output is reduce task 0's pairs,
    then reduce task 1's, and so on, and it is the next step's input.
  - If any map task fails, the step prints `  step <i> <step>: failed in
    map task <t>: <error>`, naming the lowest-numbered task that failed,
    and the run ends there. A reduce task failure is reported the same way
    as `reduce task <t>`. Every task of a stage runs, even when another
    one fails.
  - Otherwise it prints `  step <i> <step>: map <records in> in, <pairs
    mapped> out[, <pairs after combining> combined]; reduce [<pairs into
    each reduce task>] in, <pairs written> out`.

  When every step has run, it prints `  output: <n> record(s)` and the
  first `show` records as `    <json key><TAB><json value>`, then
  `    ... <n> more` if there are more.
- `jobs` (`usage: jobs`) lists the jobs by name, each as `  <name>: <steps
  joined by a space> (<n> run(s))`. A run counts once it has started.

At the end the program prints `== end`, `  <i> inputs, <j> jobs`, then
what the pool did: `  4 workers: <tasks> tasks, <records> records in,
<failed> failed` and `  by kind: map <n>, reduce <n>`. A map task's
records in are its records; a reduce task's are the pairs sent to it.

## Compiler gaps found

Written down as they were found; the rows are in kara's
`docs/bugs-v2.jsonl`.

- **B-2026-11-07-51** (false-positive, fixed): `match (self, other)` in a
  `ref self` method (`Value`'s `cmp`) was refused as a move out of a
  borrowed place, though `match (a, b)` over two `ref` parameters is
  accepted.
- **B-2026-11-07-52** (diagnostics, fixed): `impl Ord for Value` without an
  `impl PartialOrd` is refused, rightly, but `karac fix` had no edit; it
  now writes the `impl PartialOrd` whose `partial_cmp` is
  `Some(self.cmp(other))`.
- **B-2026-11-07-53** (diagnostics, fixed): `Dict[String, i64]` was "did
  you mean 'Div'?"; it now names `Map`, and `karac fix` renames it.
- **B-2026-11-07-54** (false-positive, open): `lines.last() == Some("")`
  over a `Vec[Str]` is refused ("cannot compare 'Option[ref Str]' and
  'Option[String]'"), and so is `opt == Some("")` with `opt` an
  `Option[Str]`, though `let a: Option[Str] = Some("")` is accepted. The
  program reads `lines.last().is_some_and(|l| l.is_empty())` meanwhile.
