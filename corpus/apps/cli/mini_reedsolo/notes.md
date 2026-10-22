# mini_reedsolo

A port of the Reed-Solomon codec of
[reedsolo](https://github.com/tomerfiliba-org/reedsolomon) 1.7.0 (Unlicense /
MIT-0, public domain — Tomer Filiba & Stephen Larroque). Reed-Solomon is the
forward error correction behind QR codes, CDs/DVDs, DVB broadcast, RAID-6 and
deep-space links: it appends ECC symbols so a bounded number of corrupted bytes
can be detected and repaired.

## What it does

`mk.py` writes `data/jobs.tsv`, one job per line:

    enc \t <nsym> \t <ascii message>
    dec \t <nsym> \t <hex codeword>

- `enc <nsym> <message>` → the full codeword (message + `nsym` ECC bytes) as
  hex;
- `dec <nsym> <hex codeword>` → `ok msg=<hex> ecc=<hex> errs=<n> pos=<p,p,...>`
  — the repaired message and ECC block, how many byte errors were corrected,
  and their positions in the codeword (ascending).

`oracle.py` runs the jobs through the vendored `reedsolo`; `mini_reedsolo.kara`
reproduces `expected.out` byte-for-byte on every backend. The `dec` inputs are
built by encoding a message and flipping a fixed, deterministic set of byte
positions, from zero errors up to the correction bound `nsym // 2`, over `nsym`
of 4, 6, 8 and 10.

## How the port works

Everything is GF(2⁸) arithmetic with the library's default field (primitive
polynomial `0x11d`, generator `2`, first consecutive root `fcr = 0`).
`init_tables` builds the log / anti-log tables (the anti-log table doubled to
510 entries so a product's exponent never needs a modulo), and `gmul` / `gdiv`
/ `ginv` / `gpow` are table lookups. Polynomials are `Vec[i64]` of coefficients.

- **encode** builds the generator polynomial `∏ (x − αⁱ)` and divides the
  zero-padded message by it with extended synthetic division; the remainder is
  the ECC.
- **decode** computes the syndromes (`rs_calc_syndromes`); if they are all zero
  the codeword is clean. Otherwise it finds the error-locator polynomial with
  **Berlekamp-Massey**, the error positions with a **Chien-style** root search,
  the errata evaluator and magnitudes with **Forney**, and XORs the magnitudes
  back into the codeword. Each step follows reedsolo statement for statement,
  including its coefficient-order conventions (hence the several `rev()`s).

The port's errors-only decode path was cross-checked against
`reedsolo.rs_correct_msg` for every message / error-count combination here
before being written.

## Scope

Byte **errors** (not erasures), the library's default field and parameters, and
codewords within the correction bound `nsym // 2`. Every `dec` job here is
correctable, so decoding always succeeds; reporting of over-the-bound decode
failures (the library's `ReedSolomonError` messages), the erasure API, custom
fields (`c_exp ≠ 8`, other primitive polynomials or generators), the chunked
`RSCodec` wrapper and messages longer than 255 bytes are out of scope. The
vendored `reedsolo.py` is pure Python and self-contained.

## Running

```bash
karac run mini_reedsolo.kara                  # interpret
karac build --mir mini_reedsolo.kara          # native binary
python3 -s -P -B mk.py                            # regenerate data/jobs.tsv
python3 -I oracle.py                               # regenerate expected.out
python3 -s -P -B gen_kara.py                       # regenerate the embedded jobs
```
