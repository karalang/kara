# mini_x509

OpenSSL 3.0's `x509`, `asn1parse` and `verify` commands, written from RFC
5280, X.690 (DER), FIPS 180-4 and PKCS #1. Certificate monitors, ingress
controllers, service meshes and CI checks read certificates and decide
whether to trust them. They have to read DER exactly, print names and
extensions the way operators expect, match hostnames with RFC 6125's
wildcard rules, and build and check a chain: CA flags, path lengths, key
usage, validity times and signatures.

The inputs in `data/` are a small PKI made with Python's `cryptography`:
- **root.pem**: a 4096-bit RSA root, `Kara Root CA R1`.
- **issuing.pem**: an intermediate with `pathlen:0` and a CRL
  distribution point.
- **shop.pem** (and **shop.der**, the same certificate in DER): a server
  certificate with DNS, wildcard and IPv4/IPv6 names, key usage, extended
  key usage, key identifiers, authority information access and a policy.
- **client.pem**: a client certificate with email and URI names.
- **api-ec.pem**: a P-256 EC key, signed by the RSA intermediate.
- **chain.pem**: shop's certificate followed by the intermediate.
- **old.pem**: expired. **rogue.pem**: issued by shop's certificate, which
  is not a CA. **localhost.pem**: self-signed, with no extensions.
- **Broken files**: `tampered.pem` (one bit of shop's signed data
  flipped), `garbage.pem` (not base64), and `notes.txt`.

The program runs the commands in `data/jobs.txt`:
- `x509 -text` on six certificates, in OpenSSL's layout: serial numbers,
  names, validity times, the RSA modulus and the EC point, and every
  extension above.
- `x509` with `-subject`, `-issuer`, `-serial`, `-dates`, `-enddate`,
  `-ext`, SHA-1 and SHA-256 fingerprints, DER input, and the first
  certificate of a chain file.
- `-checkhost` (wildcards, case, a two-label name), `-checkip` (IPv4 and
  IPv6) and `-checkemail`.
- OpenSSL's errors for a damaged, non-PEM and missing file.
- `asn1parse -i`, which prints the DER tree.
- `verify` at a fixed time (`-attime`). It builds each chain through
  untrusted intermediates to a trusted root, then checks:
  - CA flags and path lengths;
  - the `sslserver` purpose and hostnames;
  - the issuer's key usage;
  - RSA PKCS #1 v1.5 SHA-256 signatures, with Montgomery multiplication
    over 30-bit limbs;
  - validity times.

  It reports OpenSSL's error codes in OpenSSL's order, including where
  `verify` keeps going after an error: the rogue chain reports three
  errors before it stops.

Not supported: signature algorithms other than RSA with SHA-256 (the EC
key is printed but never used to verify), CRLs and OCSP, name
constraints, policy checking, and `-nameopt` options other than the
default.

**Oracle.** `expected.out` is the output of `oracle.py`, the same program
in Python, using `hashlib` and `pow()` where the Kāra program has its
own SHA and RSA arithmetic. `real.py` runs `jobs.txt` with OpenSSL 3.0.13
itself, putting each command's stdout before its stderr. Its output is
byte-identical to `expected.out`, except that it leaves out OpenSSL's
error-stack lines, which start with a thread id that changes on every
run. `mk.py` shows how `data/` was built, but its keys are random, so the
committed `data/` is the input.

## Edits from mini_x509.orig.kara to mini_x509.kara

Compiler gaps:
- B-2026-10-14-43: `match oid { "2.5.4.6" => .. }` on a `Str` is refused,
  so the match is on `oid.to_string()`.
- B-2026-10-14-68: `Some(("C", "countryName"))` is refused against
  `Option[(Str, Str)]`, so the OID table returns `String` pairs.
- B-2026-10-06-107: `KU_NAMES[k].to_string()` on a `const` array is
  parsed as a call on a type, so the element is bound first.

Author errors:
- `M32` and `K256` are type names to Kāra (one letter before the digits),
  so the constants are `MASK32` and `SHA256_K`.
- Bare parameters borrow (D5), so `mont_setup`, which keeps the modulus,
  takes it with `own`.

After that, the output matched `expected.out` on `__mir-run`,
`__mir-llvm-run` and `__mir-llvm-asan` on the first run. One change came
after the match, for speed: Montgomery contexts are now kept per key, as
OpenSSL keeps them, and the limbs grew from 24 to 30 bits. That took
`__mir-run` from 45 to 17 seconds.

Written 2026-10-15 by the real-world apps thread; karac at eb605b2e5b.
