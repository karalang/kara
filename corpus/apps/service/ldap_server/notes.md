# service/ldap_server

An LDAPv3 directory server on loopback TCP (RFC 4511), with every message
BER-encoded, and the scripted clients that bind, search, compare, add,
modify and delete. Each connection is a task. The directory is one tree
behind a `Mutex`, kept in tree order.

- **BER.** The program has its own codec: tag, length and value, with short
  and long lengths up to three bytes. The indefinite length (`0x80`) is
  refused, as LDAP requires, with a protocolError and a notice of
  disconnection (`1.3.6.1.4.1.1466.20036`). An `LDAPMessage` is a SEQUENCE
  of the message id and one operation.
- **Names.** DNs are parsed with RFC 4514's escapes (`cn=Smith\, John`) and
  compared case-insensitively, with the nearest RDN first. A DN that does
  not parse is invalidDNSyntax. The tree is a `SortedMap` keyed root first,
  so a subtree search walks one key range in tree order.
- **Filters.** RFC 4515 filter strings, with and, or, not, equality,
  substrings (`(cn=b*b*r)`), `>=`, `<=`, presence, approx (matched as
  equality) and `\2c` escapes. They are encoded to BER by the client and decoded by the
  server. Matching folds ASCII case, and `uidNumber` compares as a number.
- **Operations.**
  - Search runs at base, one level or subtree scope, with an attribute
    list (`1.1` for none), types only and a size limit
    (sizeLimitExceeded).
  - Compare answers compareTrue or compareFalse.
  - Add checks that the parent exists, that `objectClass` is present, and
    that the RDN's value is among the attributes.
  - Modify supports add, delete and replace. It answers noSuchAttribute,
    attributeOrValueExists, and objectClassViolation for an empty
    `objectClass`. An RDN attribute can't be dropped (namingViolation).
  - Delete refuses a non-leaf entry.
  - Bind is simple bind only, with a password per entry. A wrong password
    is invalidCredentials, and an LDAPv2 bind is a protocolError.
  - Writes need the admin, except that a user may change their own
    `mail`. Anything else is insufficientAccessRights. `userPassword` is never
    shown to anyone but the admin.
- **The clients.** There are six sessions, each its own connection:
  - anonymous reads, ending in a refused add;
  - alice, with wrong and right passwords, modifying herself and failing
    on bob and carol;
  - the admin, with every write and every refusal;
  - a version-2 bind;
  - a client that sends an indefinite length;
  - a last subtree search of the whole tree.

  At the end the server prints its entry count and per-operation and
  per-result counts. All credentials are obviously fake.

The mirror `ldap_server.py` is the same server and clients, written
independently with `socket` and `threading` and its own BER codec. It
wrote `expected.out`.

Written 2026-11-26 by the real-world apps thread. karac carries the fixes
below.

## From ldap_server.orig.kara to ldap_server.kara

The draft was written Rust-shaped:

- `use std::collections::BTreeMap` and `Arc<Mutex<State>>`;
- `&str`, `&[u8]` and `&[(&str, &[&str])]` parameters, called with
  `&[("objectClass", &["top", "domain"])]`;
- `Filter::Eq(..)` patterns and `for (t, vs) in &self.attrs`;
- `t.to_ascii_lowercase()` on Strings;
- `u8::from_str_radix(hex, 16).map_err(|_| ..)?`;
- `assert_eq!(rest, s.len(), "trailing text in filter '{}'", s)`;
- `parse_message(raw).map(Some)`;
- `TaskGroup::new()` and `group.spawn(move || ..)`.

Seven gaps turned up. Each was fixed in the compiler before the fix passes
were rerun:

- B-2026-11-26-60: `F.Eq(..)` is Rust's rest pattern for a tuple variant.
  It now gets a fix to Kāra's `F.Eq { .. }`.
- B-2026-11-26-61: returning `Some(vs)` from inside
  `for (t, vs) in ref self.attrs` was refused with E0509, as if `vs`
  borrowed a local. The loop borrows the receiver's own field.
- B-2026-11-26-62: the `attrs([("objectClass", ["top", "domain"])])` calls
  were refused 61 times with "expected '(Str, Slice[Str])', found
  '(String, Vec[String])'". A list literal in a tuple's `Slice` slot inside
  a call argument is now lent for the call, as one passed straight to a
  `Slice` parameter is (review ruling 2026-11-26).
- B-2026-11-26-63: `String` had no `to_ascii_lowercase`, which attribute
  names and DNs want. It now has that and `to_ascii_uppercase`, with Rust's
  meaning.
- B-2026-11-26-64: `.map_err(|_| e)` on Kāra's `Option`-returning
  `from_str_radix` was "fixed" to a one-argument `map_or`. It now becomes
  `.ok_or_else(|| e)`.
- B-2026-11-26-65: `assert_eq!` with format arguments lost its `!` and
  then failed on its argument count. Its message now becomes one f-string.
- B-2026-11-26-66: `.map(Some)` passed the check, then failed to build
  with "no body for `Some`". A constructor named as a value is refused, as
  a program's own variant always was, and the fix writes `|a0| Some(a0)`.

After those, `karac fix` rewrote the draft in one run of passes (1085
fixes) with no hand edits. The first run matched the mirror byte for byte.
