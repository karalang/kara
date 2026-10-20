# mini_mnemonic

python-mnemonic 0.21, Trezor's reference implementation of BIP-39: the
word lists behind the 12 and 24-word recovery phrases hardware and
software wallets hand out. The port follows the package:
- `to_mnemonic` with its length check, the SHA-256 checksum bits and the
  ideographic space Japanese phrases are joined with;
- `check` and `to_entropy`, which normalize to NFKD before looking words
  up, with their errors (`Failed checksum.`, the word count, and
  `list.index`'s `'x' is not in list`);
- `expand_word` and `expand`, `detect_language` with its prefix and
  exact-word rules, `list_languages`, custom wordlists;
- `to_seed` (PBKDF2-HMAC-SHA512, 2048 rounds), `to_hd_master_key`
  (BIP-32 `xprv`/`tprv` with HMAC-SHA512 and a double SHA-256 checksum)
  and the package's own `b58encode`, which drops leading zero bytes.

SHA-256, SHA-512, HMAC and PBKDF2 are written out. NFKD comes from
`tables.py`: decompositions and combining classes generated from
Python's `unicodedata` for the blocks the data uses, Hangul by the
Unicode algorithm, and canonical reordering. `mk.py` checks that this
gives Python's NFKD for every line in `data/`. Python's `str.split()`
and `strip()` are ported with their Unicode whitespace.

`mk.py` writes `data/`. It holds:
- the twelve wordlists and BIP-39's test vectors for ten languages;
- recovery backups as people write them: a two-column recovery sheet, a
  steel plate stamped with four-letter abbreviations, a Japanese card, a
  Korean phone note in NFC, Spanish typed without accents, a Russian
  phrase, an abbreviated phone note, and one that is a word short;
- phrases typed with a word missing, misspelt, in capitals or swapped;
- two custom wordlists, and `jobs.tsv`, the 179 jobs.

Two programs are built on the library. `backup` reads a backup, places
numbered words, detects the language, expands abbreviations and checks
the phrase. `recover` finds the word a typed phrase is missing (`?`), the
misspelt word (edit distance 2) or the swapped pair that makes the
checksum pass.

`oracle.py` runs the jobs through the package itself (`vendor/mnemonic/`,
unchanged) and wrote `expected.out`. `gen_kara.py` writes the data
section.

Three things differ from running the package:
- `generate` takes its entropy from `secrets`, so only its strength
  check and word count are compared;
- the "Language ambiguous between" message lists languages in set order,
  which changes between runs, so the oracle sorts them;
- `detect_language` walks the words as a set and names the first word
  that leaves no language, so `mk.py` only uses phrases where that word
  is the same in every order (exactly one word fits no language). The
  oracle gives the same output under any `PYTHONHASHSEED`.

The quirks are kept. Some examples:
- the Russian and Turkish wordlists are stored in NFC, so after NFKD any
  word with `й` or `ş` is not found: `check` is False and
  `detect_language` says "Language unrecognized";
- `to_entropy` given a string splits on single spaces, so a double space
  makes 13 words; `to_entropy_split` (a list from `str.split()`) does not;
- `b58encode` of `00 01` is `'2'`, not `'12'`.

It matched on every backend and found no gaps.
