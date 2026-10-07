#!/usr/bin/env python3
"""Python mirror of source.orig.kara (same algorithm, same data, same output)."""

LEVELS = ["DEBUG", "INFO", "WARN", "ERROR"]
LINES_PER_FILE = 40


class ParseError(Exception):
    pass


class Rng:
    def __init__(self, seed):
        self.state = seed

    def next(self, bound):
        self.state = (self.state * 1103515245 + 12345) % 2147483648
        return (self.state // 65536) % bound


def info_message(level, roll):
    if level == "DEBUG":
        return "cache probe" if roll % 2 == 0 else "retry scheduled"
    if level == "INFO":
        return "request served" if roll % 2 == 0 else "job finished"
    if level == "WARN":
        return "slow response" if roll % 2 == 0 else "queue backlog"
    return "unexpected failure"


def generate_log(spec):
    rng = Rng(spec["seed"])
    out = []
    for i in range(LINES_PER_FILE):
        if i % 13 == 12:
            out.append("--- log rotated ---\n")
            continue
        roll = rng.next(100)
        if roll < 15:
            level = "DEBUG"
        elif roll < 60:
            level = "INFO"
        elif roll < 80:
            level = "WARN"
        else:
            level = "ERROR"
        comps = spec["components"]
        component = comps[rng.next(len(comps))]
        latency = 5 + rng.next(400)
        if level == "ERROR":
            latency += 200
            errs = spec["errors"]
            message = errs[rng.next(len(errs))]
        else:
            message = info_message(level, rng.next(10))
        latency_text = "n/a" if i % 17 == 16 else str(latency)
        out.append(f"{i}|{level}|{component}|{latency_text}|{message}\n")
    return "".join(out)


def parse_int(s):
    s = s.strip()
    try:
        return int(s)
    except ValueError:
        return None


def parse_line(line):
    fields = line.split("|")
    if len(fields) != 5:
        raise ParseError(f"expected 5 fields, got {len(fields)}")
    seq = parse_int(fields[0])
    if seq is None:
        raise ParseError(f"bad number '{fields[0]}'")
    level = fields[1].strip()
    if level not in LEVELS:
        raise ParseError(f"unknown level '{fields[1]}'")
    latency = parse_int(fields[3])
    if latency is None:
        raise ParseError(f"bad number '{fields[3]}'")
    return {
        "seq": seq,
        "level": level,
        "component": fields[2].strip(),
        "latency_ms": latency,
        "message": fields[4].strip(),
    }


def parse_log(service, text):
    entries, problems = [], []
    line_no = 0
    for line in text.split("\n"):
        if line == "":
            continue
        line_no += 1
        try:
            entries.append(parse_line(line))
        except ParseError as e:
            problems.append(f"{service}.log:{line_no}: {e}")
    return {"service": service, "entries": entries, "problems": problems}


def percentile(sorted_samples, pct):
    if not sorted_samples:
        return 0
    rank = (len(sorted_samples) * pct + 99) // 100
    if rank < 1:
        rank = 1
    return sorted_samples[rank - 1]


def main():
    specs = [
        {"name": "app", "seed": 7, "components": ["api", "cache", "worker"],
         "errors": ["upstream timeout", "null payload", "template render failed"]},
        {"name": "auth", "seed": 42, "components": ["login", "token", "session"],
         "errors": ["invalid token", "password mismatch", "account locked"]},
        {"name": "db", "seed": 1234, "components": ["query", "pool", "replica"],
         "errors": ["deadlock detected", "connection refused", "upstream timeout"]},
    ]
    for spec in specs:
        with open(f"{spec['name']}.log", "w") as f:
            f.write(generate_log(spec))
    logs = []
    for spec in specs:
        with open(f"{spec['name']}.log") as f:
            raw = f.read()
        logs.append(parse_log(spec["name"], raw))

    level_counts = [0, 0, 0, 0]
    comp_counts, comp_lat, err_counts = {}, {}, {}
    total_lines = 0
    for log in logs:
        total_lines += len(log["entries"]) + len(log["problems"])
        for e in log["entries"]:
            level_counts[LEVELS.index(e["level"])] += 1
            key = f"{log['service']}/{e['component']}"
            comp_counts[key] = comp_counts.get(key, 0) + 1
            comp_lat.setdefault(key, []).append(e["latency_ms"])
            if e["level"] == "ERROR":
                err_counts[e["message"]] = err_counts.get(e["message"], 0) + 1

    parsed = sum(len(l["entries"]) for l in logs)
    malformed = sum(len(l["problems"]) for l in logs)
    print("== log summary ==")
    print(f"files: {len(logs)}, lines: {total_lines}, parsed: {parsed}, malformed: {malformed}")
    print("-- malformed lines --")
    for log in logs:
        for p in log["problems"]:
            print(f"  {p}")
    print("-- levels --")
    for i, name in enumerate(LEVELS):
        print(f"  {name}: {level_counts[i]}")
    print("-- components --")
    for name in sorted(comp_counts):
        samples = sorted(comp_lat.get(name, []))
        p50 = percentile(samples, 50)
        p95 = percentile(samples, 95)
        print(f"  {name}: count={comp_counts[name]} p50={p50}ms p95={p95}ms")
    print("-- top errors --")
    ranks = sorted((-c, m) for m, c in err_counts.items())
    for rank, (neg, msg) in enumerate(ranks[:5], start=1):
        print(f"  {rank}. {msg} ({-neg})")


if __name__ == "__main__":
    main()
