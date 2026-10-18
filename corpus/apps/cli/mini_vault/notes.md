# mini_vault

An encrypted secrets store of the kind a deployment keeps its API keys
and database passwords in (`ansible-vault`, a `pass` store, a sealed
`.env`), with the cryptography under it written from the specifications.

The run writes `data/app.env`, `data/notes.txt` and `data/jobs.tsv`, then
runs the 123 jobs, whose fields are separated by tabs. Each job prints
`== ` and its fields joined by spaces, then its output. A byte argument is
`x:<hex>`, `t:<text>` (with `\n` and `\t` escaped), `f:<file in data/>`,
or `-` for nothing. Outputs print as hex, 32 bytes to a line; decrypted
text that is printable UTF-8 prints as its lines after `| `.

The primitives:
- `sha256 DATA`, `hmac KEY DATA`, `pbkdf2 PW SALT ITERS LEN` and
  `hkdf IKM SALT INFO LEN`, with the FIPS 180-4, RFC 4231 and RFC 5869
  vectors and the padding boundaries at 55, 56, 63, 64 and 65 bytes;
- `aes-ecb KEY PT`, `aes-cbc KEY IV PT` and `aes-ctr KEY NONCE PT` for
  128-, 192- and 256-bit keys, with the FIPS 197 and SP 800-38A vectors
  and a counter that wraps all 128 bits;
- `gcm-seal`/`gcm-open KEY NONCE AAD DATA` and the same for
  `chacha-seal`/`chacha-open`, with the GCM specification's test cases
  (including 8- and 60-byte nonces) and RFC 8439's, and ciphertexts with
  a changed byte, a changed tag, the wrong associated data, the wrong
  nonce or a truncated tag;
- `fernet-seal KEY TIME IV PT`, `fernet-open KEY TOKEN TTL NOW` and
  `fernet-multi KEYS TOKEN TTL NOW` (a comma-separated key list, as for
  key rotation), with the Fernet specification's token, expired tokens,
  tokens from too far in the future, a wrong key, damaged tokens, and a
  token whose MAC is good but whose padding is not.

The vault jobs keep `data/team.vault`: `vault-init F PW SALT ITERS NONCE`,
`vault-list F`, `vault-cat F`, `vault-put F PW NAME VALUE NONCE`,
`vault-get F PW NAME`, `vault-del F PW NAME`, `vault-flip F NAME BYTE`
(damages an entry, as a disk or a careless edit would) and
`vault-rekey F OLD NEW SALT ITERS SEED`. The file is text:

    kvault 1
    kdf pbkdf2-sha256 <iterations> <salt>
    check <nonce> <AES-GCM of "kvault-check">
    entry <name> <nonce> <AES-GCM of the value, with the name as AAD>

in name order. The key is PBKDF2-HMAC-SHA256 of the passphrase, and the
check value tells a wrong passphrase apart from a damaged entry. A rekey
decrypts every entry first, so a damaged one stops it before anything is
written, and takes its nonces from HMAC(seed, i) so that the output is
reproducible. The iteration counts are small so that the run stays fast;
a real vault would use hundreds of thousands.

Errors print as the library's exception: `ValueError` with its message
for a bad key, IV, nonce or data length, `InvalidTag`, `InvalidToken`,
`VaultError`, and `FileNotFoundError` for a missing vault.

**The port** does SHA-256 with the HMAC key's padded blocks hashed once,
so that each PBKDF2 round costs two compressions. AES is byte-oriented
with the S-boxes as tables, GHASH multiplies in GF(2^128) bit by bit on
two 64-bit halves, and Poly1305 works in five 26-bit limbs as
poly1305-donna does. Fernet decodes tokens and keys the way Python's
`base64.urlsafe_b64decode` does, which skips characters outside the
alphabet and stops at enough padding, so a token with stray characters
in it still opens.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the
jobs with Python's `cryptography` library (and `hashlib` and `hmac`) and
keeps the vault in the same format. Run `mk.py` first; it writes `data/`
and makes the ciphertexts and tokens the open jobs read. `gen_kara.py`
prints the Kāra data section (everything after the last comment line
ending in `data` in `mini_vault.kara`) from the files `mk.py` wrote.
