# mini_barcode

Barcodes as python-barcode 0.16.1 draws them. python-barcode is the
pure-Python library that shops, warehouses and libraries use to turn a
product number into an SVG label. The port covers:
- every barcode class the library offers: EAN-13 (with and without
  guard bars), EAN-8, EAN-14, JAN, ISBN-13, ISBN-10, ISSN, UPC-A,
  Code 39, PZN, Code 128, GS1-128, Codabar and ITF, each with its check
  digits, its keyword arguments and the errors it raises;
- Code 128's charset switching between A, B and C, including the state a
  `Code128` object keeps from one build to the next;
- the `SVGWriter` and `BaseWriter.render`: packing modules into bars,
  guard blocks and the text placed between them, every writer option,
  and the document as minidom's `toprettyxml` and `toxml` write it;
- `Barcode.save`, `barcode.generate` and the `python-barcode` command
  line (`list`, `create`, `-v` and argparse's errors);
- three programs:
  - shelf labels for a shop's `data/products.csv`: the code is
    recognised by its length, the supplier's check digit is checked, an
    SVG label is written for each good row and listed in `index.csv`;
  - shipping labels for `data/shipments.jsonl`: a GS1-128 label with the
    SSCC, GTIN, expiry, batch, serial and count elements and an ITF-14
    carton label, after checking the inputs;
  - a library catalogue from `data/books.txt`: ISBN-10, ISBN-13 and ISSN
    numbers are checked, turned into EANs, written to `catalog.tsv` and
    drawn as spine labels with guard bars.

The run writes `data/`, works through the 134 jobs in
`data/jobs.jsonl`, and then runs the three programs. `mk.py` wrote
`data/`. A render prints the whole SVG document, or in most jobs the
document with the bars between the first and the last counted.

`oracle.py` runs the same jobs and programs through python-barcode
itself and wrote `expected.out`. It uses the `barcode/` package from the
0.16.1 wheel, unchanged except that the DejaVu font it ships for the
Pillow writer is left out. The oracle hides Pillow, so `ImageWriter` is
`None` and `python-barcode list` says `Pillow: disabled` wherever it
runs. The command line prints absolute paths; the oracle prints them
relative to the app's directory, as `./data/...`, and the port does the
same. `create -c` writes gzip files and is not covered.

The quirks are kept. Some examples:
- ISBN-10 and ISSN build their EAN-13 with their own digit count, 9 and
  7, so `get_fullcode()` gives a 10-digit or 8-digit "EAN" and a short
  ISBN-10 is refused with "EAN must have 9 digits". The library program
  notices and asks EAN-13 for the whole number.
- A `Code128` object starts each build in the charset its last build
  ended in. `12a` encodes as `[105, 12, 100, 65]` the first time and
  `[104, 17, 18, 65]` the second, so reading `.encoded` before a render
  changes the bars that are drawn.
- An empty Code 128 fails with `IndexError: list index out of range`,
  and an empty Codabar with `IndexError: string index out of range`.
- Code 39 looks up the checksum before it checks the characters, so a
  bad character is a `KeyError` unless `add_checksum=False`.
- EAN-14 is drawn with EAN-13's layout, and ISSN's check digit can be
  11, which gives `000000011`.
- JAN's country check runs `int()` on the first three characters, so
  ` 49` and `4_9` are both read as 49.
- With guard bars the text is split into blocks between the guards; a
  `human` text is drawn once for every block, a shorter text fills the
  first blocks only, and `guard_height_factor=1` turns the guards off.
- Bars whose colour equals the background are not drawn at all, and an
  option set once (such as `human`) stays on the writer for later
  renders.

## License

python-barcode is under the MIT License, in `LICENSE-python-barcode.txt`.
