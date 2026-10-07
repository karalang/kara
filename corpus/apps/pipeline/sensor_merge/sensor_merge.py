#!/usr/bin/env python3
"""Python mirror of pipeline/sensor_merge (source.orig.kara)."""

LABELS = {"temp": ("temp", "C"), "humid": ("humid", "%"), "press": ("press", "hPa")}

FRESH, CARRIED, MISSING = 0, 1, 2


class PipelineError(Exception):
    pass


def tdiv(a, b):
    """Integer division truncating toward zero (Kara/C semantics)."""
    q = abs(a) // abs(b)
    return q if (a >= 0) == (b >= 0) else -q


def format_tenths(v):
    sign = "-" if v < 0 else ""
    mag = abs(v)
    return f"{sign}{mag // 10}.{mag % 10}"


def pad_left(s, width):
    return " " * max(0, width - len(s)) + s


def render_cell(cell):
    tag, v = cell
    if tag == FRESH:
        return format_tenths(v)
    if tag == CARRIED:
        return format_tenths(v) + "*"
    return "-"


def wobble(i, swing):
    phase = (i * 7 + 3) % 11
    return tdiv((phase - 5) * swing, 5)


def generate_csv(spec, horizon):
    out = ["ts,value\n"]
    ts = spec["offset"]
    i = 0
    while ts <= horizon:
        d = spec["drop_every"]
        dropped = d > 0 and i % d == d - 1
        if not dropped:
            value = spec["base"] + wobble(i, spec["swing"])
            out.append(f"{ts},{value}\n")
        ts += spec["period"]
        i += 1
    return "".join(out)


def parse_log(kind, path, text):
    readings = []
    line_no = 0
    last_ts = -1
    for raw in text.split("\n"):
        line_no += 1
        line = raw.strip()
        if line == "" or line_no == 1:
            continue
        parts = line.split(",")
        if len(parts) != 2:
            raise PipelineError(f"{path}:{line_no}: cannot parse '{line}'")
        try:
            ts = int(parts[0].strip())
            value = int(parts[1].strip())
        except ValueError:
            raise PipelineError(f"{path}:{line_no}: cannot parse '{line}'")
        if ts <= last_ts:
            raise PipelineError(f"{path}:{line_no}: timestamp went backwards")
        last_ts = ts
        readings.append((ts, value))
    return {"kind": kind, "readings": readings}


def load(kind, path):
    try:
        with open(path) as f:
            text = f.read()
    except OSError:
        raise PipelineError(f"io error on {path}")
    return parse_log(kind, path, text)


def next_source(logs, cursors):
    best = None
    best_ts = 0
    for k in range(len(logs)):
        pos = cursors[k]
        if pos < len(logs[k]["readings"]):
            ts = logs[k]["readings"][pos][0]
            if best is None or ts < best_ts:
                best = k
                best_ts = ts
    return best


def merge(logs):
    n = len(logs)
    cursors = [0] * n
    last = [None] * n
    rows = []
    while True:
        first = next_source(logs, cursors)
        if first is None:
            break
        ts = logs[first]["readings"][cursors[first]][0]
        cells = []
        for k in range(n):
            pos = cursors[k]
            rs = logs[k]["readings"]
            if pos < len(rs) and rs[pos][0] == ts:
                v = rs[pos][1]
                cells.append((FRESH, v))
                last[k] = v
                cursors[k] = pos + 1
            elif last[k] is not None:
                cells.append((CARRIED, last[k]))
            else:
                cells.append((MISSING, 0))
        rows.append((ts, cells))
    return rows


def gap_stats(rows, k):
    fresh = carried = missing = longest = run = 0
    for _, cells in rows:
        tag = cells[k][0]
        if tag == FRESH:
            fresh += 1
            run = 0
        elif tag == CARRIED:
            carried += 1
            run += 1
            longest = max(longest, run)
        else:
            missing += 1
    return fresh, carried, missing, longest


def print_table(logs, rows):
    header = pad_left("ts", 6)
    for log in logs:
        label, unit = LABELS[log["kind"]]
        header += pad_left(f"{label}({unit})", 12)
    print(header)
    for ts, cells in rows:
        line = pad_left(str(ts), 6)
        for cell in cells:
            line += pad_left(render_cell(cell), 12)
        print(line)


def print_gaps(logs, rows):
    print("gap report (* = carried forward):")
    total = 0
    for k in range(len(logs)):
        fresh, carried, missing, longest = gap_stats(rows, k)
        total += carried
        name = LABELS[logs[k]["kind"]][0]
        print(f"  {name}: samples={fresh} gaps={carried} leading_missing={missing} longest_gap_run={longest}")
    print(f"merged rows: {len(rows)}, total filled cells: {total}")


def run():
    horizon = 120
    specs = [
        dict(kind="temp", path="sensor_temp.csv", period=10, offset=0, base=215, swing=12, drop_every=4),
        dict(kind="humid", path="sensor_humid.csv", period=15, offset=5, base=480, swing=30, drop_every=3),
        dict(kind="press", path="sensor_press.csv", period=20, offset=10, base=10132, swing=8, drop_every=0),
    ]
    for spec in specs:
        try:
            with open(spec["path"], "w") as f:
                f.write(generate_csv(spec, horizon))
        except OSError:
            raise PipelineError("io error on write")
    logs = [load(s["kind"], s["path"]) for s in specs]
    for log in logs:
        print(f"loaded {LABELS[log['kind']][0]}: {len(log['readings'])} readings")
    rows = merge(logs)
    print_table(logs, rows)
    print_gaps(logs, rows)


def main():
    try:
        run()
        print("done")
    except PipelineError as e:
        print(f"pipeline failed: {e}")


if __name__ == "__main__":
    main()
