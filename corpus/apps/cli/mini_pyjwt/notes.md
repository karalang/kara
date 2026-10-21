# mini_pyjwt

JSON Web Tokens as PyJWT 2.10.1 signs and checks them. PyJWT is the
JWT library under Flask-JWT-Extended, Django REST framework's simplejwt,
FastAPI's security examples and many API gateways. The port is PyJWT as
it runs without the `cryptography` package, which leaves the HMAC
algorithms (HS256, HS384, HS512) and `none`. It covers:
- `jwt.encode`, `decode`, `decode_complete` and `get_unverified_header`,
  with every header rule: `kid` must be a string, `typ` can be dropped,
  `alg` in the headers wins, `b64` and `sort_headers`;
- `PyJWS` on raw payloads, detached payloads (`b64: false`), and
  instances limited to some algorithms;
- the token's shape and its errors: segments, base64url as binascii's
  lenient decoder reads it (characters outside the alphabet are
  skipped, padding can end the input), the header and payload loaded by
  `json.loads` with each of its messages and positions, and UTF-8 decode
  errors;
- signatures with SHA-256/384/512 and HMAC written here from FIPS 180-4
  and RFC 2104, and the PEM and SSH checks that refuse asymmetric keys
  as HMAC secrets;
- the claims: `exp`, `nbf` and `iat` through Python's `int()` with
  leeway (a number or a timedelta), `aud` with and without `strict_aud`,
  `iss`, `sub`, `jti` and required claims, the options that switch each
  check off, and decoder-level options;
- `PyJWK`, `PyJWKSet` and `HMACAlgorithm.to_jwk` / `from_jwk`;
- two programs:
  - an API gateway that checks bearer tokens in an access log against a
    JWK set by `kid`, then the route's audience and scope;
  - a token issuer that writes tokens for a user list to a file, reads
    them back and checks them at six later times.

The JSON reader and writer come from `mini_json`, and SHA-2 from
`mini_hashsum`.

The run writes `data/` and then works through the 25 jobs in
`data/jobs.jsonl`, 446 operations in all. `mk.py` wrote `data/`; tokens
with odd headers or payloads are signed there with Python's own `hmac`,
so that PyJWT gets past the signature to the check being tested.

`oracle.py` runs the same jobs through PyJWT itself and wrote
`expected.out`. It uses the Python files of the 2.10.1 wheel in `jwt/`,
unchanged, and makes `import cryptography` fail as it would on a
machine without it. The clock comes from the jobs.

The quirks are kept. Some examples:
- A JWK set with one RSA or EC key fails as a whole with
  MissingCryptographyError, though keys PyJWT cannot use are otherwise
  skipped. A non-dict entry raises AttributeError.
- `algorithms="HS256"` (a string, not a list) is checked with `in`, so
  `"xHS256x"` allows HS256 too.
- `require="exp"` (a string) requires the claims `e`, `x` and `p`.
- `exp` can be a string: `" 1761048010 "`, `"+1761048010"` and
  `"1_761_048_100"` pass, as `int()` reads them. `exp: null` and
  `exp: [1]` raise TypeError, and `exp: Infinity` OverflowError, since
  only ValueError is caught.
- Options given to a `PyJWT` instance do not turn off the signature
  check: `verify_signature` is read from the call's own options.
- A header with `alg: 0` and `algorithms=[0]` fails with "The specified
  alg value is not allowed", because `not alg` is checked first.
- `aud: ""`, `aud: []` and `aud: 0` pass when no audience is asked for,
  and an empty audience list never matches.
- The `none` algorithm signs with an empty signature and never
  verifies.
- A UTF-8 BOM before a header is skipped, since `json.loads` gets
  bytes, but a JWK set read as text with a BOM fails.

Not ported:
- RSA, EC, RSA-PSS and EdDSA, which need `cryptography`; and
  `PyJWKClient`, which fetches JWK sets over HTTP.
- `json.loads`'s UTF-16 and UTF-32 detection for bytes that start with
  a zero byte, and lone surrogates in strings, which a Kāra String
  cannot hold.

## License

PyJWT is under the MIT license in `LICENSE-pyjwt.txt`:

    Copyright (c) 2015-2022 José Padilla

That covers the files in `jwt/`.
