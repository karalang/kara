# mini_pyprimes

A port of the deterministic core of [pyprimes](https://pypi.org/project/pyprimes/)
0.1 (MIT; Steven D'Aprano), a prime-number toolkit: prime generation, the
prime-counting function π(x), prime sums, exact primality testing and integer
factorisation. These are the building blocks of cryptographic key work,
number-theory tooling and maths teaching.

## What it does

`mk.py` writes `data/jobs.tsv`: a stream of queries, each printing one line.

- `nth <n>` — the nth prime (1-based);
- `first <n>` — the first n primes, space-joined;
- `below <x>` — every prime ≤ x, space-joined;
- `count <x>` — how many primes are ≤ x (π(x));
- `sum <n>` — the sum of the first n primes;
- `isprime <n>` — `yes`/`no` by exact trial division;
- `factor <n>` — the prime factorisation as `p:count` pairs, space-joined;
- `factors <n>` — the flat list of prime factors, space-joined.

`factor`/`factors` take any integer: `0`, `1` and `-1` factor to themselves, and
a negative number contributes a `-1` factor; `isprime` is false for n < 2.

`oracle.py` replays every job against the vendored `pyprimes` library and writes
`expected.out`; `mini_pyprimes.kara` reproduces it byte-for-byte on every
backend.

## How the port works

`mini_pyprimes.kara` follows the library's deterministic functions.

- **Prime generation.** Primes are produced in ascending order by trial
  division against the primes found so far, up to √i — the sequence any of the
  library's sieves yields — so `first`, `below`, `nth`, `count` and `sum` match
  regardless of which generator the library happens to alias as `primes()`.
- **`isprime` (exact).** The port mirrors `isprime_division`: trial-divide by 2
  then the odd numbers up to √n. (Dividing by every odd number rather than only
  the primes gives the same answer and needs no prime list.)
- **`factorise`.** Trial-divide the reduced value by 2 then ascending odds while
  `d*d ≤ n`, recording each `(prime, count)`; whatever is left above 1 is the
  final prime — exactly the library's loop, including the `(0,1)`, `(1,1)`,
  `(-1,1)` and negative-number cases.

The port was checked against the library over the job list on every backend,
and separately (during development) over the first few hundred primes, π(x) and
prime sums up to x = 100000, and primality and factorisation of thousands of
integers up to 10⁹ including semiprimes and prime powers.

## Scope

- **Deterministic functions only.** The library's `isprime` uses random
  Miller-Rabin witnesses (probabilistic above ~341 trillion), and
  `fermat`/`miller_rabin` depend on a chosen base; none of these has a
  reproducible oracle, so they are out of scope. Only the exact, deterministic
  surface is ported.
- **Float-exact range.** The library's `isprime_division` compares a trial
  divisor against the floating-point `n**0.5`; the port uses the integer test
  `d*d ≤ n`, which agrees exactly for the inputs used here (well below the point
  where a double can misrepresent `n` or its square root).

The vendored library is a single pure-Python module importing only the standard
library.

## Running

```bash
karac run mini_pyprimes.kara               # interpret
karac build --mir mini_pyprimes.kara       # native binary
python3 -s -P -B mk.py                       # regenerate data/jobs.tsv
python3 -I oracle.py                          # regenerate expected.out
python3 -s -P -B gen_kara.py                  # regenerate the embedded jobs
```
