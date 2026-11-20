"""docs/library/encoding.md against Python's base64, binascii and
urllib.parse, with the page's own rules where Python is more lenient
(Python accepts non-canonical Base64 and skips whitespace in hex).

    python3 conformance/encoding.py [--seed N] [--cases N]
"""

import argparse
import base64
import random
import re
import urllib.parse

from harness import Report, kara_str, run

STD = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
URL = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
HEXD = "0123456789abcdefABCDEF"


def b64_ok(text: str, url: bool) -> bytes | None:
    """The bytes encoding.md says `text` decodes to, or None for an error."""
    alpha = URL if url else STD
    body = text.rstrip("=")
    pads = len(text) - len(body)
    if pads > 2 or any(c not in alpha for c in body):
        return None
    if len(body) % 4 == 1:
        return None
    if not url and len(text) % 4 != 0:
        return None
    if url and pads and len(text) % 4 != 0:
        return None
    padded = body + "=" * (-len(body) % 4)
    raw = base64.b64decode(padded.translate(str.maketrans("-_", "+/")) if url else padded)
    canon = (base64.urlsafe_b64encode(raw).decode().rstrip("=") if url
             else base64.b64encode(raw).decode())
    if canon != (body if url else text):
        return None
    return raw


def b64_err_ok(text: str, url: bool, err: str) -> bool:
    """Whether `err` is an error encoding.md allows for `text`. Offsets and
    lengths are in bytes, as the decoders read the text."""
    alpha = URL if url else STD
    text = text.encode().decode("latin-1")
    m = re.fullmatch(r"err invalid byte at offset (\d+)", err)
    if m:
        at = int(m.group(1))
        if at >= len(text):
            return False
        b = ord(text[at])
        body_len = len(text.rstrip("="))
        c = chr(b) if b < 128 else None
        if c is None or (c not in alpha and c != "="):
            return True
        if c == "=" and at < body_len:
            return True  # `=` inside the text
        # An alphabet character can only be refused for its non-zero
        # trailing bits: the last character of the body.
        return at == body_len - 1
    if err == "err invalid length":
        body = text.rstrip("=")
        pads = len(text) - len(body)
        return (pads > 2 or len(body) % 4 == 1 or (not url and len(text) % 4 != 0)
                or (pads > 0 and len(text) % 4 != 0))
    return False


def hex_ok(text: str):
    if len(text.encode()) % 2 or any(c not in HEXD for c in text):
        return None
    return bytes.fromhex(text)


def pct_ok(text: str):
    b = text.encode()
    out = bytearray()
    i = 0
    while i < len(b):
        if b[i] == 0x25:
            h = b[i + 1:i + 3]
            if len(h) < 2 or any(chr(x) not in HEXD for x in h):
                return ("byte", i)
            out.append(int(h.decode(), 16))
            i += 3
        else:
            out.append(b[i])
            i += 1
    return bytes(out)


class Want:
    def __init__(self, fn, text):
        self.fn, self.text = fn, text

    def __call__(self, got):
        return self.fn(got)


