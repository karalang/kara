# mini_jmespath

JMESPath, the JSON query language, as the Python library `jmespath`
1.0.1 runs it on Python 3.13. It is what `aws --query`, Ansible's
`json_query` filter and the `jp` tool use. The port covers:
- The lexer, including its JSON literals in backticks, raw strings in
  single quotes and quoted identifiers.
- The Pratt parser, with its binding powers, the way it nests
  projections, and every error message it gives.
- The tree interpreter: field access, indexes, Python slices,
  projections over lists and over object values, flatten, filters,
  multi-select lists and hashes, pipes, `||`, `&&`, `!` and the six
  comparators.
- The 26 built-in functions with their argument checks. When a check
  fails, the error names the bad argument and the types the function
  accepts.

Python's value model comes along. `1 == 1.0` holds, and so does
`[true] == [1]`, though `true == 1` is false at the top level. Dicts
keep their insertion order, and only `''`, `[]`, `{}`, `null` and
`false` are false. `sum()` adds ints exactly until the first float.
After that it adds floats with Neumaier's compensation, as CPython 3.12
and later do. On the CloudWatch datapoints the plain running total
would end in `...1025` where `sum()` gives `...103`.

The run writes `data/` and then works through the 228 jobs in
`data/jobs.jsonl`:
- It runs the JMESPath compliance suite (15 files, 892 cases; see
  below). For each case it prints the result or the error and checks it
  against the suite.
- It runs 212 `jp -f <file> <expression>` queries. The files are EC2
  `describe-instances`, an S3 `list-objects-v2` listing, CloudWatch
  datapoints, IAM policies and a policy document, a `kubectl get pods`
  list, GitHub issues, and a config whose keys contain dots, spaces,
  quotes, tabs, digits and non-ASCII text, and four go to the document
  the random expressions use. The output follows the
  `jp.py` script that ships with the library: the result as JSON with
  an indent of 4, or the error on stderr and exit status 1.
- It runs 320 random expressions over one document. The generator
  breaks about one in five of them on purpose, by deleting a character
  or inserting a stray bracket, quote or operator.

`oracle.py` runs the same jobs through the real library and wrote
`expected.out`.

The library's quirks are kept. Some examples:
- `CpuOptions.CoreCount * ThreadsPerCore` is a syntax error at the `*`,
  and `max(@) - min(@)` fails in the lexer on `Unknown token '-'`.
  JMESPath has no arithmetic.
- An unquoted `pod-template-hash` stops at the `-`, as does
  `reactions.+1` at the `+`. Both need quotes. `日本` needs them too,
  because an unquoted identifier is ASCII only.
- `` `plain old string` `` is not valid JSON. The lexer retries it as a
  JSON string and accepts it, under the deprecation the spec calls
  JEP-12.
- `Contents[?Size > '1000']` compares an int with a str. Python raises
  `TypeError: '>' not supported between instances of 'int' and 'str'`,
  and `jp.py` does not catch it. An empty expression escapes the same
  way, as `EmptyExpressionError`.
- `"tab\qkey"` fails with `Invalid \escape`, which is the first part of
  the message from `json.loads`.
- In a multi-select hash the comma is optional, so `{x: name n: n}`
  parses. Function arguments work the same way: `length(tags words)`
  is a call with two arguments, and fails on its arity.
- `[items][0][1:3].n` gives `null`, where `[items][0] | [1:3].n` gives
  `[-1, 0.5]`. The slice is added to the index expression that is
  already there, so it does not start a projection.

The port matched `expected.out` on its first run, on all four MIR
backends. It needed no workarounds and added no new ledger rows.

## The compliance suite

The files in `suite/` are copied unchanged from
https://github.com/jmespath/jmespath.test. Its license says:

    MIT License

    Copyright (c) 2013 Amazon.com, Inc. or its affiliates.  All Rights Reserved

    Permission is hereby granted, free of charge, to any person obtaining a copy
    of this software and associated documentation files (the "Software"), to deal
    in the Software without restriction, including without limitation the rights
    to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
    copies of the Software, and to permit persons to whom the Software is
    furnished to do so, subject to the following conditions:

    The above copyright notice and this permission notice shall be included in all
    copies or substantial portions of the Software.

    THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
    IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
    FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
    AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
    LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
    OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
    SOFTWARE.
