"""Mirror of metrics_agg.kara: parse Prometheus text-format scrapes, compute
counter rates with reset detection, aggregate by label, estimate histogram
quantiles the way PromQL's histogram_quantile does, and re-expose the
result. A port of the Kara; the parser was also checked against
prometheus_client on the valid scrapes."""
import math

INTERVAL = 60


def scrape(job, inst, req, lat, mem, version, extra=""):
    get200, get404, post500 = req
    b = lat
    return f"""# HELP http_requests_total Requests handled.
# TYPE http_requests_total counter
http_requests_total{{job="{job}",instance="{inst}",method="GET",status="200"}} {get200}
http_requests_total{{job="{job}",instance="{inst}",method="GET",status="404"}} {get404}
http_requests_total{{job="{job}",instance="{inst}",method="POST",status="500"}} {post500}
# HELP http_request_duration_seconds Request latency.
# TYPE http_request_duration_seconds histogram
http_request_duration_seconds_bucket{{job="{job}",instance="{inst}",le="0.05"}} {b[0]}
http_request_duration_seconds_bucket{{job="{job}",instance="{inst}",le="0.1"}} {b[1]}
http_request_duration_seconds_bucket{{job="{job}",instance="{inst}",le="0.25"}} {b[2]}
http_request_duration_seconds_bucket{{job="{job}",instance="{inst}",le="0.5"}} {b[3]}
http_request_duration_seconds_bucket{{job="{job}",instance="{inst}",le="1"}} {b[4]}
http_request_duration_seconds_bucket{{job="{job}",instance="{inst}",le="+Inf"}} {b[5]}
http_request_duration_seconds_sum{{job="{job}",instance="{inst}"}} {b[6]}
http_request_duration_seconds_count{{job="{job}",instance="{inst}"}} {b[5]}
# HELP process_resident_memory_bytes Resident memory.
# TYPE process_resident_memory_bytes gauge
process_resident_memory_bytes{{job="{job}",instance="{inst}"}} {mem}
# TYPE build_info gauge
build_info{{job="{job}",instance="{inst}",version="{version}",note="say \\"hi\\"\\n\\\\o/"}} 1
{extra}"""


BROKEN = """# TYPE queue_depth gauge
queue_depth{job="worker",instance="10.0.1.7:9100",queue="emails"} 42
queue_depth{job="worker",instance="10.0.1.7:9100",queue="emails"} 43
queue_depth{job="worker",instance="10.0.1.7:9100",queue="sms" 7
queue_depth{job="worker",instance="10.0.1.7:9100",queue="push"}
queue_depth{job="worker",instance="10.0.1.7:9100",queue="fax"} lots
jobs_done_total{job="worker",instance="10.0.1.7:9100"} 5
# TYPE jobs_done_total counter
# TYPE rpc_latency summary
rpc_latency{job="worker",instance="10.0.1.7:9100",quantile="0.5"} 0.2
temperature_celsius{job="worker",instance="10.0.1.7:9100",room="lab"} -3.5e1 1760000000000
weird{job="worker",instance="10.0.1.7:9100",9lives="x"} 1
"""

SCRAPES = {
    ("api-1", 0): scrape("api", "10.0.0.1:9100", (1027, 13, 3), (600, 800, 950, 1000, 1030, 1043, 98.5), "5.24288e7", "1.4.2"),
    ("api-1", 1): scrape("api", "10.0.0.1:9100", (1627, 25, 9), (1000, 1340, 1580, 1650, 1680, 1700, 160.25), "5.4525952e7", "1.4.2"),
    ("api-2", 0): scrape("api", "10.0.0.2:9100", (5000, 40, 10), (3000, 4000, 4700, 4950, 5030, 5050, 520.0), "8.388608e7", "1.4.1"),
    ("api-2", 1): scrape("api", "10.0.0.2:9100", (300, 2, 1), (180, 240, 285, 300, 302, 303, 31.5), "2.62144e7", "1.4.2"),
    ("worker-1", 0): BROKEN,
    ("worker-1", 1): BROKEN.replace("} 42\n", "} 40\n").replace("total{job=\"worker\",instance=\"10.0.1.7:9100\"} 5", "total{job=\"worker\",instance=\"10.0.1.7:9100\"} 65"),
}


