# mini_jobqueue

A durable background job queue in the shape of Sidekiq and Oban: named
queues with concurrency limits, worker kinds with run times, timeouts and
failures, priorities and scheduled jobs, retries with Sidekiq's backoff,
unique jobs, cancellation, and a write-ahead log on disk that survives a
crash (even one that tears the last record) and is compacted by pruning.
The two configuration files and the event script are each parsed in a
task of its own.

```
> 00:00:10 enqueue payments charge_card order=1001 amount=4999
enqueued #5 charge_card on payments
00:00:10 start #5 charge_card on payments (attempt 1)
...
00:00:13 fail #5 charge_card attempt 1: card declined by issuer; retry at 00:00:32
```

## Configuration

`#` starts a comment anywhere on a line; blank lines are skipped. Tokens
are separated by whitespace. The first error on a line is reported as
`error: FILE:LINE: message` and drops the line.

`data/queues.conf` holds `queue NAME limit=N [paused]`. A name is one or
more of `a-z`, `0-9` and `_`. `limit` is 1 to 100 and required. Errors:
`unknown directive 'X'`, `queue needs a name`, `bad name 'X'`, `bad limit
'X'`, `unknown field 'X'`, `queue 'NAME' needs limit`, `duplicate queue
'NAME'`.

`data/workers.conf` holds `worker KIND duration=D [timeout=D]
[attempts=N] [fail=A,B,...] [cancel_on=A] [error=TEXT]`:

- `duration`, required: how long one attempt runs.
- `timeout`: an attempt whose duration is longer fails with `timeout after
  Ts` once `timeout` has passed.
- `attempts`: the default `max_attempts` of the kind's jobs, 1 to 25
  (default 5).
- `fail`: the attempts (each 1 to 25, comma-separated) that fail with
  the error, or `none`.
- `cancel_on`: the attempt (1 to 25) on which the worker cancels its own
  job (`cancelled by worker`).
- `error`: the failure message, the rest of the line (default `failed`).
  It must be the last field and not empty.

A duration is digits (at most seven) with an optional `s`, `m` or `h`
unit; `duration` and `timeout` must be at least one second. Errors:
`unknown directive 'X'`, `worker needs a kind`, `bad name 'X'`, `bad
duration 'X'`, `bad attempts 'X'`, `bad attempt list 'X'`, `bad attempt
'X'`, `bad error`, `unknown field 'X'`, `worker 'KIND' needs duration`,
`duplicate worker 'KIND'`.

## Events

`data/events.txt` holds `HH:MM:SS COMMAND ARGS...`, in time order. Each
line is echoed as `> LINE` (comment and outer space removed). Errors are
`error: events.txt:LINE: message`: `bad time 'X'` (two digits each,
minutes and seconds below 60), `time goes back`, `missing command`,
`unknown command 'X'`, `usage: ...` for a wrong argument count.

Before a command runs, the engine runs every internal step up to and
including its time, printing each action as `HH:MM:SS action`. After the
command, one more step runs at its time. Hours may exceed 23.

- `enqueue QUEUE KIND [priority=P] [in=D] [max_attempts=N] [unique=D]
  [fail=A,...] [KEY=VALUE ...]`: insert a job. `priority` is 0 (first)
  to 9, default 0. `in` delays it (`scheduled #ID KIND on QUEUE at T`);
  otherwise it is `enqueued #ID KIND on QUEUE`. `max_attempts` overrides
  the kind's. `fail` (a list as in `workers.conf`) overrides the kind's
  failing attempts for this job.
  Other `KEY=VALUE` pairs are the job's arguments; a key is `a-z` and
  `_`, a value is not empty. `unique=D`: when a job with the same queue,
  kind and arguments was inserted at most D seconds ago (D at least one)
  and is not cancelled or discarded, nothing is inserted and it prints
  `duplicate of #ID` (the lowest such id). Ids count from 1; a
  duplicate takes none. Errors: `usage: enqueue QUEUE KIND [OPTIONS]`,
  `unknown queue 'X'`, `unknown kind 'X'`, `bad priority 'X'`, `bad
  duration 'X'`, `bad max_attempts 'X'`, `bad attempt list 'X'`, `bad
  argument 'X'`, `duplicate argument 'KEY'`.
- `pause QUEUE` / `resume QUEUE`: `paused Q` / `resumed Q`; errors
  `unknown queue 'X'`, `queue 'Q' is already paused`, `queue 'Q' is not
  paused`. A paused queue starts nothing; running jobs finish.
- `scale QUEUE N`: `scaled Q OLD -> N` (N 1 to 100, `bad limit 'X'`).
  Running jobs over the new limit finish.
- `cancel ID`: `cancelled #ID KIND (was STATE)`. A running job stops at
  once and frees its slot. Its last error becomes `cancelled by user`.
  Errors `bad job id 'X'`, `no job ID`, `job ID
  is STATE` for a completed, discarded or cancelled job.
