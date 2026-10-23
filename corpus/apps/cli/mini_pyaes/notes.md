# mini_pyaes

A port of [pyaes](https://pypi.org/project/pyaes/) 1.6.1 (MIT, Richard
Moore): AES in pure Python, with the five common modes of operation and the
feeders and stream helpers that encrypt a file in chunks.

```
> stream encrypt m plain/notes.txt out/notes.cbc.8192 8192
  -> 400 bytes, fnv1a d5b95447
> compare plain/notes.txt out/notes.cbc.8192.txt
  -> same (391 bytes)
```

## What it does

`mk.py` writes the files a small backup tool encrypts (notes, an INI file,
a CSV report, a binary thumbnail, an empty file and a few edge sizes) to
`data/plain/`, and a script of 1758 steps to `data/jobs.tsv`. `oracle.py`
replays the script through the vendored library and `mini_pyaes.kara`
replays it through the port. Each step prints its result: bytes as hex
(or their length and FNV-1a hash past 64 bytes), lists as `repr()` shows
them, or `error <Class>: <message>`.

- **Known answers.** The FIPS-197 examples for 128, 192 and 256-bit keys
  through the `AES` class, both ways, and the NIST SP 800-38A plaintext
  through ECB, CBC, CFB-128, CFB-8, OFB and CTR for each key size.
- **Messages.** A message through each mode in pieces, so the chaining
  state carries between calls, and round trips through a second object.
- **Reusing a mode.** What happens when one CBC, OFB or CTR object both
  encrypts and decrypts, and two CTR modes share one `Counter`.
- **Counters** started at 0, 1, byte boundaries, 2^64, 2^128 and past it,
  and negative values, which keep the low 128 bits.
- **Feeders.** `Encrypter` and `Decrypter` fed in chunks of every awkward
  size, for each mode, with each padding setting (including unknown ones),
  fed again after the final call, and decrypting bytes whose last byte is
  not valid PKCS#7 padding (the library looks only at that byte).
- **Bad arguments.** Keys and IVs of the wrong size or `None`, blocks of
  the wrong length, CFB segment sizes of 0, more than 16 and negative, and
  CFB without an IV.
- **Files.** Each file through `encrypt_stream()` and `decrypt_stream()`
  with each mode and key size, compared with the original; block sizes
  from 8192 down to 1, 0 and -1; padding options on aligned and unaligned
  files; and truncated files, wrong keys, a wrong IV and a missing file.

## How the port works

- **The tables are the library's.** `gen_kara.py` copies the `AES` class's
  S-boxes, T-tables, U-tables and round constants out of the vendored
  module, and the port expands keys and runs rounds over them as
  `aes.py` does, with the signed words `struct.unpack('>i')` gives.
- **Each mode is one record**, holding what its Python object holds: CBC's
  last cipherblock, CFB's shift register and segment size, OFB's
  precipherblock and the rest of its keystream block, and CTR's leftover
  keystream and the index of its `Counter`, which other modes may share.
  A name bound again gets a new record, as a Python name gets a new
  object.
- **The streams really read and write files.** The port writes
  `data/plain/` first, then reads each input in chunks of the block size,
  as `BufferedReader.read()` returns them, and writes each converted chunk
  to the output file as it comes, so a failing step leaves the same
  partial file behind.
- **The library's quirks are kept.** A feeder fed an empty chunk carries
  on (only `feed()` finishes it); PKCS#7 stripping looks only at the last
  byte, so a pad byte of 0 strips everything; CFB with a segment size over
  16 returns 16 bytes per segment; CFB without an IV starts its shift
  register as a list and fails on the first segment with Python's
  `TypeError`; and `encrypt_stream()` with a block size of 0 reads nothing
  and encrypts only the padding.

## Compiler gaps

None. `karac fix` made 23 edits, all of them the call-site `mut` marker on
an argument passed to a `mut ref` parameter, and the port then matched the
oracle on every backend.

## Scope

- Inputs are bytes; the library's Python 2 paths for `str` are not used.
- An input file that cannot be opened is reported as `FileNotFoundError`,
  the only way one fails in these jobs.

## Running

```bash
karac run mini_pyaes.kara                 # interpret
karac build --mir mini_pyaes.kara         # native binary
python3 -s -P -B mk.py                    # regenerate data/
python3 -I -B oracle.py > expected.out    # regenerate expected.out
python3 -I -B gen_kara.py                 # regenerate the generated tables
```
