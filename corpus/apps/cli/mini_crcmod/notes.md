# mini_crcmod

crcmod 1.7 by Ray Buvel and Craig McQueen, the CRC generator Python
tools use to check file formats, serial protocols and storage (gsutil
checks CRC-32C with it). The port follows the pure-Python path
(`_crcfunpy`):
- `mkCrcFun` and `Crc`: polynomial checks and their `ValueError`,
  initial values and output XORs given as Python ints (`~0` and other
  negatives cut down by the mask), the bit-reversed and plain table
  builders, the 8/16/24/32/64-bit update loops, starting a function from
  a previous CRC, `update`, `new`, `copy`, `digest`, `hexdigest`, `str()`
  and `generateCode`, the C function writer, with its default types and
  its 24-bit masking;
- `crcmod.predefined`: the table of 41 named CRCs with their check
  values, name lookup with crcmod's name simplification and the
  identifier fallback, and the `KeyError` (with crcmod's "Unkown"
  spelling) for anything else.

CRC values and polynomials are `u128`, which holds the 65-bit
polynomials of the 64-bit CRCs.

`mk.py` writes `data/`. It holds:
- `custom.txt`: definitions to build, plain and reversed for each
  width, with output XORs, an init of -1, and polynomials of the wrong
  degree;
- `streams.txt`: byte streams fed to named CRCs in pieces, with
  snapshots copied after each piece, an empty stream, a name crcmod
  does not know and four spellings of one name;
- `png/`: PNG files whose chunk CRCs are checked, one good, one with a
  broken CRC, one with broken data and one cut short;
- `gz/`: gzip files of stored blocks whose CRC-32 trailers are checked,
  one with a wrong CRC and one empty;
- `modbus.txt`: Modbus RTU frames, requests, replies and an exception,
  one with a bad CRC and one too short;
- `xmodem/`: XMODEM-CRC transfers, one clean and one with a noisy
  block that is NAKed, with the payload's CRC-32 after the padding is
  taken off;
- `armor/`: OpenPGP ASCII-armored blocks whose CRC-24 checksums are
  checked;
- `onewire.txt`: 1-Wire ROM codes (DS18B20, DS18S20, DS2401) with their
  CRC-8/Maxim bytes;
- `dumps/`: Redis-style dumps with a CRC-64/Jones footer;
- `jobs.tsv`, the jobs, including C code for seven custom CRCs and for
  the CRCs the Linux kernel, iSCSI, FlexRay and Redis use.

Each job prints `== ` and its fields, then its output.

`oracle.py` runs the jobs through crcmod itself (the copy in
`crcmod/`, unchanged, which falls back to `_crcfunpy` without its C
extension) and wrote `expected.out`. `gen_kara.py` writes the data
section; binary files are stored as hex.

The quirks are kept. Some examples:
- the `KeyError` message is the repr of "Unkown CRC name '...'", in
  double quotes because the name is in single quotes;
- `crc_32` is not found, while `crc32`, `CRC-32` and `Crc32` are;
- `generateCode` declares a 24-bit CRC as `UINT32` and masks it on the
  way in for a reversed CRC and on the way out otherwise;
- `fun(data, crc)` starts from `crc` with the output XOR applied to it
  again, so feeding a previous result continues a stream.

crcmod is under the MIT License (`LICENSE-crcmod.txt`).