- `retry ID`: a discarded or cancelled job becomes available now. When it
  has no attempts left, `max_attempts` becomes its attempt count plus
  one. Prints `retrying #ID KIND (attempt A of M)`, A being the next
  attempt. Errors as for `cancel`, and `job ID is STATE, not discarded or
  cancelled`.
- `crash` / `crash torn`: the process dies. Memory is lost; the log on
  disk is what remains. `crash torn` dies in the middle of writing the
  last record: the file is cut short by half the last line's length
  (newline included), rounded up. Prints `crashed (log N bytes)` or
  `crashed mid-write (log cut from N to M bytes)`. While down, the engine
  does nothing and every command but `recover` is `queue is down`.
- `recover`: read the log back and replay it (a last line with no newline
  is a torn record and is dropped), then rescue every job that was
  running: it becomes available again, or is discarded with `rescued
  after crash` when it has no attempts left. Prints `recovered N records`
  plus `, dropped a torn record of K bytes`, then a `rescued #ID KIND
  (attempt A of M)` or `discarded #ID KIND (rescued after crash)` line
  per job in id order. Error `queue is not down`.
- `prune AGE`: delete completed, discarded and cancelled jobs that
  finished at least AGE ago, and rewrite the log without their records:
  `pruned N jobs, log B -> B2 bytes`. The rewritten log starts with `ids
  NEXT`, so ids of pruned jobs are not handed out again after a crash.
- `stats`: one line per queue, in name order: `Q limit=N[ paused]:
  executing a, available b, scheduled c, retryable d, completed e,
  discarded f, cancelled g`.
- `jobs STATE` (`all` or a state name): one line per job in id order,
  `#ID STATE KIND QUEUE A/M`, then ` next=T` for a scheduled or
  retryable job, the arguments as `KEY=VALUE` in key order, and the last
  error as ` (ERROR)`. `no jobs` when none. Error `bad state 'X'`.
- `drain`: run the engine until nothing is left to happen, then
  `drained at T`; the clock is then T.

## The engine

A job is `available`, `scheduled`, `executing`, `retryable`, `completed`,
`discarded` or `cancelled`. A step at time T:

1. finishes the running jobs that end at T, in id order;
2. makes every scheduled and retryable job whose time has come
   available (on paused queues too);
3. for each queue in name order, unless paused, starts available jobs
   while fewer than `limit` run, lowest `(priority, time it became
   due, id)` first.

`start #ID KIND on Q (attempt A)`. An attempt ends after the kind's
duration, or after its timeout when the duration is longer, and then, in
this order: a timeout fails it, `cancel_on` cancels it (`cancel #ID KIND
attempt A: cancelled by worker`), a listed attempt fails it, and
otherwise `done #ID KIND (attempt A)`. A failure prints `fail #ID KIND
attempt A: ERROR; ` and then `retry at T` or, at `max_attempts`,
`discarded`. The retry waits `c^4 + 15 + (h % 10) * (c + 1)` seconds
with `c = A - 1` and `h` the 32-bit FNV-1a hash of `ID-A`.

## The log

`data/out/queue.log` is rewritten whole after every event line. Records:

```
insert ID QUEUE KIND PRIORITY MAX DUE AT FAIL [KEY=VALUE ...]
start ID AT END
complete ID AT
retry ID AT DUE ERROR
discard ID AT ERROR
cancel ID AT REASON
requeue ID AT MAX
rescue ID AT
pause QUEUE | resume QUEUE | scale QUEUE N
ids NEXT
```

`FAIL` is the job's failing attempts as `A,B`, `none`, or `-` for the
kind's. Times are seconds. Every change to a job or queue is a record,
and memory changes only by applying one, except step 2.

## Output

`== load` gives `queues.conf: N queues`, `workers.conf: N workers` (each
followed by its errors) and `events.txt: N events`; `== events` the
script; `== end` the `stats` lines (or `queue is down`); and `== log` the
log read back from disk: `data/out/queue.log: N records, B bytes`, the
count per record kind as `KIND N` in kind order, and `replay: N jobs,
matches memory` when replaying it from the configuration (step 2 applied
at the final time) gives the same queues, jobs and next id as memory
(`replay: skipped, queue is down` while down).

## Compiler gaps found

- B-2026-11-05-41 (fixed; first filed as -24, renumbered at landing): the first draft promoted due jobs with
  `for j in jobs.values_mut()`, and karac answered a bare
  `no method 'values_mut' on type 'SortedMap'`, so the program collected
  the due ids and called `get_mut` on each. The library spec now gives
  `Map` and `SortedMap` `values_mut`, `into_keys`, `into_values` and
  `retain`, `SortedMap` `first`, `last`, `pop_first` and `pop_last`, and
  `SortedSet` `pop_first`, `pop_last` and `retain`; Rust names Kāra spells
  differently (`first_key_value`, a sorted set's `first`) get a diagnostic
  naming Kāra's method. `promote` is back to the `values_mut` loop.