class Bad(Exception):
    pass


def parse_labels(s, i):
    """s[i] is '{'; returns (labels, index after '}')."""
    labels = []
    i += 1
    while True:
        while i < len(s) and s[i] == " ":
            i += 1
        if i < len(s) and s[i] == "}":
            return labels, i + 1
        j = i
        while j < len(s) and (s[j].isalnum() or s[j] == "_"):
            j += 1
        name = s[i:j]
        if not name or name[0].isdigit():
            raise Bad(f"bad label name at column {i + 1}")
        if j >= len(s) or s[j] != "=" or j + 1 >= len(s) or s[j + 1] != '"':
            raise Bad(f"expected =\" after label {name}")
        j += 2
        val = ""
        while True:
            if j >= len(s):
                raise Bad(f"unterminated value for label {name}")
            c = s[j]
            if c == "\\":
                if j + 1 >= len(s):
                    raise Bad(f"unterminated value for label {name}")
                n = s[j + 1]
                val += "\n" if n == "n" else n
                j += 2
            elif c == '"':
                j += 1
                break
            else:
                val += c
                j += 1
        labels.append((name, val))
        while j < len(s) and s[j] == " ":
            j += 1
        if j < len(s) and s[j] == ",":
            j += 1
        elif j < len(s) and s[j] == "}":
            pass
        else:
            raise Bad(f"expected , or }} after label {name}")
        i = j


def parse_value(t):
    if t in ("+Inf", "Inf"):
        return math.inf
    if t == "-Inf":
        return -math.inf
    if t == "NaN":
        return math.nan
    try:
        return float(t)
    except ValueError:
        raise Bad(f"bad value '{t}'")


class Scrape:
    def __init__(self):
        self.types = {}
        self.helps = {}
        self.samples = {}  # (name, sorted labels tuple) -> value
        self.order = []
        self.errors = []


def family_of(name, types):
    for suf in ("_bucket", "_sum", "_count"):
        if name.endswith(suf) and types.get(name[:-len(suf)]) == "histogram":
            return name[:-len(suf)]
    return name


def parse(text):
    sc = Scrape()
    seen_family = set()
    for n, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        if line.startswith("#"):
            w = line.split(" ", 3)
            if len(w) >= 4 and w[1] == "TYPE":
                name, ty = w[2], w[3].strip()
                if name in seen_family:
                    sc.errors.append(f"line {n}: TYPE for {name} after its samples")
                    continue
                if ty not in ("counter", "gauge", "histogram", "summary", "untyped"):
                    sc.errors.append(f"line {n}: unknown type {ty}")
                    continue
                if ty == "summary":
                    sc.errors.append(f"line {n}: summary {name} kept as untyped")
                    ty = "untyped"
                sc.types[name] = ty
            elif len(w) >= 4 and w[1] == "HELP":
                sc.helps[w[2]] = w[3]
            continue
        try:
            i = 0
            while i < len(line) and (line[i].isalnum() or line[i] in "_:"):
                i += 1
            name = line[:i]
            if not name:
                raise Bad("missing metric name")
            labels = []
            if i < len(line) and line[i] == "{":
                labels, i = parse_labels(line, i)
            rest = line[i:].split()
            if not rest:
                raise Bad(f"no value for {name}")
            if len(rest) > 2:
                raise Bad(f"trailing text after value of {name}")
            value = parse_value(rest[0])
            if len(rest) == 2:
                try:
                    int(rest[1])
                except ValueError:
                    raise Bad(f"bad timestamp '{rest[1]}'")
            key = (name, tuple(sorted(labels)))
            if key in sc.samples:
                raise Bad(f"duplicate series {show_series(name, key[1])}")
            sc.samples[key] = value
            sc.order.append(key)
            seen_family.add(family_of(name, sc.types))
        except Bad as e:
            sc.errors.append(f"line {n}: {e}")
    return sc


