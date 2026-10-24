# mini_ecdsa

A port of [python-ecdsa](https://github.com/tlsfuzzer/python-ecdsa) 0.19.0
(MIT; Brian Warner and contributors), the pure-Python ECDSA library: the
standard curves, key generation, signatures with random, given or RFC 6979
deterministic nonces, verification, public key recovery, ECDH, and keys in
DER and PEM. Two programs run on top: an issuer of ES256 tokens that checks
the tokens customers present, and a ledger that accepts a transfer only when
the key recovered from its signature belongs to the sending account.

```
> sign rfc sha256 string sample
  64 bytes efd48b2aacb6a8fd1140dd9cd45e81d69d2c877b56aaf991c34d0ea84eaf3716f7cb1c942d657c41d436c7a1b6e29f65f3e900dbb9aff4064dc4ab2f843acda8
> point NIST192p - 06188da80eb03090f67cbf20eb43a18800f4ff0afd82ff101207192b95ffc8da78631011ed6b24cdd573f977a11e794811
  MalformedPointError: Inconsistent hybrid point encoding
> !ledger accounts.txt ledger.txt
  alice|bob|25: ok
  ...
  bob|dave|12: forged, signed by alice
  balances: alice 75, bob 35, carol 30, dave 15
```

## What it does

`mk.py` writes 14 files to `data/` (listed in `data/files.txt`): the token
issuer's P-256 key as SEC1 and PKCS#8 private keys, a public key and a
public key with explicit curve parameters, a secp256k1 key, five broken or
unusual public key files, the ledger's accounts and entries, the token
claims and the presented tokens; and `data/jobs.txt` (96 jobs). `oracle.py`
runs each job on the vendored library and prints the result;
`mini_ecdsa.kara` runs the same jobs.

- **`curves`** and **`curve`** look curves up by name or OpenSSL name, and
  print their sizes and OIDs, or `UnknownCurveError` with the list of known
  curves.
- **`genkey`** generates keys on seven curves. **`secret`** builds keys from
  a secret exponent: 1 (the generator), 0 (refused), 3, and the private key
  of RFC 6979's P-256 test vectors.
- **`show`**, **`encodings`**, **`pem`** and **`der`** print a key's `repr`,
  its point in the four encodings, and its DER and PEM forms: SEC1 and
  PKCS#8, uncompressed, compressed and hybrid points, named and explicit
  curve parameters.
- **`load`** and **`loadpub`** read the PEM files back, including the
  explicit parameters (matched to NIST256p), a public file loaded as a
  private key, an unknown curve OID, an RSA algorithm OID, a raw point,
  trailing bytes and explicit parameters for a curve that is not NIST256p.
  **`same`** compares the loaded keys.
- **`sign`**, **`signk`**, **`signrand`** and **`digest`** sign with RFC
  6979, a given k, a random k and a digest that may not be truncated, over
  SHA-1 and SHA-2, as `r||s` strings, DER and canonical (low s) DER.
  **`verify`** checks them, including a changed message, the wrong key and
  the wrong signature format. The RFC 6979 jobs reproduce the RFC's P-256
  signatures for `sample` and `test`.
- **`badsig`** verifies five malformed DER signatures, and **`recover`**
  recovers the two candidate public keys from a signature.
- **`point`** decodes points: compressed points on P-256 and P-224 (whose
  square roots take the `p % 8 == 1` path), an `x` with no point, a bad
  prefix, hybrid points with the right and wrong parity, a point off the
  curve, and lengths that no enabled encoding accepts.
- **`ecdh`** computes shared secrets both ways, and refuses keys on
  different curves.
- **`!tokens`** issues three ES256 tokens and checks eight presented ones:
  copies of the issued tokens, a changed payload, a token signed by another
  key, a truncated signature, an `alg: none` header and a string that is not
  a token.
- **`!ledger`** runs 8 entries over four secp256k1 accounts: transfers that
  go through, a changed amount, entries signed by a key no account owns, an
  overdraft, an entry signed by the wrong account, and a signature one byte
  short.

## How the port works

- **Big integers** are the `Vec[u64]` limbs of `mini_rsa`. Products mod
  p are Montgomery multiplications, so the point arithmetic never divides.
  `inverse_mod` uses the extended Euclidean algorithm.
- **Points** are added and doubled in Jacobian coordinates, with the
  cheaper doubling when a = -3. A point is multiplied four bits at a time
  from a table of its first 15 multiples; the generator through a comb
  table `gen_kara.py` precomputes from the library (the library keeps
  precomputed generator tables too); and `u1 G + u2 Q` for verifying by
  Shamir's trick, two bits of both scalars per step. The two keys a
  signature recovers to share their two products. The library uses
  non-adjacent forms instead. Both compute the same points, so the output
  does not show the difference. With plain double-and-add over division
  the jobs took 179 s under the MIR interpreter, past the corpus runner's
  60 s limit; with these they take about 41 s there and 0.05 s built.
- **Square roots mod p** follow `square_root_mod_prime` for `p % 4 == 3` and
  `p % 8 == 5`. For `p % 8 == 1` (P-224) the library uses a polynomial
  method and the port uses Tonelli-Shanks. Every caller picks the root by
  its parity, so both give the same point.
- **Curve parameters** are copied by `gen_kara.py` from the vendored
  `ecdsa.curves`, all 26 curves, so lookups and the list in errors match.
- **RFC 6979** is `rfc6979.generate_k` over HMAC with the ported SHA-1 and
  SHA-2 (from `mini_hashsum`). The RFC's test vectors check it.
- **Randomness.** `ecdsa` draws from `os.urandom`, so `mk.py` and
  `oracle.py` replace it with the seeded generator in `rng.py`, and the port
  has the same generator. Generated keys and random-nonce signatures match
  the oracle's.
- **DER and PEM** port `der.py`'s readers and writers with their error
  messages. An exception raised with an exception as its second argument
  prints as the tuple of both, as `BadSignatureError` and
  `MalformedPointError` do here.

## Compiler gaps

None. `karac check` refused only `.as_bytes()` on a `Str`, which has no such
method; `to_bytes` replaced it. After that the interpreter matched the
oracle byte for byte, as did the LLVM, ASan and native builds.

## Scope

- Edwards curves (Ed25519, Ed448) are in the curve table but not ported: a
  key on one raises `NotImplementedError`. No job uses them.
- Not ported: `ecdsa.ssh`, `VerifyingKey.precompute`, `Curve.from_pem`, the
  `randrange_from_seed__*` helpers and `numbertheory`'s factoring and prime
  tests, which the library does not use for signing.
- The library builds the "no enabled encoding" message by joining a `set`,
  whose order changes from run to run. `oracle.py` passes the encodings as
  a list, so the message is stable.
- The library bundled `six` for Python 2. This kata does not vendor it:
  `vendor/six/` is a small Python 3 stand-in with only what `ecdsa` imports.

## Running

```bash
karac run mini_ecdsa.kara                  # interpret
karac build --mir mini_ecdsa.kara          # native binary
python3 -s -P -B mk.py                     # regenerate data/
python3 -I -B oracle.py > expected.out     # regenerate expected.out
python3 -I -B gen_kara.py                  # regenerate the embedded data
```
