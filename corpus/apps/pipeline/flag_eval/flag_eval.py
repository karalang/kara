# Python mirror of source.kara: same configs, requests, decisions and output.
import json

def dumps(o):
    return json.dumps(o, separators=(",", ":"))


CONFIG_V1 = {
    "version": 1,
    "flags": [
        {"name": "new-checkout", "rules": [{"op": "Equals", "attr": "country", "value": "NZ"}],
         "rollout_percent": 50, "variant": "blue"},
        {"name": "beta-search", "rules": [{"op": "OneOf", "attr": "plan", "values": ["pro", "team"]},
                                          {"op": "AtLeast", "attr": "age_days", "min": 30}], "variant": None},
        {"name": "dark-mode", "rules": [], "rollout_percent": 25, "variant": "dim"},
        {"name": "legacy-export", "enabled": False, "rules": [], "variant": None},
    ],
}
CONFIG_V2 = {
    "version": 2,
    "flags": [
        {"name": "new-checkout", "rules": [{"op": "OneOf", "attr": "country", "values": ["NZ", "AU"]}],
         "rollout_percent": 100, "variant": "green"},
        {"name": "beta-search", "rules": [{"op": "AtLeast", "attr": "age_days", "min": 7}], "variant": None},
        {"name": "dark-mode", "rules": [], "rollout_percent": 60, "variant": "dim"},
    ],
}

def bucket(flag, user):
    h = 14695981039346656037
    for b in f"{flag}:{user}".encode():
        h ^= b
        h = (h * 1099511628211) % (1 << 64)
    return h % 100

def check_rule(rule, attrs):
    attr = rule["attr"]
    if attr not in attrs:
        return f"no {attr}"
    v = attrs[attr]
    if rule["op"] == "Equals":
        return None if v == rule["value"] else f"{attr}={v} not {rule['value']}"
    if rule["op"] == "OneOf":
        return None if v in rule["values"] else f"{attr}={v} not in list"
    try:
        n = int(v)
    except ValueError:
        return f"{attr} not a number"
    return None if n >= rule["min"] else f"{attr}={n} < {rule['min']}"

def decision(req, on, variant, reason):
    return {"user": req["user"], "flag": req["flag"], "on": on, "variant": variant, "reason": reason}

def evaluate(cfg, req):
    flag = None
    for f in cfg["flags"]:
        if f["name"] == req["flag"]:
            flag = f
    if flag is None:
        return decision(req, False, None, "unknown flag")
    if not flag.get("enabled", True):
        return decision(req, False, None, "disabled")
    for r in flag["rules"]:
        why = check_rule(r, req["attrs"])
        if why is not None:
            return decision(req, False, None, why)
    rollout = flag.get("rollout_percent", 100)
    b = bucket(flag["name"], req["user"])
    if b >= rollout:
        return decision(req, False, None, f"bucket {b} >= {rollout}%")
    return decision(req, True, flag["variant"], f"bucket {b} < {rollout}%")

def valid_config(c):
    try:
        if not isinstance(c["version"], int):
            return False
        for f in c["flags"]:
            if not isinstance(f["name"], str) or not isinstance(f["rules"], list):
                return False
        return True
    except (KeyError, TypeError):
        return False

def load_config(path):
    c = json.loads(open(path).read())
    if not valid_config(c):
        raise ValueError(f"{path}: invalid config")
    return c

def requests_file():
    users = ["ana", "bo", "chen", "dev", "eli", "fay"]
    countries = ["NZ", "AU", "US", "NZ", "DE", "AU"]
    plans = ["free", "pro", "team", "pro", "free", "team"]
    ages = ["3", "45", "12", "30", "x", "8"]
    flags = ["new-checkout", "beta-search", "dark-mode", "legacy-export", "no-such-flag"]
    out = ""
    for i in range(6):
        for f in flags:
            attrs = {"country": countries[i], "plan": plans[i]}
            if i != 2:
                attrs["age_days"] = ages[i]
            attrs = dict(sorted(attrs.items()))
            out += dumps({"user": users[i], "flag": f, "attrs": attrs}) + "\n"
    out += '{"user": "broken"\n'
    return out

def run(cfg, text, out_path):
    ds, out = [], ""
    for line_no, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        try:
            req = json.loads(line)
            req["user"], req["flag"], req["attrs"]
        except (ValueError, KeyError):
            print(f"  line {line_no}: skipped, not a request")
            continue
        d = evaluate(cfg, req)
        out += dumps(d) + "\n"
        ds.append(d)
    open(out_path, "w").write(out)
    return ds

def summarize(label, ds):
    on, reasons = {}, {}
    for d in ds:
        on[d["flag"]] = on.get(d["flag"], 0) + (1 if d["on"] else 0)
        kind = d["reason"].split(" ")[0]
        reasons[kind] = reasons.get(kind, 0) + 1
    print(f"{label}: {len(ds)} decisions")
    for f in sorted(on):
        print(f"  on {f}: {on[f]}")
    for r in sorted(reasons):
        print(f"  reason {r}: {reasons[r]}")

def b(v):
    return "true" if v else "false"

def main():
    open("requests.jsonl", "w").write(requests_file())
    text = open("requests.jsonl").read()
    open("flags.json", "w").write(json.dumps(CONFIG_V1))
    v1 = load_config("flags.json")
    print(f"config v{v1['version']}: {len(v1['flags'])} flags")
    first = run(v1, text, "decisions.v1.jsonl")
    summarize("v1", first)
    open("flags.json", "w").write('{"version": 3, "flags": [{"name": 7}]}')
    try:
        load_config("flags.json")
        print("bad config accepted?")
    except ValueError as e:
        print(f"reload refused: {e}, keeping v{v1['version']}")
    open("flags.json", "w").write(json.dumps(CONFIG_V2))
    v2 = load_config("flags.json")
    print(f"config v{v2['version']}: {len(v2['flags'])} flags")
    second = run(v2, text, "decisions.v2.jsonl")
    summarize("v2", second)
    print("changed decisions:")
    for a, c in zip(first, second):
        if a["on"] != c["on"] or a["variant"] != c["variant"]:
            va = a["variant"] or "-"
            vb = c["variant"] or "-"
            print(f"  {a['user']} {a['flag']}: {b(a['on'])}/{va} -> {b(c['on'])}/{vb} ({c['reason']})")
    lines = open("decisions.v2.jsonl").read().splitlines()
    print(f"decisions.v2.jsonl: {len(lines)} lines, first {lines[0]}")

main()
