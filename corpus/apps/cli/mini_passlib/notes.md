# mini_passlib

A port of [passlib](https://pypi.org/project/passlib/) 1.7.4 (BSD; Eli
Collins and Assurance Technologies): the password hashing library behind
many Python web stacks, with over 30 hash schemes, the `CryptContext`
policy object that hashes, verifies and migrates hashes, and readers and
writers for Apache `htpasswd` and `htdigest` files.

```
> C.verify_and_update('password', 'abJnggxhB/yWI')
  (True, '$5$rounds=1000$7MbT4jOx.uv8KAPB$inhSnAR5jpxJr2a2brlf37Wea50v0U81ZL13W4Wuc83')
> C.hash('password', scheme='des_crypt')
  warning DeprecationWarning: CryptContext.hash(): 'scheme' keyword is deprecated as of Passlib 1.7, and will be removed in Passlib 2.0
  hrjt/QAMcR89k
> HtpasswdFile('data/dup.htpasswd').get_hash('alice')
  log WARNING: username occurs multiple times in source file: b'alice'
  b'{SHA}abc='
```

## What it does

`mk.py` writes 668 jobs (and 15 section comments) to `data/jobs.tsv`, one
Python expression each, plus the `htpasswd`, `htdigest` and INI files they
read. `oracle.py` evaluates each job against the vendored library and
prints the value, the warnings it raised and any exception;
`mini_passlib.kara` evaluates the same jobs. Salts and varied rounds come
from a seeded 31-bit generator patched in for passlib's `rng` and reseeded
before every job, so every hash is reproducible. 186 jobs raise and 39
print warnings.

- **45 schemes**: the hex digests and LDAP digests (plain and salted),
  `des_crypt`, `bsdi_crypt`, `bigcrypt`, `crypt16`, MD5-crypt and Apache's
  `apr1`, SHA-256/512-crypt, `sha1_crypt`, `bcrypt` (with the 2a/2b/2x/2y
  idents and the padding-bit repair warning), `bcrypt_sha256` v1 and v2,
  `scrypt` in both formats, the PBKDF2 family (passlib's own, Cryptacular,
  dlitz, GRUB, Atlassian, Django), phpass, MySQL, PostgreSQL, NT and LM
  hashes, MS-DCC, Cisco type 7, PIX and ASA, Django's salted digests,
  plaintext and the disabled-account markers, plus the `{CRYPT}`, `{MD5}`,
  `{PBKDF2-SHA256}` and `$3$$` prefix wrappers.
- **The handler API**: `hash`, `verify`, `identify`, `parsehash` (with
  `checksum=False` and `sanitize=True`), `needs_update`, `normhash`,
  `genconfig` / `genhash` and their deprecation warnings, class attributes,
  and `using()` with its salt, rounds, ident, version, block size and
  truncation checks, `relaxed=True` turning errors into warnings.
- **CryptContext**: built from keywords, INI strings and files, with
  categories, per-scheme and per-category options, `deprecated = auto`,
  `copy`, `update`, `to_string` / `to_dict`, `verify_and_update` migrating
  an old hash, `disable` / `enable`, and the policy errors it raises.
- **Apache files**: an `htpasswd` file with comments and blank lines kept
  through edits, duplicate users, malformed lines, user validation,
  `default_scheme` aliases, autosave, and a file whose stale hashes are
  rehashed on login; an `htdigest` file with realms and `default_realm`.

## How the port works

- **Every class is a table row.** Each scheme, each `using()` subclass and
  each context record is a `Cls` with its kind and settings, and a parsed
  hash is an instance of one. Construction runs the mixins' checks in
  passlib's MRO order (checksum, salt, rounds, ident), so the first error
  reported is passlib's.
- **The crypto is written out.** MD4, MD5, SHA-1, SHA-256 and SHA-512,
  HMAC, PBKDF2, DES with crypt's salted E-box, Blowfish's `eksblowfish`
  key schedule, scrypt's Salsa20/8 and the hash64, bcrypt64, ab64 and
  base64 encodings are all in the port; nothing comes from a library.
- **CryptContext keeps passlib's ordered source.** Options are an ordered
  dict keyed by `(category, scheme, option)`, merged on `update` the way a
  Python dict update merges, and `to_string` replays them in passlib's
  `iter_config` order.
- **The jobs are evaluated.** A small evaluator reads Python expressions:
  names, attribute access, calls with keyword arguments, string and bytes
  literals with escapes, numbers, lists, dicts, `+` and `*`. Values are a
  Python value enum, printed with Python's `repr`.

## Compiler gaps

None. `karac fix` supplied the call-site `mut` markers and two `ref`
patterns, all language rules. The interpreter matched the oracle on the
first full run.

## Scope

- Builtin backends only: no `os_crypt`, the `bcrypt` C package or
  `hashlib.scrypt`. The oracle forces passlib's pure-Python backends, so
  rounds in the jobs are kept small.
- `CryptContext`'s `min_verify_time` and `harden_verify` are accepted with
  passlib's deprecation warnings and otherwise ignored, as passlib 1.7
  does; `CryptPolicy` and `LazyCryptContext` are not ported.
- The `totp` and `ifc` modules, the Django and Flask integrations, and the
  registry's plugin loading are not ported.

## Running

```bash
karac run mini_passlib.kara                  # interpret
karac build --mir mini_passlib.kara          # native binary
python3 -s -P -B mk.py                       # regenerate data/
python3 -I -B oracle.py > expected.out       # regenerate expected.out
python3 -I -B gen_kara.py                    # regenerate the embedded inputs
```
