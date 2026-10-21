# mini_pyhcl

pyhcl 0.4.5, the Python parser for HashiCorp's configuration language
(HCL 1) that `hcltool` runs and that deployment scripts, linters and
Vault and Nomad tooling use to read Terraform, Vault, Nomad, Consul and
Packer files into plain dicts. pyhcl is a PLY grammar: a lexer of
regex rules in five states and an LALR(1) parser whose actions build
the value. The port covers:
- the lexer: PLY's token loop (one master regex for each state, the
  ignored characters, the rule functions and their state changes), with
  pyhcl's rules for numbers, identifiers, strings with `\"` and `${...}`,
  heredocs (`<<EOF` and the tab-stripping `<<-EOF`, CRLF line ends
  included), line and block comments, line counting, and the errors
  `t_error` and the end-of-input rules raise, with their line, column
  and index;
- `export_comments` (`LINE`, `MULTILINE`, `ALL`), which turns comments
  into `comment-L<line>` items and comment tokens;
- the parser: PLY's LALR tables for pyhcl's 131 productions, the parse
  loop (a lookahead is read only when a state needs one, so an action's
  error comes before a lexer error further on), and `p_error`'s message
  with the tokens the state expected, in the table's order;
- the actions: the object lists, `objectlist_flat`'s handling of keys
  given twice (at the top level a later value replaces the earlier one
  and a block merges into it; inside a block they make a list), blocks,
  lists, the arithmetic in keys and numbers, the conditional and
  comparison forms, functions and `flatten`;
- `hcl.loads`: JSON when the text opens with `{` (Python 3.13's `json`
  decoder, from `mini_json`), and hcltool's output, `json.dumps` with
  `sort_keys` and an indent of 4.

`mk.py` writes `data/`:
- `infra/`: a small company's infrastructure repository: the Terraform
  variables, main configuration (also as `main.tf.json`) and outputs of
  a web service, two Vault policies, a Nomad job, a Consul agent and a
  Packer template;
- `cases/` and `broken/`: 56 files for the corners: keys given twice,
  merges, numbers, strings, heredocs, comments, functions, and files
  each lexer, parser and action error comes from;
- `fixtures/`: the 51 decoder and lexer fixtures of pyhcl's tests;
- `jobs.tsv`: 149 jobs. They load every file (some with each
  `export_comments` mode), list the tokens of the lexer fixtures, and
  report on the repository: the Terraform files' providers, variables,
  resources, modules and outputs, which requests each Vault policy
  allows, the Nomad job's resources, and the Consul agent's settings.

Each job prints `-- ` and its columns, then its output. A Python
exception prints `! `, its class and its message.

`oracle.py` runs the jobs through pyhcl itself (the copy in `hcl/`,
unchanged, with the PLY 3.11 it bundles) and wrote `expected.out`.
`gen_kara.py` writes the generated section at the end of
`mini_pyhcl.kara`: each lexer state's master regex as PLY builds it, the
rule each group belongs to, PLY's LALR tables, and `data/`.

PLY lexes with `re`, so this carries the regex engine of `mini_re`
(Python 3.13's parser and an sre-style backtracking matcher). Python's
values are kept as they are: lists and dicts live in a heap and are
shared, not copied, because pyhcl's merging changes dicts and lists
that other values still hold, and its results show it.

The quirks are kept. Some examples:
- a key with arithmetic (`a + 1 = 2`) is a tuple, which `json.dumps`
  cannot write;
- `objectlist_flat` merges a block into a value that is not a dict by
  calling `.keys()` on it, an `AttributeError`, or assigns into a string,
  a `TypeError`;
- `flatten` raises a `TypeError` whose message is its arguments, the
  format string unfilled;
- `1e17` written as `100000000000000000.0e2` goes through `str()`
  first, which makes it `1e+17e2`, and `float()` refuses it.

pyhcl is under the Mozilla Public License 2.0, in `LICENSE-pyhcl.txt`,
and so are its test fixtures; PLY is under the BSD license, in
`LICENSE-ply.txt`.
