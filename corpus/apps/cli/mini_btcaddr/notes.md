# mini_btcaddr

base58 2.1.1 and bech32 1.2.0, the two encodings behind cryptocurrency
addresses: Base58Check for legacy Bitcoin, Litecoin and XRP addresses,
extended keys and WIF private keys, and Pieter Wuille's reference Bech32
code for SegWit addresses. The port follows both packages:
- `b58encode`, `b58decode`, `b58encode_int`, `b58decode_int`, the
  `_check` variants with their double SHA-256 checksum, custom alphabets,
  and `autofix`, which maps `0Oo` and `Il1` onto the one character of
  each group the alphabet has;
- the `python -m base58` command line (`-d`, `-c`), which reads bytes;
- `bech32_encode`, `bech32_decode`, `convertbits`, and the segwit
  `encode` and `decode` with their length and version rules.

SHA-256 is written out. Numbers are big-endian digit lists converted
between bases, which covers Python's big ints here (a 99-digit
`b58encode_int` included).

`mk.py` writes `data/`. It holds:
- `withdrawals.csv`, 38 withdrawal requests for btc, tbtc, ltc, xrp and
  doge, with good addresses, typos, a pasted space, a trailing NBSP and
  zero-width space, an address for the wrong network and a wrong length;
- extended keys (BIP-32 test vector 1, an account xpub as zpub and ypub,
  tpub, xprv, and broken ones), WIF keys, and output scripts;
- files for the command line, and `jobs.tsv`, the 142 jobs.

Four programs are built on the library. `withdrawals` checks each
request's address for its coin, says what kind of address it is, and
suggests the autofixed address for a typo. `xkeys` decodes extended keys,
checks them and rewrites ypub/zpub as xpub. `wif` reads private keys and
`scripts` turns output scripts into P2WSH and P2WPKH addresses.

`oracle.py` runs the jobs through the packages themselves (`vendor/`,
unchanged) and wrote `expected.out`. `gen_kara.py` writes the data
section.

The quirks are kept. Some examples:
- a str is checked as ASCII, with `UnicodeEncodeError`'s exact message,
  while bytes are not;
- `b58decode` strips Unicode whitespace from a str (an NBSP or U+3000
  goes) but only ASCII whitespace from bytes, and not at all for an
  alphabet with a space in it (`b58decode_int`);
- `b58decode_check` of fewer than four bytes checks an empty payload;
- `bech32_encode` takes `-1` as `CHARSET[-1]`, and 32 is an IndexError.

It matched on every backend and found no gaps.
