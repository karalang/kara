# mini_pydes

A port of [pyDes](https://pypi.org/project/pyDes/) 2.0.1 (MIT; Todd
Whiteman), the pure-Python DES and Triple DES library that projects reach
for when they cannot ship a C extension: payment and banking integrations,
legacy protocol clients, smart-card and HSM tooling, old file formats. The
vendored copy in `vendor/` is the library's single module, unchanged.

```
== FIPS 81 and the classic DES vectors
new k = ok
enc = h:3fa40e8a984d48156a271787ab8883f9893d51ec4b563b53
...
== Empty data
...
new t = ok
enc = ''
...
new tc = ok
enc = h:
...
== ISO 9564 format 0 PIN blocks under a zone PIN key
pin 4111111111111111 1234: clear 041225EEEEEEEEEE, under ZPK 2A3D408A1977DDE9, format 0, recovered 1234
...
== ANSI X9.19 retail MAC
mac 24 bytes = A1C72E74 (full A1C72E74EA3FA9B6)
```

## What it does

`mk.py` writes `data/jobs.txt`, 27 jobs of 282 steps. Each step makes a
`des` or `triple_des` object with its options, encrypts or decrypts with
it, reads or changes a setting, or runs one of the payment tools built on
the library. `oracle.py` plays the jobs through the vendored pyDes and
`mini_pydes.kara` through the port; both print each result as hex or as
Python's `repr()`, and each error as `! ValueError: ...`.

- **Known answers**: the FIPS 81 ECB and CBC examples, the classic
  `133457799BBCDFF1` vector, sixteen NIST SP 800-17 variable-plaintext and
  eight variable-key tests, the docstring's own example, and Triple DES
  with three keys, two keys and a repeated key (which is single DES).
- **Padding**: PAD_NORMAL with a pad character set on the object or passed
  per call, the well-known pitfall where decryption strips real data that
  ends in the pad character, a pad of more than one byte (which leaves the
  data misaligned, so `crypt()` pads it again or refuses), PAD_PKCS5 at
  every length around a block, and PKCS5 decryption under the wrong key,
  which trusts the last byte and strips the whole message.
- **Empty data**: `crypt()` returns the str `''` for empty input, so single
  DES and Triple DES ECB hand back `''` while Triple DES CBC, which joins
  its blocks itself, hands back `b''`.
- **Checks**: every key, IV and padding error the constructors, setters,
  `encrypt` and `decrypt` raise, including a CBC object with no IV, data
  that is not a multiple of eight, and str arguments, which must be ASCII.
  A str key gives Triple DES CBC an IV that stays a str (`getIV()` returns
  `'K1K1K1K1'`) while the three DES objects inside get bytes.
- **Setters**: changing the key, IV, mode, pad and padding mode between
  calls, on `des` and on `triple_des`, whose `setKey` takes the first eight
  key bytes as the IV when CBC has none.
- **Payment tools**: key check values; ISO 9564 format 0 PIN blocks
  encrypted under a zone PIN key and recovered; PIN translation from one
  zone key to another; the ANSI X9.19 retail MAC (and the X9.9 MAC for a
  single-length key); a 24-line settlement file under Triple DES CBC with
  PKCS5; a 300-byte record under single DES ECB; sixteen round trips over
  the four modes.

## How the port works

- **Values** are a `PyVal` enum of bytes, str or None, with Python's
  truthiness, `len()` (code points for a str), slicing and `repr()`. The
  guard (`_guardAgainstUnicode`) turns an ASCII str into bytes and refuses
  anything else.
- **`_baseDes`** is `Base`: the mode, IV, pad, padding mode and key, with
  `_padData` and `_unpadData` in pyDes's order, which differ: `_padData`
  fills in the default padding mode before it refuses a pad character with
  PKCS5, and `_unpadData` checks the mode it was given first, so a pad
  character with no mode passes on an object set to PKCS5.
- **`des`** keeps pyDes's bit lists: `__String_to_BitList`,
  `__BitList_to_String`, `__permutate` over the tables (const `Array`s
  here), the sub-key schedule with its left rotations, the sixteen rounds
  with the S-boxes, and `crypt()` with its own padding and the CBC chain.
- **`triple_des`** builds its three `des` objects in `setKey` (the third is
  the first again for a 16-byte key), runs ECB as three passes over the
  whole data and CBC as its own loop that resets every object's IV to the
  last block, and forwards each setter to all three.

## Compiler gaps

None. `karac check` explained that a one-letter constant (`P`, pyDes's
`__p` table) is always Type-class, so it is `PERM_P`, and `karac fix` added
`ref` where `Cipher.base()` returned a field of a matched variant. After
that the port matched pyDes on the first run under the MIR interpreter,
the LLVM backend, the ASan build and the native binary, with nothing on
stderr.

## Scope

All of pyDes is ported. Python 2's str handling is left out, since the
vendored module's Python 3 branches are the ones that run. The payment
tools are the textbook constructions, written for the jobs; they are not
part of pyDes.

## Running

```
python3 -s -P -B mk.py            # writes data/jobs.txt
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py         # embeds data/jobs.txt in mini_pydes.kara
karac run mini_pydes.kara
```

About 29 s on the MIR interpreter (a debug build of `karac`) and 0.02 s
built.
