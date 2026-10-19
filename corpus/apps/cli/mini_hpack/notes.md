# mini_hpack

hpack 4.1.0, the HPACK (RFC 7541) header compression of the Python
HTTP/2 stack: h2 uses it, and through h2 so do httpx, hypercorn and
twisted. The port covers:
- the integer representation with its N-bit prefixes, and string
  literals with and without Huffman coding;
- Huffman coding: the encoder with the codes of RFC 7541 Appendix B,
  and hpack's decoder, which walks a state machine (ported from
  nghttp2) four bits at a time;
- the static table, and the dynamic table with its size accounting
  (32 bytes per entry), eviction and size changes;
- the Encoder: indexed fields, literals with incremental indexing,
  never-indexed literals for sensitive headers, table size updates,
  and dict input with the pseudo-headers moved first;
- the Decoder: every representation, its header list limit, its table
  size limit, and its errors.

The run writes `data/` and plays HTTP/2 connections through an Encoder
and a Decoder, from `data/stories.txt`: the RFC 7541 Appendix C
request and response examples, a browser loading a shop's home page
and its subresources, the server's answers, an API client with
`authorization` and an idempotency key marked sensitive, SETTINGS
changes that shrink and grow the table, headers big enough to evict
or clear it, non-ASCII values, and decoders that close the connection
for a header list over their limit or an encoder that ignores their
table size. For each block it prints the encoded bytes, the
compression, whether the decoder got the same headers back and the
dynamic table.

Then it decodes the blocks of `data/vectors.txt` with a Decoder alone:
the RFC examples, hpack's own test blocks (a response from Apache
Traffic Server, a name that is not UTF-8) and malformed blocks: bad
indexes, integers and strings cut short, EOS or bad padding in a
Huffman string, table size updates out of place or over the limit,
invalid UTF-8, eviction.

`oracle.py` runs the same scripts through hpack itself (the copy in
`hpack/`, unchanged) and wrote `expected.out`. `mk.py` writes the
scripts and `gen_kara.py` the data section at the end of the program.

The quirks are kept. Some examples:
- A perfect match on an empty value is encoded like a match on the
  name alone: hpack tests the matched value for truth. So `x-empty:`
  sent twice adds a second entry instead of using the first.
- A header marked sensitive that is already in the table, in the same
  form, is sent from the table, and the decoder sees an ordinary header.
- The table's size setter overwrites the `resized` flag, so a change
  to 512 followed by a change to 512 sends nothing, and the pending
  update goes out with the next real change.
- The decoder adds a header to its table before a later error in the
  same block, and checks UTF-8 only at the end of the block.
- Integer errors print the `memoryview` they read from; its address is
  not stable, so both the oracle and the port print `<memory>`.

Licenses: hpack is MIT (`LICENSE-hpack`). Its Huffman state machine is
derived from nghttp2's; the nghttp2 notice in `hpack/huffman_table.py`
gives permissive MIT-style terms.
