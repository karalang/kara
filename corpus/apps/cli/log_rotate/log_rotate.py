# Python mirror of source.kara: same rotation, same scan, same output.
import os

def segment_path(base, n):
    return base if n == 0 else f"{base}.{n}"

class Rotator:
    def __init__(self, base, max_bytes, keep):
        self.base, self.max_bytes, self.keep = base, max_bytes, keep
        self.segment, self.written = 0, 0
        self.file = open(base, "w")

    def rotate(self):
        self.file.flush()
        self.segment += 1
        content = open(self.base).read()
        open(segment_path(self.base, self.segment), "w").write(content)
        if self.segment > self.keep:
            open(segment_path(self.base, self.segment - self.keep), "w").write("")
        self.file.close()
        self.file = open(self.base, "w")
        self.written = 0

    def write_line(self, line):
        b = len(line.encode()) + 1
        if self.written + b > self.max_bytes and self.written > 0:
            self.rotate()
        self.file.write(line + "\n")
        self.written += b

def parse_line(line):
    parts = line.split(" ", 3)
    if len(parts) < 4:
        return None
    try:
        ts = int(parts[0])
    except ValueError:
        return None
    return ts, parts[1], parts[2], parts[3]

def scan(path, s):
    n = 0
    with open(path) as f:
        for line in f:
            line = line.rstrip("\n")
            n += 1
            e = parse_line(line)
            if e is None:
                s["bad"] += 1
                continue
            ts, level, comp, msg = e
            s["lines"] += 1
            s["by_level"][level] = s["by_level"].get(level, 0) + 1
            s["by_component"][comp] = s["by_component"].get(comp, 0) + 1
            if s["first_ts"] == 0 or ts < s["first_ts"]:
                s["first_ts"] = ts
            if ts > s["last_ts"]:
                s["last_ts"] = ts
            i = msg.find("took=")
            if i >= 0:
                rest = msg[i + 5:]
                end = rest.find("ms")
                if end < 0:
                    end = len(rest)
                try:
                    ms = int(rest[:end])
                except ValueError:
                    ms = 0
                s["slowest"] = max(s["slowest"], ms)
    return n

def tail_line(path):
    data = open(path, "rb").read().decode()
    body = data.rstrip()
    i = body.rfind("\n")
    return body[i + 1:] if i >= 0 else body

def make_line(i):
    levels = ["INFO", "INFO", "WARN", "INFO", "ERROR", "DEBUG"]
    comps = ["http", "db", "cache", "auth"]
    level, comp, ts = levels[i % 6], comps[(i * 7) % 4], 1700000000 + i * 3
    if i % 9 == 4:
        return f"{ts} {level} {comp} query done took={(i * 37) % 250}ms rows={i % 13}"
    if i % 17 == 16:
        return "garbled line without fields"
    return f"{ts} {level} {comp} request {i} served"

def main():
    rot = Rotator("app.log", 400, 3)
    for i in range(60):
        rot.write_line(make_line(i))
    rot.file.flush()
    print(f"segments rotated: {rot.segment}, live bytes: {rot.written}")
    s = {"lines": 0, "bad": 0, "by_level": {}, "by_component": {}, "first_ts": 0, "last_ts": 0, "slowest": 0}
    for seg in range(rot.segment, -1, -1):
        p = segment_path("app.log", seg)
        print(f"{p}: {scan(p, s)} lines")
    if os.path.exists("app.log.99"):
        print("app.log.99 exists?")
    else:
        print("app.log.99: not found")
    print(f"parsed {s['lines']}, unparsable {s['bad']}, span {s['last_ts'] - s['first_ts']}s, slowest {s['slowest']}ms")
    csv = "kind,name,count\n"
    for k in sorted(s["by_level"]):
        csv += f"level,{k},{s['by_level'][k]}\n"
    for k in sorted(s["by_component"]):
        csv += f"component,{k},{s['by_component'][k]}\n"
    open("summary.csv", "w").write(csv)
    total = 0
    for row in open("summary.csv").read().splitlines()[1:]:
        cols = row.split(",")
        if cols[0] == "level":
            total += int(cols[2])
        print(f"  {row}")
    print(f"level total {total}")
    rot.file.close()
    print(f"tail: {tail_line('app.log')}")

main()
