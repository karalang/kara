# mini_inflection

[inflection](https://github.com/jpvanhal/inflection) 0.5.1, the Python
port of Rails' inflector. ORMs, API frameworks and code generators use it
to turn a model name into a table name (`BlogPost` → `blog_posts`), a
column into a label (`author_id` → `Author`) and a title into a URL slug
(`Crème brûlée — the recipe` → `creme-brulee-the-recipe`).

inflection is a thin layer over `re`: every rule is a pattern and a
replacement template, tried in order with `re.search` and applied with
`re.sub`. So this app carries [mini_re](../mini_re/)'s engine (Python
3.13's pattern parser and an sre-style backtracking matcher) and builds
inflection on top of it, the way the library is built on the standard
library.

The run writes two files into `data/`:
- `jobs.tsv`, the operations. A line `== name` starts a job; every other
  line is an operation and its arguments, separated by tabs;
- `unicode.txt`, the lowercase, uppercase and title-case mappings, the
  cased and case-ignorable flags, the combining class and the NFKD
  decomposition of every character the jobs reach.

Each operation prints `-- ` with its name and arguments, then the result's
`repr`, `ok`, or the error. The jobs are:
- `rails_pairs`: Rails' own singular/plural pairs through `pluralize` and
  `singularize`, in lower, capitalized and upper case;
- `nouns`: nouns an API names its resources with, and round trips;
- `identifiers`: class and column names through `camelize` (both forms),
  `underscore`, `dasherize` and `tableize`;
- `labels`: column names through `humanize`, `titleize` and
  `str.capitalize`, including Greek with its final sigma, title-case
  digraphs and ligatures;
- `slugs`: blog titles through `transliterate` and `parameterize` with
  seven separators;
- `ordinals`: `ordinal` and `ordinalize` on ints, floats, bools and the
  strings Python's `int()` accepts or rejects;
- `configure`: the irregular words, uncountables and rules an application
  adds at startup, including rules that do not compile and templates
  that refer to groups that do not exist, which raise when they are
  reached;
- `scaffold`: model definitions to the table, route, controller, labels
  and associations a Rails generator derives;
- `edges`: empty and odd inputs to every helper.

The output matches `oracle.py`, which runs the same jobs through
inflection (vendored unchanged in `inflection/`, MIT, see
`LICENSE-inflection`). Run `python3 mk.py && python3 oracle.py` to
regenerate `expected.out`.
