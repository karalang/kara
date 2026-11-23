# service/snmp_agent

An SNMP agent (SNMPv1, RFC 1157, and SNMPv2c, RFCs 1901 and 3416) on
loopback UDP, and the managers that query it. Every message is BER-encoded
ASN.1 (X.690): a type tag, a length in short or long form, and the
contents. A message is a SEQUENCE of the version, the community string and
one PDU, and a PDU is a request id, two integers (error status and index,
or GETBULK's non-repeaters and max-repetitions) and a list of varbinds.

- **The MIB.** The agent keeps its objects in a `SortedMap[Vec[u32],
  Value]` keyed by OID, so GETNEXT is `mib.higher(oid)` and a walk ends
  when the next OID leaves the subtree (`next.starts_with(base)`). Values
  are INTEGER, OCTET STRING, NULL, OBJECT IDENTIFIER, IpAddress, Counter32,
  Gauge32, TimeTicks and Counter64, the last at 2^64 - 1 to check the
  unsigned encodings. sysUpTime is counted from the packets the agent has
  received, so every run prints the same ticks.
- **The agent.** One task serves the socket. It answers GET, GETNEXT,
  GETBULK and SET under the `public` (read) and `private` (write)
  communities, drops a wrong community, a response PDU and anything that
  does not decode, and reports why. v1 errors are `noSuchName` and
  `badValue` with the index; v2c answers a missing object with
  `noSuchObject` or `noSuchInstance` in the varbind and a walk past the end
  with `endOfMibView`. A SET checks every binding before it writes any,
  and v2c refuses one with `notWritable`, `noCreation`, `wrongType`,
  `wrongLength` or `wrongValue`. A reply over 484 bytes is `tooBig`, and a GETBULK is cut to
  the varbinds that fit. A SET of the halt object stops it, and its log and
  counts print last.
- **The managers.** `nms` (v2c, public) gets and walks, `old` (v1) gets,
  `ops` and `v1ops` set, and `probe` sends malformed and stray packets.

The mirror `snmp_agent.py` is the same agent and managers written
independently with `socket` and `threading`; it wrote `expected.out`.

Written 2026-11-19 by the real-world apps thread; karac carries the fixes
below.

## From snmp_agent.orig.kara to snmp_agent.kara

The draft was written Rust-shaped (`use` and `::` paths,
`BTreeMap<Vec<u32>, Value>` with
`range((Bound::Excluded(o.to_vec()), Bound::Unbounded)).next()`,
`HashMap<String, String>`, `impl fmt::Display for Value` with `write!`,
`thread::spawn(move || { agent.serve(); agent })` and
`handle.join().unwrap()`, `format!("{:#04x}")`, `vec!["..."; 20]`,
`&[u32]`, `collect::<Vec<_>>()`, `next.starts_with(&base)`). Six gaps
turned up, and each was fixed in the compiler before the fix passes were
rerun:

- B-2026-11-19-80: `thread::spawn(move || ..)` had a message and no fix. In a
  function body it now gets `let group = TaskGroup.new();` before the
  statement and `group.spawn(|| ..)` in its place.
- B-2026-11-19-81: Rust's alternate form `{:#04x}` had no fix, in `format!` or
  in a hole. It is now `0x{v:02x}`, as Rust's width counts the prefix.
- B-2026-11-19-82: `SortedMap` had no `lower` or `higher`, so the `Bound`
  lookups had nothing to become. Both exist now, and
  `use std::ops::Bound;` is deleted with each
  `range((Excluded(k), Unbounded)).next()` written `higher(k)` (and its
  siblings `ceiling`, `lower` and `floor`).
- B-2026-11-19-83: `vec!["1.3.6.1.2.1.1.1.0"; 20]` mended to `Vec.filled(20,
  "..")`, a `Vec[String]`, and passing it as `Slice[Str]` was refused with
  no fix. The binding now gets `: Vec[Str]`.
- B-2026-11-19-84: `Slice` (so `Vec`) had no `starts_with` or `ends_with`. Both
  compare elements now.
- B-2026-11-19-85: the spawned closure writes `agent` (`agent.serve()`) and then
  returns it. `karac check` passed it and `run` refused it as a move out
  of a borrowed place; it now takes `agent` by move, as core semantics
  §9.1 says.

After those, `karac fix` rewrote the draft in one pass (713 fixes), with
no hand edits, and the first run matched the mirror byte for byte.
