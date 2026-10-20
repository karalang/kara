# mini_intervaltree

intervaltree 3.1.0 by Chaim Halbert, the mutable, self-balancing
interval tree Python programs use to ask "what overlaps this?": genome
annotation, calendars, IP ranges, media timelines. The port follows:
- `Interval`: overlap and containment tests, `overlap_size`,
  `distance_to`, the null-interval checks of `lt`, `le`, `gt` and `ge`,
  its ordering (range, then data, then the data's type name when the
  data cannot be compared), and its repr;
- `IntervalTree`: adding, removing and discarding, point, range and
  envelope queries, slicing (`tree[a:b]`, `tree[p]`, `del tree[p]`),
  `chop` and `slice` with and without a data function,
  `split_overlaps`, `merge_overlaps` and `merge_equals` with and
  without a data reducer, the set operations and their in-place forms,
  `find_nested`, `begin`, `end`, `span`, `copy` and `verify`;
- the tree itself: AVL-balanced nodes, each holding the intervals that
  contain its center, with single and double rotations, promotion of
  intervals on rotation, pruning by greatest child, and the sorted
  boundary table (a sortedcontainers `SortedDict`) that range queries
  walk.

`mk.py` writes `data/`. It holds:
- three command traces over small, wide and dense ranges, with the
  corners of `Interval` a random trace rarely reaches, verifying the
  tree every 30 commands;
- `genes.tsv`, `variants.tsv` and `reads.tsv`: gene annotations on
  three chromosomes, variants to annotate (with the nearest gene for
  intergenic ones), sequencing reads to place, nested genes, and
  coverage blocks counted with `merge_overlaps`;
- `meetings.txt`: two meeting rooms over three days, with clashes, a
  lunch block that `chop`s meetings in two, cancellations and free slots
  across both rooms;
- `allowlist.txt`: CIDR allow rules, deny rules that `chop` holes in
  them, address checks, and the rules flattened with `split_overlaps`
  and `merge_equals`;
- `subtitles.tsv`: subtitle cues cut at scene changes with `slice`, and
  the stretches where cues stack;
- `jobs.tsv`, the jobs.

Each job prints `== ` and its fields, then its output. A trace line
prints the command and what it returned, or the exception it raised.

`oracle.py` runs the jobs through intervaltree itself (the copy in
`intervaltree/`, unchanged, on the sortedcontainers copy in
`sortedcontainers/`) and wrote `expected.out`. An `IntervalTree` is a
set, so every set of intervals is printed sorted, as the tree's repr
does. The tree's shape (`print_structure`, `score`) follows the set's
hash order, and so does `find_nested` for intervals with the same
range, so neither is printed. `gen_kara.py` writes the data section.

The quirks are kept. Some examples:
- `chop(begin, end)` also "splits" an interval that starts exactly at
  `end`, so the meeting right after lunch comes back renamed
  `(after)` though it was never cut.
- `merge_overlaps` without a reducer gives a merged interval `None` as
  its data but leaves an unmerged one its own.
- `overlap_size` with a point raises `AttributeError` when the point
  is inside the interval, and returns 0 when it is not.
- `Interval(3, 9) < 3` is false and `Interval(3, 9) > 3` is true: a
  point compares as the one-element tuple `(3,)`.
- `tree.remove` of a missing interval raises a `ValueError` with no
  message.

intervaltree and sortedcontainers are under the Apache License 2.0
(`LICENSE-intervaltree.txt`, `LICENSE-sortedcontainers.txt`).