def mutate(rng, s, alpha):
    s = list(s)
    for _ in range(rng.randint(1, 3)):
        op = rng.randrange(5)
        pos = rng.randrange(len(s) + 1)
        if op == 0 and s:
            del s[min(pos, len(s) - 1)]
        elif op == 1:
            s.insert(pos, rng.choice(alpha + "=  \n.%é"))
        elif op == 2 and s:
            s[min(pos, len(s) - 1)] = rng.choice(alpha)
        elif op == 3:
            s.append("=")
        else:
            s.insert(pos, "=")
    return "".join(s)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--cases", type=int, default=300)
    args = ap.parse_args()
    rng = random.Random(args.seed)
    rep = Report("encoding")

    datas = [b"", b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar", b"hello", bytes(range(256))]
    datas += [bytes(rng.randrange(256) for _ in range(rng.randrange(40))) for _ in range(args.cases)]
    texts_b64 = ["", "Zg==", "Zm8=", "Zm9v", "Zg=", "Zg", "Zh==", "Zm9=", "====", "Z===", "Zm9v\n",
                 " Zm9v", "Zm9vYg==", "Zm9vYmE=", "Zm9vYmFy", "-_8=", "+/8=", "Zg==Zg==", "=Zg="]
    texts_b64 += [mutate(rng, base64.b64encode(d).decode(), STD) for d in datas[:args.cases]]
    texts_url = ["", "Zg", "Zg==", "Zg=", "-_8", "+/8", "Zh", "Zm8", "Zm9", "Zm8=", "Zm8==", "Zg==="]
    texts_url += [mutate(rng, base64.urlsafe_b64encode(d).decode().rstrip("="), URL)
                  for d in datas[:args.cases]]
    texts_url += [base64.urlsafe_b64encode(d).decode() for d in datas[:30]]
    texts_hex = ["", "0", "00", "fF", "Ab", "g0", "0g", "00 ", " 00", "é0"]
    texts_hex += [mutate(rng, d.hex(), HEXD) for d in datas[:args.cases // 2]]
    texts_hex += [d.hex().upper() for d in datas[:20]]
    texts_pct = ["", "%", "%4", "%41", "%4g", "%g4", "a%2", "a%20b", "%e9%E9", "+%2B+", "100%", "%%41",
                 "é%C3%A9", "%00", "%FF%fe"]
    texts_pct += [mutate(rng, urllib.parse.quote(d, safe=""), HEXD + "%") for d in datas[:args.cases // 2]]

    lines, labels, want = [], [], []

    def emit(label, expr, w):
        lines.append(f"    println({expr});")
        labels.append(label)
        want.append(w)

    for d in datas:
        lit = kara_str(d.hex())
        src = f"hex_decode({lit}).unwrap().as_slice()"
        emit(f"base64_encode {d.hex()}", f"base64_encode({src})", base64.b64encode(d).decode())
        emit(f"base64url_encode {d.hex()}", f"base64url_encode({src})",
             base64.urlsafe_b64encode(d).decode().rstrip("="))
        emit(f"hex_encode {d.hex()}", f"hex_encode({src})", d.hex())
        emit(f"percent_encode {d.hex()}", f"percent_encode({src})", urllib.parse.quote(d, safe=""))

    for fn, texts, url in (("base64_decode", texts_b64, False), ("base64url_decode", texts_url, True)):
        for t in texts:
            ok = b64_ok(t, url)
            if ok is not None:
                w = "ok " + ok.hex()
            else:
                w = Want(lambda g, t=t, url=url: b64_err_ok(t, url, g), f"an error allowed for {t!r}")
            emit(f"{fn} {t!r}", f"show({fn}({kara_str(t)}))", w)
    for t in texts_hex:
        ok = hex_ok(t)
        if ok is not None:
            w = "ok " + ok.hex()
        else:
            n = len(t.encode())
            def herr(g, t=t, n=n):
                if g == "err invalid length":
                    return n % 2 == 1
                m = re.fullmatch(r"err invalid byte at offset (\d+)", g)
                if not m or int(m.group(1)) >= n:
                    return False
                return chr(t.encode()[int(m.group(1))]) not in HEXD
            w = Want(herr, f"an error allowed for {t!r}")
        emit(f"hex_decode {t!r}", f"show(hex_decode({kara_str(t)}))", w)
    for t in texts_pct:
        r = pct_ok(t)
        w = f"err invalid byte at offset {r[1]}" if isinstance(r, tuple) else "ok " + r.hex()
        emit(f"percent_decode {t!r}", f"show(percent_decode({kara_str(t)}))", w)

    src = (
        "import std.encoding.{base64_encode, base64_decode, base64url_encode, base64url_decode,\n"
        "    hex_encode, hex_decode, percent_encode, percent_decode, DecodeError};\n\n"
        "fn show(r: Result[Vec[u8], DecodeError]) -> String {\n"
        "    match r {\n"
        "        Ok(v) => f\"ok {hex_encode(v.as_slice())}\",\n"
        "        Err(e) => f\"err {e}\",\n"
        "    }\n"
        "}\n\n"
        "fn main() {\n" + "\n".join(lines) + "\n}\n"
    )
    rep.check_runs(run(src), labels, want)
    rep.finish()


if __name__ == "__main__":
    main()
