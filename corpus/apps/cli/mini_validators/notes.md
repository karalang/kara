# mini_validators

[python-validators](https://github.com/python-validators/validators) 0.36.0,
the library web services use to check what users type into forms and APIs,
with a signup form and a webhook registry built on it.

The port follows validators' own code, function by function:
- `email`, `domain` (with the IANA TLD list for `consider_tld`, and the
  RFC 1034 trailing dot and RFC 2782 service labels), `hostname` with its
  port handling, `ipv4` and `ipv6` addresses and networks (with `cidr`,
  `strict`, `host_bit` and the `private` check);
- `url` and `uri`: the scheme list, the user and password part, bracketed
  IPv6 hosts, the path and fragment patterns, and `parse_qs` in strict mode,
  whose `ValueError` becomes the reason (`bad query field: 'b'`);
- `mac_address`, `uuid`, `slug`, the hash and base-N formats, `card_number`
  and the eight card brands, `iban`, `cusip`, `isin`, `sedol`, Bitcoin, Tron
  and BSC addresses;
- `between` and `length` with `AbsMin` and `AbsMax` and the `TypeError`s of
  mixed comparisons, `cron`, `country_code`, `calling_code` and `currency`;
- the national ids: `es_cif`, `es_nif`, `es_nie`, `es_doi`,
  `fi_business_id`, `fi_ssn`, `fr_department`, `fr_ssn`, `ind_aadhar`,
  `ind_pan` and `ru_inn`.

Each result is `True` or the `ValidationError` the `@validator` wrapper
builds, with its arguments and, when a `ValueError`, `TypeError` or
`UnicodeError` stopped the check, its reason.

Underneath are the parts of Python validators leans on: the `re` module
(the regular-expression engine of mini_re, with `IGNORECASE`, `VERBOSE`,
lookarounds and named groups), `urllib.parse`'s `urlsplit`, `parse_qs` and
`unquote`, `ipaddress`'s address and network parsing, `int()` over every
script's decimal digits, `str.isdigit`, `isdecimal`, `isalpha` and
`isspace` (from tables `gen_kara.py` writes out of `unicodedata`), the
`idna` codec's ASCII path, `uuid.UUID`, SHA-256 and base58.

The run writes `data/jobs.tsv`, 744 calls across the validators, and the
two report inputs:
- `signups.tsv`: 40 signups whose username, email, website, country, card
  and IBAN are checked one by one, the card's brand found, and the address
  checked for being a public IPv4 one;
- `webhooks.tsv`: 18 callback URLs, refused when they are invalid or when
  they point at a private, loopback or multicast address.

Host names with non-ASCII labels are left out of the jobs: the `idna`
codec's Unicode path (nameprep and punycode) is not ported, so they would
be refused as if they could not be encoded.

The output matches `oracle.py`, which runs the same jobs through validators
itself. The Python files are vendored unchanged in `validators/` (MIT, see
`LICENSE-validators`). Run `python3 mk.py && python3 oracle.py` to
regenerate `expected.out`.
