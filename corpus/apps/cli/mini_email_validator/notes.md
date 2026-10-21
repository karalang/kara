# mini_email_validator

Email addresses as email-validator 2.2.0 checks them. FastAPI and
pydantic call it for `EmailStr`, and sign-up forms use it to turn what
a user typed into an address worth storing. The port covers:
- splitting off a display name (`Name <addr>`), quoted names and the
  unquoting of their escapes;
- the local part:
  - dot-atom and internationalized characters;
  - quoted strings;
  - the unsafe-character categories and how each is shown;
  - the RFC 2142 role names, folded to lower case;
- the domain:
  - UTS #46 mapping, NFC, the label rules and IDNA 2008, through the
    IDNA code of `mini_idna`;
  - special-use and test names;
  - the length limits on the ASCII and Unicode forms;
- domain literals, `[IPv4]` and `[IPv6:...]`, through the address code
  of `mini_ipaddress`;
- deliverability:
  - MX records, sorted, with the null MX;
  - the A and then AAAA fallback, for globally routable addresses only;
  - the SPF record that rejects all mail;
  - the DNS failures that leave deliverability unknown;
- the normalized, ASCII and SMTPUTF8 results, `as_constructor` and the
  JSON dictionary, and the `python -m email_validator` command line;
- a sign-up log that keeps one account per normalized address.

DNS answers come from `data/dns.tsv`, the way upstream's own tests
replay a cache of real answers instead of asking the network. The
Unicode tables are read from `data/` as in `mini_idna`. `mk.py` writes
them for every character the jobs can reach, including what each
A-label in the jobs decodes to.

The run writes `data/` and then works through the 13 jobs in
`data/jobs.jsonl`, 1426 operations in all:
- the 278 inputs of upstream's `test_syntax.py`, under the defaults,
  ASCII only, and every option on in a test environment;
- 22 unsafe characters from the categories the library names;
- 10 results shown with `as_constructor`, as the README shows them;
- 124 corner cases under four sets of options, among them quoted
  strings, display names, domain literals, lengths at the limits, and
  special-use, test and dotless domains;
- 5 inputs given as bytes;
- 37 deliverability checks against 48 DNS rows;
- 21 command lines, with arguments, environment variables and stdin;
- a log of 48 newsletter sign-ups from three forms, which reports each
  new account, refusal and duplicate, then counts and lists them.

`oracle.py` runs the same jobs through email-validator itself and wrote
`expected.out`. It uses the Python files of the 2.2.0 wheel in
`email_validator/`, unchanged, with idna from `../mini_idna/idna` and
dnspython from `../mini_dnspython/dns`.

The quirks are kept. Some examples:
- The command line ignores `ALLOW_DISPLAY_NAME`, so
  `Jane <jane@example.com>` is refused even with it set.
- A variable counts as set whenever it is not empty, so
  `ALLOW_DOMAIN_LITERAL=0` turns domain literals on.
- `DEFAULT_TIMEOUT` is passed to `validate_email` as a keyword the
  function does not take, so it ends in a `TypeError`.
- With display names allowed, `Jane <jane@example.com>>>` is accepted.
- U+FFFF is shown as `U+0000FFFF`: the four-digit form is used only
  below U+FFFF.
- The IPv4 pattern ends in `$`, which also matches before a final
  newline. So `x@[1.2.3.4\n]` gets past it and is refused by
  `ipaddress` instead.
- The SPF record is checked only when there is no MX record and the
  check falls back to A or AAAA.

Not ported:
- inputs with lone surrogates, which Kāra strings cannot hold;
- `caching_resolver` and the network resolver with its timeout.

## License

email-validator is in the public domain under the Unlicense, in
`LICENSE-email-validator.txt`. That covers the files in
`email_validator/` and `ev_tests/`.
