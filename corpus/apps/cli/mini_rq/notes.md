# mini_rq

A job queue in the shape of rq. Jobs wait on named queues, which are listed
by priority, and a pool of three worker tasks runs them. The workers take
runs from one channel, which they share behind a mutex. They count what
they do in atomics and in a locked table, and send each outcome back on a
second channel. The queue itself stays on the main task. It follows job
dependencies, retries a failed job at once or after a backoff, times out a
slow job, schedules a job for later, cancels and requeues jobs, and expires
old results on a fake clock. Unlike mini_jobqueue, which simulates its
workers, the jobs here really run on worker tasks.

The program writes its own input (`data/script.txt`), plays it, and prints
a report. `oracle.py` is the reference: it runs every job in order on one
thread, and the program's output must not depend on which worker ran what.
`mk.py` writes the data, and `gen_kara.py` embeds it in the program.

## Common rules

- A **name** (queue, job id) is 1 to 32 of `a-z 0-9 _`, starting with a
  letter.
- A **number** is an optional `-` and 1 to 9 digits. A **count** from `lo`
  to `hi` is a number without a sign in that range.
- `#` starts a comment and words are separated by spaces. Blank lines are
  skipped. Every other line is echoed as `> ` and its words joined by one
  space. A problem with it is `  error: script.txt:<line>: <message>`, and
  the line does nothing.

## Functions

A job calls one of these with its arguments. The arguments are checked when
the job is made (`bad arguments for <f>: <usage>`); the outcome is decided
when a worker runs it.

| function | usage | outcome |
|---|---|---|
| add | `add <a> <b>`, two numbers | their sum |
| fib | `fib <n>`, a count | the `n`th Fibonacci number; above 90, `ValueError: fib(<n>) does not fit in 64 bits` |
| words | `words <word>...` | `<k> words` |
| echo | `echo <word>...` | the words joined by spaces |
| flaky | `flaky <n>`, a count | fails with `FlakyError: attempt <a> of <n>` until attempt `n`, then `ok on attempt <a>` |
| slow | `slow <seconds>`, a count | `slept <s>s`, or `JobTimeoutException: Task exceeded maximum timeout value (<timeout> seconds)` when `s` is above the job's timeout |
| primes | `primes <n>`, a count | `<k> primes below <n>`; above 1000000, `ValueError: primes below <n> is too many` |
| boom | `boom` | fails with `ZeroDivisionError: division by zero` |

## Commands

- `queue <name>` adds a queue below the ones before it (`usage: queue
  <name>`; `bad queue name '<n>'`; `queue <q> already exists`).
- `enqueue <queue> <id> <func> [args] [options]` makes a job (`usage:
  enqueue <queue> <id> <func> [args] [options]` for fewer than three words
  after `enqueue`). It is checked in this order: `no queue '<q>'`, `bad job
  id '<id>'`, `job <id> already exists`, `unknown function '<f>'`, then
  each word after the function: `at_front` is a flag, a word with `=` is an
  option (`unknown option '<k>'`; `duplicate option <k>`), and any other
  word is an argument. Then the arguments, then the options:
  - `retry=<count 0 to 9>` (`bad retry '<v>'`): how many times a failure is
    retried.
  - `backoff=<count 1 to 9999>,...` (`bad backoff '<v>'`; `backoff needs
    retry`): the delay before each retry, the last one repeating. Without
    it a retry is queued at once.
  - `timeout=<count 1 to 9999>` (`bad timeout '<v>'`), 180 by default.
  - `ttl=<count 1 to 9999>` (`bad ttl '<v>'`): how long a result is kept,
    500 by default.
  - `depends_on=<id>,...`: jobs that must finish first (`no job '<d>'`;
    `duplicate dependency <d>`).

  A job whose dependencies have not all finished prints `<id>: deferred on
  <q> until <ids> finish`. Otherwise it is queued at the back of its queue,
  or the front with `at_front`: `<id>: queued on <q>[ at the front]`.
- `enqueue_in <seconds> <queue> <id> <func> [args] [options]` makes a job
  that is due `seconds` (a count from 1 to 99999, `bad delay '<s>'`) from
  now: `<id>: scheduled on <q> for <time>`. It takes no `depends_on`
  (`depends_on is not allowed with enqueue_in`).
- `cancel <id>` (`usage: cancel <id>`) takes a queued, scheduled or deferred job out (`no job
  '<id>'`; `job <id> is <status>`) and prints `canceled <id>`.
- `requeue <id>` (`usage: requeue <id>`) puts a failed job at the back of its queue for another
  attempt (`job <id> is not failed`) and prints `requeued <id> on <q>`.
- `tick <seconds>` (`usage: tick <seconds>`; a count from 1 to 99999, `bad seconds '<s>'`) moves the
  clock on and prints `clock <time>`. Each scheduled job now due is queued
  at the back of its queue, earliest first and then in the order they were
  scheduled (`<id>: due, queued on <q>`). Each finished job whose result
  has expired, in the order the jobs were made, drops its result
  (`<id>: result expired`).
- `work` (`usage: work`) runs rounds until every queue is empty. A round takes every queued
  job, the first queue's first, and prints `round <k>: <ids>`. The workers
  run them all, and then each outcome is settled in that order:
  - a result: `<id>: finished -> <result>`, kept for its `ttl`;
  - a failure with retries left: `<id>: failed (<error>), retry <n> of
    <retry> now` (queued at the back of its queue) or `in <delay>s`
    (scheduled);
  - any other failure: `<id>: failed (<error>)`.

  Then each deferred job whose dependencies have all finished (a result
  that has expired still counts as finished) is queued, in the order the
  jobs were made: `<id>: dependencies met, queued on <q>`. The command ends
  with `idle after <k> round(s)`, or `nothing to do` when no job was
  queued.

Any other command is `unknown command '<c>'`. A job's attempt starts at 1
and goes up with each retry and requeue.

### Inspection

`@ status`, `@ registries` and `@ job <id>` (`missing command` for a bare
`@`; `unknown command '@ <c>'`; `usage: @ status`, `usage: @ registries`
or `usage: @ job <id>` for the wrong number of words; `no job '<id>'`).

- `status` prints `clock <time>`, then each queue's ids in order, or
  `empty`.
- `registries` prints the deferred, scheduled, finished, failed, canceled
  and expired jobs, each list sorted by id, or `none`. A scheduled job is
  shown as `<id>@<due>`.
- `job` prints `<id>: <status> on <q>, <func>(<args>), attempt <a>`, then
  `, <left> of <retry> retries left` for a job with retries, then
  `, result <r>, expires at <t>` (finished), `, error <e>` (failed),
  `, due at <t>` (scheduled) or `, waiting on <ids>` (deferred).

## Report

`== script (3 workers)` and the commands above, then `== end` with the
clock and the registries, then what the workers counted:
`3 workers: <n> runs, <n> failed` and `by function: <f> <n>, ...` in name
order.

## Compiler gaps found

- B-2026-11-07-14, fixed in kara before this app landed: `karac check`
  refused moving a binding made by a `let .. else` inside a loop
  (`let Some(done) = rx.recv() else { .. }; out[done.slot] =
  Some(done.outcome);`) as moved on the next iteration, while every backend
  ran it.
- B-2026-11-07-15, open: for `let job = self.jobs[j];` whose fields are
  then written, `karac fix` derives `Clone` and adds `mut`, steering
  toward a copy that would drop the writes, where `mut ref self.jobs[j]`
  is the repair. The program writes `mut ref` by hand.
