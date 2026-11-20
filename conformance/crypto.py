"""docs/library/crypto.md against Python's hashlib and hmac, and against the
published vectors: FIPS 180-4's examples (the NIST "abc" and two-block
messages) and RFC 4231's HMAC-SHA-2 test cases. The streaming types are
checked to give the one-shot digest for any split of the same bytes.

    python3 conformance/crypto.py [--seed N] [--cases N]
"""

import argparse
import hashlib
import hmac
import random

from harness import Report, kara_str, run

# FIPS 180-4 example messages (csrc.nist.gov examples) and their SHA-256.
FIPS = [
    (b"abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
    (b"", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
    (b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
     "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"),
]

# RFC 4231 §4 test cases 1-4, 6, 7: (key, data, hmac-sha256).
RFC4231 = [
    (b"\x0b" * 20, b"Hi There", "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"),
    (b"Jefe", b"what do ya want for nothing?",
     "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"),
    (b"\xaa" * 20, b"\xdd" * 50, "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"),
    (bytes(range(1, 26)), b"\xcd" * 50, "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b"),
    (b"\xaa" * 131, b"Test Using Larger Than Block-Size Key - Hash Key First",
     "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"),
    (b"\xaa" * 131,
     b"This is a test using a larger than block-size key and a larger than block-size data. "
     b"The key needs to be hashed before being used by the HMAC algorithm.",
     "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2"),
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--cases", type=int, default=60)
    args = ap.parse_args()
    rng = random.Random(args.seed)
    rep = Report("crypto")

    # The published vectors check the oracle itself.
    for m, d in FIPS:
        assert hashlib.sha256(m).hexdigest() == d
    for k, m, d in RFC4231:
        assert hmac.new(k, m, hashlib.sha256).hexdigest() == d

    datas = [m for m, _ in FIPS] + [m for _, m, _ in RFC4231]
    datas += [bytes(rng.randrange(256) for _ in range(rng.choice([0, 1, 55, 56, 63, 64, 65, 111, 112,
                                                                    127, 128, 129, 200, 1000])))
              for _ in range(args.cases)]
    keys = [k for k, _, _ in RFC4231] + [b"", b"k", b"\x00" * 64, b"\x01" * 65, b"\x02" * 128,
                                         b"\x03" * 129]
    keys += [bytes(rng.randrange(256) for _ in range(rng.randrange(200))) for _ in range(args.cases // 2)]

    lines, labels, want = [], [], []

    def emit(label, expr, w):
        lines.append(f"    println({expr});")
        labels.append(label)
        want.append(w)

    def b(x: bytes) -> str:
        return f"hex_decode({kara_str(x.hex())}).unwrap()"

    for d in datas:
        for name, h in (("sha256", hashlib.sha256), ("sha384", hashlib.sha384), ("sha512", hashlib.sha512),
                        ("sha1", hashlib.sha1)):
            emit(f"{name} {d.hex()[:40]}", f"hex_encode({name}({b(d)}.as_slice()).as_slice())",
                 h(d).hexdigest())
    for i, d in enumerate(datas):
        k = keys[i % len(keys)]
        for name, h in (("hmac_sha256", hashlib.sha256), ("hmac_sha384", hashlib.sha384),
                        ("hmac_sha512", hashlib.sha512)):
            emit(f"{name} k={k.hex()[:20]} d={d.hex()[:20]}",
                 f"hex_encode({name}({b(k)}.as_slice(), {b(d)}.as_slice()).as_slice())",
                 hmac.new(k, d, h).hexdigest())
    # The streaming types: random splits give the one-shot digest.
    for i, d in enumerate(datas[: args.cases // 2 + 8]):
        cuts = sorted(rng.randrange(len(d) + 1) for _ in range(rng.randrange(4)))
        parts = [d[a:c] for a, c in zip([0] + cuts, cuts + [len(d)])]
        k = keys[i % len(keys)]
        for ty, h in (("Sha256", hashlib.sha256), ("Sha384", hashlib.sha384), ("Sha512", hashlib.sha512),
                      ("Sha1", hashlib.sha1)):
            ups = "".join(f" h.update({b(p)}.as_slice());" for p in parts)
            emit(f"{ty} parts {[p.hex()[:8] for p in parts]}",
                 f"{{ let mut h = {ty}.new();{ups} hex_encode(h.finish().as_slice()) }}", h(d).hexdigest())
        for ty, h in (("HmacSha256", hashlib.sha256), ("HmacSha384", hashlib.sha384),
                      ("HmacSha512", hashlib.sha512)):
            ups = "".join(f" h.update({b(p)}.as_slice());" for p in parts)
            emit(f"{ty} parts", f"{{ let mut h = {ty}.new({b(k)}.as_slice());{ups} hex_encode(h.finish().as_slice()) }}",
                 hmac.new(k, d, h).hexdigest())
    # constant_time_eq: the same bytes, different bytes, different lengths.
    pairs = [(b"", b""), (b"a", b"a"), (b"a", b"b"), (b"ab", b"a"), (b"", b"\x00"), (b"\x00" * 32, b"\x00" * 32)]
    for d in datas[:20]:
        e = bytearray(d)
        if e:
            e[rng.randrange(len(e))] ^= 1 << rng.randrange(8)
        pairs += [(d, d), (d, bytes(e)), (d, d + b"\x00")]
    for x, y in pairs:
        emit(f"constant_time_eq {x.hex()[:16]} {y.hex()[:16]}",
             f"constant_time_eq({b(x)}.as_slice(), {b(y)}.as_slice())", "true" if x == y else "false")
    # crypto.md's WebSocket example.
    emit("websocket accept", 'accept_value("dGhlIHNhbXBsZSBub25jZQ==")', "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
    # One million `a` (FIPS 180-4's long example).
    emit("sha256 a*1e6", 'hex_encode(sha256("a".repeat(1000000).bytes()).as_slice())',
         hashlib.sha256(b"a" * 1000000).hexdigest())

    src = (
        "import std.crypto.sha2.{sha256, sha384, sha512, Sha256, Sha384, Sha512};\n"
        "import std.crypto.hmac.{hmac_sha256, hmac_sha384, hmac_sha512, HmacSha256, HmacSha384, HmacSha512};\n"
        "import std.crypto.legacy.{sha1, Sha1};\n"
        "import std.crypto.{constant_time_eq};\n"
        "import std.encoding.{hex_encode, hex_decode, base64_encode};\n\n"
        "fn accept_value(key: Str) -> String {\n"
        "    base64_encode(sha1(f\"{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11\".bytes()))\n"
        "}\n\n"
        "fn main() {\n" + "\n".join(lines) + "\n}\n"
    )
    rep.check_runs(run(src), labels, want)
    rep.finish()


if __name__ == "__main__":
    main()
