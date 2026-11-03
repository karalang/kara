# mini_roaring

Roaring bitmaps (Chambi, Lemire, Kaser and Godin): the compressed sets of
32-bit integers that Lucene, Druid, ClickHouse, Spark and Pilosa keep
their postings lists and row sets in. The program loads a week of product
events into one bitmap per day and per event, answers retention, churn
and funnel questions by intersecting them, edits bitmaps with ranges, and
writes and reads `.roar` files in the portable format that CRoaring, the
Java library and pyroaring share.

```
> and funnel e_signup e_cart e_buy
funnel = and(e_signup, e_cart, e_buy): card=24
> retention d1 d7
retention d1 -> d7: kept 191 of 550 (347 per mille)
> hexdump small
small:
  0000: 3a 30 00 00 02 00 00 00 00 00 03 00 01 00 01 00
  0010: 18 00 00 00 20 00 00 00 0a 00 0b 00 0c 00 e8 03
  0020: 00 00 ff ff
  36 bytes
```

## Containers

A value splits into a 16-bit chunk key (`v >> 16`) and a 16-bit low half.
A bitmap is its chunks in key order, and each chunk holds its lows in one
of three containers:

- **array**: the sorted lows, when there are at most 4096 of them;
- **bitmap**: 1024 64-bit words, one bit per low, past 4096;
- **run**: a list of runs, each a start and a length less one.

The rules for which container a chunk uses are the ones the oracle checks:

- A chunk that is not a run is an array at 4096 values or fewer and a
  bitmap above that, so adding the 4097th value turns an array into a
  bitmap and removing back down to 4096 turns it into an array.
- `addrange` into a chunk that holds nothing makes a run container.
- Any other change to a run chunk (an add, a remove, a range over a
  chunk that already held values, a flip) leaves it an array or a bitmap.
- `optimize` makes a chunk a run when its runs serialize smaller than the
  array or bitmap would: `2 + 4 * runs < min(2 * card, 8192)`, and turns
  a run that no longer pays back into an array or a bitmap.
- `and`, `or`, `xor` and `andnot` produce arrays and bitmaps only.

## The file format

`.roar` files follow
[RoaringFormatSpec](https://github.com/RoaringBitmap/RoaringFormatSpec),
little-endian:

- A cookie: `12346` and a 32-bit chunk count when no chunk is a run, or
  `12347 | (count - 1) << 16` followed by a bitset of which chunks are runs.
- For each chunk, its key and its cardinality less one, both 16 bits.
- An offset header (a 32-bit offset per chunk) when there are no runs, or
  when there are four chunks or more.
- The container bodies: an array's 16-bit lows, a bitmap's 1024 64-bit
  words, or a run list's 16-bit count and then start and length pairs.

A file that does not follow it is refused with what is wrong and where:
a bad cookie, a header or container that runs past the end, keys out of
order, an offset that does not point at its container, an unsorted array,
a run that leaves its chunk, a container whose count disagrees with its
header, or bytes after the last container.

## The data and the jobs

`mk.py` writes `data/`:

- `events.csv`: 3346 `day,user,event` rows over a week, with user ids
  drawn from four blocks the way sharded id allocators hand them out (a
  dense block of early accounts, one past 65536, a sparse one past a
  million, and a few near the top of u32), plus four malformed rows.
- `shipped.roar`: a valid file with an array, a bitmap and two run
  containers, as another Roaring library would write it.
- `bad_cookie.roar`, `truncated.roar` and `unsorted.roar`: three that are
  not.
- `jobs.txt`: 99 jobs. `load` builds the eleven bitmaps (`d1`..`d7`,
  `e_view`, `e_signup`, `e_cart`, `e_buy`), one task each; `and`, `or`,
  `xor` and `andnot` combine bitmaps into a new one; `retention`,
  `jaccard` and `equal` compare; `add`, `remove`, `addrange`,
  `removerange` and `flip` edit; `rank`, `select` and `contains` query;
  `optimize`, `stats`, `show` and `hexdump` look at containers; `save` and
  `loadroar` write and read `.roar` files (with a CRC-32 of each saved
  file); and the rest are errors the program must report and survive.

## Running it

The program writes its own `data/` first (the files are embedded at the
end of `mini_roaring.kara`, from `gen_kara.py`), so it runs from any
directory:

```
karac run mini_roaring.kara
karac build mini_roaring.kara && ./mini_roaring
```

Both print `expected.out`.

## The oracle

`oracle.py` runs the same jobs over plain Python sets: a bitmap there is
a set of values and the set of chunk keys held as runs, and every
container, byte and error message follows from the rules above. It is a
specification of the observable behaviour, not a second Roaring
implementation. To regenerate:

```
python3 -s -P -B mk.py
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py
```
