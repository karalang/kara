# mini_raft

A Raft cluster in a deterministic simulator: the consensus protocol under
etcd, Consul, CockroachDB, TiKV and Kafka's KRaft. The program runs seven
scenario files, each a cluster of its own on a simulated millisecond clock,
and prints every election, step-down, replication and commit, then checks
the logs for Raft's safety properties.

```
t=700 n5 crashes (leader, term 1)
t=881 n4 starts an election for term 2
t=891 n4 becomes leader of term 2 (votes n2 n3 n4)
t=900 c6 -> n1: not the leader, redirected to n4
t=900 c6 -> n4: set config v2 appended at index 8 (term 2)
t=912 c6 committed: set config v2 at index 8 (term 2)
```

## The protocol

Each node follows Raft's Figure 2.

- **Elections.** A follower or candidate whose election timer runs out
  (150 ms plus a random 0 to 149) starts an election: it takes the next
  term, votes for itself and asks every other node for a vote. A node grants
  one vote per term, and only to a candidate whose log is at least as up to
  date as its own (a later last term, or the same last term and a log at
  least as long). A candidate with a majority becomes leader and appends a
  no-op entry in its term.
- **Replication.** A leader sends AppendEntries (up to four entries, the
  index and term of the entry before them, and its commit index) to every
  node every 50 ms and after each new entry. A follower whose log has no
  matching entry before them refuses, and the leader backs up one entry and
  tries again. A follower that holds an entry with a different term at the
  same index drops it and everything after it.
- **Commit.** The leader commits the newest entry of its own term that a
  majority holds, and with it every entry before it. Nodes apply committed
  commands to their key-value map in log order.
- **Terms.** A node that sees a higher term in any message takes it and
  becomes a follower; a leader that steps down starts its election timer.
- **Crashes.** A crashed node keeps its term, its vote and its log, as if
  on disk, and loses everything else.

## The simulator

Every millisecond the clock runs the scenario's actions for that time,
then delivers the messages due, in the order they were sent, and then fires
the heartbeats and election timers of the live nodes in id order. A message
takes 2 to 9 ms, and is lost if its receiver is down or on the other side
of a partition. The randomness is xorshift32: one stream per node for its
timers, seeded `seed * 1000 + id * 7919`, and one for the network, seeded
`seed * 31 + 17`, which also decides each loss when a loss rate is set.

## The scenario files

`data/*.sim`, written by `mk.py`: a `nodes`, `seed` and `until` line, then
one action per line after its time.

- `client <target> set k v | del k | incr k n`: a client sends a command.
  It follows one redirect to the leader the node names.
- `crash <target>` and `restart <nX | all>`.
- `partition leader+1 | follower | 1,2`: split the cluster in two, and
  `heal`.
- `drop <per mille>`: lose that share of messages.
- `status`: every node's state, term, log, commit index and map.

A target is `nX`, `leader` (the live leader with the highest term),
`follower` (the first live node that is not a leader), or `minority` /
`majority` (the leader on that side of a partition, or its first live node).

The scenarios cover a first election and writes, a leader crash and its
return, a leader cut off with one follower while the majority elects a new
leader (and the old leader's uncommitted entries being replaced after the
heal), a 30% lossy network, followers crashing two at a time, a follower
isolated alone whose term climbs, and a file with mistakes, which the loader
reports and does not run. Each scenario ends with the final status, the
leader of each term, which clients committed, and the two safety checks:
Log Matching (two logs with the same term at an index agree up to it) and
that every node's committed entries are a prefix of the most advanced
node's.

## Running it

The program writes its own `data/` inputs first (embedded at the end of
`mini_raft.kara` by `gen_kara.py`) and runs one task per scenario:

```
karac run mini_raft.kara
karac build mini_raft.kara && ./mini_raft
```

Both print `expected.out`.

## The oracle

`oracle.py` is the same simulator written plainly in Python from the rules
above; the Kāra port must match its event log line for line. To regenerate:

```
python3 -s -P -B mk.py
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py
```
