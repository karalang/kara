# mini_lsm

A log-structured merge-tree key-value store: the design under LevelDB,
RocksDB, Cassandra and ScyllaDB's storage engines, and the write path of
most databases built since. The program runs a session against two stores,
a product catalog and a metrics series, that writes, reads, flushes,
compacts, crashes, tears and corrupts its own files, and recovers.

```
> get shop sku:05079 @before_sale
sku:05079 = steel_fan|98.80|244 (L0 000004, read 1, skipped 1)
> open shop
opened shop: seq 169, 3 tables, 000009.log: 4 records (4 ops) replayed, stopped at record 5: torn record, 36 bytes dropped
> check metrics
check metrics: 7 tables
  000029.sst: ok, 41 entries in 8 blocks
  000032.sst: block 0: bad checksum
```

## How a store works

- **Writes** go first to the write-ahead log and then to the memtable, a
  sorted map from each key to its versions, newest first. Every operation
  takes the next sequence number; a batch is one log record.
- **A flush** happens when the memtable holds 2048 bytes or more (each
  version counts its key, its value and 16 bytes). It writes the memtable,
  every version of every key, to a new table in level 0, starts a new
  log, commits the MANIFEST and deletes the old log.
- **A compaction** runs when level 0 reaches four tables, or on request.
  It merges every level-0 and level-1 table into new level-1 tables of
  about 2048 bytes, cut only between keys, so level-1 tables never
  overlap. For each key it keeps the newest version each live snapshot
  can see and the newest version of all, and then drops deletions with
  nothing older kept under them.
- **A read** looks in the memtable, then the level-0 tables newest first,
  then the one level-1 table whose key range holds the key. A table whose
  bloom filter says no is skipped without reading a block. `get` reports
  the tables it read and skipped.
- **Snapshots** name a sequence number. A read at a snapshot sees the
  newest version at or below it, and compaction keeps what it sees until
  it is released. Snapshots live in memory only, as LevelDB's do.
- **Opening** a store reads its MANIFEST and replays its log into the
  memtable. Replay stops at the first record that is torn (a header or
  body that runs past the end) or fails its checksum, and the log is cut
  there, so the next write follows the last good record.

## The files

A store is a directory under `data/`. File numbers come from one counter,
shared by logs and tables.

- `NNNNNN.log`, the write-ahead log: records of a CRC-32 of the payload
  (u32), the payload's length (u32) and the payload, which is the first
  sequence number (u64), the operation count (u32), and per operation a
  kind byte (1 put, 0 delete), the key as a varint length and bytes, and
  for a put the value the same way. Integers are little-endian.
- `NNNNNN.sst`, a table:
  - data blocks of about 256 bytes, each entry stored as the length shared
    with the previous key, the rest of the key's length and the value's
    length (varints), then the key's rest and the value. Every fourth entry
    is a restart point that shares nothing. A block ends with its restart
    offsets and their count (u32 each) and is followed by its CRC-32;
  - a bloom filter over the table's keys: ten bits per key (64 at least),
    six probes, LevelDB's 32-bit hash and double hashing, and a last byte
    holding the probe count;
  - an index with the last internal key of each block, its offset and its
    size (varints);
  - a 40-byte footer: the index's offset and size, the filter's offset and
    size, and the magic number `0x4B4152414C534D31` (u64 each).
  Keys inside a table are internal keys: the key's bytes followed by
  `seq << 8 | kind` as a u64, ordered by key and then newest first.
- `MANIFEST`: text, with the next file number, the last sequence number,
  the live log and one line per table (level, number, entries, bytes,
  smallest and largest key). It is written to `MANIFEST.tmp` and renamed
  over the old one, so a crash leaves one or the other.

Tables are read through a file handle: the footer, filter and index when
a table opens, and each data block by seeking to it when it is needed, its
checksum checked before use.

## The data and the jobs

`mk.py` writes `data/catalog.csv` (160 products, plus three rows the loader
must skip) and `data/jobs.txt`, 102 jobs:

- `create`, `open` and `crash` (forget the memtable without flushing);
- `put`, `del`, `batch` (`k=v` puts and `-k` deletes in one record),
  `putn` (generated keys and values) and `load` (the catalog);
- `get` and `scan`, both optionally at a snapshot (`@name`), `snap` and
  `release`;
- `flush`, `compact`, `stats`, `ls`, `sst` (a table's blocks) and `check`,
  which verifies every table, one task per table, and prints in table
  order;
- `tear` (cut bytes off the end of the log, a torn write) and `flip`
  (invert one byte of the log or a table, a bad sector), which also work on
  a store that is not open;
- and the error cases.

The session loads the catalog through three flushes, reads through the
levels, updates and deletes under a snapshot and reads both views, compacts
with and without the snapshot, crashes with a deletion in the log, tears
the log's last record, flips a byte in a middle record, bulk-loads the
metrics store through flushes and an automatic compaction, and corrupts a
table block that `check` and `get` then report.

## Running it

The program writes its own `data/` inputs first (embedded at the end of
`mini_lsm.kara` by `gen_kara.py`) and creates the stores under it:

```
karac run mini_lsm.kara
karac build mini_lsm.kara && ./mini_lsm
```

Both print `expected.out`.

## The oracle

`oracle.py` is the same store written plainly in Python, from the rules
above, so the Kāra port's files must match it byte for byte: every size in
`stats`, `ls` and `sst` and every bloom-filter skip depends on the exact
bytes. To regenerate:

```
python3 -s -P -B mk.py
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py
```
