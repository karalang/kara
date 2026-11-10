#!/usr/bin/env python3
"""The M4b auto-par comparison: two `run.py` logs, the same backend with
`KARAC_AUTO_PAR` on and off, compared entry by entry. The bar is zero
differences.

    python3 scripts/corpus/m4b_compare.py on.log off.log

An entry differs when its verdict (PASS / FAIL / SKIP / MREJ) or its
message differs. A MIR refusal names locations (`bb7[1]`), and auto-par
reshapes the CFG before the move check, so the same refusal names
different blocks in the two lanes: locations are normalized to `bbN`
before comparing. `run.py` cuts a message at a fixed length, so a refusal
whose locations are longer in one lane can differ only in its last
characters; the shorter message must then be a prefix of the longer one.
`scripts/corpus/m4b_compare.sh` builds everything and runs both lanes.

Exits 0 with no differences, 1 with any, 2 when a log holds no entries
(a void run is not a pass).
"""
import re
import sys

LINE = re.compile(r"^(PASS|FAIL|SKIP|MREJ) (\S+)(.*)$")
LOCATION = re.compile(r"\bbb\d+(\[\d+\])?")


def load(path):
    entries = {}
    with open(path, errors="replace") as f:
        for line in f:
            m = LINE.match(line)
            if m:
                entries[m.group(2)] = (m.group(1), m.group(3).strip())
    return entries


def norm(message):
    return LOCATION.sub("bbN", message)


def same(a, b):
    if a is None or b is None or a[0] != b[0]:
        return False
    x, y = norm(a[1]).rstrip(")"), norm(b[1]).rstrip(")")
    if x == y:
        return True
    # A message cut at run.py's limit after locations of different lengths.
    short, long_ = sorted((x, y), key=len)
    return a[1] != b[1] and len(short) >= 150 and long_.startswith(short)


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    on, off = load(sys.argv[1]), load(sys.argv[2])
    print(f"on entries={len(on)} off entries={len(off)}")
    if not on or not off:
        print("a log holds no entries: void")
        sys.exit(2)
    for name, lane in (("on", on), ("off", off)):
        counts = {v: sum(1 for e in lane.values() if e[0] == v) for v in ("PASS", "FAIL", "SKIP", "MREJ")}
        print(f"{name}: " + " ".join(f"{k.lower()}={n}" for k, n in counts.items()))
    raw = [k for k in sorted(set(on) | set(off)) if on.get(k) != off.get(k)]
    diff = [k for k in raw if not same(on.get(k), off.get(k))]
    print(f"raw differences={len(raw)} after location normalization={len(diff)}")
    for k in raw:
        print(("  DIFF " if k in diff else "  locations-only ") + k)
        for name, lane in (("on ", on), ("off", off)):
            verdict, message = lane.get(k, ("MISSING", ""))
            print(f"     {name}: {verdict} {message[:300]}")
    sys.exit(1 if diff else 0)


if __name__ == "__main__":
    main()
