# mini_dockerfile

dockerfile-parse 2.0.1 by Red Hat, the Python library container build
tools (OSBS, atomic-reactor) use to read and rewrite Dockerfiles. The
port follows the package:
- `structure`: instructions with their start and end lines, raw content
  and value, line continuations, comments (also inside a continuation),
  the `# escape=` and `# syntax=` directives at the top, the backtick
  escape, CRLF lines;
- `json`, `parent_images` (with build-arg substitution into `FROM`),
  `is_multistage`, `baseimage`, `cmd`, and `labels`, `envs` and `args`
  for the last stage, with `$VAR` and `${VAR}` substitution, build args,
  parent image ENV and `env_replace`;
- `context_structure`, the args, envs and labels in force at each
  instruction and the ones it sets;
- `WordSplitter` (quotes, backslashes, substitution) and the
  `name value` and `name=value ...` forms of `LABEL`, `ENV` and `ARG`;
- the setters for `baseimage`, `parent_images`, `cmd`, `labels`, `envs`
  and `args`, single-label changes, `add_lines` (`all_stages`,
  `at_start`, `skip_scratch`), `add_lines_at` (line number, instruction
  or line text, `replace`, `after`) and `_delete_instructions`, with
  values quoted by `shlex.quote`.

The `FROM` pattern and the structure patterns run on a small
backtracking regex engine with Python's semantics. Every property reads
the file again and every setter writes it, as the library does.

`mk.py` writes `data/`. It holds:
- Dockerfiles as people write them: a multi-stage Go service with build
  args and a platform, a Python web app, a Windows image with the
  backtick escape, a legacy file with lowercase instructions, three
  stages with a `scratch` one, a CRLF file;
- files for the corners: quoting and substitution, continuations with
  blank lines, comments and trailing spaces, bare `FROM` and `RUN`, a
  `LABEL` before the first `FROM`, a `LABEL` missing its `=`, no `FROM`
  at all, an empty file, labels with accents;
- `jobs.tsv`, the jobs.

Each job copies its Dockerfile to `data/work/Dockerfile` and opens it.
A read-only job prints the instructions, the JSON, the properties or the
contexts. A changing job prints any error it raised and then the file
as it was left. `oracle.py` runs the jobs through the package itself
(`dockerfile_parse/`, unchanged) and wrote `expected.out`.
`gen_kara.py` writes the data section.

The quirks are kept. Some examples:
- a blank line ends a continuation, so the line after it is read as an
  instruction of its own (`GIT`);
- `FROM --platform=linux/amd64` with no image gives the image `4`: the
  platform group gives back one character so the image can match;
- changing a stage's image drops its `--platform`;
- setting the parents to what `parent_images` returned still rewrites
  `FROM golang:${GO_VERSION}`, since the setter compares with the text
  before substitution;
- `add_lines_at` with a line number past the end fails on a bare
  `assert`.

dockerfile-parse is under the BSD 3-Clause License
(`LICENSE-dockerfile-parse.txt`).
