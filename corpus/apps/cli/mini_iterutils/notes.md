# mini_iterutils

A port of the sequence toolkit of
[boltons](https://github.com/mahmoud/boltons) 26.2.0's `iterutils` module (BSD,
Mahmoud Hashemi): the small, composable helpers for chopping, grouping,
de-duplicating and inspecting sequences that turn up all over data wrangling and
glue code. `boltons` is one of the most-depended-on utility kits on PyPI, and
`iterutils` is its sequence half — the functions you reach for instead of
writing the same chunk/window/partition loop again.

## What it does

`mk.py` writes `data/jobs.tsv`: a register machine over `Vec[i64]`. `new` ops
append a register (printing `reg k = [..]`, `k` in creation order); every other
op reads a register (or bare integers, for `cranges`) and prints a result. One
op per line (tab-separated):

    new   <v...>                   a sequence                 -> reg k = [..]
    split <r> <sep>                split on a value           -> [[..], ..]
    lstrip <r> <v>                 strip leading <v>          -> [..]
    rstrip <r> <v>                 strip trailing <v>         -> [..]
    strip <r> <v>                  strip both ends            -> [..]
    chunked <r> <size>             fixed-size chunks          -> [[..], ..]
    chunkfill <r> <size> <fill>    chunks padded with <fill>  -> [[..], ..]
    cranges <isz> <csz> <off> <ovl> <align>   chunk_ranges    -> [(s, e), ..]
    pairwise <r>                   adjacent pairs             -> [(a, b), ..]
    pairend <r> <end>              pairs, last paired w/ end  -> [(a, b), ..]
    windowed <r> <size>            sliding windows            -> [(..), ..]
    winfill <r> <size> <fill>      windows padded to the end  -> [(..), ..]
    bucket <r> mod3|even           bucketize by key           -> {k: [..], ..}
    part1 <r> even|truthy          partition on one predicate -> ([..], [..])
    part2 <r>                      partition on >0 then <0    -> ([..], [..], [..])
    unique <r>                     first-seen uniques         -> [..]
    redund <r>                     first redundant of each    -> [..]
    redundg <r>                    groups of redundants       -> [[..], ..]
    one <r> truthy|gt40|even       the sole match, else None  -> v | None
    first <r> truthy|gt40|even     first match, else None     -> v | None
    same <r>                       all equal?                 -> yes | no
    sameref <r> <ref>              all equal to <ref>?        -> yes | no

`oracle.py` replays the sequence against the vendored `boltons.iterutils`,
calling the real function with the key or predicate each mode names, so the port
is checked against the library's own behaviour; `mini_iterutils.kara` reproduces
`expected.out` byte-for-byte on every backend. Every op here is a deterministic,
defined output — chunk boundaries, window order, first-appearance bucket order,
partition order, first-seen uniques — so it is a clean A/B surface with no
hash-ordered output.

## How the port works

Each helper follows `iterutils.py` directly over a `Vec[i64]`. `split` uses the
single-value separator path (a trailing, possibly empty, group is always
yielded and separators are never grouped). `lstrip`/`rstrip`/`strip` scan from
the ends. `chunked` slices in `size` steps, padding the last chunk only when a
fill is given. `chunk_ranges` reproduces the library's alignment arithmetic
(the initial short chunk when `align` and the offset is not on a step boundary,
then the overlapping `step = chunk_size - overlap_size` walk). `windowed`
returns nothing for a too-short source without a fill, and one window per
element (padding past the end) with one. `bucketize` groups into first-
appearance key order; `partition` keeps the library's "first predicate wins,
unmatched to the last list" rule. `unique` and `redundant` key on identity, with
`redundant` tracking first-redundancy order and either the single first
redundant value per group or the full groups. `one` returns the sole truthy
match (or `None` for zero or many); `first` returns the first; `same` tests all
elements equal (optionally to a reference).

## Scope

The flat-sequence toolkit over `i64`, with fixed key/predicate modes in place of
arbitrary callables. `iterutils` also provides the famous nested-structure
visitors — `remap`, `get_path`, `research` — and the float range generators
`frange`/`xfrange`/`backoff`; those need an arbitrary nested value model and
floating point respectively, so they are out of scope here. The vendored
`boltons/iterutils.py` is a single pure-Python module importing only the
standard library, so `oracle.py` runs exactly the code the port mirrors.

## Running

```bash
karac run mini_iterutils.kara                  # interpret
karac build --mir mini_iterutils.kara          # native binary
python3 -s -P -B mk.py                              # regenerate data/jobs.tsv
python3 -I oracle.py                                 # regenerate expected.out
python3 -s -P -B gen_kara.py                         # regenerate the embedded jobs
```
