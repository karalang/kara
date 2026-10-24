# mini_maxminddb

A port of MaxMind's [maxminddb](https://github.com/maxmind/MaxMind-DB-Reader-python)
2.6.2, the pure-Python reader for MaxMind DB files (the format of GeoIP2
and GeoLite2), and of the database half of
[geoip2](https://github.com/maxmind/GeoIP2-python) 4.8.1: the City,
Country and ASN models with their locale fallback. Both are Apache-2.0;
the pure-Python files are vendored under `vendor/` with their licences
(`LICENSE-maxminddb.txt`, `LICENSE-geoip2.txt`), without the C extension
and without geoip2's web-service client. The oracle runs them unchanged,
in `MODE_MEMORY`.

```
== get | city.mmdb | 2001:DB8:200:ab::1
/40 {'city': {'geoname_id': 2988507, 'names': {'de': 'Paris', 'en': 'Paris', ...
== city | city.mmdb | 214.1.33.33 | en
city Munich geoname=2867714
subdivision Bavaria (BY) of 1 / BY
location 48.1374,11.5755 radius=20 tz=Europe/Berlin metro=-
postal 80331
country Germany (DE) eu=True geoname=2921044
continent EU Europe
registered United States (US)
represented United States (US) type=military
traits ip 214.1.33.33 network 214.1.0.0/16 flags -
== country | city.mmdb | 81.2.69.142 | en
TypeError: The country method cannot be used with the GeoIP2-City database
== get | corrupt.mmdb | 1.2.3.4
InvalidDatabaseError: The MaxMind DB file's search tree is corrupt
== firewall | policy.txt | access.log
...
line 238: deny 214.78.123.80 /catalog (rule 6: deny anycast)
...
214 allowed, 86 denied
```

## What it does

`mk.py` builds the databases itself, from the format specification, with
a small writer of its own (a binary trie numbered breadth first, a data
section that reuses pointers for repeated strings, maps and arrays, and
the metadata map after the `\xab\xcd\xefMaxMind.com` marker). The places
and networks belong to Larkfield, an invented library-loans service, and
use documentation and test ranges, not real allocations:

- `city.mmdb`: GeoIP2-City style, IPv6 with 28-bit records, 413 nodes:
  cities in eight locales, subdivisions, locations, postcodes, a network
  registered in the US but located in Tokyo, an anycast network, a
  military network with a represented country, and a satellite provider
  with no country. The IPv4 tree is aliased from `::ffff:0:0/96` and
  `2002::/16`, two of the aliases MaxMind's own writer makes.
- `country.mmdb` (GeoLite2-Country style, IPv4, 24-bit records) and
  `asn.mmdb` (GeoLite2-ASN style, IPv6, 32-bit records).
- `types.mmdb`, a decoder test database: every data type at zero, at its
  maximum and minimum, nested containers, strings that need each of the
  three size extensions, and records far enough into the data section
  that their pointers take two bytes.
- Nine broken files: no metadata, metadata that is a list, metadata
  without `build_epoch`, a 20-bit record size, a record pointing past the
  end of the file, type number 12, a four-byte double, a pointer out of
  the file, and a search tree that runs out of address bits before it
  reaches a record.
- `access.log`, 302 lines of web-server log from those networks plus
  private and unallocated ranges and two lines with bad addresses, and
  `policy.txt`, a first-match-wins geo policy for an admin panel.

`data/jobs.txt` lists 122 jobs. `mini_maxminddb.kara` carries the same
files as hex, writes them to the working directory, runs the jobs and
removes the files again.

- **`meta`** prints the `Metadata` repr, node byte size and search tree size.
- **`get`** prints `Reader.get_with_prefix_len`: the record's repr and the
  prefix length, or `None`. The addresses include IPv4 in an IPv6 tree,
  IPv4-mapped and 6to4 forms, upper-case IPv6, and invalid ones.
- **`dump`** and **`networks`** iterate the database the way
  `Reader.__iter__` does, which skips the IPv4 aliases.
- **`city`**, **`country`** and **`asn`** go through
  `geoip2.database.Reader` with a locale list and print the model's
  fields; names fall back through the locales, and a lookup with the
  wrong model for the database type raises geoip2's `TypeError`.
- **`annotate`** looks every log line up in the City and ASN databases and
  totals requests and bytes per country and per network.
- **`firewall`** applies the policy to the log and reports the denied
  requests and each rule's hit count.

## How the port works

- **The tree.** `read_node` decodes 24, 28 (the shared middle nibble) and
  32-bit records the way `Reader._read_node` does; a lookup walks from the
  root or, for an IPv4 address in an IPv6 database, from the node 96 left
  steps down. Iteration is the same recursive walk as
  `_generate_children`, with its rule for when a network prints as IPv4.
- **The decoder** is `Decoder.decode`: the control byte, extended types,
  the size extensions 29, 30 and 31, the four pointer sizes with their
  bases, and every type decoder. A Python slice that runs off the end of
  the buffer is cut short instead of failing, so the port does the same,
  and the failures Python would raise from `struct.unpack` or a byte
  index come out with Python's messages.
- **Values.** A decoded value is a `Val`. Unsigned integers keep their
  big-endian bytes and print in decimal by long division, so `uint128`
  values print whole. Doubles and widened floats print with Python's
  `repr`, and strings and bytes are quoted with CPython's rules.
- **`ipaddress`.** `ip_address` follows Python 3.13's parsers: four
  decimal octets without leading zeros, and the IPv6 rules for `::`, a
  trailing dotted quad, and at most four hex digits a group. Printing
  compresses the longest zero run and writes an IPv4-mapped address with
  its dotted quad. `ip_network(..., strict=False)` builds geoip2's
  `network` properties.
- **geoip2.** The models read the record maps directly: the
  `database_type` substring check, `AddressNotFoundError` with its
  network, `PlaceRecord.name`, `Subdivisions.most_specific` and the
  `Traits` flags.

## Compiler gaps

None. `karac check` asked for call-site `mut` markers on the four
arguments that are mutated, `ref` bindings in two matches whose
`Option` is used again afterwards, and one `.into_iter()`. `karac fix`
applied all of them. The first run stopped on a shift of a `u128` by 128
bits while parsing `::`. That was the port's bug: Python's integers are
unbounded, so its code can shift that far. Once that was fixed, the
port printed the oracle's output under all four backends, with nothing
on stderr.

## Scope

The port covers what the jobs exercise. Not ported: the memory-mapped and
file modes and the C extension (the data is the same in every mode),
lookups by `ipaddress` objects or IPv6 scope ids, map keys that are not
strings (the format requires them), geoip2's Enterprise, ISP, Domain,
Connection-Type and Anonymous-IP models, and the web-service client.

## Running

```
python3 -s -P -B mk.py                   # writes data/
python3 -I -B oracle.py > expected.out   # the oracle, on vendor/
python3 -I -B gen_kara.py                # refreshes jobs_text() and inputs_text()
karac run mini_maxminddb.kara            # the port; prints the same
```

About 8 s on the MIR interpreter (a debug build of `karac`) and 2.6 s
on the LLVM backend.
