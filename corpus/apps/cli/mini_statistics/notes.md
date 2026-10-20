# mini_statistics

Python 3.13's `statistics` module, the standard library's toolkit for
reports, dashboards and A/B tests. The port follows `statistics.py` and
the parts of `mathmodule.c` it calls:
- the exact sums: `_sum` and `_ss` keep every int and float as a
  Fraction (here a dyadic big-int fraction), so `mean`, `variance`,
  `pvariance`, `stdev` and `pstdev` are exact before the last rounding.
  `_convert` gives back an int when every input was an int and the
  result is whole. `_float_sqrt_of_frac` is the correctly rounded square
  root, by round-to-odd, and int / int division is correctly rounded with
  a sticky bit;
- `math.fsum` (Shewchuk's partials, with the special-value and overflow
  rules), `math.sumprod` (the exact int path, the triple-length float
  path and the plain path, switching as CPython's state machine does) and
  `math.hypot` (`vector_norm`);
- `mean`, `fmean` with weights, `geometric_mean`, `harmonic_mean` with
  weights, the four medians, `mode`, `multimode`, `quantiles` (exclusive
  and inclusive), the four spread functions with `xbar`/`mu`,
  `covariance`, `correlation` (linear and ranked, with `_sqrtprod`) and
  `linear_regression` (with `proportional`);
- `NormalDist`: `from_samples`, `pdf`, `inv_cdf` (Wichura's AS241),
  `quantiles`, `zscore`, the properties, `+`, `-`, `*`, `/`, negation and
  `==`.

`mk.py` writes `data/`: request latencies per endpoint, an A/B test's
revenue per visitor, a load test (requests per second against p99), a
survey, sensor readings with NaN, infinities and garbage, two years of
monthly revenue, and `jobs.tsv`, the 176 jobs. The data comes from a fixed
generator, so every run writes the same bytes.

The jobs call the functions one by one (154 calls and 14 `NormalDist`
jobs) and run six programs: `latency` (per endpoint: mean, median, p50,
p90, p95, stdev, a normal fit and its outliers), `abtest` (each variant,
Welch's t and degrees of freedom, a 95% interval from `inv_cdf`),
`capacity` (covariance, both correlations, the fit, the capacity at an
SLO, the fit through the origin, residuals and where the curve bends),
`survey` (`median_grouped` by decade, modes of strings, a distance
weighted harmonic mean of speeds, the geometric mean of raises),
`sensors` (every statistic on dirty data) and `growth` (geometric growth,
a normal band, a forecast).

`oracle.py` runs the jobs through Python's own `statistics` and `math`
modules and wrote `expected.out`. `gen_kara.py` writes the data section.

The quirks are kept. Some examples:
- `mean([1, 2, 3, 4])` is `2.5` but `mean([2, 4, 6])` is the int `4`,
  and `pvariance([1, 3])` is the int `1`;
- `geometric_mean([2, -1])` raises with the tuple
  `('No negative inputs allowed', -1)` as its message;
- `harmonic_mean([1, 0, 2])` is the int `0` (the `ZeroDivisionError` is
  swallowed), while `harmonic_mean([1, -2, 0])` raises for the negative
  value it meets first;
- `mode([1, 1.0, 2])` is `1` and `mode([1.0, 1, 2])` is `1.0`; two NaNs
  are never the same key;
- `variance` of data with a NaN is `nan` but `stdev` raises `ValueError`;
  `variance` of three readings near 1e308 overflows only in the final
  division, while `stdev` of the same data is finite;
- `correlation` of values near 1e200 overflows `sumprod`'s float path
  and comes out `0.0`.

`NormalDist.cdf` and `overlap` are left out, and so is `kde`: they call
`math.erf`, which CPython takes from the C library, and glibc's `erf`
differs from the freely licensed fdlibm one in the last bit for a few
percent of inputs, so they cannot be matched without porting glibc's
LGPL code. `samples` is left out because it needs Python's Mersenne
Twister.

It matched on every backend. Kāra floats have no `is_nan`,
`is_infinite` or `is_finite`, so the port tests `x != x` and compares
against `1.0 / 0.0` (filed as a ledger row).
