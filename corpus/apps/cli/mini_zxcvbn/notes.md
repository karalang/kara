# mini_zxcvbn

A port of [zxcvbn](https://pypi.org/project/zxcvbn/) 4.5.0 (MIT; Daniel
Wolf), the Python port of Dropbox's password strength estimator, which many
sign-up forms use to rate a new password. It reads a password as a sequence
of guessable pieces (dictionary words, keyboard walks, repeats, sequences,
years and dates), picks the sequence that is cheapest to guess, and turns
that guess count into crack times, a 0-4 score and feedback.

```
> check P@ssw0rd
  guesses 17 log10 1.2304489213782739 score 0
  online_throttling_100_per_hour 612.0000000000000339728245535 (10 minutes)
  online_no_throttling_10_per_second 1.7 (2 seconds)
  offline_slow_hashing_1e4_per_second 0.0017 (less than a second)
  offline_fast_hashing_1e10_per_second 1.7E-9 (less than a second)
  sequence of 1
    dictionary 0-7 'P@ssw0rd' matched_word='password' rank=2 dictionary_name='passwords' reversed=False l33t=True sub='@ -> a, 0 -> o' base_guesses=2 uppercase_variations=2 l33t_variations=4 guesses=16 log10=1.2041199826559246
  warning 'This is similar to a commonly used password.'
  suggest 'Add another word or two. Uncommon words are better.'
  suggest "Capitalization doesn't help very much."
  suggest "Predictable substitutions like '@' instead of 'a' don't help very much."
```

## What it does

`mk.py` writes 174 jobs (and 10 section comments) to `data/jobs.tsv`, plus
the library's own tables: the six frequency lists (93,855 ranked words) to
`data/frequency_lists.tsv` and the four keyboard graphs (qwerty, dvorak,
keypad, mac keypad) to `data/adjacency_graphs.tsv`. `oracle.py` runs each
job against the vendored library and prints the result, or
`error <class>: <message>` when it raises; `mini_zxcvbn.kara` runs the same
jobs. 3 jobs raise.

- **`check`** calls `zxcvbn()` on a password, with optional user inputs
  (a name, a birth year passed as an int) and a `max_length=` override,
  and prints the guess count, score, the four crack times with their
  display strings, the match sequence the search settled on with every
  match's guess estimate, and the feedback. The passwords cover common
  passwords, capitals, reversals and l33t, keyboard walks, repeats and
  sequences, years and dates, phrases and random strings, non-ASCII text,
  the empty password and the length limit.
- **`matches`** prints every candidate match `omnimatch` finds. The
  library keeps the last call's user inputs in its module-level dictionary
  table, so a `matches` job after a `check` with inputs sees them, as a
  caller of the library would.
- **`subs`** prints the l33t substitution table relevant to a password and
  every substitution `enumerate_l33t_subs` tries.
- **`time`** prints the score and attack times for a guess count given as
  a Decimal string.

## How the port works

- **Numbers keep their Python type.** zxcvbn's guess counts are ints,
  floats or Decimals depending on which estimator made them (`nCk` divides
  in floats, keyboard walks multiply by a float average degree, repeats
  multiply a Decimal by `Decimal(float)`, and the search works in
  Decimals), and the type shows in the output: `guesses=16`,
  `guesses=3888.0000000000005`, `base_guesses=100000000001`. Each count is
  a `Num` of the same kind, combined with Python's promotion rules and
  compared by exact value as Python compares them.
- **Ints are arbitrary precision and Decimals are the decimal module's.**
  A bruteforce run over 66 characters is `10 ** 66`, so ints are base-10^9
  limb vectors. Decimals keep coefficient and exponent separately (so
  `0E+56` prints as Python prints it), round to the default 28-digit
  context half-even on every multiply, add and divide, and division
  follows `_pydecimal`'s algorithm, including stripping trailing zeros
  toward the ideal exponent. `Decimal(float)` and `float_to_decimal` are
  exact, from the float's bits.
- **Matches live in an arena.** The library mutates match dicts in place:
  `estimate_guesses` caches its result on the match, and the same object
  sits in the search tables and the final sequence. The port keeps every
  match in one `Vec[Match]` and passes ids around; bruteforce matches,
  which the search makes afresh on every update, are only materialised
  for the winning sequence.
- **The search is the library's.** `optimal` keeps each prefix's entries
  in dict insertion order (an update keeps its position), the competing
  check, the bruteforce pairs, the stable sort by `i`, and the unwind's
  first strict minimum all match, and repeat matching recurses into
  `omnimatch` and the search on the base token as the library does.
- **Tables are embedded.** The frequency lists are a plain string
  constant and the keyboard graphs a hex blob, both written back to
  `data/` at startup and parsed once.

## Compiler gaps

None. `karac check` asked for `mut` markers on the arguments passed to
`mut ref` parameters, which is a language rule that `karac fix` applied
(34 of them), and suggested `pow` for a misnamed `powf`. After that the
interpreter matched the oracle byte for byte on its first run, and the
LLVM, ASan and native builds matched too.

## Scope

- The console script (`python -m zxcvbn`) and its JSON output are not
  ported; `check` prints the same fields.
- `calc_time` (wall-clock time of the call) is not printed, since it
  differs from run to run.
- Only the English feedback strings are produced; the library's
  `gettext` hook returns them unchanged when no translation is installed.

## Running

```bash
karac run mini_zxcvbn.kara                  # interpret
karac build --mir mini_zxcvbn.kara          # native binary
python3 -s -P -B mk.py                      # regenerate data/
python3 -I -B oracle.py > expected.out      # regenerate expected.out
python3 -I -B gen_kara.py                   # regenerate the embedded data
```
