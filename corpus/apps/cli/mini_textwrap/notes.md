# mini_textwrap

Python 3.13's `textwrap`, ported from `Lib/textwrap.py`: `TextWrapper`
with every option, `wrap`, `fill`, `shorten`, `dedent` and
`indent`, plus a small `fmt` that refills each paragraph of a file.

The run writes five text files into `data/` and runs `data/jobs.tsv`,
93 jobs with tab-separated fields. In a field, `⏎` stands for a
newline, `⇥` for a tab, `¤` for a no-break space, `␍` for a carriage
return, `␋` for a vertical tab and `␌` for a form feed; a field that
starts with `@` names a file in `data/`. Each job prints `== ` and its
fields joined by ` | `, then each output line between bars, with the
same markers.

The files are a commit message with an em-dash, a URL longer than any
line and a trailer; a docstring to dedent, with a whitespace-only line
and a tab after the margin; lines indented with mixed spaces and tabs; a
tab-separated table; and Markdown notes with a long identifier, no-break
spaces and accented words.

The jobs cover:
- `wrap` at widths from 1 to 72, and its `ValueError` for a width of
  0 or less;
- splitting on hyphens (compound words, `co-op`, `-flag`, `--opt`,
  digits next to hyphens, runs of dashes) and em-dashes, and with
  `break_on_hyphens` off;
- long words broken at a hyphen or not, and `break_long_words` off;
- `expand_tabs`, `tabsize`, `replace_whitespace` and
  `drop_whitespace`, including text that is only spaces;
- `fix_sentence_endings`;
- `initial_indent` and `subsequent_indent`, including an indent wider
  than the width;
- `max_lines` with several placeholders, the cases where the
  placeholder moves to the line before, and the "placeholder too large"
  error;
- `fill` and `shorten`;
- `dedent` on tabs against spaces, blank and whitespace-only lines, and
  a line holding a carriage return;
- `indent` with the default predicate and with one that takes every
  line, over `\r`, `\r\n` and form-feed line ends;
- `fmt` over three of the files.

**The port** follows `textwrap.py` method by method:
`_munge_whitespace` with `str.expandtabs`; `wordsep_re` written out as
code (its lazy word match, the two hyphen lookbehinds and lookahead, and
both em-dash rules); `wordsep_simple_re`; `_fix_sentence_endings`;
`_handle_long_word`; and `_wrap_chunks` with its `max_lines` logic.
`dedent` follows its two regexes and its margin loop, and `indent`
uses `str.splitlines(True)`. Widths and lengths count code points, so
text is held as `Vec[char]`.

Not supported:
- `\w` is taken to be `is_alphanumeric` or `_`, and `\d` to be
  ASCII digits; Python's tables differ for some scripts and numerals.
- `indent` takes only the default predicate or one that takes every
  line.

**Oracle.** `expected.out` is Python 3.13.16's output for the same jobs,
made by `oracle.py`. Run `mk.py` first; it writes `data/`.
`gen_kara.py` prints the Kāra data section from the same files.

## Edits from mini_textwrap.orig.kara to mini_textwrap.kara

None. The first draft passed `karac check`, `karac fix` found nothing,
and the program matched expected.out on both MIR surfaces on the first
run.

Written 2026-10-18 by the real-world apps thread; karac at 0b54a904fd.
