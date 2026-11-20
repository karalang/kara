# Library conformance probes

Each probe here checks one page of `docs/library/` against an oracle that
does not come from karac. Wherever possible the oracle is a reference
implementation: Python's `base64`, `binascii`, `urllib.parse`, `hashlib`
and `hmac`, FIPS 180-4's example digests, and RFC 4231's HMAC test cases.
Where none exists, the oracle is the page itself, as a fixed case. Every
program runs on the MIR interpreter and, when karac has the LLVM backend,
compiled. Both must give the expected output.

| Script | Page | Oracle |
|---|---|---|
| `encoding.py` | encoding.md | Python's `base64`, `urllib.parse`, and the page's own rules where Python is laxer (non-canonical Base64, whitespace in hex) |
| `crypto.py` | crypto.md | FIPS 180-4, RFC 4231, `hashlib`, `hmac`; the streaming types against the one-shot functions for random splits |
| `cases.py <dir>...` | log.md, pool.md, and the examples on encoding.md and crypto.md | The page: `<name>.out` / `.err` hold the output the page says, `.refused` the diagnostics `karac check` must give, and `.unordered` marks output whose line order the page leaves open |

```bash
python3 conformance/encoding.py --seed 1      # random cases; any seed
python3 conformance/crypto.py --seed 1
python3 conformance/cases.py conformance/log conformance/pool conformance/encoding conformance/crypto
```

`KARAC=<path>` picks the binary (default `target/release/karac`), and
`CONF_BUILD=0` skips the compiled run. A script exits 1 on any failure and 2
when it compared nothing.

data.md describes the post-v1 `kara-data` package. The page says nothing on
it runs yet, and `karac check` refuses `Column` and `DataFrame` with that
reason, so it has no probe until the package ships.