def esc(v):
    return v.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")


def show_series(name, labels):
    if not labels:
        return name
    return name + "{" + ",".join(f'{k}="{esc(v)}"' for k, v in labels) + "}"


def num(v):
    if math.isnan(v):
        return "NaN"
    if math.isinf(v):
        return "+Inf" if v > 0 else "-Inf"
    if v == int(v) and abs(v) < 1e15:
        return str(int(v))
    return repr(v)


def label(labels, k):
    for a, b in labels:
        if a == k:
            return b
    return ""


def without(labels, *names):
    return tuple((a, b) for a, b in labels if a not in names)


def rates(s0, s1, name):
    out = {}
    resets = []
    for key, v1 in s1.samples.items():
        if key[0] != name:
            continue
        v0 = s0.samples.get(key)
        if v0 is None:
            continue
        if v1 < v0:
            resets.append(show_series(name, key[1]))
            delta = v1
        else:
            delta = v1 - v0
        out[key[1]] = delta / INTERVAL
    return out, resets


def histogram_quantile(q, buckets):
    """buckets: list of (upper, cumulative count) sorted by upper."""
    if q < 0:
        return -math.inf
    if q > 1:
        return math.inf
    if not buckets or not math.isinf(buckets[-1][0]):
        return math.nan
    fixed = []
    run = 0.0
    for up, c in buckets:
        run = max(run, c)
        fixed.append((up, run))
    total = fixed[-1][1]
    if total == 0:
        return math.nan
    rank = q * total
    b = 0
    while b < len(fixed) and fixed[b][1] < rank:
        b += 1
    if b == len(fixed) - 1:
        return fixed[-2][0]
    if b == 0 and fixed[0][0] <= 0:
        return fixed[0][0]
    start = 0.0 if b == 0 else fixed[b - 1][0]
    end = fixed[b][0]
    count = fixed[b][1]
    if b > 0:
        count -= fixed[b - 1][1]
        rank -= fixed[b - 1][1]
    return start + (end - start) * (rank / count)


