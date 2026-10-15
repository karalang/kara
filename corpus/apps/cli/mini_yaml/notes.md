# mini_yaml

A YAML loader for the files that deploy tooling reads, answering what
PyYAML 6.0.1's `yaml.safe_load_all` answers. The run writes twelve files
into `data/` and loads each one. For every document it prints what
Python's `json.dumps(doc, indent=2, default=str)` prints, or PyYAML's
error message. Then it prints a deployment report read off the loaded
values.

The inputs:
- a three-document Kubernetes manifest (Deployment, Service, ConfigMap),
  with an anchor and alias for the labels, flow sequences, and `|` and
  `>` block scalars;
- a docker-compose file with an `x-common` block merged in by `<<:`, and
  port strings, one of which (`22:22`) YAML 1.1 reads as the number 1342;
- a GitHub Actions workflow, where `on:` is the boolean key `true` and
  the unquoted Python version `3.10` is the float 3.1;
- an Ansible playbook, where `become: yes` is a boolean and `mode: 0644`
  is the octal integer 420;
- an edge-case file: the Norway problem, octal, hex, binary and
  sexagesimal integers, `1_000_000`, `.inf`, dates and timestamps,
  escapes, `''` quotes, UTF-8, multi-line plain scalars, nested flow
  collections, `|+`, `|-` and `|2`, duplicate keys, and keys that Python
  treats as one key (`1` and `true`);
- a four-document stream;
- six broken files: a tab at the start of a line, an unterminated
  quote, a bad indent, an undefined alias, a `:` in a flow sequence, and
  a `?` inside a block sequence.

The report shows what such tooling reads off the values: the replica
counts and images, the service ports, the ConfigMap entries and their
lengths, the compose restart policies and ports (flagging the port read
as a number), the workflow's triggers (showing that `on` became `True`),
the Python matrix (showing `3.1`), and each Ansible task's module and
file mode.

**The loader** is a port of PyYAML's pipeline:
- the scanner: tokens, the indentation stack, and the simple-key rules
  that decide where a mapping key starts;
- the parser's state machine, which turns tokens into events;
- the composer: nodes, anchors and aliases;
- the safe constructor: YAML 1.1's implicit types, in PyYAML's resolver
  order, and merge keys `<<` (a mapping or a list of mappings).

Values follow Python's. A mapping is a dict, so the keys 1, 1.0 and True
are the same key and a later duplicate replaces the earlier value in
place. A date or a timestamp prints as `str()` of the `date` or
`datetime`. A float prints as `repr()` does. The error messages, with
their `in "data/f", line L, column C` marks, are PyYAML's.

Not supported:
- tags (`!!str`, `!custom`) and directives (`%YAML`, `%TAG`), which are
  reported as errors;
- complex keys (`? ` with a collection key), beyond the error case;
- the snippet lines PyYAML adds to an error read from a string (a file
  stream, which is what this reads, has none);
- `yaml.dump`.

**Oracle.** `expected.out` is PyYAML 6.0.1's and Python's json module's
output on the same files, made by `oracle.py`, which is run from this
directory after `mk.py` writes `data/`. The `data/` files the run writes
are byte-identical to the ones `mk.py` writes.

## Edits from mini_yaml.orig.kara to mini_yaml.kara

Author errors (the diagnostics were right):
- Three functions stored bare `Mark` parameters in a struct, and the
  scanner and parser read a token's `start` again after moving it.
  `Mark` is three integers, so it derives `Copy, Clone`, as the
  diagnostic suggested.
- `let files = ["deploy.yaml", ...]` is an array of `String` (a string
  literal is a `String` by default), so `files[i]` could not move out.
  It is `Array[Str, 12]`.
- `dump(docs[k], 0, out)` needed the `mut` marker on `out`.

Compiler gaps, worked around:
- B-2026-10-15-43: a local named `seq` in an `if` condition
  (`if seq { K_SEQ } else { K_MAP }`) is parsed as a `seq {}` block, and
  the error points at the `else`. The local is `is_seq`.

Written 2026-10-15 by the real-world apps thread; karac at 72cc0d4a00.
