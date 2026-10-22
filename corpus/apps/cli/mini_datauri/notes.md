# mini_datauri

A port of [datauri](https://pypi.org/project/datauri/) 1.1.0 (BSD-3-Clause;
EclecticIQ), the library for **`data:` URIs** as defined in RFC 2397 — the
inline-resource scheme behind images embedded in CSS/HTML (`url(data:image/png;base64,…)`),
small payloads carried in config and email, and favicons baked into a page.

## What it does

`mk.py` writes `data/jobs.tsv`: one tab-separated job per line, either

- `p <uri>` — parse one `data:` URI;
- `d <text>` — discover every `data:` URI embedded in a string.

`oracle.py` replays each against the vendored `datauri` library and writes
`expected.out`; `mini_datauri.kara` reproduces it byte-for-byte on every backend.

A parse result is printed as `media_type|hex|repr` (the media type is `-` when
empty/None, the payload as lowercase hex, and the library's `ParsedDataURI`
repr); an error is `ERR <message>`. A discover result is a count followed by the
space-joined `media_type|hex` of each URI found. `|` and space are both outside
the data-URI character set, so the columns are unambiguous.

## How the port works

`mini_datauri.kara` follows `datauri/datauri.py`:

- **parse** checks the `data:` prefix, requires a comma, and splits the URI at
  its *first* comma into a media type and a payload. A `;base64` suffix on the
  media type selects the base64 branch (and is stripped); otherwise the payload
  is percent-decoded. An empty media type becomes `None`.
- **base64** mirrors the library exactly, including its length-based padding
  (`'=' * (-len(payload) % 4)`) appended *before* decoding, and the invalid
  cases (a kept-character count of `n % 4 == 1`).
- **percent-decoding** reproduces `urllib.parse.unquote_to_bytes`: `%XX` with two
  hex digits is one byte, a lone or malformed `%` stays literal, `+` is *not*
  turned into a space.
- **discover** performs the same greedy character-class scan the library's regex
  (`data:[<unreserved><reserved>%]+`) performs: find `data:`, consume the allowed
  run, parse it, and skip anything that fails to parse.
- **repr** reproduces CPython's `str` and `bytes` repr — the quote-selection rule
  (single quotes unless the data holds `'` but not `"`), the `\t \n \r \\` and
  quote escapes, `\xNN` for non-printable bytes, and the 20-byte data truncation
  (`data[:17] + b'...'`).

## Scope

base64 payloads use the standard alphabet with the library's trailing padding.
CPython's `binascii` additionally *discards* interior non-alphabet bytes in its
non-strict mode; the port drops the same leading bytes but treats a data byte
after padding as invalid, which is the documented boundary (well-formed payloads
never hit it). The vendored `datauri.py` imports only `base64`, `re` and
`urllib.parse`, so `oracle.py` runs exactly the transform the port mirrors.

## Running

```bash
karac run mini_datauri.kara                  # interpret
karac build --mir mini_datauri.kara          # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I oracle.py                           # regenerate expected.out
python3 -s -P -B gen_kara.py                   # regenerate the embedded jobs
```
