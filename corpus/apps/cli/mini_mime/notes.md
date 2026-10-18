# mini_mime

Binary-to-text encodings as Python 3.13's `base64`, `binascii` and
`quopri` do them, and a mail packer and reader built on them. Mail
clients, HTTP basic auth, data URLs, PDF streams and git's binary
patches all rest on these encodings. The program ports them with their
padding rules and their exact error messages.

The run has three parts:
- **codecs**: the 133 cases in `data/cases.txt`. They cover:
  - base64: standard, URL-safe, alternative characters and
    `encodebytes`/`decodebytes`. Decoding follows binascii's
    `a2b_base64`: outside strict mode it skips anything off the alphabet
    and only checks the padding at the end. Strict mode (`validate`)
    gives each of its six messages: leading, excess and discontinuous
    padding, data after padding, non-alphabet data, and the "1 more than
    a multiple of 4" length error.
  - base32 and base32hex, with `casefold` and `map01`.
  - base16.
  - Ascii85, with `foldspaces`, `wrapcol`, `pad` and Adobe's `<~ ~>`
    framing.
  - base85 and Z85, including their overflow and bad-character errors.
  - `hexlify` with separators counted from either end, `unhexlify`,
    `crc32` and `crc_hqx`.
  - quoted-printable encoding and decoding, with `quotetabs`, header
    mode, soft line breaks and bad escapes.
- **send**: writes a generated `café-logo.png`. It then packs
  `data/outbox` into a `multipart/mixed` message: a quoted-printable text
  body, base64 attachments, a quoted-printable encoded-word subject and
  a base64 encoded-word file name. It writes the message to
  `data/out/message.eml` and prints it.
- **receive**: reads that message back and checks that every part
  round-trips byte for byte. It then reads three messages written by
  other clients:
  - a phone's base64 body, with an attachment wrapped at 60 columns and
    a truncated one that fails with "Incorrect padding";
  - an invoice whose quoted-printable parts are in latin-1 and UTF-8,
    with soft breaks, trailing white space and a bad escape;
  - a CRLF export whose base64 attachment has junk characters in it.

  For each part the program prints its size, CRC-32 and first bytes. It
  prints the decoded text parts and saves the attachments under
  `data/out/received`.

Not supported: `binascii`'s C quoted-printable codec (`b2a_qp`/`a2b_qp`).
`quopri` uses that codec when it is present. The program follows quopri's
own pure-Python codec instead, and the oracle switches quopri to that
codec too. `uu`, `binhex`, and the `email` package's own header and MIME
handling are also not supported. The mail parsing here is the program's
own, the same in both versions.

**Oracle.** `expected.out` is the output of `oracle.py`, the same
program written with `base64`, `binascii` and `quopri`. Run it from this
directory after `mk.py` writes `data/`.

## Edits from mini_mime.orig.kara to mini_mime.kara

Compiler gaps:
- **B-2026-10-15-51 (new):** the latin-1 decoder wrote
  `out.push(0xC0 | (c >> 6))` with `c: u8`. `karac check` typed the
  literal on the left of `|` as `i64` and refused the push as a
  narrowing coercion. With the literal on the right,
  `(c >> 6) | 0xC0`, it passes. The same happens for `&` and `^`, but
  not for `+` or `-`.
- **B-2026-10-15-32:** `split_parts` kept the current part in an
  `Option[Vec[String]]` and took it with `if let Some(c) = cur`. check
  treated `cur` as moved on the path where nothing matched, around the
  loop. It now uses a `Vec` and an `in_part` flag.

Author errors:
- The alphabets were named `B64`, `B32`, `B85` and `Z85`. A name with a
  single letter is Type class, so they are now `B64_ALPHABET` and so on.
- Two parameters that are given away needed `own`.

After that, `karac check` passed. The output matched `expected.out` on
the first run under `__mir-run`, `__mir-llvm-run` and `__mir-llvm-asan`.

Written 2026-10-15 by the real-world apps thread; karac at eb605b2e5b.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits. The app matches expected.out on all four MIR backends again (karac at a4b0967495).
