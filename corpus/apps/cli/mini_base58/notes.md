# mini_base58

A port of the [base58](https://pypi.org/project/base58/) library (2.1.1; MIT,
David Keijser): encode a byte string with Bitcoin's Base58 alphabet and decode
it back. Base58 is the encoding behind **Bitcoin addresses**, **IPFS CIDs** and
**Flickr short URLs** — base-64 without the characters (`0`, `O`, `I`, `l`, `+`,
`/`) that are easy to confuse by eye or that break a double-click selection.

## What it does

`mk.py` writes `data/jobs.tsv`: one lowercase hex string per line, the raw input
bytes for a job. For each line the job Base58-encodes the bytes and decodes the
result back, printing two lines:

```
enc <base58 string>
dec <hex of the decoded bytes>     (equals the input)
```

For example, the bytes `00 11 11 … 11 de ad be ef` (a P2PKH-shaped 25-byte
payload) encode to `1MSp9Dq1gadqWMGiytTxHRVkfCpteD1g`, and a single leading zero
byte (`00`) encodes to `1`.

`oracle.py` replays every line through the vendored `base58` library and writes
`expected.out`; `mini_base58.kara` reproduces it byte-for-byte on every backend.

## How the port works

The library turns the bytes into one big integer and changes its base with
repeated `divmod` (`int.from_bytes` to build it, `divmod(i, 58)` to take it
apart). Kāra has no big integers, so the number lives in a **`Vec[i64]` of
base-`B` digits, big-endian**, and the base change is **repeated long division
of that array** by the target base:

- **Encode** (bytes → Base58): the input bytes are the base-256 digits. Divide
  the whole array by 58, keep the remainder as one Base58 digit, repeat on the
  quotient until it is empty, then reverse. Each leading zero byte becomes a
  leading `1`.
- **Decode** (Base58 → bytes): the inverse — base-58 digits divided repeatedly
  by 256. Each leading `1` becomes a leading zero byte.

Each long-division step computes `number mod base` and `number div base`
exactly as the library's `divmod` does, so the output is byte-identical by
construction. The intermediate `carry * base + digit` never exceeds `255·256 +
255 < 2¹⁶`, so it stays well inside `i64` — no big-integer type is needed and no
value overflows.

## Scope

- **The plain `b58encode` / `b58decode` path.** The SHA256-based
  `b58encode_check` / `b58decode_check` (which append a 4-byte double-SHA256
  checksum) are out of scope — a documented library boundary, since the port
  carries no hash primitive.
- The **Bitcoin alphabet only**, without the `autofix` character-confusion
  remapping or the Ripple/XRP alternate alphabets.

The port was checked against the library over the job list on every backend,
and separately (during development) over 60,000 random byte strings, biased
toward leading-zero and full-byte-range cases — encode, decode and a round-trip
all matched.

The vendored library is pure Python and imports only the standard library.

## Running

```bash
karac run mini_base58.kara             # interpret
karac build --mir mini_base58.kara     # native binary
python3 -s -P -B mk.py                     # regenerate data/jobs.tsv
python3 -I oracle.py                        # regenerate expected.out
python3 -s -P -B gen_kara.py                # regenerate the embedded jobs
```
