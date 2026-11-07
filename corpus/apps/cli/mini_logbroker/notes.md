# mini_logbroker

A single-node log broker in the shape of Apache Kafka. Topics are split
into partitions, and each partition is an append-only log of segment files
on disk. Producers append keyed records, which Kafka's murmur2 partitioner
places. Consumer groups share partitions by the range assignor and commit
their offsets to an internal compacted topic. Retention deletes old
segments, and compaction keeps the latest record per key and drops
expired tombstones. After a crash, the broker recovers its logs from disk,
truncating a torn tail, and rebuilds its groups' committed offsets from
the offsets topic.

The program writes its own inputs (`data/topics.conf`, `data/script.txt`),
parses the two in tasks of their own, runs the script against logs under
`data/out/logs`, and prints a report. `oracle.py` is the reference.
`mk.py` writes the data, and `gen_kara.py` embeds it in the program.

## topics.conf

`#` starts a comment, and blank lines are skipped. Each line reads
`topic <name> key=value...`, and a bad line is reported as
`topics.conf:<line>: <message>` and skipped. The first problem found is
the one reported.

- A name is 1 to 40 of `a-z 0-9 _ . -`, else `bad topic name '<n>'`.
  A name starting with `__` gets `topic name '<n>' is reserved`, and a
  repeated name gets `topic <n> already defined`. A line whose first word
  is not `topic` gets `expected 'topic', found '<w>'`, and a bare `topic`
  gets `topic needs a name`.
- `partitions` takes 1 to 16 and is required (`partitions is required`).
- `segment_bytes` takes 64 to 1048576, default 1024.
- `retention` takes a duration `<n>[s|m|h]` (bare is seconds) greater than
  zero, or `none` (the default).
- `retention_bytes` takes 1 to 10^9 or `none` (the default).
- `cleanup` is `delete` (the default) or `compact`.
- `delete_retention` takes a duration, default 1h. It is how long a
  tombstone survives compaction.
- A word without `=` gets `expected key=value, found '<w>'`. An unknown key
  gets `unknown setting <k>`, a repeated one `duplicate setting <k>`, and a
  value out of range `bad value for <k>: '<v>'`. A compacted topic with a
  retention setting gets `a compacted topic has no retention`.

The broker adds `__consumer_offsets`: 3 partitions, `segment_bytes` 512,
compacted, `delete_retention` 0s.

## Records on disk

