# mini_huffman

A port of the Huffman core of [dahuffman](https://pypi.org/project/dahuffman/)
0.4.2 (MIT; Stefaan Lippens), which builds a **canonical Huffman code** from the
symbol frequencies of some data and encodes the data to bytes. Huffman coding is
the entropy stage inside DEFLATE/gzip, JPEG and PNG.

## What it does

`mk.py` writes `data/jobs.tsv`: one input string per line. For each line the job
builds a Huffman code from the string's UTF-8 bytes and prints two lines:

```
enc <hex of the encoded bytes>
tab <symbol>:<bits>:<value> ...     symbols sorted ascending, E (end-of-file) first
```

For example, `aaaa`:

```
enc f0
tab E:1:0 97:1:1
```

— `a` (byte 97) gets the 1-bit code `1`, the end-of-file symbol `E` gets `0`, and
`aaaa` + the `E` guard packs into the single byte `0xf0`.

`oracle.py` replays every line through the vendored `dahuffman` library and
writes `expected.out`; `mini_huffman.kara` reproduces it byte-for-byte on every
backend.

## How the port works

dahuffman builds the tree from a heap of `(frequency, node)` entries, where a
node carries the `(symbol, (bits, value))` assignments built so far. The two
smallest are merged (prepending a `0` bit to one side's codes and a `1` to the
other) until one node remains. An end-of-file symbol `E` (frequency 1) is added
so the decoder knows where the real data ends inside the last byte.

Two observations make this port byte-exact by construction:

- **Tie-break.** When two entries have equal frequency, Python compares the node
  lists; but the entries hold **disjoint** symbol sets, so the comparison is
  always decided by each node's **first symbol** alone. That makes the heap order
  a strict total order on `(frequency, key_symbol)` (with `E` keyed below every
  byte), so "pop the two smallest" is a plain linear min-scan — no heap internals
  to reproduce.
- **Bit packing.** Codes are accumulated into a `0/1` **bit vector** and packed to
  bytes at the end, with dahuffman's exact final-byte rule (append the `E` code,
  then keep the first eight bits, zero-padded on the right). No bitwise operators
  are used — `1 << n` is `pow2(n)`, a shift is a divide — so the bytes match the
  library's.

## Scope

- **The `from_data` / `encode` / `get_code_table` path.** The pickle-based
  `save`/`load` of a codec to disk is out of scope (the port reads nothing and
  builds the code fresh from each input).
- Code lengths for ordinary text stay small (6 bits over the job list), so the
  integer bit values stay well inside `i64`.

The port was checked against the library over the job list on every backend, and
separately (during development) over 50,000 random byte strings — the full code
table, the encoded bytes and a decode round-trip all matched.

The vendored library is pure Python and imports only the standard library.

## Running

```bash
karac run mini_huffman.kara             # interpret
karac build --mir mini_huffman.kara     # native binary
python3 -s -P -B mk.py                     # regenerate data/jobs.tsv
python3 -I oracle.py                        # regenerate expected.out
python3 -s -P -B gen_kara.py                # regenerate the embedded jobs
```
