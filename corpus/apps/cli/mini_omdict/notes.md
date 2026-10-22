# mini_omdict

A port of [orderedmultidict](https://github.com/gruns/orderedmultidict) 1.0.2
(Unlicense, Ansgar Grunseid). `omdict` is an **ordered multivalue dictionary**:
one key can hold many values, and the full insertion order of every item is
remembered. It is the data structure behind the `furl` URL library — exactly
what a query string (`?a=1&a=2&b=3`) or a set of HTTP headers needs, where a key
may repeat, order matters, and `keys()` still reports each key once.

## What it does

`mk.py` writes `data/jobs.tsv`, a scripted sequence of operations on one
evolving `omdict`; each op prints one line:

    a <k> <v>      add — append a value for k           -> allitems()
    al <k> <csv>   addlist — add comma-separated values -> allitems()
    s <k> <v>      set — replace all of k's values with one -> allitems()
    sl <k> <csv>   setlist — replace in place / append / drop; empty = delete -> allitems()
    rv             reverse — flip the item order         -> allitems()
    g <k>          get      -> <first v> | miss
    gl <k>         getlist  -> [v, ...]
    p <k>          pop      -> <first v> | miss   (removes every value of k)
    pl <k>         poplist  -> [v, ...] | miss
    pv <k>         popvalue last  -> <v> | miss
    pf <k>         popvalue first -> <v> | miss
    h <k>          has_key  -> yes | no
    l              size (total items, duplicates counted)
    ks / vs / it   keys() / values() / items()     — unique views
    ai / ak / av   allitems() / allkeys() / allvalues() — every item

Every mutation prints the resulting `allitems()`, so `set`/`setlist`'s in-place
replace, its appends and drops, and `reverse`'s ordering are directly visible.
`oracle.py` replays the sequence against the vendored `orderedmultidict.omdict`;
`mini_omdict.kara` reproduces `expected.out` byte-for-byte on every backend.

## How the port works

The library stores a doubly linked `itemlist` plus a `map` from key to its list
of nodes. The port is observationally identical with **two plain vectors** —
`ks`/`vs`, the global `(key, value)` items in insertion order — plus `korder`,
the distinct keys in first-insertion order. The split is what makes `reverse()`
correct: it flips the item order but leaves `keys()` order alone, so `values()`
then reports the new *first* value of each key — exactly as the library reverses
its itemlist and each per-key node list without ever reordering the map's keys.
`setlist` follows the library branch for branch: it pairs the key's existing
positions with the new values, replacing in place, appending any extras at the
end, dropping any surplus, and an empty list deletes the key; `set(k, v)` is
`setlist(k, [v])`. `pop`/`poplist` remove every value of a key; `popvalue(last=)`
removes the first or last occurrence and drops the key once its last value goes.

The two backends agree with each other and with Python because the key order is
first-insertion order (deterministic) and the item order is explicit — there is
no hash-map walk anywhere, so nothing depends on per-process hash order.

## Scope

The `omdict` core: `add`, `addlist`, `set`, `setlist`, `get`, `getlist`, `pop`,
`poplist`, `popvalue`, `has_key`, `size`, `reverse`, and the unique
(`keys`/`values`/`items`) and all (`allitems`/`allkeys`/`allvalues`) views, over
string keys and int values. The constructor-from-items, `update`/`updateall`,
`fromkeys`, `lists`/`listitems`, `popitem`, `setdefault`, `removevalues` and
slicing are out of scope. The vendored `orderedmultidict` and its one dependency
`six` (both pure Python) are included, so `oracle.py` runs exactly the code the
port mirrors.

## Running

```bash
karac run mini_omdict.kara                    # interpret
karac build --mir mini_omdict.kara            # native binary
python3 -s -P -B mk.py                            # regenerate data/jobs.tsv
python3 -I oracle.py                               # regenerate expected.out
python3 -s -P -B gen_kara.py                       # regenerate the embedded jobs
```
