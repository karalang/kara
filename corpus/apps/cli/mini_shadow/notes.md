# mini_shadow

Linux keeps accounts in `/etc/passwd`, `/etc/shadow` and `/etc/group`.
This program ports the tools that check and change them (shadow 4.13's
`pwck`, `chage` and `passwd`), the password hashes they hold (crypt's
MD5, SHA-256 and SHA-512 methods, `openssl passwd` and Apache's
`htpasswd -v`), and the decision `pam_unix` makes when someone logs
in. It prints what `mini_shadow.py`, the same program in Python,
prints, and `real.py` checks that mirror against the real tools.

The run writes two roots under `data/`: `sys`, a small system with
fourteen accounts in every state an account can be in, and `broken`,
whose files have truncated, duplicate, orphaned and malformed entries.
Today is 2026-10-15.

**pwck -r [-q] -R root** checks each passwd entry (well formed, unique,
its group, home directory and shell present, a shadow entry), then
each shadow entry (well formed, unique, a passwd entry, not changed in
the future), with pwck's messages, its read-only "No" answers, `-q`'s
quieter set, and exit status 2.

**chage** lists aging with `-l` (and `-i` for ISO dates), with "never"
and "password must be changed" where shadow prints them, and sets it
with `-d`, `-E`, `-I`, `-m`, `-M` and `-W`. Dates are day numbers or
`YYYY-MM-DD`, normalized as mktime does (2026-02-30 is March 2). Bad
numbers and dates print chage's usage, exit 2.

**passwd** prints `-S` status lines (`P`, `L`, `NP`, and the short form
for an entry without dates) for one account or `-a`, locks and unlocks
(refusing to unlock to an empty password, exit 3), deletes and expires
passwords, and sets `-n`, `-x`, `-w` and `-i`.

Both rewrite only the line they change, so the shadow file after each
job is byte-identical to the one the real tools leave.

**Hashes.** `crypt PASSWORD SETTING` is libxcrypt's crypt() for `$1$`,
`$5$` and `$6$`, with `rounds=`, the salt cut to 8 or 16 characters,
and `*0` for a setting it refuses (too few rounds, an unknown method).
`openssl passwd -1/-apr1/-5/-6 -salt` prints what OpenSSL 3.0 prints,
including `<NULL>` for an empty SHA salt. The MD5, SHA-1, SHA-256 and
SHA-512 functions are written from their specifications.

**login ROOT USER PASSWORD** follows pam_unix: the password (from
shadow, or the passwd field when it is not `x`, with `nullok` for an
empty one), then the account's expiry date, a forced change (`lstchg`
of 0), inactivity after expiry, an expired password, and the expiry
warning, with pam_unix's messages; then `nologin` shells.

**htpasswd -vb FILE USER PASSWORD** checks `$apr1$`, `{SHA}` and crypt
hashes with Apache's messages and exit codes.

**Divergences.** The real tools need `-R` to be an absolute path, so
`real.py` passes them one; mini_shadow takes it as given. `login` and
`htpasswd` have no counterpart to run here (no PAM stack, no Apache),
so they are checked only through the hashes they compare. yescrypt
(`$y$`) and bcrypt (`$2y$`) are not implemented and say so, where the
real tools would verify them.

Run `python3 real.py` to check the mirror against the real tools.

## Kāra notes

Author errors: a local named `final`, which is reserved (the one change
from `mini_shadow.orig.kara`); and, while speeding up the hash loops,
`append(mut x, if odd { p } else { c })`, which moves `p` or `c`
because an `if` is a value, not a place. Compiler gaps: none found. The
port matched the mirror on mir-interp and mir-llvm at the first run.

The hashes use `rounds=1000` (crypt's minimum) where they can, and only
one hash uses the default 5000 rounds: mir-interp, in the corpus's debug
build, takes about 2.6 ms per SHA round here, and the whole run has 60
seconds. It takes 43 seconds as it stands.

## Back to the original, 2026-10-18

The rows behind the edits above are fixed, so `mini_shadow.kara` is now exactly what `karac fix` makes of `mini_shadow.orig.kara`. It matches expected.out on all four MIR backends (karac at 4ea06e27b9).
