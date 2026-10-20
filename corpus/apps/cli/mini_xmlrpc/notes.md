# mini_xmlrpc

An XML-RPC service as Python 3.13's `xmlrpc` runs it, ported from
`xmlrpc/client.py` and `xmlrpc/server.py`:
- `loads()` with `use_builtin_types`: the `Unmarshaller` driven by an
  expat-like parser, with every type tag (`i1` to `i8`, `biginteger`,
  `double`, `float`, `nil`, `base64`, `dateTime.iso8601`), namespace
  prefixes stripped, untyped values read as strings, duplicate struct
  members, members with a missing name or value, unknown tags, faults
  turned back into `Fault`, and expat's own errors with their line and
  column;
- the conversions underneath it: Python's `int()` and `float()` syntax
  (underscores, `inf`, `nan`), `binascii.a2b_base64` outside strict mode
  with its two padding errors, and `datetime.strptime` with the regex
  `_strptime` builds, backtracking and case-insensitive;
- `dumps()` through the `Marshaller` with `allow_none` off, including the
  32-bit int limit and `base64.encodebytes`' 76-character lines;
- `SimpleXMLRPCDispatcher._marshaled_dispatch` with introspection and
  `system.multicall`, where a failing call becomes a fault struct but a
  malformed entry fails the whole request.

The service behind it has arithmetic, string and byte helpers, a
key-value store kept across requests and date shifting, so the faults
carry the messages Python's own operations raise: `+` across types,
`sum()` with CPython's compensated float loop, `str.join`, `str.split`,
`len()`, `timedelta`, calls with the wrong number of arguments.

`mk.py` writes `data/requests/`: 161 request bodies, read in name order.
`oracle.py` prints what `loads()` makes of each and the response the
dispatcher sends back, and wrote `expected.out`.

It matched on every backend the first time it ran; no gaps were found.
