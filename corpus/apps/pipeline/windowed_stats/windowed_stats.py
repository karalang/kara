#!/usr/bin/env python3
"""Mirror of source.orig.kara: per-sensor tumbling/sliding window stats with
allowed lateness over a deterministic synthetic out-of-order feed."""


class ParseError(Exception):
    def __init__(self, msg):
        self.msg = msg


class Stats:
    def __init__(self):
        self.count = 0
        self.min = 0
        self.max = 0
        self.sum = 0
        self.sum_sq = 0

    def add(self, v):
        if self.count == 0 or v < self.min:
            self.min = v
        if self.count == 0 or v > self.max:
            self.max = v
        self.count += 1
        self.sum += v
        self.sum_sq += v * v

    def mean_centi(self):
        return round_div(self.sum, self.count)

    def stddev_milli(self):
        spread = self.count * self.sum_sq - self.sum * self.sum
        return isqrt(spread * 100) // self.count


class Ledger:
    def __init__(self):
        self.on_time = 0
        self.merged = 0
        self.dropped = 0
        self.worst_merged = 0

    def record(self, kind, late):
        if kind == "on_time":
            self.on_time += 1
            return True
        if kind == "merged":
            self.merged += 1
            if late > self.worst_merged:
                self.worst_merged = late
            return True
        self.dropped += 1
        return False


def floor_to(ts, size):
    return ts - ts % size


