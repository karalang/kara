# mini_quinemccluskey

A port of [quine-mccluskey](https://pypi.org/project/quine-mccluskey/) 0.3
(MIT, Thomas Pircher): the **Quine–McCluskey algorithm**, the classic tabular
method for minimising a Boolean function into a minimal sum of products. It is
the algorithmic backbone of two-level digital-logic synthesis — reducing a truth
table to the fewest AND/OR gates — and the same minimisation shows up wherever a
set of boolean conditions or rules has to be simplified.

## What it does

`mk.py` writes `data/jobs.tsv`: one boolean function per line, given as the
number of input variables, its ON-set (the minterms where the output is 1), and
an optional set of don't-care minterms.

```
qm <num_bits> <ones-csv> <dc-csv>      dc-csv is "-" when there are none
```

`oracle.py` builds each function with the library's own
`QuineMcCluskey(use_xor=False)`, calls `simplify(ones, dc, num_bits)`, and
prints the minimal cover as implicant strings — one character per variable, most
significant bit first, where `1`/`0` fix the variable and `-` means it drops out
of that product term:

```
<job line> => <count>
<t0 t1 ...>        the cover terms, sorted, space-joined (one line)
```

For example `qm 4 2,6,10,14 -` (the function that is 1 exactly at 2, 6, 10, 14)
minimises to `--10`, i.e. *bit 1 set and bit 0 clear* — the other two variables
don't matter.

`mini_quinemccluskey.kara` reproduces it byte-for-byte on every backend.

## How the port works

The algorithm has two stages, and the port follows both:

- **Prime implicants.** Starting from the minterm bit-strings, repeatedly pair
  terms that differ in exactly one fixed bit and merge them, turning that bit
  into `-`; a term that can no longer be merged is a *prime implicant*. This is
  the tabular "combine adjacent groups" pass, run until nothing combines.
- **The cover.** From the prime implicants, keep the **essential** ones: a prime
  implicant is essential when it is the only one covering some ON minterm. The
  output is the sorted set of essential prime implicants.

Terms are character strings over `{'0','1','-'}`, and the "sets" the algorithm
needs are deduplicated `Vec`s walked linearly, so the whole thing is
integer/string only — the four backends agree by construction and there is no
floating point. Kāra has no bitwise operators, so a bit of a minterm is
recovered with division and modulo (`bit k` is `(m / 2^k) % 2`).

## Scope and the uniqueness precondition

Every job here is chosen so its minimal cover is **unique**: every prime
implicant is essential, so the cover is exactly the full set of prime
implicants. That is deliberate, and it is the one precondition for the
`run == build` A/B rule:

- The reference library accumulates its result in Python **sets**, whose
  iteration order is per-process random. When a function has two or more equally
  minimal covers, which one the library emits can depend on that order — so its
  output would differ between runs, and no single deterministic port could match
  it. A **unique** cover is the same whatever the set order is. Each job was
  checked to produce byte-identical output across many `PYTHONHASHSEED` values
  before being included, and the port's result was compared against the library
  on every backend over the whole job list.
- Because the cover is unique, the port does not need to reproduce the library's
  internal tie-breaking (its MRV-style ranking and greedy
  `__reduce_implicants` pass); it only needs to produce the same set, which the
  uniqueness forces. Both sides sort the result for printing.

The library's XOR/XNOR mode (`use_xor=True`), which can yield more compact
covers containing `^`/`~` terms, is out of scope — the port uses the default
AND-only minimisation, which is the one these functions need. The jobs cover
single-variable projections, constants (tautology and the all-`-` result),
two-literal products, the XNOR-style functions that have no AND-only reduction
(so the cover is the bare minterms), a two-term cover, and several
don't-care-assisted reductions.

The vendored library package imports only the standard library; `vendor/` is
otherwise left untouched.

## Running

```bash
karac run mini_quinemccluskey.kara             # interpret
karac build --mir mini_quinemccluskey.kara     # native binary
python3 -s -P -B mk.py                           # regenerate data/jobs.tsv
python3 -I oracle.py                              # regenerate expected.out
python3 -s -P -B gen_kara.py                      # regenerate the embedded jobs
```
