# mini_graphviz

The source-building side of the Python `graphviz` package: graphs and
digraphs assembled a statement at a time, with the package's quoting of
DOT identifiers, its attribute lists, clusters and nested subgraphs,
copies, clears and saved `.gv` files. Nothing is rendered, so no Graphviz
binary is involved.

The program writes its own script (`data/script.txt`), plays it and prints
a report. `oracle.py` plays the same script against the vendored
`graphviz` 0.21 (`vendor/`, MIT) and is the reference; `mk.py` writes the
script and `gen_kara.py` embeds it in the program.

## The script

- Each line is split into words at runs of spaces. Blank lines and lines
  whose first word starts with `#` are skipped. Every other line is echoed
  as `> ` followed by its words joined by one space, then runs.
- A problem is reported as `  error: script.txt:<line>: <message>`, and the
  line then changes nothing. The messages are the package's, except the
  ones this README gives in full.
- **Text.** In a word read as text, `\s` is a space and `\N` a newline;
  every other character, a backslash included, is itself.
- **Values.** Names, labels, attribute values and the `name` and
  `comment` options are text, except that a first `!` makes the rest a
  `nohtml` string and a first `%` makes it `escape(rest)`: the text with
  every backslash doubled, also nohtml. Attribute keys are plain text.
- **Attributes** are `K=V` words (split at the first `=`); a word without
  `=` is `bad attribute <word>`. They form a dict: a repeated key keeps the
  last value. A `node` or `edge` takes the key `label` out as its label;
  the rest are passed as the package's `_attributes`, a plain dict, which
  it writes sorted by key.

## Graphs

- `begin KEY graph|digraph [OPTION ...]` starts a graph. The options are
  `strict`, `name=V`, `comment=V`, and `graph.K=V`, `node.K=V` and
  `edge.K=V`, which fill the graph's `graph_attr`, `node_attr` and
  `edge_attr`. Anything else is `bad option <word>`, a second `name` or
  `comment` is `repeated option <name>`, a missing or wrong kind is
  `usage: begin KEY graph|digraph [OPTION ...]`, a key already taken is
  `graph <KEY> exists`, and a `begin` while a graph is open is
  `begin inside <open KEY>`.
- `subgraph [OPTION ...]` opens a subgraph of the open graph or subgraph,
  as the package's `with g.subgraph(...)` does, with the options above
  except `strict`.
- `end` closes the innermost one. A subgraph is added to its parent; a
  graph is kept under its key and prints `  built KEY: <n> body lines`.
- `node NAME [K=V ...]`, `edge TAIL HEAD [K=V ...]`,
  `edges TAIL HEAD [TAIL HEAD ...]` and `attr [KW] [K=V ...]` add to the
  innermost open graph (`attr`'s KW is a first word without `=`). They,
  `subgraph` and `include` need one: `<command> needs an open graph`, and
  so does `end`. Wrong word counts are
  `usage: node NAME [K=V ...]`, `usage: edge TAIL HEAD [K=V ...]`,
  `usage: edges TAIL HEAD [TAIL HEAD ...]`, `usage: include KEY` and
  `usage: end`.
- `include KEY` adds a built graph as a subgraph of the open one, as
  `g.subgraph(other)` does. A graph of the other kind is
  `<graphviz.graphs.Digraph object> cannot add subgraph of different kind: <graphviz.graphs.Graph object>`
  (the package's message, with the object's address left out, kinds as
  they are); a strict one is `subgraphs cannot be strict`.
- `print KEY`, `copy KEY NEW`, `clear KEY [keep]` and `save KEY FILE`
  work on built graphs, and with a graph open are `<command> inside <open KEY>`.
  An unknown key is `no graph <KEY>`, a taken NEW is `graph <NEW> exists`,
  and wrong word counts are `usage: print KEY`, `usage: copy KEY NEW`,
  `usage: clear KEY [keep]` and `usage: save KEY FILE`.
- `copy` makes an independent copy. `clear` empties the body and, without
  `keep`, the three attribute dicts.
- `save` writes the source to `data/out/FILE` (FILE is `[a-z0-9_]+\.gv`,
  else `bad file name <word>`), prints
  `  saved out/FILE: <n> lines, <m> bytes`, then the file it read back.
- `unknown command <word>` ends the list. The report ends with `== end`
  and `  <n> graphs built`.

## The DOT source

`print` writes the source with each line indented by two spaces (the
source's own lines split at every newline, so a newline inside a quoted
name starts a new report line).

- An optional `// <comment>` line, then `graph NAME {` or `digraph NAME {`
  (`strict ` first for a strict graph; without a name, `graph {`). Then
  `\tgraph [...]`, `\tnode [...]` and `\tedge [...]` for the attribute
  dicts that are not empty, then the body, then `}`.
- Body lines: a node is `\t<name>` plus ` [<a_list>]` when it has a label
  or attributes; an edge `\t<tail> -> <head>` (`--` in a graph) plus its
  list; `attr KW` gives `\t<KW as written> [<a_list>]`, `attr` alone
  `\t<a_list>`, and nothing when there are no attributes. An `a_list` is
  `label=<v>` first when there is a label, then `k=v` for each attribute,
  joined by one space, keys and values quoted.
- A subgraph's lines are its own source, with `subgraph NAME {` (or `{`)
  as its head and no `strict`, each prefixed by a tab when added.
- **Quoting** (`quote`): a string that starts with `<` and ends with `>`
  (or `>` and a final newline) is HTML-like and written as it is, unless
  it is nohtml. Otherwise it is written as it is when it matches
  `[a-zA-Z_][a-zA-Z0-9_]*` or `-?(\.[0-9]+|[0-9]+(\.[0-9]*)?)` (the match
  may also end before a final newline) and its lowercase is not `node`,
  `edge`, `graph`, `digraph`, `subgraph` or `strict`. Any other string is
  put in double quotes, with every `"` escaped: a run of backslashes then
  a `"` becomes the run with one backslash dropped if it is odd, then
  `\"`. Nothing else is changed.
- A quoted string whose backslash run at the end (or before a final
  newline) is odd draws the package's DotSyntaxWarning, reported as
  `  warning: script.txt:<line>: expect syntax error scanning invalid quoted string: <repr>`
  before the line's other output, with `<repr>` Python's `repr` of the
  string. A graph's own name and attribute dicts are quoted each time its
  source is made (`print`, `save`, `include`), so their warnings come then.
- **Edge ends** (`quote_edge`): split at the first `:` into node and
  rest, the rest at its first `:` into port and compass. Node and port
  are quoted, the compass is written as it is. Python's `partition`
  hands back the string itself when there is no `:`, so a nohtml name
  without one stays nohtml; the parts of a split are plain strings, so a
  nohtml name with a `:` loses that here.

## Scope

- Rendering, piping, viewing, `unflatten`, engines, formats, `Source` and
  encodings other than UTF-8 are out of scope.
- The script's text is printable ASCII apart from `é` and the newlines
  `\N` writes, and the strings a warning reprs hold no quote of either
  kind together, tab or other control character but a newline.

## Running

```bash
python3 -s -P -B mk.py            # writes data/script.txt
python3 -I -B gen_kara.py         # embeds data/ in the program
python3 -I -B oracle.py > expected.out
karac run mini_graphviz.kara
karac build mini_graphviz.kara && ./mini_graphviz
```
