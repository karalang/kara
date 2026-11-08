# mini_multidict

The `multidict` package that aiohttp and yarl keep HTTP headers and query
strings in: a dict that can hold a key more than once and keeps every pair
in order, a case-insensitive variant for headers, read-only proxies over
both, and key and item views that work as sets.

The program writes its own script (`data/script.txt`), plays it and prints
a report. `oracle.py` plays the same script against the vendored
`multidict` 7.2.1 (`vendor/`, Apache-2.0; its pure Python implementation)
and is the reference; `mk.py` writes the script and `gen_kara.py` embeds it
in the program.

## The script

- Each line is split into words at runs of spaces. Blank lines and lines
  whose first word starts with `#` are skipped. Every other line is echoed
  as `> ` followed by its words joined by one space, then runs; what it
  prints follows, each line indented by two spaces.
- A problem is reported as `  error: script.txt:<line>: <message>` and the
  line changes nothing. The package's errors are `<Type>: <str(e)>`
  (`KeyError: 'accept'`, `KeyError: "Key not found: 'sort'"`); the port's
  own are given below in full.
- **Text.** Keys and values are the script's words with `\s` read as a
  space. A `K=V` word is split at its first `=`; a word without one is
  `bad pair <word>`.
- **Names.** `new NAME md|ci [K=V ...]` makes a `MultiDict` or a
  `CIMultiDict` of the pairs in order; `new NAME md|ci from OTHER` makes
  one from another multidict or proxy. `proxy NAME md|ci OF` makes a
  `MultiDictProxy` or `CIMultiDictProxy` over OF, and `copy NAME NEW` is
  `.copy()`. A taken NAME is `<NAME> exists`, an unknown one
  `no multidict <NAME>`, and wrong words
  `usage: new NAME md|ci [K=V ...] | new NAME md|ci from OTHER`,
  `usage: ... from OTHER` (a `from` without exactly one name after it),
  `usage: proxy NAME md|ci OF` and `usage: copy NAME NEW`.
- **Commands on a name** (`<cmd> NAME ...`; without a NAME,
  `usage: <cmd> NAME ...`): `show` (repr), `len`, `keys`, `values` and
  `items` (the view's repr, then ` len <n>`), `reversed NAME
  keys|values|items` (the list's repr, else
  `usage: reversed NAME keys|values|items`), `todict`, `has K`,
  `hasitem K V`, `hasvalue V`, `item K` (`md[K]`), `get K [DEFAULT]`,
  `getone K [DEFAULT]`, `getall K [DEFAULT]`, `set K V`, `del K`,
  `add K V`, `setdefault K V`, `popone K [DEFAULT]`, `popall K [DEFAULT]`,
  `popitem`, `clear`, `extend`, `update` and `merge` (with `K=V` pairs or
  `from OTHER`), `eq OTHER`, `eqdict [K=V ...]` (against a plain `dict`
  of the pairs, a later key winning), `keyset NAME OP [K ...]` and
  `itemset NAME OP [K=V ...]`. A call that returns something prints its
  repr. The wrong number of words after the name is
  `<cmd> takes <n> word(s) after the name`, `<n>` being `0 words`,
  `1 word`, `2 words` or `1 or 2 words`. An unknown command is
  `unknown command <word>`. The report ends with `== end` and
  `  <n> multidicts`.

## The package's rules

A multidict is a list of entries, each a key as written, its *identity*
and a value. The identity is the key itself in a `MultiDict` and the key
lowercased in a `CIMultiDict`; every lookup compares identities.

- `getone`, `item` and `get` give the first entry's value; `getall` the
  values of every entry, in order. Missing, `getone` and `getall` give the
  default or raise `KeyError("Key not found: <repr>")`, `get` gives the
  default or `None`.
- Keys and the key view hold each identity once, under its first entry's
  key; values and items hold every entry. `reversed` walks the same lists
  backwards.
- `to_dict` maps each identity's first key to the list of its values.
- `add` and `extend` append. `set` writes the first match's key and value
  in place and drops the other matches, or appends. `del` and `popall`
  drop every match, `popone` the first; missing, they raise `KeyError(key)`
  unless given a default. `popitem` removes the last entry
  (`KeyError('empty multidict')` when empty). `setdefault` gives the first
  match's value or appends.
- `update` takes its pairs in order. Each writes its key and value into
  the first match this call has not written yet, or appends. When the call
  ends, every match of a key it wrote that it did not write is dropped.
- `merge` appends a pair whose identity was not in the multidict before
  the call.
- `from OTHER` reads OTHER's entries, its own included, as pairs of key
  and value taken before anything changes.
- A proxy shows the multidict it was made over (a proxy's own target, for
  a proxy) as that multidict changes. A `CIMultiDictProxy` needs a
  `CIMultiDict` or `CIMultiDictProxy`, else
  `TypeError: ctor requires CIMultiDict or CIMultiDictProxy instance, not <class 'multidict._multidict_py.MultiDict'>`
  (or `...MultiDictProxy'>`); a `MultiDictProxy` takes any multidict. A
  proxy's `copy` is a `MultiDict` or `CIMultiDict` by the proxy's kind,
  made `from` its target. A proxy has no mutating methods
  (`AttributeError: 'CIMultiDictProxy' object has no attribute 'add'`),
  and `set` and `del` are
  `TypeError: 'CIMultiDictProxy' object does not support item assignment`
  and `... does not support item deletion`.
- `eq` compares the multidicts under proxies: equal lengths and, entry by
  entry, equal identities and values (so a `MultiDict` of `a` equals a
  `CIMultiDict` of `A`). `eqdict` needs equal lengths and, for each entry
  in order, the dict's value at that entry's key (as written) equal to the
  entry's value.
- `repr` is `<Class('k': 'v', ...)>` over every entry, the views
  `<_KeysView('k', ...)>`, `<_ValuesView('v', ...)>` and
  `<_ItemsView('k': 'v', ...)>`; strings are Python's `repr`.

## The views as sets

`keyset` and `itemset` combine the key or item view with the list of
words. `and`, `or`, `sub`, `xor` and `disjoint` are `view & list`,
`view | list`, `view - list`, `view ^ list` and `view.isdisjoint(list)`;
`rand`, `ror` and `rsub` are `list & view`, `list | view` and
`list - view`. Any other OP is `bad set operation <word>`, and no OP is
`usage: keyset NAME OP [...]` (or `itemset`). A set prints sorted, as
`{'a', 'b'}` or `set()`, since Python's own order changes from run to run.

- Keys: `and` gives the first entry's key of each word that matches,
  `rand` the matching words as written, `or` the view's keys and the words
  that match nothing, `ror` the words and the first keys of identities no
  word has, `sub` the view's keys whose identity no word has, `rsub` the
  words that match nothing, `xor` `sub` and `rsub` together.
- Items (an item matches an entry of its identity and value): `and` gives
  each matching entry's key and value, `rand` the matching pairs as
  written, `or` the view's items and the pairs that match nothing, `ror`
  the pairs and the entries no pair matches, `sub` the entries no pair
  matches, `rsub` the pairs that match nothing, `xor` `sub` and `rsub`
  together.

## Scope

- Values are strings; `istr`, `getversion`, keyword arguments, pickling,
  threads and the C extension are out of scope.
- The script is printable ASCII, and no string a repr shows holds both
  kinds of quote.

## Running

```bash
python3 -s -P -B mk.py            # writes data/script.txt
python3 -I -B gen_kara.py         # embeds data/ in the program
python3 -I -B oracle.py > expected.out
karac run mini_multidict.kara
karac build mini_multidict.kara && ./mini_multidict
```
