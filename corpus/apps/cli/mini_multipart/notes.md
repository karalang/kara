# mini_multipart

python-multipart 0.0.32, the streaming form parser that Starlette and
FastAPI use for every file a browser or curl uploads to them. These parts
of `python_multipart/multipart.py` and `decoders.py` are ported:
- `MultipartParser`, the state machine that finds the boundaries of a
  `multipart/form-data` body fed to it in chunks of any size. It keeps a
  look-behind for a boundary split across two chunks and limits header
  count and size.
- `QuerystringParser` and `OctetStreamParser`, for the other two bodies a
  form can send.
- `parse_options_header`, which reads `Content-Type` and
  `Content-Disposition` values. That includes the full Windows paths IE
  sends as a file name.
- the Base64 and quoted-printable decoders for
  `Content-Transfer-Encoding`.
- `FormParser`, `Field` and `File`. Fields stay in memory. Files spill to
  disk once they pass `MAX_MEMORY_FILE_SIZE`, under their own name if
  `UPLOAD_KEEP_FILENAME` asks for that.

The library's quirks are kept. Some examples:
- The quoted-printable decoder holds back only two bytes. A chunk that
  splits `=C3` after the `C` therefore decodes the `=` alone.
- A Base64 part is fed to the decoder with its CRLFs, which count toward
  the four-byte groups. Whether a part decodes can depend on where the
  chunks fall.
- A Base64 or quoted-printable cache is only flushed for the last part,
  after its field has been reported.
- `actual_file_name` is the bare file name, not the path the file went to.
- A UNC path loses its leading backslash to the unquoting and keeps the
  rest.
- An octet-stream body that goes to disk under `UPLOAD_KEEP_FILENAME` dies
  with `AttributeError`, because a `File` with no name has no `_file_base`.

The run writes `data/jobs.jsonl`, request bodies under `data/bodies/` and
raw HTTP requests under `data/requests/`. It then runs the 234 jobs, each
printed after `== ` and the job:
- `mp`, `qs` and `octet` jobs feed a body to a parser in chunks of a given
  size and print each callback. A run of one data callback is joined into a
  line with its count.
- `options`, `b64` and `qp` jobs call `parse_options_header` or a decoder.
- `form` jobs parse a body as an upload handler would, with headers, a
  chunk size and a config. `parse_form` passes no config to the parser, so
  the handler is `parse_form` with a config added.
- the last job is a small upload server. It reads each request in
  `data/requests/`, saves the files it carries into `data/uploads/`
  (spilled ones go there by name), prints a status for each request, and
  lists the directory at the end.

Temporary files go to `data/tmp/`, are deleted at the end of each job, and
their random names are never printed.

`oracle.py` runs the same jobs through the real python-multipart
(`pip install python-multipart==0.0.32`, CPython 3.13) and wrote
`expected.out`.

Author errors:
- a `mut` inside a tuple pattern (`let (key, mut val)`; in Kāra the
  `mut` goes before the whole pattern);
- a Base64 decoder that stopped at the first complete pad. CPython 3.13's
  non-strict `a2b_base64` skips every `=` and only checks the padding at
  the end. `expected.out` showed the difference on `Zg==Zm9v`.

Compiler gaps:
- `fs.write_bytes(path, Vec.new())` is refused with "expected
  'Slice[u8]', found 'Vec[?T]'": an unsolved `Vec.new()` is not pinned by
  a `Slice[T]` parameter, although B-2026-10-06-124 made a `Vec[T]`
  parameter pin it (B-2026-10-19-1, Thread A). Worked around with an
  annotated `let empty: Vec[u8]`.

Written 2026-10-19 by the real-world apps thread; karac at b6761f33ac.
