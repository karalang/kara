# inventory: an order service generic over its storage

`Service[S: Store]` holds the business rules:

- stock levels;
- reservations that take all their lines or none;
- quotes with tiered discounts, using `Money` with `Add`, `Sub` and
  `Display` impls, and rounding half up to the cent;
- idempotent commands, where a repeated request key gets the first reply,
  marked `(replayed)`;
- ship and cancel;
- an audit trail and a report, which uses a generic `top_n[T: Ord + Clone]`.

It is generic over a `Store` trait with a default method, and the trait has
two implementations:

- `MemStore`, a `SortedMap`.
- `FileStore`, an append-only log on disk. It compacts the log when it
  grows, and replays it on open. A torn last line, as after a crash
  mid-write, is dropped and the log is rewritten. A corrupt line in the
  middle is refused with its line number.

The commands come from a script file (`orders.txt`). The same script runs
against both stores, and the two transcripts are checked for equality. Then
the file store is reopened from disk, its log is torn, and it is reopened
again.

Oracle: `inventory.py`, the same program. `expected.out` is its output.

## Compiler gaps (filed in kara `docs/bug-ledger.jsonl`)

- **B-2026-10-15-19:** when a method is called through the `S: Store`
  bound, a `String` argument is not taken for a `Str` parameter ("expected
  'StringSlice', found 'ref String'"). The same call on the concrete type
  is accepted. There were eight such calls; each now passes `.as_str()`.
- **B-2026-10-15-20:** check calls a loop-local binding "moved inside a
  loop" when it is written inside an `if` before being moved
  (`let mut i = ..; if commit { i.on_hand -= qty; } store.put(i)`). The
  app writes `i.on_hand -= if commit { qty } else { 0 };` instead.
- **B-2026-10-15-21:** effects through a generic bound are the union over
  every impl, so `MemStore`'s methods are charged `FileStore`'s file
  effects. Check prints this as a "mutual recursion group" note between
  them. A function declaring only `allocates(Heap)` that runs the service
  on `MemStore` is refused.
- Known and worked around: B-2026-10-12-3 (no `SortedMap.get_mut`),
  B-2026-10-12-6 (`parse` returns `Option`), B-2026-10-12-7 (`split` and
  `lines` are eager `Vec`s).

## Author errors

- `Some(mut i) =>` is Rust; Kāra rebinds with `let mut i = i;`.
- `Result[(), StoreError>`, a typo.

`inventory.orig.kara` is the first draft.
