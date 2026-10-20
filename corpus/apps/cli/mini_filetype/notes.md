# mini_filetype

filetype 1.2.0, the Python library that tells a file's type from its
first bytes, as upload checks and media pipelines use it. The port
follows the package:
- the 79 matchers in the package's order (images, audio, video, fonts,
  documents, archives, applications), each with its own length checks;
- the ones that read structure: APNG's chunk walk to an `acTL` before
  `IDAT`, the ISO-BMFF `ftyp` box with its major and compatible brands
  (read with `decode(errors='ignore')`) for MP4, MOV, HEIC and AVIF, the
  ZIP entry names that tell Word, Excel and PowerPoint files from plain
  ZIPs, the `mimetype` entry of OpenDocument files, the OLE signatures of
  the old Office formats, the byte order mark before `%PDF`, Matroska and
  WebM doctypes, and zstd's skippable frames;
- `guess`, `guess_mime`, `guess_extension`, the per-family `*_match`
  functions and `is_*` helpers, `get_type`, `is_extension_supported`,
  `is_mime_supported` and `add_type`, over the first 8192 bytes;
- `python -m filetype`, with argparse's handling of `-h`, `-f/--file`
  and `-v/--version`, long-option prefixes and its errors.

`mk.py` writes `data/`. It holds:
- the first bytes of files as the tools that make them write them: PNG
  and APNG chunks, a JFIF JPEG, TIFF both ways, HEIC, HEIF and AVIF
  boxes, MP4, MOV, 3GP, Matroska and WebM headers, WAV and AVI RIFF
  files, Word, Excel and PowerPoint ZIPs in Microsoft's and LibreOffice's
  entry order, OpenDocument files, OLE documents, a tar, an ELF binary,
  a `.deb`, zstd frames with skippable frames before them, and more;
- files that sit on the edge of a matcher: text that starts like a
  format, a Matroska doctype just before and just after byte 8192, an
  `ftyp` box longer than the file, brands that are not UTF-8;
- `jobs.tsv`, the jobs.

A `guess` job prints what each function says about a file, and a `head`
job does the same for its first N bytes. `get_type`, `supported` and
`types` jobs run the lookups. A `cli` job runs `filetype` in `data/` and
prints its output, its errors and its exit code. A `custom` job adds a
matcher of its own with `add_type`, which stays in the list for every
later job. `oracle.py` runs the jobs through the package itself
(`vendor/filetype/`, unchanged) and wrote `expected.out`. `gen_kara.py`
writes the data section.

The quirks are kept. Some examples:
- a text file that starts with `BM` is a BMP, one that starts with
  `GIF` is a GIF, and one that starts with `%!` is PostScript;
- an `.m4v` with the usual `mp42` and `isom` brands is an MP4, since MP4
  comes first;
- the 3GP matcher looks for `ftyp3gp` at offset 0, where no 3GP file has
  it, so a 3GP is an MP4 when it lists `isom` and unknown when it does
  not;
- an EOT font as Microsoft's format lays it out is not recognised, since
  the matcher reads the version bytes in a different order;
- an MSI installer is taken for a PowerPoint file;
- the archive family's DICOM matcher compares three bytes with four and
  never matches;
- a Word file whose `word/` entry comes sixth or later is a ZIP.

filetype is under the MIT License (`LICENSE-filetype.txt`).
