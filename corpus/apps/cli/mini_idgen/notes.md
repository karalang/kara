# mini_idgen

The ids a web service hands out, as Python 3.13 makes them with its
`uuid` module, hashids 1.3.1 and shortuuid 1.0.13. hashids is the library
behind short public ids such as `jR` for row 1, and shortuuid writes a
UUID in 22 characters instead of 36. The port covers:
- `uuid.UUID` in every form the constructor takes: a hex string, `bytes`,
  `bytes_le`, `fields` and `int`, with the `version` override and every
  `ValueError` and `TypeError` it raises. Every property is shown: `str`,
  `repr`, `hex`, `int`, `urn`, `fields`, `time`, `clock_seq`, `node`,
  `variant`, `version`, `bytes` and `bytes_le`, with Python's bytes repr.
  UUIDs also sort and compare.
- `uuid1` with a given node and clock sequence, including the module-wide
  last timestamp that keeps two ids minted in the same 100 ns apart.
- `uuid3` and `uuid5` on their own MD5 and SHA-1, and `uuid4` over given
  bytes.
- hashids: the alphabet, separator and guard setup with its error, and
  `encode`, `decode`, `encode_hex` and `decode_hex`, with padding to a
  minimum length.
- shortuuid: the alphabet set-up and its error, `encode` with padding,
  `decode` with the legacy order and its errors, `uuid(name)`, the length
  for an alphabet and `encoded_length`.

The run writes `data/` and then works through the 312 jobs in
`data/jobs.jsonl`:
- 112 UUID constructions, 67 of them from hex strings, and 4 sorts.
- 4 runs of `uuid1` calls, with the clock going backwards and standing
  still.
- 150 `uuid3` and `uuid5` names over the four standard namespaces and two
  others, with names that cross the hash block sizes.
- 10 `uuid4` ids, 17 hashids settings and 11 shortuuid alphabets.
- Four reports of the app's own:
  - public order ids minted for each tenant with its own salt;
  - an event log whose version-1 ids give the time and host of each
    event, sorted, with the gaps between events;
  - short links made from URLs, with repeats marked;
  - checks of the ids clients send back: the real id, one with a changed
    last character, one with its case swapped, one too long, one too
    short, and each under the wrong tenant's salt.

`oracle.py` runs the same jobs through Python 3.13's `uuid` and the
packages in `hashids.py` and `shortuuid/`, which are the 1.3.1 and 1.0.13
wheels unchanged, and wrote `expected.out`. For the `uuid1` and `uuid4`
jobs it replaces the clock and `os.urandom` with the job's values; the
rest of those functions runs as it is.

The quirks are kept. Some examples:
- A hex string goes through `int(s, 16)` after the URN prefix, braces and
  hyphens are removed. So `12_4567_-1234-...` is accepted and gives
  `00124567-1234-...`, and so are a `0x` prefix, a `+` sign, surrounding
  spaces and full-width digits, as long as the result is 32 characters.
- `urn:` and `uuid:` are removed wherever they appear, so
  `uuid:urn:12345678-...` is accepted too.
- hashids' `decode` only returns numbers when encoding them again gives
  the same string, so any tampered id decodes to `()`.
- shortuuid sorts its alphabet and drops repeats, so `"AAAAAAAAAB"` is a
  two-letter alphabet, and `0` is not in the default one.
- shortuuid's `uuid(name)` uses the URL namespace when the name starts
  with `http://` or `https://` in any case, and the DNS namespace for
  anything else, `ftp://` included.

Python's `repr` of a string treats every character from U+00A1 up as
printable except U+00AD, which is right for every string the jobs print
but is not Python's full `isprintable` table. A hashids decode that would
need a number above the i128 range gives `()`; no job reaches it.

One workaround: the MIR interpreter orders u128 values as if they were
signed (B-2026-10-19-22), so the sort job sorts UUIDs with its own
insertion sort on `<`.

## License

hashids is under the MIT license in `LICENSE-hashids`:

    Copyright (c) 2012-2014 Ivan Akimov, David Aurelio

shortuuid is under the BSD 3-clause license in `LICENSE-shortuuid`:

    Copyright (c) 2011, Stavros Korokithakis

Those cover `hashids.py` and the package in `shortuuid/`.
