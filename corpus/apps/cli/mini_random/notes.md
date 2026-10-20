# mini_random

Python 3.13's `random` module, ported from `random.py` and the Mersenne
Twister in `Modules/_randommodule.c`. Simulations, load generators, A/B
bucketing and test fixtures all lean on one property: a seeded generator
gives the same stream on every machine. This port gives Python's stream,
bit for bit.

A `Random` holds the 624 words of MT19937 state, the index of the next
word, and the second normal deviate that `gauss()` keeps for its next
call. The port covers:
- `seed()` from ints of any size (`init_by_array` over the absolute
  value's 32-bit words), from floats (through Python's numeric hash), and
  from `str` and `bytes`: version 2 appends the SHA-512 digest, version 1
  is the old string hash;
- `getstate` and `setstate`, with the C checks on the state tuple;
- `random()`, `getrandbits()` and `randbytes()`, and `randrange()` and
  `randint()` over ints of any size through `_randbelow`;
- `choice`, `shuffle`, `sample` (with and without `counts`, and both of
  its strategies) and `choices` with `weights` or `cum_weights`;
- every distribution: `uniform`, `triangular`, `normalvariate`, `gauss`,
  `lognormvariate`, `expovariate`, `vonmisesvariate`, `gammavariate`,
  `betavariate`, `paretovariate`, `weibullvariate` and `binomialvariate`.
  Its BTRS path uses `math.lgamma`, so CPython's `m_lgamma` (Lanczos'
  approximation, same coefficients and order) is ported too;
- `python -m random` with its argparse usage, help and errors.

`mk.py` writes `data/`: `jobs.tsv` and the files the programs read. Each
job makes a fresh generator, and each line is one call, printed with its
result or the exception it raises. The last jobs are programs:
- a load-test traffic generator with weighted endpoints and think times;
- A/B test bucketing, seeded by experiment and user;
- retry backoff with full, equal and decorrelated jitter;
- reservoir sampling of a log;
- a Monte Carlo project estimate from triangular and PERT task durations;
- a tournament draw and a Secret Santa;
- a synthetic test-data generator;
- a weather-sensor simulator.

`oracle.py` runs the same jobs through the real `random` module and wrote
`expected.out`. `gen_kara.py` writes the data section.

The quirks are kept. Some examples:
- `randint(6, 1)` says `empty range in randrange(6, 2)`, because it calls
  `randrange(a, b + 1)`;
- `betavariate(-1, 1)` reports `gammavariate: alpha and beta must be > 0.0`;
- `choices([], k=1)` is an `IndexError: list index out of range`, not the
  `Cannot choose from an empty sequence` that `choice` gives;
- `setstate` unpacks `gauss_next` before the C code checks the words, so a
  refused state still replaces the kept normal deviate;
- `python -m random` puts its first block of positionals in `input` and
  reports a later block as unrecognized arguments.

It matched on every backend. `jobs.kara`'s argparse emulation compares
`*g != long` under `Some(ref g)`, which failed on MIR until
B-2026-10-20-24 was fixed (kara d9bd46cacb). Floats
have no `PI`, `TAU` or `E`, so the module defines its own; an unknown
constant like `f64.PI` passes `karac check` and stops MIR lowering
(B-2026-10-20-23). The struct-pattern shorthand `P { ref name }` did not
parse (B-2026-10-20-25, fixed in kara 4dd4f51777).
