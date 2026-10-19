# mini_itsdangerous

The signed tokens a web service hands out, as itsdangerous 2.2.0 makes
them. Flask signs its session cookie with itsdangerous, and
password-reset, email-confirmation and unsubscribe links are usually
made the same way. The port covers:
- `Signer`:
  - the four key derivations (`concat`, `django-concat`, `hmac` and
    `none`), and the `TypeError` for any other;
  - HMAC over SHA-1 (the default), SHA-256, SHA-512 or MD5, each hash
    written here;
  - the separator check, key rotation (a list of keys, newest last), and
    the unsigned `NoneAlgorithm`.
- `TimestampSigner`:
  - `max_age` and `return_timestamp`;
  - the signing date in its errors;
  - the error Python's `datetime` raises for a timestamp it cannot show.
- `Serializer`, `TimedSerializer`, `URLSafeSerializer` and
  `URLSafeTimedSerializer`, with fallback signers and `loads_unsafe`.
- The JSON both kinds of serializer write, and the `json` module's
  reader.
- zlib's compressor at level 6, byte for byte. The URL-safe serializers
  keep a compressed payload only when it is shorter, so the port has to
  compress exactly as zlib does to make the same token. The inflater
  has zlib's checks.
- CPython's lenient base64 decoder and its strict UTF-8 decoder.

The run writes `data/` and then works through the 58 jobs in
`data/jobs.jsonl`, 1059 operations in all:
- signing and unsigning, with tokens cut short, extended and edited;
- each key derivation with each hash;
- salts, separators and key rotation;
- timestamps from 0 to past 2**64;
- both JSON serializers on payloads signed by hand, valid and not;
- compressed payloads, whole and broken;
- password-reset links and a Flask session cookie;
- a move from SHA-1 to SHA-256 through fallback signers;
- 24 jobs drawn at random from a fixed seed.

`oracle.py` runs the same jobs through the Python files of the 2.2.0
wheel in `itsdangerous/`, unchanged, and wrote `expected.out`. The
timestamp signers read the job's clock instead of `time.time()`.

The quirks are kept. Some examples:
- base64 decoding is lenient:
  - characters outside the alphabet are skipped;
  - `+` and `/` are read as `-` and `_`;
  - `=` inside the data is skipped too, but still counts as padding for
    a short last group.

  So a signature with `=` or `==` on the end still matches. One with a
  stray letter or `!` on the end does not.
- The separator check asks whether the separator occurs in the string
  `a`-`z`, `A`-`Z`, `0`-`9`, `-_=`. So `ab` is refused, but `az` is
  allowed even though both letters are in the alphabet.
- A token whose timestamp is too large to show:
  - unsigns when no date is asked for;
  - raises Python's own `ValueError`, `OSError` or `OverflowError`
    when a date is asked for, and `TimedSerializer.loads` always asks
    for one;
  - gives "Malformed timestamp" when the signature is bad as well.
- Just below the point where glibc's `gmtime` gives up, the year in the
  `ValueError` is negative. CPython adds 1900 to `tm_year` in a C int,
  which wraps.
- `TimestampSigner` refuses to sign with a clock below 0 or past 2**64,
  since `struct` cannot pack it.
- A key list that is empty fails with `IndexError` when signing, but
  unsigning against it just reports a bad signature.
- `loads_unsafe` gives up on a bad signature with no payload, and on a
  payload that will not load. Errors that are not about the signature,
  such as a bad payload under a good signature, still raise.
- The plain `Serializer` writes JSON with `ensure_ascii` and `", "`
  separators. The URL-safe ones write it compact, without escaping
  non-ASCII.

## License

itsdangerous is under the BSD 3-Clause license in `LICENSE-itsdangerous`:

    Copyright 2011 Pallets

That covers the package in `itsdangerous/`.