Partition `p` of topic `t` lives in `data/out/logs/<t>-<p>/`, one file per
segment named by its base offset in 20 digits (`00000000000000000003.log`).
A record is one line: `<offset> <time> k<key> v<value> <crc>`. A key or
value is `-` when absent, or else `:` and its bytes in lower-case hex. The
crc is the CRC-32 (zlib's) of everything before the space that precedes
it, in 8 lower-case hex digits. A record's size is its line's length,
counting the newline. A time is seconds since midnight.

## script.txt

`#` starts a comment. Each line is `HH:MM:SS <command> <args...>`. A bad
time is `bad time '<t>'`, a time before the previous command's is
`time goes back`, and a time alone is `missing command`. Every line is
echoed as `> <line>`, and a failed command prints
`error: script.txt:<line>: <message>` and changes nothing. While the
broker is down, every command but `restart` fails with `broker is down`.
A command with the wrong number of arguments prints its usage. `<t>:<p>`
names a partition, and an unknown topic, a partition out of range or the
internal topic are errors.

- `produce <t>[:<p>] <key|-> <value...|null>`: appends one record. The
  value is the remaining words joined by single spaces, and `null` makes a
  tombstone. Without `:<p>`, a keyed record goes to
  `(murmur2(key) & 0x7fffffff) % partitions` (Kafka's murmur2, seed
  `0x9747b28c`), and a keyless one round-robin per topic. A compacted
  topic needs a key. Prints `ok <t>-<p>@<offset>`.
- Appending rolls a new segment, based at the new offset, when the active
  segment has records and the record would take it past `segment_bytes`.
  The roll prints `roll <t>-<p> at <offset>`, for the internal topic too.
- `batch <t> <count> <keys> <prefix>`: `count` (1 to 1000) records, record
  `i` keyed `k<i mod keys>` (no key when `keys` is 0) with value
  `<prefix><i>`. Prints
  `ok <count> records: <t>-<p>=<first>..<last> ...`, by partition.
- `fetch <t>:<p> <offset> [max]`: up to `max` (default 10) records at or
  after `offset`. The offset must lie between the log start and the end,
  inclusive (`offset <o> out of range [<start>, <end>]`). Prints
  `<t>-<p> @<o>: <n> records, next <o'>`, then each record as
  `  <offset> <key|-> <value|null>`.
- `join <group> <member> <t,...> [reset=earliest|latest]`: the first join
  creates the group with its reset policy (default earliest) and writes
  `group/<g>` = policy to the offsets topic. A later join may not name a
  different policy (`group <g> resets to <policy>`), and a member joins
  once.
- `leave <group> <member>`.
- Every join and leave is a rebalance. The generation goes up, every
  member's fetch positions are dropped (eager rebalancing), and each topic's
  partitions are split among its subscribers sorted by name: `n / m` each,
  with the first `n % m` taking one more. Prints
  `group <g> generation <n>: <member>=[<t>-<p>,...] ...`, or `... : empty`.
- `poll <group> <member> [max]`: up to `max` (default 5) records over the
  member's partitions in order. A partition without a position starts at
  its committed offset, or at the log start (earliest) or end (latest). A
  position outside the log moves by the policy and prints
  `  reset <t>-<p> from <o> to <o'> (out of range)`.
- `commit <group> <member>`: writes `offset/<g>/<t>/<p>` = position for
  each partition the member has a position in. A group's records all go to
  offsets partition `murmur2(group) % 3`.
- `lag <group>`: the total, then for each committed partition
  `  <t>-<p> committed=<c> end=<e> lag=<e-c>`.
- `delete-group <group>`: only an empty group. Writes tombstones for its
  offsets and its group record.
- `retention`: in each delete-policy partition (topics by name), deletes
  the oldest segment while it is not the active one and either its newest
  record is at least `retention` old, or the log without it is still at
  least `retention_bytes`. The log start becomes the first remaining base.
- `compact`: in each compacted partition, every segment but the active one
  keeps only records that are the latest for their key in the whole log,
  minus tombstones at least `delete_retention` old. Segments left empty are
  deleted, except the first, so the log start does not move. Prints
  `compact <t>-<p>: kept <k> of <n> records, removed <r> tombstones,
  segments [<bases>]` for each partition that changed.
- `describe <t>`: the topic's settings, then
  `  <t>-<p> start=<s> end=<e> segments=[<base>:<bytes>, ...]`.
- `crash [<t>:<p>:<bytes>]`: the broker stops, losing everything in
  memory. With an argument, the last `bytes` of that partition's active
  segment are cut first (at most the segment's size).
- `restart`: reads every partition back. Each segment is read in order,
  and the first record that has no newline (`torn record`), does not parse
  (`bad record`), fails its crc (`bad checksum`), or does not go past the
  previous offset and the segment base (`offset out of order`) truncates
  the file there and deletes the later segments. Each truncation prints
  `truncated <t>-<p> at offset <o>: <reason>`. Groups come back from the
  offsets topic, latest value per key, with no members and generation 0.

At the end the report lists every topic as `describe` does, then the groups.
It then reads every log back from disk the way `restart` does and checks it
against memory, unless the broker is down.

## Compiler gaps found

- B-2026-11-06-36 (fixed): `dec(..).map_err(|_| bad.clone())?` followed
  by `return Err(bad)` was "moved inside a loop", with no loop. The
  record parser had cloned on the return and used a fresh literal in
  each closure.
- B-2026-11-06-37 (fixed): `Slice[T]` had no `iter_mut`. Compaction's
  `part.segs[0..n - 1].iter_mut()` was `part.segs.iter_mut().take(n - 1)`.
- B-2026-11-06-38 (fixed): the per-topic load task's `Result` error type
  was fixed only by an annotation inside the closure
  (`let done: Result[..., String] = Ok(got); done`). Annotating the
  join result instead, as `?`'s own diagnostic suggests, failed lowering.
  The closure now ends in plain `Ok(got)` and the join is annotated.
- B-2026-11-06-39 (fixed, diagnostics): a let-chain
  (`if let Some(v) = v and ..`) got two unrelated errors. It is now one
  error naming the shape, and `karac fix` nests the `if`, which is how
  Kāra writes it and what the app does.
