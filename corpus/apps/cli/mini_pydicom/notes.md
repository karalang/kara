# mini_pydicom

A port of the file layer of [pydicom](https://github.com/pydicom/pydicom)
3.0.1 (MIT; Darcy Mason and pydicom contributors, with a private dictionary
generated from GDCM's, BSD), the pure-Python DICOM library most Python
medical-imaging tools read their files with. It covers `dcmread`, the lazy
conversion of raw elements into values, `str(dataset)`, `walk`,
`remove_private_tags` and `dcmwrite`. The vendored copy is trimmed to the
modules the oracle imports, and numpy is blocked, so pydicom runs its
pure-Python paths.

```
> show ct_explicit.dcm
Dataset.file_meta -------------------------------
(0002,0000) File Meta Information Group Length  UL: 176
(0002,0001) File Meta Information Version       OB: b'\x00\x01'
(0002,0002) Media Storage SOP Class UID         UI: CT Image Storage
(0002,0010) Transfer Syntax UID                 UI: Explicit VR Little Endian
...
(0008,0008) Image Type                          CS: ['ORIGINAL', 'PRIMARY', 'AXIAL']
> show bad_nested.dcm
error: BytesLengthException: With tag (0018,6011) got exception: Expected total bytes to be ...
> convert mr_implicit.dcm big out/big_mr_implicit.dcm
convert mr_implicit.dcm to big: 1168 bytes, crc32 0f6b050c
```

## What it does

`mk.py` builds 27 input files byte by byte (so the broken ones are broken
exactly where they need to be) and writes `data/jobs.txt`, 94 jobs.
`oracle.py` runs each job on the vendored pydicom; `mini_pydicom.kara` runs
the same jobs on the same files, which it writes to `data/` first.

- The inputs: a CT in explicit VR little endian with the ISO_IR 100
  character set, two private blocks (GEMS_IDEN_01 and the Siemens CSA
  header), FL/FD/AT/SL/SS/UL values and a UN element whose dictionary VR is
  FD; an MR in implicit VR with an undefined-length sequence inside another
  and signed pixel values; a big-endian US with a Greek name in UTF-8; a
  deflated SR; six secondary captures that form two studies; and the odd
  ones: no preamble, no transfer syntax, a transfer syntax the data
  disagrees with, encapsulated JPEG pixel data, a US value of odd length,
  a bad value inside a nested item, text values with backslashes, quotes,
  Latin-1 and padding, empty and long values, an unterminated sequence and
  OB, a value and an item cut short, UN elements, a VR switch mid-file,
  group length elements, a text file and a 64-byte file.
- **`show`** (30 jobs) prints `str(dcmread(path))`, with `force=True` on
  two, and on five of the files the write jobs produce.
- **`get`** (35) and **`pn`** (5) read one keyword: DS and IS values with
  their original strings, an IS that is not an integer, multi-values,
  missing and empty elements, UIDs, pixel data, and a person name's
  family, given, middle, prefix, suffix, ideographic and phonetic parts.
- **`resave`** (8) reads with `force=True`, writes the dataset back and
  reports the size, the CRC-32 and whether the bytes are identical.
- **`anon`** (4) walks the dataset setting every PN to `Anonymous`,
  deletes two elements, sets PatientID, removes the private tags and
  writes it.
- **`convert`** (10) sets the file meta's transfer syntax to implicit,
  explicit, big endian or JPEG Baseline and writes the file.
- **`index`** (2) builds the study, series and instance tree of a set of
  files, and lists the ones that are not DICOM.

## How the port works

- **Reading** follows `filereader.py`: the preamble and `DICM`, the file
  meta (retried as implicit VR when its first element does not convert),
  the command set, the transfer syntax with its fallbacks when the meta has
  none, raw inflate for the deflated syntax (the inflater of mini_png),
  `data_element_generator` with undefined-length values, encapsulated
  pixel data, sequences and items of either length, and the peek at the VR
  bytes that `_is_implicit_vr` uses to overrule a wrong transfer syntax.
  A dataset read without a byte length loses every element on an
  EOFError, as pydicom's dict comprehension does; one with a length keeps
  what it read.
- **Conversion is lazy**, as in `Dataset.__getitem__`: an element stays a
  `Raw` until something reads it, then `hooks.py` picks the VR (the
  dictionary for implicit and UN, the private dictionary for private tags,
  the private creator found through `__setitem__`), `values.py` converts
  it, a number string that does not parse is retried through
  `convert_retry_VR_order`, and the ambiguous VRs (pixel data, the US-or-SS
  tags, overlays) are resolved. The error messages carry the same
  suffixes, and `tag_in_exception` wraps an error inside a sequence item
  with the sequence's tag, but not one at the top level, which escapes from
  the iteration itself.
- **Printing** is `Dataset._pretty_str` and `DataElement.__str__`: the
  35-column name, the repval summaries ("Array of N elements" over 16
  values or bytes), a UID's name, and the `repr` of each value type, which
  differs between DSfloat, IS, ISfloat, PersonName and MultiValue.
- **Writing** is `dcmwrite` with its defaults: the encoding comes from the
  transfer syntax or from the dataset; raw elements are copied through when
  the encoding is unchanged and converted when it is not; each VR has its
  writer and padding; sequence items get their lengths filled in after they
  are written; group length elements past group 6 are dropped and the file
  meta's is recomputed; and pixel data under a compressed syntax becomes
  undefined length and must start with an item tag.
- **The dictionaries** are the parts of pydicom's three that the inputs
  can reach, chosen by `gen_kara.py` and embedded as text.

## Compiler gaps

None found. `karac fix` applied 82 machine fixes, 78 of them the `mut`
marker on an argument to a `mut ref` parameter, which I kept leaving off;
the others were a `#[derive(Clone)]` and clones at three moves. By hand,
while checking the parts as I wrote them: `private` is a keyword, so a
field became `privs`; an `Entry` enum shadowed the prelude's and became
`Node`; `char.try_from` and `bits_as_f64` instead of the `from_u32` and
`from_bits` I reached for; and parameters that keep their argument need
`own`. Once it checked, the interpreter, the LLVM backend, the ASan build
and the native binary all matched the oracle byte for byte on the first
run, with nothing on stderr.

## Scope

- Writing a deflated dataset is not ported (it would need zlib's own
  compressor to match the bytes); no job writes one.
- Only the Latin-1 and UTF-8 character sets decode and encode; the other
  codecs keep their names but are treated as Latin-1. No input uses them.
- The US-or-SS correction looks at the dataset itself, not at the pixel
  representation of enclosing datasets; no input nests those tags.
- Not ported: deferred reads, `stop_before_pixels`, the date and time
  value types (`datetime_conversion` is off by default), pixel data
  handlers, JSON, `FileSet`/DICOMDIR, and `enforce_file_format=True`.

## Running

```bash
karac run mini_pydicom.kara                  # interpret
karac build --mir mini_pydicom.kara          # native binary
python3 -s -P -B mk.py                       # regenerate data/
python3 -I -B oracle.py > expected.out       # regenerate expected.out
python3 -I -B gen_kara.py                    # regenerate the embedded data
```
