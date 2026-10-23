# mini_textfsm

A port of [textfsm](https://pypi.org/project/textfsm/) 2.1.0 (Apache-2.0,
Google): the template-driven state machine that turns semi-structured text —
classically the CLI output of a router or switch — into a table of records. It
is the parsing core of the network-automation stack (Netmiko, NAPALM, ntc-
templates), and the same "describe the output with a little template, get rows
back" shape serves any line-oriented report a regex alternation would be too
blunt for.

## What it does

A **template** declares `Value` columns (a name, a capture regex, and options)
and then one or more `State`s of rules; each rule is `^<regex> -> <action>`.
`ParseText` walks the input a line at a time, tries the current state's rules in
order, assigns the named groups a matching line captures, and runs the rule's
record/line operators to build and emit rows.

`mk.py` writes `data/jobs.tsv`: a sequence of `(name, template, input)` jobs in a
block format that preserves the significant blank lines (a template's
Value/State separator, an input's record-terminating blank line). `oracle.py`
runs each job through the library; `mini_textfsm.kara` runs it through the port.
Both print each job under a `=== <name> ===` header as a `HEADER` row of column
names and one `ROW` per record (a `List` column renders `[a, b, c]`), or
`ERROR\t<message>` when the FSM raises:

```
=== routes ===
HEADER	Network	NextHop
ROW	10.0.0.0/24	[192.168.1.1, 192.168.1.2, 192.168.1.3]
```

The ten jobs exercise `Filldown` / `Required` / `Key` / `List` options, the line
operators `Next` / `Continue` / `Error`, the record operators `Record` / `Clear`
/ `Clearall`, multi-state transitions, the implicit end-of-file record, template
comments, and `${var}` substitution.

## How the port works

TextFSM's regexes are real regexes — named groups, `${var}` templating, the
`.match()` anchored scan — so the port is built on the **mini_re engine**
embedded at the top of this file (the same validated `re` port that backs
mini_mistune and mini_pygments; `compile` / `new_matcher` / `find`, with group
spans read by name out of the compiled program). On top of it the FSM is
transcribed from `parser.py`:

- **Values** become a vector of records (name, the `(?P<name>…)` template, the
  option flags, the current value, a `List` accumulator, the `Filldown` carry).
- **Rules** are parsed exactly as the library does: split the match from the
  `-> action` at the last ` ->`, substitute `${Name}` with that value's template
  (string.Template semantics, `$$`→`$`), compile the result, then parse the
  action's line/record operators and optional new state through the same
  three-tier fallback.
- **The run loop** matches each line's rules in order; a match assigns every
  named group back to its Value, runs the record operator (`Record` builds a row
  through each option's save hook — `Required` skips an empty record, `List`
  freezes its accumulator) and then the line operator (`Continue` stays on the
  line, `Error` raises with the library's exact message, `Next` takes the next
  line), and follows any state transition. An implicit end-of-file `Record`
  fires unless a `Null`-`EOF` state suppresses it.

Every rule is tried by index and every group is read by name — never an
iteration over a hash map — so the output is deterministic and `run == build`
holds. The whole thing is byte/codepoint level over the shared engine, so the
four backends agree by construction.

## Scope

- Options covered: `Required`, `Filldown`, `Key`, `List` (the plain
  string-list form). **Out of scope (documented):** `Fillup` (it rewrites
  already-emitted rows), and `List` values whose regex has nested `(?P<…>)`
  groups (the dict-list form) — the common string-list case is what the jobs
  use.
- Input and templates are ASCII, which is what the engine and the job set need.
- The `clitable` / `texttable` / `terminal` layers of the library (table
  formatting and CLI dispatch) are out of scope; this is the parser core.

The port was checked against the library on every backend over the job stream,
and separately a restricted-op reimplementation was compared job-for-job against
`textfsm.TextFSM` during development.

The vendored library package imports only the standard library; `vendor/` is
otherwise left untouched.

## Running

```bash
karac run mini_textfsm.kara               # interpret
karac build --mir mini_textfsm.kara       # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I oracle.py                           # regenerate expected.out
python3 -s -P -B gen_kara.py                   # regenerate the embedded jobs
```
