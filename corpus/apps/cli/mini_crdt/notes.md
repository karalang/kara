# mini_crdt

Conflict-free replicated data types: the model under offline-first apps
and collaborative editors (Automerge, Yjs, Riak's data types, Redis
Enterprise's active-active databases). Replicas edit shared objects while
cut off from each other, exchange operations when they can, and end up
in the same state whatever order the operations arrive in. The program
runs six session files, each with its own replicas, as its own task.

```
ann: doc insert "black " at 4 -> "The black cat sat."
bob: doc insert "fat " at 4 -> "The fat cat sat."
cy: doc delete 4..8 -> "The sat."
sync all: ann +2, bob +2, cy +2
show cy: clock 18, 4 ops
  text doc = "The fat black sat." (18 visible, 4 deleted)
check: converged (ann bob cy)
```

## The data types

- **counter**: `add n` with a signed n. The value is the sum of every add,
  so concurrent adds commute.
- **set**: an add-wins (observed-remove) set. Each add carries a unique
  tag, its replica and sequence number; a remove takes away the tags its
  replica has seen for that element. An add the remover had not seen
  survives, so an element removed on one phone and added again on another
  is still there after the sync.
- **reg**: a last-writer-wins register. Each write is stamped with its
  Lamport time and replica; the larger stamp wins, and on equal times the
  later replica name.
- **text**: an RGA sequence. Every character gets an id, its Lamport
  counter and replica, and is inserted after a named character. Where
  several replicas insert after the same character, the larger id goes
  first, and a run of characters typed together stays together. A delete
  marks characters as tombstones, so later inserts can still name them.

## Operations and sync

Every edit becomes an operation with its replica, a sequence number, a
Lamport time (one counter per inserted character) and a version vector:
how many operations of each replica its replica had applied. A replica
applies its own operations at once. `sync x y` sends y every operation x
holds that y lacks, sorted by the sum of the version vector, then replica
and sequence number; the sum grows along every happened-before edge, so
y always has an operation's causes before the operation. `sync all` does
the same from the union of what every live replica holds.

Each replica appends every operation it applies to its op log,
`data/run/<session>/<replica>.log`, one line each:

```
op tablet 1 3 phone:2,tablet:1,laptop:0 set cart remove milk phone.1
```

`crash` drops a replica's memory; it refuses edits and syncs until
`restart`, which rebuilds it by replaying its log.

## The session files

`data/*.crdt`, written by `mk.py`: a `replicas` line, then one command per
line.

- `<replica> counter <name> add <n>`
- `<replica> set <name> add|remove <element>`
- `<replica> reg <name> set <value>`
- `<replica> text <name> insert <position> <text...>` and
  `<replica> text <name> delete <position> <count>`
- `sync <from> <to>`, `sync all`, `show <replica>`, `check` (do the live
  replicas agree?), `crash <replica>`, `restart <replica>`

The sessions are page-view and stock counters on three servers, a shared
shopping list on two phones and a laptop, a profile edited on three
devices (with a Lamport tie), a document with concurrent inserts at one
place and an overlapping delete, a field app whose tablets go offline,
crash and restart from their logs, and a file of mistakes, which the
loader reports and does not run. Each session ends with every replica's
op count, clock and log size.

## Running it

The program writes its own `data/` inputs first (embedded at the end of
`mini_crdt.kara` by `gen_kara.py`):

```
karac run mini_crdt.kara
karac build mini_crdt.kara && ./mini_crdt
```

Both print `expected.out`, and the logs under `data/run/` match the
oracle's byte for byte.

## The oracle

`oracle.py` is the same replicas written plainly in Python from the rules
above. To regenerate:

```
python3 -s -P -B mk.py
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py
```
