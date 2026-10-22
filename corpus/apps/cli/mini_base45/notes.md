# mini_base45

A port of [base45](https://github.com/kirei/python-base45) 0.4.4 (BSD), the
**Base45** data encoding of [RFC 9285](https://www.rfc-editor.org/rfc/rfc9285).
Base45 encodes binary data over a 45-character alphabet
(`0-9 A-Z` plus `space $ % * + - . / :`), chosen so the result fits the
alphanumeric mode of a QR code. Each pair of input bytes becomes a 16-bit value
written as three Base45 characters (a trailing odd byte becomes two), making it
denser in a QR code than Base64 — which is why it is the transport encoding of
the EU Digital COVID Certificate.

## What it does

`mk.py` writes `data/jobs.tsv`: one job per line (tab-separated), either
`enc <hex>` (encode the bytes given as hex) or `dec <base45>` (decode a Base45
string). `oracle.py` replays the list against the vendored `base45` library and
writes `expected.out` — for `enc`, the Base45 string; for `dec`, the decoded
bytes as lowercase hex, or `ERR` when the string is not valid Base45.
`mini_base45.kara` reproduces it byte-for-byte on every backend.

The job list exercises both directions and every validation branch: round-trips
of the RFC's own vectors (`AB` → `BB8`, `Hello!!` → `%69 VD92EX0`,
`base-45` → `UJCLQE7W581`), the `0x00`/`0xFF` byte boundaries, the two
decoded-byte orders, and each decode failure — a length that is 1 mod 3, a
character outside the alphabet, a final pair above `0xFF`, and a triple above
`0xFFFF` right at the edge (`FGW` = 65535 decodes, `GGW` = 65536 is rejected).

## How the port works

`mini_base45.kara` follows `base45/__init__.py` line for line. Encode walks the
input two bytes at a time, forms `x = (hi << 8) + lo`, and splits it with the
same `divmod` chain (`x / 2025`, then `/ 45`, then `% 45`) to index the alphabet
least-significant-digit first; an odd trailing byte takes the two-character
path. Decode maps each character back to its value by scanning the alphabet
(rejecting any character not found), checks the `len % 3 == 1` rule, then
reassembles each triple into a 16-bit value (high byte then low) and each
trailing pair into one byte, rejecting a value that overflows its width. The
`enc`/`dec` dispatch and the hex parsing/formatting are the kata's own I/O
shell; the codec itself is the library's.

## Scope

Inputs and outputs are bytes, carried through `data/jobs.tsv` as hex (encode
input) or as the literal Base45 string (decode input), so the empty input — which
the library round-trips to the empty string — is not exercised through the TSV
shell; the codec handles it the same way. The vendored `base45/` package is pure
Python importing only `typing`, so `oracle.py` runs exactly the code the port
mirrors.

## Running

```bash
karac run mini_base45.kara                     # interpret
karac build --mir mini_base45.kara             # native binary
python3 -s -P -B mk.py                              # regenerate data/jobs.tsv
python3 -I oracle.py                                 # regenerate expected.out
python3 -s -P -B gen_kara.py                         # regenerate the embedded jobs
```
