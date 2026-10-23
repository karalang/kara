# mini_graphql

A port of the language module of
[graphql-core](https://github.com/graphql-python/graphql-core) 3.2.6 (MIT;
the GraphQL Contributors, Syrus Akbary, Christoph Zwerschke), the Python
reference implementation of GraphQL that Strawberry, Graphene and Ariadne
build on. It covers the lexer, the recursive-descent parser for executable
documents and the type system definition language, `print_ast`, and the
way `GraphQLError` prints a syntax error with the lines around it. The
programs check a client app's queries the way its build tooling does.

```
> !manifest app/queries/admin.graphql ...
  app/queries/broken.graphql: skipped, Syntax Error: Unexpected ')'. at 4:19
  query AdminDashboard  [app/queries/admin.graphql]
    variables: none
    fragments: MoneyFields, ProductCard
    missing fragments: MissingFragment
    root fields: stats
    depth: 4
    document: 373 characters, id 4852bc67
```

## What it does

`mk.py` writes 144 files to `data/`: 133 GraphQL documents in
`data/cases/`, one per case, and a small client app in `data/app/` (seven
query files, a two-file schema, and a JSX and a TypeScript file with
`gql` templates). It writes `data/jobs.txt`, 152 jobs in 11 sections.
`oracle.py` runs them against the vendored library and prints each
result; `mini_graphql.kara` runs the same jobs.

- **Parse and print** (119 `parse` jobs): `parse()` then `print_ast()`,
  or the error. They cover graphql-core's own two kitchen-sink fixtures,
  what the printer keeps and drops, string escapes and block strings,
  descriptions, legacy fragment variables and the `max_tokens` limit,
  34 parser errors, 37 lexer errors, and nine cases about where an
  error is reported.
- **Values and types** (20 jobs): `parse_value`, `parse_const_value` and
  `parse_type` on one value or type each.
- **Tokens** (8 jobs): the lexer's token chain, comments included, with
  each token's `repr()` and its line and column.
- **`!format`** checks that each query file is already in `print_ast`
  form, and prints the formatted text when it is not. **`!manifest`**
  builds a persisted-query manifest: each operation's variables, the
  fragments it uses (and any that are missing), its root fields, its
  depth, and an FNV-1a id over the printed operation and its fragments.
  **`!schema`** summarises a schema split over files, with extensions
  merged in, and lists unknown types and directives with where they were
  first used. It runs twice: on the app's schema and on the schema
  kitchen sink. **`!extract`** finds the `gql` templates in JavaScript
  sources and parses each one at its place in the file.

## How the port works

- **A source is a `Vec[char]`.** Python indexes a `str` by code point, so
  the port keeps the body as characters and every position, column and
  length (the 80-column field rule, the 70-character block-string rule)
  counts code points.
- **The lexer is a token chain.** As in graphql-core, `lookahead()` lexes
  the next token onto the chain and skips comments, and the parser keeps
  a current and a last token. The chain is a `Vec[Token]`, which is also
  what the `tokens` jobs print.
- **The AST is typed.** Values, type references and selections are
  enums; one `TsDef` struct holds every type system definition and
  extension, with the lists a kind does not use left empty. `print_ast`
  becomes one function per node kind, keeping the printer's `join`, which
  drops empty strings, and its `wrap`, `indent` and `block`.
- **Error locations keep their quirks.** `Source.get_location` runs
  `splitlines()` over the text before the error, so a position just after
  a line break reports the end of the line before, and the form feed,
  vertical tab, file and group separators, NEL, and the Unicode line and
  paragraph separators all count as line breaks. The excerpt splits only
  on `\r\n`, `\n` and `\r`. When the two disagree, `str()` of the error
  raises `IndexError`; the oracle prints that, and so does the port.
  Lines longer than 120 characters print in 80-column chunks, and a
  `gql` template's errors are offset to its line and column in the file.
- **The lexer's `IndexError`s are kept.** A backslash or an unfinished
  `\u` escape at the very end of a string makes graphql-core index past
  the body, so three of the lexer cases raise `IndexError` instead of a
  syntax error. The port returns the same error.

## Compiler gaps

None. `karac check` asked for an `own` parameter on one function, a
`mut` marker on 187 call-site arguments that forward a `mut ref`
parameter (`karac fix` made all 188 of these changes), signed lengths in three
loops, and flagged that the structs named `Arg` and `Parser` shadow
prelude types. After those, the interpreter matched the oracle byte for
byte on its first run, as did the LLVM, ASan and native builds.

## Scope

- The language module only: no schema building, validation or execution.
- No visitor. The programs walk the AST directly.
- `no_location` changes nothing the jobs print, so the port accepts the
  option and ignores it.
- A body read from a UTF-8 file cannot hold a surrogate, so the lexer's
  surrogate-pair branches only matter inside `\u` escapes.

## Running

```bash
karac run mini_graphql.kara                       # interpret
karac build --mir mini_graphql.kara               # native binary
python3 -s -P -B mk.py                            # regenerate data/
python3 -I -B oracle.py > expected.out            # regenerate expected.out
python3 -I -B gen_kara.py                         # regenerate the embedded data
```
