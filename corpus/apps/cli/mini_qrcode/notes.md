# mini_qrcode

qrcode 8.0, the Python QR code generator, and its `qr` command. QR codes
are on boarding passes, Wi-Fi stickers, business cards, payment slips
and posters; qrcode is what most Python tools make them with. The port
covers:
- splitting the data into numeric, alphanumeric and byte chunks with
  qrcode's regexes (`\d{20,}` and its alphanumeric twin, or `^...+$` for
  short data), the bit buffer, and the smallest version that holds the
  bits at an error correction level;
- Reed-Solomon error correction over GF(256) with qrcode's polynomial
  class, and the interleaving of the blocks;
- the matrix: finder, alignment and timing patterns, the format and
  version information with their BCH codes, the zigzag placement of the
  bits, the eight masks and the four penalty rules that choose one;
- `print_ascii` (with half blocks, and inverted), `get_matrix`, and the
  SVG images: rects, circles, one path, with a white background, with
  sizes in millimetres as Python's Decimal prints them.

`mk.py` writes `data/`: fourteen payloads (a URL, a Wi-Fi login, a vCard
with CRLF line ends, phone numbers and long digit runs, an uppercase
tracking URL, a shipping notice with runs of digits and capitals, UTF-8
text, an airline boarding pass, `123` and a newline, an empty file, a
paragraph, 1800 bytes, and all 256 byte values) and `jobs.txt`, 27 jobs
that encode them with different levels, versions, masks, borders, box
sizes and chunk lengths.

The run writes `data/`, then for each job reads the payload as the `qr`
command reads stdin and prints what the job asks for: the chunks, the
version, the codewords (a CRC-32 and the first twelve), the mask chosen
with the penalty of each, the ASCII art, the matrix, or an SVG image
(its size, CRC-32 and first lines).

`oracle.py` runs the same through qrcode itself (the copy in `qrcode/`,
unchanged, without lxml or PIL) and wrote `expected.out`. `gen_kara.py`
writes the data section at the end of the program.

The quirks are kept. Some examples:
- Data too long for version 40 is a ValueError about version 41, not a
  DataOverflowError: the version setter checks the number before
  `best_fit` looks for 41.
- Short data is split with anchored patterns, and `$` also matches
  before a final newline, so `123\n` is a numeric chunk and a byte one.
- An inverted ASCII code with a border ends in a row of upper half
  blocks, because the extra row below the code counts as dark.
- With the circle drawers only the modules are circles: the finder
  patterns are drawn by the default eye drawer, which is the square one.
- The penalty rules skip a column after some comparisons, as qrcode's
  loops call `next()` on their own iterator.

License: qrcode is BSD (`LICENSE-qrcode`, which also carries the MIT
license of the JavaScript library it came from).