def main():
    for (t, k), text in SCRAPES.items():
        with open(f"{t}.{k}.prom", "w") as f:
            f.write(text)
    targets = ["api-1", "api-2", "worker-1"]
    parsed = {}
    print("== parse")
    for t in targets:
        for k in (0, 1):
            with open(f"{t}.{k}.prom") as f:
                sc = parse(f.read())
            parsed[(t, k)] = sc
            fams = len({family_of(key[0], sc.types) for key in sc.order})
            print(f"  {t}.{k}.prom: {fams} families, {len(sc.order)} samples, {len(sc.errors)} errors")
            if k == 0:
                for e in sc.errors:
                    print(f"    {e}")
    bi = [key for key in parsed[("api-1", 0)].order if key[0] == "build_info"][0]
    print(f"  label escapes: note=[{label(bi[1], 'note')}] -> {show_series('build_info', without(bi[1], 'job', 'instance'))}")
    temp = [(k, v) for k, v in parsed[("worker-1", 0)].samples.items() if k[0] == "temperature_celsius"][0]
    print(f"  temperature_celsius = {num(temp[1])} (with a timestamp)")

    print("== rate(http_requests_total[60s])")
    all_rates = {}
    for t in targets:
        r, resets = rates(parsed[(t, 0)], parsed[(t, 1)], "http_requests_total")
        for s in resets:
            print(f"  counter reset: {s}")
        all_rates.update(r)
    for labels in sorted(all_rates, key=lambda l: show_series("", l)):
        print(f"  {show_series('', without(labels, 'job'))} {all_rates[labels]:.4f}/s")
    by_status = {}
    for labels, v in all_rates.items():
        cls = label(labels, "status")[0] + "xx"
        by_status[cls] = by_status.get(cls, 0) + v
    print("  sum by (class): " + ", ".join(f"{c}={by_status[c]:.3f}" for c in sorted(by_status)))
    total = sum(all_rates.values())
    errors = sum(v for l, v in all_rates.items() if label(l, "status").startswith("5"))
    print(f"  error ratio: {errors / total * 100:.3f}%")

    print("== latency")
    agg = {}
    sums = counts = 0.0
    for t in ("api-1", "api-2"):
        r, resets = rates(parsed[(t, 0)], parsed[(t, 1)], "http_request_duration_seconds_bucket")
        for s in resets:
            print(f"  counter reset: {s}")
        for labels, v in r.items():
            le = parse_value(label(labels, "le"))
            agg[le] = agg.get(le, 0) + v
        rs, _ = rates(parsed[(t, 0)], parsed[(t, 1)], "http_request_duration_seconds_sum")
        rc, _ = rates(parsed[(t, 0)], parsed[(t, 1)], "http_request_duration_seconds_count")
        sums += sum(rs.values())
        counts += sum(rc.values())
    buckets = sorted(agg.items())
    print("  buckets/s: " + " ".join(f"le={num(u)}:{c:.3f}" for u, c in buckets))
    for q in (0.5, 0.9, 0.99, 1.0):
        print(f"  histogram_quantile({q}) = {histogram_quantile(q, buckets):.4f}s")
    print(f"  edge cases: q=-1 {num(histogram_quantile(-1, buckets))}, q=2 {num(histogram_quantile(2, buckets))}, "
          f"no +Inf {num(histogram_quantile(0.5, buckets[:-1]))}")
    print(f"  mean latency: {sums / counts * 1000:.2f} ms")

    print("== memory (latest scrape)")
    mem = []
    for t in targets:
        for key, v in parsed[(t, 1)].samples.items():
            if key[0] == "process_resident_memory_bytes":
                mem.append((label(key[1], "instance"), v))
    for inst, v in sorted(mem):
        print(f"  {inst} {v / 1048576:.1f} MiB")
    vals = [v for _, v in mem]
    print(f"  max {max(vals) / 1048576:.1f} MiB, min {min(vals) / 1048576:.1f} MiB, avg {sum(vals) / len(vals) / 1048576:.2f} MiB")
    versions = {}
    for t in targets:
        for key in parsed[(t, 1)].order:
            if key[0] == "build_info":
                v = label(key[1], "version")
                versions[v] = versions.get(v, 0) + 1
    print("  versions: " + ", ".join(f"{v}x{versions[v]}" for v in sorted(versions)))

    print("== aggregated.prom")
    out = ["# HELP job:http_requests:rate60s Request rate by job and status class.",
           "# TYPE job:http_requests:rate60s gauge"]
    by_job = {}
    for labels, v in all_rates.items():
        k = (label(labels, "job"), label(labels, "status")[0] + "xx")
        by_job[k] = by_job.get(k, 0) + v
    for (job, cls) in sorted(by_job):
        out.append(f'job:http_requests:rate60s{{class="{cls}",job="{job}"}} {num(round(by_job[(job, cls)], 6))}')
    out.append("# TYPE job:latency_seconds:p90 gauge")
    out.append(f'job:latency_seconds:p90{{job="api"}} {num(round(histogram_quantile(0.9, buckets), 6))}')
    out.append("# TYPE build_info gauge")
    out.append(show_series("build_info", without(bi[1], "job", "instance")) + " 1")
    text = "\n".join(out) + "\n"
    with open("aggregated.prom", "w") as f:
        f.write(text)
    for l in text.splitlines():
        print(f"  {l}")
    with open("aggregated.prom") as f:
        back = parse(f.read())
    print(f"  re-parsed: {len(back.order)} samples, {len(back.errors)} errors, note survives: "
          f"{'true' if label([k for k in back.order if k[0] == 'build_info'][0][1], 'note') == label(bi[1], 'note') else 'false'}")


main()