def round_div(num, den):
    if num >= 0:
        return (2 * num + den) // (2 * den)
    return -((-2 * num + den) // (2 * den))


def isqrt(n):
    if n < 2:
        return n
    x = n
    y = (x + 1) // 2
    while y < x:
        x = y
        y = (x + n // x) // 2
    return x


def fmt_fixed(v, places):
    scale = 10 ** places
    mag = abs(v)
    frac = str(mag % scale).rjust(places, "0")
    sign = "-" if v < 0 else ""
    return f"{sign}{mag // scale}.{frac}"


def pad_left(s, width):
    return s.rjust(width)


def parse_fixed(text):
    negative = text.startswith("-")
    body = text[1:] if negative else text
    parts = body.split(".")
    if len(parts) != 2 or len(parts[1]) != 2:
        return None
    try:
        whole = int(parts[0])
        frac = int(parts[1])
    except ValueError:
        return None
    v = whole * 100 + frac
    return -v if negative else v


def parse_line(line_no, line):
    fields = line.split(",")
    if len(fields) != 3:
        raise ParseError(f"line {line_no}: expected 3 fields")
    try:
        ts = int(fields[0])
    except ValueError:
        raise ParseError(f"line {line_no}: bad timestamp '{fields[0]}'")
    value = parse_fixed(fields[2])
    if value is None:
        raise ParseError(f"line {line_no}: bad value '{fields[2]}'")
    return (ts, fields[1], value)


def synthesize(sensor, seed, base, count):
    state = seed
    clock = 0
    out = ["ts,sensor,value\n"]
    for i in range(count):
        state = (state * 1103515245 + 12345) % 2147483648
        clock += 1 + state % 4
        state = (state * 1103515245 + 12345) % 2147483648
        ts = clock
        if state % 7 == 0:
            ts = clock - (state // 7) % 26
            if ts < 0:
                ts = 0
        state = (state * 1103515245 + 12345) % 2147483648
        value = base + (state % 1601) - 800
        if i % 37 == 19:
            out.append(f"{ts},{sensor},n/a\n")
        elif i % 53 == 41:
            out.append(f"{ts};{sensor};{fmt_fixed(value, 2)}\n")
        else:
            out.append(f"{ts},{sensor},{fmt_fixed(value, 2)}\n")
    return "".join(out)


def load_stream(name, content):
    events = []
    errors = 0
    lines = content.split("\n")
    for i in range(1, len(lines)):
        line = lines[i].strip()
        if not line:
            continue
        try:
            events.append(parse_line(i, line))
        except ParseError as e:
            errors += 1
            print(f"  [{name}] skipped {e.msg}")
    print(f"  [{name}] parsed {len(events)} events, {errors} rejected")
    return events


class Pipeline:
    def __init__(self, config):
        self.config = config
        self.high_water = {}
        self.tumbling = {}
        self.sliding = {}
        self.ledgers = {}

    def classify(self, ts, sensor):
        if sensor not in self.high_water:
            return ("on_time", 0)
        lateness = self.high_water[sensor] - ts
        if lateness <= 0:
            return ("on_time", 0)
        if lateness <= self.config["allowed_lateness"]:
            return ("merged", lateness)
        return ("dropped", lateness)

    def ingest(self, ev):
        ts, sensor, value = ev
        kind, late = self.classify(ts, sensor)
        ledger = self.ledgers.pop(sensor, None) or Ledger()
        accepted = ledger.record(kind, late)
        self.ledgers[sensor] = ledger
        if not accepted:
            return
        high = self.high_water.get(sensor, ts)
        if ts >= high:
            self.high_water[sensor] = ts
        bump(self.tumbling, (sensor, floor_to(ts, self.config["tumble_size"])), value)
        start = floor_to(ts, self.config["slide_step"])
        while start > ts - self.config["slide_size"]:
            if start >= 0:
                bump(self.sliding, (sensor, start), value)
            start -= self.config["slide_step"]


def bump(windows, key, value):
    st = windows.pop(key, None) or Stats()
    st.add(value)
    windows[key] = st


def print_windows(title, windows, size):
    print(f"== {title} ==")
    print("sensor        window      n      min      max     mean    stddev")
    for key in sorted(windows.keys()):
        sensor, start = key
        st = windows[key]
        span = f"[{start},{start + size})"
        print(f"{pad_left(sensor, 7)} {pad_left(span, 12)} {pad_left(str(st.count), 6)} "
              f"{pad_left(fmt_fixed(st.min, 2), 8)} {pad_left(fmt_fixed(st.max, 2), 8)} "
              f"{pad_left(fmt_fixed(st.mean_centi(), 2), 8)} {pad_left(fmt_fixed(st.stddev_milli(), 3), 9)}")


def run():
    config = {"tumble_size": 60, "slide_size": 40, "slide_step": 20, "allowed_lateness": 12}
    print(f"config: tumble={config['tumble_size']}s slide={config['slide_size']}s/{config['slide_step']}s "
          f"lateness={config['allowed_lateness']}s")

    feeds = [("boiler", 17, 6500, 90), ("intake", 4242, 1200, 80), ("exhaust", 991, 9800, 70)]
    for name, seed, base, count in feeds:
        with open(f"feed_{name}.csv", "w") as f:
            f.write(synthesize(name, seed, base, count))
    raws = {}
    for name, _, _, _ in feeds:
        with open(f"feed_{name}.csv") as f:
            raws[name] = f.read()

    print("loading feeds:")
    streams = [load_stream(name, raws[name]) for name, _, _, _ in feeds]

    longest = max(len(s) for s in streams)
    pipeline = Pipeline(config)
    for i in range(longest):
        for s in streams:
            if i < len(s):
                pipeline.ingest(s[i])

    print_windows("tumbling", pipeline.tumbling, config["tumble_size"])
    print_windows("sliding", pipeline.sliding, config["slide_size"])

    print("== lateness ==")
    total_dropped = 0
    total_merged = 0
    for name in sorted(pipeline.ledgers.keys()):
        l = pipeline.ledgers[name]
        total_dropped += l.dropped
        total_merged += l.merged
        print(f"{pad_left(name, 7)} on_time={l.on_time} merged={l.merged} dropped={l.dropped} "
              f"worst_merged={l.worst_merged}s")
    print(f"total: merged={total_merged} dropped={total_dropped} "
          f"tumbling_windows={len(pipeline.tumbling)} sliding_windows={len(pipeline.sliding)}")


if __name__ == "__main__":
    run()
