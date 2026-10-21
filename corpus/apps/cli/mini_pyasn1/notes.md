# mini_pyasn1

ASN.1 as pyasn1 0.6.1 handles it. pyasn1 is the pure-Python ASN.1
library behind pysnmp, ldap3, rsa, google-auth and many X.509 tools.
The port covers:
- the types: `Integer`, `Boolean`, `BitString`, `OctetString`, `Null`,
  `ObjectIdentifier`, `Real`, `Enumerated`, the character strings and
  the useful times, `Sequence`, `Set`, `SequenceOf`, `SetOf`, `Choice`
  and `Any`;
- tags, tag sets and tag maps, named types, named values, open types and
  every constraint, with their `repr`, `prettyPrint` and errors;
- schemas built at run time from `data/schemas.json`, the way an ASN.1
  module turns into Python classes (`schemas.py` holds the modules:
  parts of RFC 5280 (X.509), RFC 2986 (CSRs), SNMPv2c and LDAPv3, plus test types);
- the BER, CER and DER encoders and decoders: definite and indefinite
  lengths, chunked strings, the CER/DER ordering of `Set` and `SetOf`,
  decoding with and without a spec, open types, and every way a broken
  substrate fails; and the native codec to and from Python values;
- four programs:
  - a certificate report on the root CAs in `data/certs/`: subject,
    issuer, validity, key, and each extension decoded;
  - a CSR builder: the requests in `data/csr/` built, DER-encoded and
    written as PEM to `data/out/`, and a broken one refused;
  - an SNMP agent over the MIB in `data/snmp/mib.json`, answering get,
    get-next, get-bulk and set requests;
  - an LDAP server over `data/ldap/directory.json`, answering bind,
    search (with and, or, not, present, substrings, equality and
    greater-or-equal filters) and unbind.

The run writes `data/`, works through the 1005 jobs in `data/jobs.json`,
and then runs the four programs. `mk.py` wrote `data/`; the requests for
the SNMP agent and the LDAP server are encoded by pyasn1 there, the way
a peer would send them. The certificates are copied from the system's
CA store (`/etc/ssl/certs`), so `mk.py` needs one.

`oracle.py` runs the same jobs and programs through pyasn1 itself and
wrote `expected.out`. It uses the `pyasn1/` package from the 0.6.1
wheel, unchanged.

The quirks are kept. Some examples:
- An explicit tag over a primitive type in CER (or any indefinite mode)
  gets a definite length for the inner value and an end-of-contents
  marker for the outer one.
- The CER and DER decoders keep BER's decoders for `Boolean`,
  `BitString` and `OctetString`, because their type map only adds what
  is missing, so with a spec the strict checks never run.
- `T61String` and `ISO646String` share a base tag, so decoding without a
  spec falls back to the plain string type.
- Decoding tag 10 without a spec gives an `Integer`, not an
  `Enumerated`.
- An `Any` with an indefinite length always decodes to raw bytes.
- The decoder's marked position is shared across calls.
- A short substrate fails with "Short substrate on input", and an empty
  one with an `EndOfStreamError` that has no message.

## License

pyasn1 is under the BSD 2-Clause License, in `LICENSE-pyasn1.txt`.
