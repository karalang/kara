# mini_dask

A task graph in the shape of dask's. Keys name values or calls, and a call's
arguments are numbers or other keys. `compute` culls the graph to what the
requested keys need, fuses linear chains of calls into one run, orders the
runs, and hands them to a pool of four worker tasks as soon as their inputs
are ready, so the runs really do overlap. The workers take runs from one
channel, which they share behind a mutex, count what they do in atomics and
a locked table, and send each result back on a second channel. A failed run
fails everything downstream of it, and everything else still runs.
`persist` computes keys and keeps their results in the graph.

The program writes its own input (`data/script.txt`), plays it, and prints
a report. `oracle.py` is the reference: it runs every task in order on one
thread, and the program's output must not depend on which worker ran what
or in which order the runs finished. `mk.py` writes the data, and
`gen_kara.py` embeds it in the program.

## Common rules

- A **key** is 1 to 32 of `a-z 0-9 _`, starting with a letter.
- A **number** is an optional `-` and 1 to 9 digits.
- `#` starts a comment and words are separated by spaces. Blank lines are
  skipped. Every other line is echoed as `> ` and its words joined by one
  space. A problem with it is `  error: script.txt:<line>: <message>`, and
  the line does nothing.
- A value is an int (64 bits) or a list of ints. A list of at most 8 items
  prints as `[a, b, c]`; a longer one as its first three items, `...`, its
  last two and its length: `[0, 1, 2, ..., 98, 99] (100 items)`.

## Functions

The arguments are counted when the task is made (`bad arguments for <f>:
<usage>`); their kinds are checked, and the result worked out, when a worker
runs it.

| function | usage | result |
|---|---|---|
| inc | `inc <x>` | `x + 1` |
| neg | `neg <x>` | `-x` |
| add | `add <x> <y>...` | the sum, left to right |
| mul | `mul <x> <y>` | the product |
| div | `div <x> <y>` | the quotient rounded down (`-7 / 2` is `-4`); `ZeroDivisionError: integer division or modulo by zero` when `y` is 0 |
| range | `range <n>` | the list `0` to `n - 1` (empty for `n` below 1); above 100000, `ValueError: range(<n>) is too long` |
| sum | `sum <list>` | the sum of its items, left to right |
| len | `len <list>` | its length |
| concat | `concat <list>...` | the lists joined |
| primes | `primes <n>` | how many primes are below `n`; above 2000000, `ValueError: primes below <n> is too many` |
| boom | `boom` | fails with `ZeroDivisionError: division by zero` |

A function given a list where it takes an int fails with `TypeError: <f>
expects an int, got a list`, and the other way round with `TypeError: <f>
expects a list, got an int`; the arguments are checked in order. A result,
or a partial sum on the way to one, outside 64 bits fails with
`OverflowError: <f> result does not fit in 64 bits`.

## Commands

- `value <key> <number>` (`usage: value <key> <number>`; `bad key '<k>'`;
  `key <k> already exists`; `bad number '<n>'`) adds a key holding a value.
- `task <key> <func> [args]` (`usage: task <key> <func> [args]` for fewer
  than two words after `task`) adds a key holding a call. It is checked in
  this order: `bad key '<k>'`, `key <k> already exists`, `unknown function
  '<f>'`, `bad argument '<a>'` for each argument that is neither a number
  nor a key, then the count. An argument may name a key that does not exist
  yet.
- `drop <key>` (`usage: drop <key>`; `no key '<k>'`) removes a key and
  prints `dropped <k>`.
- `compute [--nofuse] <key>...` (`usage: compute [--nofuse] <key>...` with
  no keys; `unknown option '<o>'` for any other word starting with `--`;
  `no key '<k>'`; `duplicate key <k>`) works the keys out:
  1. **Cull.** It walks the graph depth first from each requested key in
     turn, a call's key arguments in order, and keeps every key it reaches.
     A key argument that is not in the graph is `missing dependency <d> of
     <k>`, and one that is still being walked is `cycle <k1> -> <k2> -> ...
     -> <k1>` (from that key, along the walk, back to it); either ends the
     command. The walk lists each key after its arguments, and that is the
     **order**. It prints `cull: <kept> of <all> keys`.
  2. **Fuse.** Unless `--nofuse` is given, a call is folded into the one
     call that uses it when it is not requested, that call is the only kept
     key that uses it, and it is the only key that call uses. Folding
     repeats, so a chain of calls becomes one run, named for its last key.
     Each chain prints `fuse: <k1> + <k2> + ...`, in the order of their last
     keys.
  3. **Order.** The runs are the kept calls that were not folded away,
     each placed at its last key's position in the order: `order: <keys>`,
     or `order: none`.
  4. **Run.** A run whose inputs have all been worked out goes to the
     workers; while all four are busy, the waiting runs go by order. A run
     whose input failed does not run: it fails with the error of its first
     failed input, in argument order. Values need no run.
  5. **Report.** Each requested key, in the order given: `<key> = <value>`,
     `<key> ! <error>` when its own run failed, or `<key> ! <error> (from
     <k>)` when the failure came from `k` upstream. Then `runs <n>, failed
     <n>, skipped <n>`.
- `persist [--nofuse] <key>...` (`usage: persist [--nofuse] <key>...`)
  does what `compute` does, then makes each requested call that worked out
  a value holding its result: `persisted <n>`.

Any other command is `unknown command '<c>'`.

### Inspection

`@ graph`, `@ deps <key>` and `@ dependents <key>` (`missing command` for a
bare `@`; `unknown command '@ <c>'`; `usage: @ graph`, `usage: @ deps <key>`
or `usage: @ dependents <key>` for the wrong number of words; `no key
'<k>'`).

- `graph` prints each key, in the order they were added: `<key>: <value>`
  or `<key>: <func>(<args>)`, or `empty`.
- `deps` prints `<key> needs <keys>` or `<key> needs nothing`: every key it
  reaches through call arguments, sorted, a missing one written
  `<k> (missing)`.
- `dependents` prints `<key> is used by <keys>` or `<key> is used by
  nothing`: the calls with it among their arguments, in the order they were
  added.

## Report

`== script (4 workers)` and the commands above, then `== end` and `<n>
keys`, then what the workers counted: `4 workers: <n> runs, <n> tasks, <n>
failed` (a fused run applies several tasks) and `by function: <f> <n>, ...`
in name order, or `by function: none`.

## Compiler gaps found

- B-2026-11-07-30, fixed in kara ec6e5415ba: `for a in args`, with `args`
  an owned `Vec` not used after the loop, refuses moving a payload out of
  `a`, and `karac fix` copied it with `.clone()` where the message itself
  names `args.into_iter()`. The fix now writes `.into_iter()`.
- B-2026-11-07-31, fixed in kara ec6e5415ba: `let mut chain = [k];` with
  `k` from `order.iter()` is a `Vec[ref i64]`, refused with no fix where a
  `Vec[i64]` is wanted. The fix now annotates the `let`.

With both, `karac fix` turns the program's first draft into this one.
