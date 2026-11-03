# mini_mvcc

A multi-version key-value store with transactions: the concurrency
control under PostgreSQL, CockroachDB, Badger and etcd. Writers never
block readers. Every commit adds a new version of each key it wrote,
stamped with its commit time; a transaction reads the versions its
snapshot can see, and at commit it is checked against what committed
since, by its isolation level. Commits go to a write-ahead log first, so
a crash loses only the transactions still open. The program runs seven
session files, each a script of interleaved transactions, each as its own
task.

```
a begin ser at 4
b begin ser at 4
a get oncall:alice = yes
a get oncall:bob = yes
b get oncall:alice = yes
b get oncall:bob = yes
a put oncall:alice no
b put oncall:bob no
a commit at 5 (1 write)
b abort: read-write conflict on oncall:alice (a committed it at 5)
```

## Versions and snapshots

The store keeps every key's versions, oldest first, each with its commit
time, the transaction that committed it, and a value or a delete. The
clock is the time of the last commit. `begin` takes a snapshot at the
clock; a read sees the newest version at or before the snapshot. A
transaction's writes are buffered and it always reads them first, and a
`scan` merges them over the snapshot.

The isolation levels:

- **rc**, read committed: every statement reads at the clock, so a value
  can change between two reads in one transaction. Commit checks nothing:
  the last writer wins, and an update made in between is lost.
- **si**, snapshot isolation (the default): reads stay at the snapshot.
  Commit aborts when a key it writes has a version committed after the
  snapshot (first committer wins). Two transactions that each read what
  the other writes can both commit (write skew).
- **ser**, serializable: as si, and commit also aborts when a key it read,
  or a key under a prefix it scanned, has a version committed after the
  snapshot (backward validation, as Badger does). A transaction that wrote
  nothing commits without checks: it saw one consistent snapshot.

A commit with writes takes the next clock tick; its versions carry it.

## Vacuum

`vacuum` drops the versions no open or future snapshot can see. The
horizon is the oldest snapshot among the open si and ser transactions, or
the clock when there are none (a read committed transaction holds nothing
back). For each key it keeps the versions after the horizon and the
newest at or before it, and drops that one too when it is a delete; a key
left with no versions is gone.

## The write-ahead log

Each commit and each vacuum is appended to `data/run/<session>.wal` before
the store changes:

```
commit 5 t3 2
put acct:alice 60
put acct:bob 110
end 86121fd9
```

and, from another session, `vacuum 4` then `end 94175904`.

The end line carries the FNV-1a 32-bit checksum of the record's bytes
before it. `crash` drops everything in memory, open transactions
included. `tear n` cuts the last n bytes off the log, a write the disk
never finished. `recover` loads the snapshot file if there is one, then
replays the log record by record; at the first record that is cut short or
fails its checksum it stops, and cuts the log back to the last whole
record. `checkpoint` writes every version to `data/run/<session>.snap`
(through a temporary file and a rename) and empties the log.

## The session files

`data/*.mvcc`, written by `mk.py`, one command per line:

- `begin <txn> [rc|si|ser]`
- `<txn> get <key>`, `<txn> put <key> <value>`, `<txn> del <key>`,
  `<txn> add <key> <n>` (read, add, write back), `<txn> scan <prefix>`,
  `<txn> sum <prefix>`, `<txn> commit`, `<txn> abort`
- `dump`, `vacuum`, `crash`, `tear <n>`, `recover`, `checkpoint`

The sessions are a lost update under read committed and its retry under
snapshot isolation, write skew among doctors on call and the same under
serializable with a phantom, what each level sees as others commit, vacuum
held back by a long reader, crashes with a torn log tail and a
checkpoint, ten accounts under 72 overlapping transfers at mixed levels,
and a file of mistakes, which the loader reports and does not run. An
`add` or `sum` on a value that is not a number aborts its transaction.
Each session ends with its commit and abort counts, clock, keys, versions
and log size.

## Running it

The program writes its own `data/` inputs first (embedded at the end of
`mini_mvcc.kara` by `gen_kara.py`):

```
karac run mini_mvcc.kara
karac build mini_mvcc.kara && ./mini_mvcc
```

Both print `expected.out`, and the log and snapshot files under
`data/run/` match the oracle's byte for byte.

## The oracle

`oracle.py` is the same store written plainly in Python from the rules
above. To regenerate:

```
python3 -s -P -B mk.py
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py
```

## Gaps found

- B-2026-11-03-28: `karac fix` turned `levels.contains(w[2].as_str())`,
  where a `ref String` is wanted, into `w[2].as_str().to_string()`, a copy
  of text the place already holds. It now drops the `.as_str()`.
