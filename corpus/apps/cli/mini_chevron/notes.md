# mini_chevron

chevron 0.14.0, the Mustache template renderer for Python, and its
`chevron` command. Mustache is the logic-less template language behind
config generators, e-mail templates, static sites and code generators;
chevron is a small, fast Python implementation of it. The port covers:
- the tokenizer: literals and tags, standalone lines (a section, comment,
  partial or set-delimiter tag alone on its line takes the line with
  it), set-delimiter tags, the triple mustache and `&`, and chevron's
  error messages with their line numbers;
- the renderer: a stack of scopes, dotted names looked up by subscript,
  then by attribute, then as a list or string index (negative too),
  sections over lists, over a value that becomes the scope, and over
  falsy values, inverted sections, the implicit iterator `{{.}}`;
- partials from a dictionary or from a directory of files with an
  extension, indented when the partial tag is alone on an indented line;
- Python's `str()` and `repr()` of what `json.load` gives (lists and
  dicts print as Python values, with the quotes Python picks).

`mk.py` writes `data/`: `cases.txt`, 73 templates with their
JSON data, partials and delimiters, and `site/`, the files of six
command-line jobs: an nginx config with a partial for each location, an
order e-mail in HTML with a header and footer, a Kubernetes deployment,
a GitHub Actions workflow saved with CRLF line ends that uses `<% %>`
delimiters so it can keep its own `${{ ... }}`, the nginx config again
without data, and a template with a section closed under the wrong
name.

The run writes `data/`, renders each case and runs each job the way the
`chevron` command does (`chevron.main.main`: the template file, the
JSON data file, a partials directory and extension, the delimiters and
`-w`). Each prints the warnings chevron writes to stderr, then the
output (a line per line, with `\r` shown) or the error.

`oracle.py` runs the same through chevron itself (the copy in
`chevron/`, unchanged; PyYAML is kept out so the data is read as JSON)
and wrote `expected.out`. `gen_kara.py` writes the data section at the
end of the program.

The quirks are kept. Some examples:
- The tokenizer is a generator that keeps the line count in module
  globals, and every tokenize() resets them, so after a partial the
  line numbers in the outer template's errors are wrong.
- A tag opener at the very end of a template is dropped, an empty tag
  `{{}}` is an IndexError, and a lookup that subscripts a number is a
  TypeError, all out of Python rather than chevron.
- `{{x.real}}` and `{{n.denominator}}` work on numbers, because the
  lookup falls back to getattr.
- An indented partial is indented only when the text before it on the
  line is all whitespace, so `- {{>container}}` in a YAML list is not,
  and a missing partial on an indented line leaves the indentation in
  front of the next line.
- Inside a list section the counting of nested sections with the same
  name ignores inverted ones, so `{{^list}}...{{/list}}` there ends the
  list early and the outer end pops the last scope.
- In a comment, `{{! for {{env}} }}` ends at the first `}}`.
- Only spaces are trimmed before a standalone tag; a tab stays.

License: chevron is MIT (`LICENSE-chevron`).
