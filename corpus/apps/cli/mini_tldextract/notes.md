# mini_tldextract

[tldextract](https://github.com/john-kurkowski/tldextract) 5.1.3, the
library crawlers, analytics and ad pipelines and security tools use to
split a host into its subdomain, registered domain and public suffix by the
[Public Suffix List](https://publicsuffix.org/), with three reports built
on it.

The port follows tldextract's own code:
- the suffix list snapshot tldextract ships, split into public and private
  rules by `PUBLIC_SUFFIX_RE` and the private-domains marker, and the
  reversed-label `Trie` it builds, with wildcard rules (`*.ck`,
  `*.kawasaki.jp`) and their exceptions (`!www.ck`, `!city.kawasaki.jp`);
- `lenient_netloc`'s way of finding the host in a URL or a bare name
  (schemes, `//` URLs, user info, ports, brackets, the ideographic full
  stops and trailing dots), and `looks_like_ip` and `looks_like_ipv6`;
- `suffix_index`, which lower-cases each label and decodes a Punycode
  label through `idna.decode` before looking it up, so `www.xn--fiqs8s`
  ends in the suffix `中国`;
- private domains per extractor and per call, `extra_suffixes`, the
  `registered_domain`, `fqdn`, `ipv4` and `ipv6` properties, and the
  `tldextract` command line with its `--json` output.

The Punycode decoding is mini_idna's port of idna 3.20, with its label
checks (NFC, the hyphen rules, CONTEXTJ and CONTEXTO, the Bidi Rule), so
an `xn--` label that does not decode to a valid U-label is looked up as it
is, as tldextract does.

The run writes `data/jobs.tsv`, 230 jobs, and the report inputs:
- `access.log`: 120 requests whose referrers are grouped by site, with
  private domains on, so `alice.github.io` and `bob.github.io` are two
  sites;
- `cookies.tsv`: 26 `Set-Cookie` Domain attributes checked the way a
  browser does: refused when they name a public suffix or do not match the
  host;
- `samesite.tsv`: 12 pairs of URLs checked for being the same site.

Nothing is fetched: every extractor reads the snapshot, as one built with
no cache directory and no suffix list URLs does.

The output matches `oracle.py`, which runs the same jobs through
tldextract itself (with stand-ins for `filelock` and `requests_file`,
which only its disk cache and `file://` URLs use). The Python files are
vendored unchanged in `tldextract/` (BSD-3-Clause, see
`LICENSE-tldextract`) and `idna/` (BSD-3-Clause, see `LICENSE-idna`). The
suffix list snapshot is the Public Suffix List, under the Mozilla Public
License 2.0 as its own header says. Run `python3 mk.py && python3
oracle.py` to regenerate `expected.out`.
