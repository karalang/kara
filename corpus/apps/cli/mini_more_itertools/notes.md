# mini_more_itertools

more-itertools 11.2.1, the library that fills the gaps `itertools` leaves,
as a job list of 85 of its functions run against the library itself. It is
the toolkit a data pipeline reaches for when the standard library stops
short: break a stream into even chunks, slide a window over it, dedupe
within a window, interleave several sources evenly, walk every set
partition, or index directly into the nth combination without enumerating
the ones before it.

The port follows more-itertools' own definitions, and where the order of
the output is load-bearing it follows the exact algorithm the library uses:
`distinct_permutations` is the Wikipedia next-permutation walk over the
sorted multiset, `set_partitions` is the recursive helper that grows a
partition one element at a time, `gray_product` is Knuth's Algorithm H,
`circular_shifts` rotates a buffer by `gcd(n, steps)` places, `nth_product`
/ `nth_combination` / `nth_permutation` are the closed-form index walks, and
`chunked_even` / `divide` / `distribute` front-load their larger pieces the
way the originals do. `windowed` reproduces the deque-and-filler stepping
exactly, so an under-full final window appears only when the real library
would emit one.

**Jobs.** `data/jobs.tsv` holds 151 jobs, one per line, tab-separated, the
function name first. Integer lists are written `1,2,3` (`-` or empty is the
empty list), several lists `1,2;3,4`, a character run as a bare word, and a
nested list `1,[2,[3,4]],5` for `collapse`. Each job prints `== ` and its
fields joined by ` | `, then two spaces and the Python `repr` of the result
— a list for an iterator, the scalar for a reduction, `True`/`False` for a
predicate, or the raised error's message for `one` and the two-item `only`.

The functions cover every group more-itertools draws:

- **grouping** — `chunked`, `chunked_even`, `distribute`, `divide`,
  `sliced`, `split_at` (with `maxsplit` and `keep_separator`),
  `split_before`, `split_after`, `split_when`, `split_into`, `grouper`
  (fill / ignore / strict), `partition`, `bucket`;
- **windowing** — `windowed` (with `step` and `fillvalue`), `substrings`,
  `substrings_indexes`, `stagger`, `windowed_complete`, `triplewise`,
  `sliding_window`, `subslices`;
- **augmenting** — `count_cycle`, `intersperse`, `padded` (with
  `next_multiple`), `repeat_each`, `repeat_last`, `ncycles`, `mark_ends`,
  `padnone`;
- **combining** — `collapse`, `flatten`, `interleave`,
  `interleave_longest`, `interleave_evenly`, `roundrobin`, `zip_offset`,
  `dotproduct`, `convolve`, `sort_together`, `transpose`, `unzip`,
  `reshape`;
- **summarizing** — `ilen`, `take`, `first`, `last`, `nth`, `nth_or_last`,
  `only`, `one`, `all_equal`, `all_unique`, `first_true`, `quantify`,
  `is_sorted`, `minmax`, `iequals`, `map_reduce`, `exactly_n`,
  `run_length` (encode and decode), `consecutive_groups`, `locate`,
  `rlocate`, `replace`, `longest_common_prefix`, `argmin`/`argmax`,
  `running_max`, `running_min`;
- **selecting** — `unique_everseen`, `unique_justseen`, `unique_in_window`,
  `duplicates_everseen`, `duplicates_justseen`, `classify_unique`,
  `powerset`, `partitions`, `set_partitions`, `circular_shifts`,
  `distinct_permutations`, `distinct_combinations`, `nth_product`,
  `nth_combination`, `nth_permutation`, `gray_product`, `partial_product`;
- **math** — `sum_of_squares`, `polynomial_eval`, `polynomial_from_roots`,
  `polynomial_derivative`, `matmul`, `factor`, `is_prime`, `sieve`,
  `totient`, `multinomial`.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the
same jobs through the copy of more-itertools in `vendor/more_itertools/`
(MIT licence, in `LICENSE-more-itertools.txt`). `mk.py` writes
`data/jobs.tsv`; `gen_kara.py` embeds it into the Kāra source as
`jobs_text()`.

Written 2026-10-22 by the real-world apps thread. It compiled clean under
`karac check` and matched `expected.out` byte for byte on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` and `build --mir` on the first run, with
no new compiler gaps. karac at 502e55cb69.
