# pipeline/click_train

An ad click-through-rate job, the shape of a nightly ads or recommendations
pipeline. It generates four days of impression logs and writes each as a
CSV shard, reads the shards back and parses them, hashes every impression
into features, runs five reports per day, and trains an online logistic
regression one day after another. A fifth day is held out to score the
model, which is saved to a file, loaded back and scored again.

- The logs: 4000 impressions a day over 3000 users and 40 ads, with the
  hour, the device (desktop, mobile, tablet) and the market (US, IN, DE,
  BR, JP). Whether an impression was clicked comes from a hidden model of
  basis-point rates: a base rate per ad, lifts for mobile, the evening and
  one market, a drop for tablets, and a user-to-ad affinity the features
  cannot see directly. The noise is a 31-bit linear congruential
  generator.
- The parser checks the header, the field count, the device and market
  names and every number. A copy of day 4 cut at 5000 bytes, mid-line,
  shows its error.
- The features use the hashing trick: a bias and seven hashed features
  (user, ad, hour, device, market, ad by device, ad by hour) into 4096
  weights, through a 32-bit xor-shift-multiply mix.
- The reports: click rate by device, a 24-character bar of the click rate
  by hour, the top five ads by clicks, the market mix, and how the hashing
  filled the table.
- Training scores each impression before it learns from it (progressive
  validation), prints the day's Brier score, and moves each active weight
  by the error over 32. The sigmoid is a 33-entry table of the logistic
  function over [-8, 8] with linear steps between the entries.
- The holdout prints the Brier score, the AUC (ties counted as half) and
  the predicted against the actual click rate in five bands. The model is
  written as big-endian 32-bit weights and its CRC-32 printed.

Everything is integer arithmetic (Q16 probabilities and weights), so the
backends and the Python mirror `click_train.py` agree by construction; the
mirror wrote `expected.out`. Kāra's `/` truncates toward zero, so the
mirror divides through `tdiv()`.

stmt-par: six groups of statements that do not depend on each other, in
`run()` (`click_train.orig.kara` lines):
- L512-L515 four `gen_day` calls, each from its own seed. This is one of
  the two places most of the time goes;
- L518-L521 four `fs.write_bytes` calls, one shard each;
- L533-L536 four `fs.read_bytes` calls;
- L545-L548 four `parse_day` calls. Each can fail with `?`, so a parallel
  run has to report the first failure in source order;
- L555-L558 four `featurize` calls, the other hot group;
- L561-L580 twenty reports, five per day, that only read the parsed days
  and their features.

The four `train_day` calls (L591-L594) are the case that must stay in
order: each takes `mut ref w`, and the next day starts from the weights
the last one left, so the model, its progressive scores and everything
after depend on the order. The holdout's `gen_day(5)` and `parse_day` on
L601 depend on neither, so they could run alongside the training.

## Edits from click_train.orig.kara to click_train.kara

Author errors. One `karac fix` pass made 26 edits, which fixed the first
kind and half of the second; the rest were by hand:
- `let (mut n, mut c) = ..` is Rust syntax; Kāra puts the `mut` before
  the pattern, `let mut (n, c) = ..`. Seven such lets.
- `.collect::[Vec[char]]()` is a turbofish, which Kāra does not have. The
  fix deleted it, which left `let marks = ...collect();` with nothing to
  infer the collection from (E0238), so I moved the type onto the
  binding, `let marks: Vec[char] = ...`, as the parse error's own text
  says to. Filed as B-2026-10-24-13.
- `format!("bias {}, strongest buckets:", w[BIAS])` became
  `f"bias {w[BIAS]}, strongest buckets:"`. The error names the f-string
  but carries no fix. Filed as B-2026-10-24-12.
- `field.parse_int()` does not exist; the library's form is
  `i64.parse(field)`.

Compiler gaps: the two `karac fix` rows above. After those edits it
matched the mirror on the first run under the MIR interpreter, the LLVM
backend, the ASan build and the native binary, with nothing on stderr.

## Running

```bash
karac run click_train.kara
karac build --mir click_train.kara && ./click_train
python3 -I -B click_train.py > expected.out
```

About 21 s on the MIR interpreter and 0.02 s built. It writes its six
files to the working directory and removes them at the end.
