# mini_tinytag

tinytag 2.3.2, the audio metadata reader for Python by Tom Wallroth, Mat
(mathiascode) and contributors, on a shelf of small audio files. Music
players, library managers, podcast feeds and upload checks all read tags
from files they did not write. They have to know the format from the
name or, failing that, from the first bytes, accept tags in four text
encodings and three ID3 versions, estimate a duration without decoding
audio, and say clearly which file was broken and why.

The port follows tinytag's own model. A tag is the attribute dict of a
Python object: the public fields in `__init__` order, then `images`,
`other` and the private attributes. `_set_field` sends a second, different
value to `other` and renames an `other.` field that collides with an
attribute (`_filename`, `_other`, `_bitrate`), and `_update` merges a
nested parser's tag (FLAC inside ID3, MPEG after ID3, ID3 inside WAV and
AIFF, FLAC inside Ogg) the same way. The Ogg page reader is a Python
generator whose state the packet loop changes; the port runs the two in
line so each packet is handled as the generator yields it.

The inputs in `data/` were made by `mk.py`:
- **MP3**: ID3v2.3 with most of the mapped frames, iTunes comments, TXXX,
  PRIV XMP, synced lyrics, two pictures and an ID3v1 tag (song.mp3);
  ID3v2.4 with an extended header, a Xing header, invalid UTF-8 and an
  unknown frame with a bad encoding byte (vbr.mp3); a VBRI header
  (vbri.mp3); ID3v2.2 with layer II frames after garbage, a lone UTF-16
  surrogate and a picture (old.mp3); ID3v1 only (v1only.mp3); and a bare
  MPEG stream with no tag or extension (stream.dat).
- **FLAC**: STREAMINFO, Vorbis comments and two pictures (album.flac),
  and FLAC behind an ID3 tag (tagged.flac).
- **Ogg**: Vorbis with the comment packet split across pages and a
  picture in METADATA_BLOCK_PICTURE, Opus, FLAC in Ogg and Speex.
- **WAV**: PCM with LIST INFO, an ID3 chunk and XMP; WAVE_FORMAT_EXTENSIBLE
  with six channels; IMA ADPCM with a fact chunk.
- **MP4**: AAC with ilst items, a cover, `----` custom atoms, unknown
  items and an XMP uuid atom; ALAC.
- **ASF** (show.wma), **AIFF** with NAME/AUTH/ANNO/(c), an ID3 chunk and
  XMP, and **AIFC** with `sowt`.
- Broken and mislabelled files: empty, plain text, bad Ogg/FLAC/WAV/MP4
  headers, an MP4 whose mvhd is too short, an ID3 tag cut short, FLAC
  named `.bin`, Opus named `.mp4`, and bytes with no known magic.

The program runs the jobs in `data/jobs.txt`:
- `tinytag ...`: the command line (`python -m tinytag`) with those
  arguments: json, csv, tsv and tabularcsv output, `-i`/`--save-image`
  for one file and for several (numbered names), `-s`, `-h`, an unknown
  format, and the error lines and exit status for broken files.
- `get`: `TinyTag.get` with `tags`, `duration`, `image` and
  `check_magic_bytes` switched off and on, printing `as_dict()` and the
  images.
- `images`: the images of a file, as `Image.__repr__` prints them, and
  `images.any`.
- `supported`: `TinyTag.is_supported` for some names.
- `files`: the cover art the command line saved in `data/out/`.

Not ported: `file_obj` (the port always reads a file by name), the
`encoding` override, the deprecated `get_image`, `audio_offset` and
`extra`, and `TINYTAG_DEBUG` output.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the
same jobs through the copy of tinytag in `tinytag/` (MIT licence, in
`LICENSE-tinytag.txt`). `gen_kara.py` writes tinytag's lookup tables
(frame ids, genres, picture types, the Vorbis, RIFF, ASF and MP4 maps,
the MPEG tables and the extension map) and the data files into the Kāra
source.

## Edits from the first draft

Author errors:
- An enum named `MV` is Const-class; enum names are PascalCase, so it is
  `Mv`.
- `let (mut typ, mut header_size, mut size) = ..` is Rust syntax; Kāra
  puts `mut` before the whole pattern.
- `karac fix` added the call-site `mut` markers (66 of them) and one
  `.clone()` where `as_dict` pushed a borrowed loop element.

After that, the output matched `expected.out` on `__mir-run` and
`__mir-llvm-run` on the first run.

Written 2026-10-21 by the real-world apps thread; karac at 30d9fe87f9.
