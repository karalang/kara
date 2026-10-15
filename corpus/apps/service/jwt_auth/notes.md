# jwt_auth: a JWT login service

This app is an HTTP service that issues and verifies HS256 JSON Web Tokens
(RFC 7519) over a real TCP socket. The crypto is written from the specs:

- **SHA-256** (FIPS 180-4), checked on the empty string, "abc", the
  two-block 56-byte message, and 1000 `a`s;
- **HMAC-SHA256** (RFC 2104), checked on RFC 4231 cases 2 and 6 (case 6
  has a key longer than a block, which is hashed first);
- **PBKDF2-HMAC-SHA256** (RFC 8018) for the stored password keys, checked
  on the c=1 and c=2 vectors;
- **base64url without padding** (RFC 4648 §5).

`POST /login` takes a form body and returns a signed token; a wrong
password and an unknown user get the same 401. `GET /me` and `GET /admin`
take `Authorization: Bearer`, and `/admin` needs the `admin` role (403
otherwise). Verification checks, in order, the segment count, the
base64url alphabet, that the header's `alg` is HS256 (so an `alg: none`
token is refused rather than trusted), the signature in constant time, the
issuer, and `exp` and `nbf` with 30 seconds of leeway. A failed check
returns 401 with `WWW-Authenticate: Bearer error="invalid_token"`.

The attack section sends a token whose claims were edited after signing,
an `alg: none` token, one signed with another key, an expired one, one
expired by less than the leeway (accepted), one not valid yet, one from
another issuer, and three malformed ones.

Oracle: `jwt_auth.py`. It uses `hashlib`, `hmac` and `base64`, so every
hash, signature and token is checked against an independent
implementation. The service logic is a port. `expected.out` is its output.

## Edits from jwt_auth.orig.kara to jwt_auth.kara

Author errors (the diagnostics were right):
- A struct named `Request` shadowed the prelude type; it is now
  `HttpRequest`.

Compiler gaps, worked around and listed in the source header:
- **B-2026-10-15-24:** `match self.vals[i] { JVal.JInt(n) => Some(*n) }`
  passes `karac check`, but every MIR surface stops: "unary '*' requires
  'ref T' ... found 'i64'". The MIR builder binds `n` by value when the
  scrutinee is an index place, while check (and MIR itself, for
  `let x = ref v[i]; match x`) treats it as a `ref`. The app binds the
  element with `ref` first.
- **B-2026-10-15-10:** there is no `Clock.now()`, so the service's clock
  is a constant.

Reverted 2026-10-15, karac at aec4d15cc0: B-2026-10-15-24 is fixed and a
user `Request` no longer breaks anything (B-2026-10-15-48), so
`jwt_auth.kara` is `jwt_auth.orig.kara` again and still matches on all
four MIR surfaces. The constant clock stays: it is what makes the output
reproducible.

Written 2026-10-15 by the real-world apps thread; karac at fee58fc7e2.
