# mini_argon2pure

A port of [argon2pure](https://pypi.org/project/argon2pure/) 1.3 (MIT;
Bas Westerbaan), the pure-Python implementation of Argon2, the password
hash that won the Password Hashing Competition and that RFC 9106, OWASP
and most current login stacks recommend. Projects use it where the C
bindings (argon2-cffi) cannot be installed. The vendored copy in
`vendor/` is the library's single module, unchanged.

```
== RFC 9106 test vectors: Argon2d, Argon2i and Argon2id
a2 type=d t=3 m=32 p=4 pw=r:32:01 salt=r:16:02 secret=r:8:03 ad=r:12:04 = 512b391b6f1162975371d30919734294f868e3be3984f3c1a13a4db9fabe4acb
...
a2 t=1 m=15 p=2 = ! Argon2ParameterError: memory_cost can't be less than 8 times the number of lanes
...
chain unkeyed = fbf206a49876bb827af18cd6ddacb72ad017570984126d184138ac7a0463592587b66c9f380ce661861be15d19d5b8a15ad165126a007819f00db0148e71ca39
...
== The policy is raised and logins rehash
policy = argon2id t=2 m=16 p=2
login alice: ok, rehashed to $argon2id$v=19$m=16,t=2,p=2$9385ML6pIz2WiZCacbNzxQ$QaUY6pjLHhjIh2cUNJablp9jI1dwd2v/XyUcCFBTZP4
login alice: ok
login bob: denied
...
check: ! bad hash: bad parameters 't=1,m=16,p=2'
```

## What it does

`mk.py` writes `data/jobs.txt`, 15 jobs of 269 steps. `oracle.py` plays
them through the vendored argon2pure and `mini_argon2pure.kara` through
the port; both print each result as hex and each error as
`! Argon2ParameterError: ...` or `! AssertionError`.

- **Argon2**: argon2pure's README example; the three RFC 9106 test
  vectors (Argon2d, Argon2i and Argon2id with a secret and associated
  data), which the library and the port both reproduce; argon2pure's own
  tests (every type and both versions, 1.0 and 1.3, at 8 to 17 KiB, and
  at four memory sizes with two passes and two lanes), run against the
  library here rather than against argon2-cffi; tags from 4 to 200 bytes;
  one to four lanes, one or three passes, empty and long passwords and
  salts; and every parameter check in the order the library makes them.
- **Blake2b**: the library's minimal Blake2b, one-shot and in pieces
  across the 128-byte block boundary, keyed (up to 128 bytes of key) and
  with every digest length, its asserts, and the three chains from
  `test_argon2pure.py` with their expected values.
- **H'**: Argon2's variable-length hash at the lengths where it switches
  from one Blake2b to a chain of them.
- **A password store**, this app's own code, the same in the oracle and
  the port: users registered as PHC strings
  (`$argon2id$v=19$m=...,t=...,p=...$salt$hash`, unpadded base64) under a
  server-side pepper passed as Argon2's secret, with salts derived from a
  counter so the output is fixed; logins that are denied, accepted, or
  accepted and rehashed when the stored parameters are not the current
  policy; a users file saved, dropped and loaded back; a new pepper that
  locks out every old hash; and PHC strings from elsewhere, including the
  README example's (valid only as version 1.3), and malformed ones.

`threads` is checked and clamped as in the library, but the worker pool
is not ported: the segments of one slice never read each other, so
filling them lane by lane gives the same blocks (`threads=1`, `2` and
`8` print the same hash). The oracle passes `use_threads=True` so that
no worker processes start. A block is kept as its 128 little-endian
words, so the 16-byte registers the library slices out of a block are
word pairs.

## Edits from mini_argon2pure.orig.kara to mini_argon2pure.kara

`karac check` raised five errors. Two were the constants `M32` and
`B64`, which the naming rule reads as type names because each has a
single letter; they are `LOW32` and `B64_ALPHABET`. Three were
`Ok(mut b) => ...` match arms, whose message says to rebind the value in
the arm but carries no fix (B-2026-10-25-8); each is now
`Ok(b) => { let mut b = b; ... }`. With those fixed, the check raised
five more. `karac fix` applied one, `B64.find(ch)` with a `char` written
as `ch.to_string()`. The other four were `s.bytes()` passed or assigned
where a `Vec[u8]` is expected, refused with
`expected 'Vec[u8]', found 'Slice[u8]'` and no fix (B-2026-10-25-9);
each is now `s.bytes().to_vec()`. After that it matched the oracle on
the first run under the MIR interpreter, the LLVM backend, the ASan
build and the native binary, with nothing on stderr.

## Running

```
python3 -s -P -B mk.py            # writes data/jobs.txt
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py         # embeds data/jobs.txt in mini_argon2pure.kara
karac run mini_argon2pure.kara
```

The oracle needs `six`, which argon2pure imports; it is not vendored.
About 1.5 s on the MIR interpreter (a debug build of `karac`) and 0.1 s
built. The run writes `users.db` to the working directory and removes it
at the end.

## License

argon2pure is under the MIT license in `vendor/LICENSE`:

    Copyright (c) 2016 Bas Westerbaan

That covers the module in `vendor/`.
