# mini_rsa

A port of [python-rsa](https://github.com/sybrenstuvel/python-rsa) 4.9
(Apache-2.0; Sybren A. Stüvel and contributors), the pure-Python RSA
library: key generation, PKCS#1 v1.5 encryption and signatures, MGF1, and
keys in PEM and DER. Its ASN.1 schemas come from
[pyasn1](https://github.com/pyasn1/pyasn1) 0.6.1 (BSD-2-Clause), vendored
for the oracle. Two programs run on top: a license server that signs
records and checks the ones customers present, and a vault that seals
secrets for several people.

```
> verify bankpub transfer 900 to account 42
  VerificationError: Verification failed
> load old keys/v1.pem
  ValueError: Unable to read this file, version 1 != 0
> !licenses licenses.txt tampered.txt bank bankssl
  ...
  acme-corp|inventory-pro|seats=25|expires=2025-12-31: valid (SHA-256)
  globex|inventory-pro|seats=50|expires=2024-06-30: REJECTED (Verification failed)
  hooli|reports|seats=10|expires=2025-01-01: no signature
```

## What it does

`mk.py` writes 10 files to `data/` (listed in `data/files.txt`): a
1024-bit bank key as a PKCS#1 private key, a PKCS#1 public key and an
OpenSSL `PUBLIC KEY`, four broken or unusual key files, the license
records, a tampered copy and the secrets; and `data/jobs.txt` (62 jobs).
`oracle.py` runs each job on the vendored library and prints the result;
`mini_rsa.kara` runs the same jobs.

- **`newkeys`** generates 256, 512 and 16-bit keys, and refuses a 15-bit
  one. **`show`**, **`pem`** and **`der`** print a key's `repr` and its
  PKCS#1 encodings.
- **`load`**, **`loadpub`** and **`loadssl`** read PEM files: the bank's
  three, one with a CRLF preamble and a `Comment:` header, an OpenSSL key
  whose algorithm is not RSA, a private-key load of a public file, a file
  with no end marker and a version-1 private key. **`same`** compares the
  loaded public keys and **`resave`** checks that saving gives the same
  bytes back.
- **`encrypt`** and **`decrypt`** run PKCS#1 v1.5 encryption, including a
  message one byte too long and a decryption with the wrong key.
- **`sign`** and **`verify`** cover MD5, SHA-1, SHA-224, SHA-256, SHA-384
  and SHA-512, a tampered message, the wrong key, a hash too big for a
  512-bit key and an unknown hash name.
- **`mgf1`**, **`inverse`**, **`crt`** and **`prime`** call the helpers in
  `pkcs1_v2`, `common` and `prime` directly.
- **`!licenses`** signs four license records with the bank key and checks
  five presented ones against the bank's OpenSSL public key.
- **`!vault`** encrypts each of four secrets for Alice and Bob and has each
  of them try to open every box; the longest secret does not fit Alice's
  256-bit key.

## How the port works

- **Big integers** are `Vec[u64]` limbs, little-endian, with `u128`
  intermediates, schoolbook multiplication and Knuth's algorithm D for
  division. `pow_mod`, `extended_gcd` (with a signed wrapper), `inverse`
  and `crt` follow `rsa.common` and `rsa.core`.
- **Randomness.** `rsa` draws from `os.urandom`, so `mk.py` and `oracle.py`
  replace it with the seeded generator in `rng.py` (splitmix64), and the
  port has the same generator. With that, key generation (`getprime`,
  Miller-Rabin with `randint`), padding and blinding consume the same bytes
  in the same order, and every key, ciphertext and signature in the output
  matches the oracle's.
- **Blinding** keeps its per-key state, as `AbstractKey` does: the first
  private operation picks a blinding factor and each later one squares it,
  so decrypting the same ciphertext twice runs through different values.
- **ASN.1.** The port writes and reads DER directly for the shapes `rsa`
  uses (`RSAPrivateKey`, `RSAPublicKey` and the OpenSSL
  `SubjectPublicKeyInfo`), with the same errors as the pyasn1 path for the
  inputs here. PEM follows `rsa.pem`: markers as bytes, lines with a `:`
  skipped, base64 of what is left.
- **Hashes** are the MD5, SHA-1 and SHA-2 code from `mini_hashsum`.

## Compiler gaps

None. `karac check` refused a field named `private` (a reserved word) and a
move out of a `for` element; both were my mistakes. After those changes the
interpreter matched the oracle byte for byte, as did the LLVM, ASan and
native builds.

## Scope

- SHA3-256, SHA3-384 and SHA3-512 are known names, so the error that lists
  hash methods prints them, but hashing with them is not ported and raises
  `NotImplementedError`. No job uses them.
- Not ported: `rsa.cli`, `rsa.parallel` (multiprocess prime search),
  `rsa.randnum`'s use of the real `os.urandom`, and pyasn1 beyond the three
  key structures.

## Running

```bash
karac run mini_rsa.kara                  # interpret
karac build --mir mini_rsa.kara          # native binary
python3 -s -P -B mk.py                   # regenerate data/
python3 -I -B oracle.py > expected.out   # regenerate expected.out
python3 -I -B gen_kara.py                # regenerate the embedded data
```
