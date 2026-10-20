# mini_torrentool

torrentool 1.2.0, the Python library and command-line tool for reading,
editing and making BitTorrent `.torrent` files. The port follows the
package:
- `Bencode.decode` with its two stacks, `parse_forward` running the
  digits through Python's `int()` (so `i 12 e`, `i1_000_000e` and `i+7e`
  decode, and `i0x10e` raises `ValueError`), `byte_keys`, the
  `errors="replace"` fallback, the dict sort with Python's comparisons and
  `TypeError`s, and every `BencodeDecodingError`;
- `Bencode.encode`, including the `BencodeEncodingError` for `None` and
  for the container creators an unclosed list or dict leaves behind;
- `Torrent`: `files` through `os.path.join`, `total_size`, `info_hash`,
  `announce_urls`, `webseeds`, `httpseeds`, `comment`, `source`,
  `creation_date` through `utcfromtimestamp`, `created_by`, `private`,
  `get_magnet` with `urlencode`, and the setters;
- `create_from` over `os.walk`, `normpath` and 32 KiB or 256 KiB pieces;
- `humanize_filesize`, and the `info` and `create` commands of `cli.py`.

`mk.py` writes `data/`. It holds:
- a downloads folder of `.torrent` files: an Alpine image with a mirror's
  copy of it, a LibreOffice release, a CC-licensed album, a private
  tracker's upload, an old Japanese client's Shift-JIS names, an
  archive.org torrent with unsorted info keys, and broken ones (empty,
  truncated, an HTML error page, a top-level list, a missing `info`, a
  missing `length`, a date as text, a date in year 10000);
- a partly downloaded OpenStreetMap extract and release notes, with the
  `.torrent` files a client checks them against;
- folders to make new torrents from, and `jobs.tsv`, the jobs.

Jobs decode 45 bencoded strings and four read_string inputs, show every property of each torrent,
re-encode each one, edit 16 of them, and create seven new ones. Two
programs are built on them: a library listing that finds two torrents
for the same content, and a recheck that hashes a download's pieces and
reports what is missing or corrupt. `oracle.py` runs the jobs through the
package itself (`vendor/torrentool/`, unchanged) and wrote
`expected.out`. `gen_kara.py` writes the data section.

The quirks are kept. Some examples:
- a `url-list` given as one string, which BEP 19 allows, is iterated
  character by character, so the magnet link gets `ws=h&ws=t&ws=t&...`;
- a name that is not UTF-8 decodes with U+FFFD, so re-encoding the
  torrent changes its info hash;
- setting `announce_urls` to a list of tiers stores the first tier, a
  list, as `announce`;
- an empty `.torrent` file decodes to `[]`, which is false, so it becomes
  a torrent with an empty `info`.

Two things differ from running the tool itself. `create` sets the
creation date to a fixed time, since `create_from` stamps `utcnow()`.
And `os.walk` lists a directory's subdirectories in the file system's
order, so the folders to make torrents from have at most one
subdirectory each.
