# The mirror of mini_iptables: the same rule sets, commands and packets
# in Python. expected.out is its output.
import os, shutil
import mk

VERSION = "v1.8.10 (legacy)"
DATE = "Sat Oct 18 02:00:00 2026"
BUILTIN = {"filter": ["INPUT", "FORWARD", "OUTPUT"],
           "nat": ["PREROUTING", "INPUT", "OUTPUT", "POSTROUTING"]}
TABLE_ORDER = ["nat", "filter"]
TARGETS = ["ACCEPT", "DROP", "REJECT", "LOG", "RETURN", "DNAT", "SNAT", "MASQUERADE"]
STATES = ["INVALID", "NEW", "RELATED", "ESTABLISHED"]
LOCAL = ["127.0.0.1", "203.0.113.10", "10.0.0.1"]
IFADDR = {"lo": "127.0.0.1", "eth0": "203.0.113.10", "eth1": "10.0.0.1"}
ICMP_NAMES = {"echo-request": 8, "echo-reply": 0}


class Fail(Exception):
    def __init__(self, msg, code):
        self.msg = msg
        self.code = code


def ip_num(s):
    parts = s.split(".")
    if len(parts) != 4:
        return None
    n = 0
    for p in parts:
        if not p.isdigit() or int(p) > 255:
            return None
        n = n * 256 + int(p)
    return n


def ip_str(n):
    return "%d.%d.%d.%d" % (n >> 24, (n >> 16) & 255, (n >> 8) & 255, n & 255)


def parse_net(s):
    if "/" in s:
        a, b = s.split("/", 1)
        plen = int(b)
    else:
        a, plen = s, 32
    n = ip_num(a)
    if n is None:
        raise Fail("host/network `%s' not found" % a, 2)
    mask = 0 if plen == 0 else ((1 << 32) - 1) ^ ((1 << (32 - plen)) - 1)
    return (n & mask, plen)


def in_net(ip, net):
    n, plen = net
    mask = 0 if plen == 0 else ((1 << 32) - 1) ^ ((1 << (32 - plen)) - 1)
    return (ip & mask) == n


def net_str(net):
    return "%s/%d" % (ip_str(net[0]), net[1])


def parse_range(s):
    if ":" in s:
        a, b = s.split(":", 1)
        return (int(a), int(b))
    return (int(s), int(s))


def range_str(r):
    return str(r[0]) if r[0] == r[1] else "%d:%d" % r


def tokenize(line):
    out = []
    i = 0
    while i < len(line):
        c = line[i]
        if c == " ":
            i += 1
        elif c == '"':
            j = line.index('"', i + 1)
            out.append(line[i + 1:j])
            i = j + 1
        else:
            j = i
            while j < len(line) and line[j] != " ":
                j += 1
            out.append(line[i:j])
            i = j
    return out


class Rule:
    def __init__(self):
        self.src = None
        self.src_neg = False
        self.dst = None
        self.dst_neg = False
        self.in_if = ""
        self.out_if = ""
        self.proto = ""
        self.matches = []  # dicts, in the order they were loaded
        self.target = ""
        self.goto = False
        self.reject_with = ""
        self.log_prefix = ""
        self.to_addr = ""
        self.to_port = 0
        self.pkts = 0
        self.bytes = 0

    def match(self, kind):
        for m in self.matches:
            if m["kind"] == kind:
                return m
        return None


def new_match(r, kind):
    m = {"kind": kind, "sport": None, "dport": None, "syn": False, "icmp": -1,
         "states": [], "ports": [], "rate": "", "interval": 0, "burst": 5,
         "credit": 0, "prev": 0, "comment": ""}
    r.matches.append(m)
    return m


def proto_match(r):
    """The tcp/udp/icmp match a port option belongs to, loaded on first use."""
    if r.proto not in ("tcp", "udp", "icmp"):
        raise Fail("unknown option", 2)
    m = r.match(r.proto)
    return m if m else new_match(r, r.proto)


def parse_rule(tokens, chains):
    """tokens after `-A CHAIN`; chains is the set of chain names in the table."""
    r = Rule()
    i = 0
    neg = False
    cur = None
    while i < len(tokens):
        t = tokens[i]
        a = tokens[i + 1] if i + 1 < len(tokens) else ""
        if t == "!":
            neg = True
            i += 1
            continue
        if t in ("-s", "--source"):
            r.src, r.src_neg = parse_net(a), neg
        elif t in ("-d", "--destination"):
            r.dst, r.dst_neg = parse_net(a), neg
        elif t in ("-i", "--in-interface"):
            r.in_if = a
        elif t in ("-o", "--out-interface"):
            r.out_if = a
        elif t in ("-p", "--protocol"):
            r.proto = a
        elif t in ("-m", "--match"):
            if a in ("tcp", "udp", "icmp"):
                cur = r.match(a) or new_match(r, a)
            elif a in ("conntrack", "state", "multiport", "limit", "comment"):
                cur = new_match(r, a)
            else:
                raise Fail("Couldn't load match `%s':No such file or directory" % a, 2)
        elif t in ("--dport", "--destination-port", "--sport", "--source-port") and r.proto in ("tcp", "udp"):
            m = proto_match(r)
            m["dport" if "d" in t[2] else "sport"] = parse_range(a)
        elif t == "--syn" and r.proto == "tcp":
            proto_match(r)["syn"] = True
            i += 1
            neg = False
            continue
        elif t == "--icmp-type" and r.proto == "icmp":
            proto_match(r)["icmp"] = ICMP_NAMES[a] if a in ICMP_NAMES else int(a)
        elif t in ("--ctstate", "--state") and cur and cur["kind"] in ("conntrack", "state"):
            cur["states"] = [s for s in STATES if s in a.split(",")]
        elif t == "--dports" and cur and cur["kind"] == "multiport":
            cur["ports"] = [parse_range(p) for p in a.split(",")]
        elif t == "--limit" and cur and cur["kind"] == "limit":
            n, unit = a.split("/", 1)
            cur["rate"] = a
            cur["interval"] = {"sec": 1000, "min": 60000, "hour": 3600000}[unit] // int(n)
        elif t == "--limit-burst" and cur and cur["kind"] == "limit":
            cur["burst"] = int(a)
        elif t == "--comment" and cur and cur["kind"] == "comment":
            cur["comment"] = a
        elif t in ("-j", "--jump", "-g", "--goto"):
            if a not in TARGETS and a not in chains:
                raise Fail("Couldn't load target `%s':No such file or directory" % a, 2)
            r.target, r.goto = a, t in ("-g", "--goto")
        elif t == "--reject-with" and r.target == "REJECT":
            r.reject_with = a
        elif t == "--log-prefix" and r.target == "LOG":
            r.log_prefix = a
        elif t in ("--to-destination", "--to-source") and r.target in ("DNAT", "SNAT"):
            if ":" in a:
                r.to_addr, p = a.split(":", 1)
                r.to_port = int(p)
            else:
                r.to_addr = a
        else:
            raise Fail('unknown option "%s"' % t, 2)
        neg = False
        i += 2
    if r.target == "REJECT" and not r.reject_with:
        r.reject_with = "icmp-port-unreachable"
    for m in r.matches:
        if m["kind"] == "limit":
            m["credit"] = m["burst"] * m["interval"]
    return r


def spec(r):
    """The rule as iptables -S prints it, after the chain name."""
    out = []
    if r.src:
        out += (["!"] if r.src_neg else []) + ["-s", net_str(r.src)]
    if r.dst:
        out += (["!"] if r.dst_neg else []) + ["-d", net_str(r.dst)]
    if r.in_if:
        out += ["-i", r.in_if]
    if r.out_if:
        out += ["-o", r.out_if]
    if r.proto:
        out += ["-p", r.proto]
    for m in r.matches:
        k = m["kind"]
        out += ["-m", k]
        if k in ("tcp", "udp"):
            if m["sport"]:
                out += ["--sport", range_str(m["sport"])]
            if m["dport"]:
                out += ["--dport", range_str(m["dport"])]
            if m["syn"]:
                out += ["--tcp-flags", "FIN,SYN,RST,ACK", "SYN"]
        elif k == "icmp":
            out += ["--icmp-type", str(m["icmp"])]
        elif k == "conntrack":
            out += ["--ctstate", ",".join(m["states"])]
        elif k == "state":
            out += ["--state", ",".join(m["states"])]
        elif k == "multiport":
            out += ["--dports", ",".join(range_str(p) for p in m["ports"])]
        elif k == "limit":
            out += ["--limit", m["rate"]]
            if m["burst"] != 5:
                out += ["--limit-burst", str(m["burst"])]
        elif k == "comment":
            out += ["--comment", '"%s"' % m["comment"]]
    if r.target:
        out += ["-g" if r.goto else "-j", r.target]
    if r.target == "REJECT":
        out += ["--reject-with", r.reject_with]
    if r.target == "LOG" and r.log_prefix:
        out += ["--log-prefix", '"%s"' % r.log_prefix]
    if r.target in ("DNAT", "SNAT"):
        to = r.to_addr + (":%d" % r.to_port if r.to_port else "")
        out += ["--to-destination" if r.target == "DNAT" else "--to-source", to]
    return " ".join(out)


def extras(r):
    """The trailing column of iptables -L."""
    out = []
    for m in r.matches:
        k = m["kind"]
        if k in ("tcp", "udp"):
            s = [k]
            for key, name in (("sport", "spt"), ("dport", "dpt")):
                p = m[key]
                if p and p[0] == p[1]:
                    s.append("%s:%d" % (name, p[0]))
                elif p:
                    s.append("%ss:%d:%d" % (name, p[0], p[1]))
            if m["syn"]:
                s.append("flags:0x17/0x02")
            out.append(" ".join(s))
        elif k == "icmp":
            out.append("icmptype %d" % m["icmp"])
        elif k == "conntrack":
            out.append("ctstate " + ",".join(m["states"]))
        elif k == "state":
            out.append("state " + ",".join(m["states"]))
        elif k == "multiport":
            out.append("multiport dports " + ",".join(range_str(p) for p in m["ports"]))
        elif k == "limit":
            out.append("limit: avg %s burst %d" % (m["rate"], m["burst"]))
        elif k == "comment":
            out.append("/* %s */" % m["comment"])
    if r.target == "REJECT":
        out.append("reject-with " + r.reject_with)
    elif r.target == "LOG":
        out.append('LOG flags 0 level 4 prefix "%s"' % r.log_prefix)
    elif r.target in ("DNAT", "SNAT"):
        out.append("to:" + r.to_addr + (":%d" % r.to_port if r.to_port else ""))
    if r.goto:
        out.append("[goto]")
    return " ".join(out)


class Chain:
    def __init__(self, name, policy):
        self.name = name
        self.policy = policy  # "" for a user chain
        self.pkts = 0
        self.bytes = 0
        self.rules = []


def new_table(name):
    return {"order": list(BUILTIN[name]), "chains": {c: Chain(c, "ACCEPT") for c in BUILTIN[name]}}


def references(table, name):
    return sum(1 for c in table["order"] for r in table["chains"][c].rules if r.target == name)


# ------------------------------------------------------------ restore

def restore(tables, path):
    text = open(path).read()
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    staged = {}
    cur = None
    for n, raw in enumerate(lines, 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("*"):
            cur = line[1:]
            staged[cur] = {"order": [], "chains": {}}
            continue
        if line == "COMMIT":
            cur = None
            continue
        t = staged[cur]
        if line.startswith(":"):
            name, policy, counts = line[1:].split(" ")
            c = Chain(name, "" if policy == "-" else policy)
            p, b = counts[1:-1].split(":")
            c.pkts, c.bytes = int(p), int(b)
            t["order"].append(name)
            t["chains"][name] = c
            continue
        tokens = tokenize(line)
        try:
            r = parse_rule(tokens[2:], t["chains"])
        except Fail as e:
            print("iptables-restore %s: %s" % (VERSION, e.msg))
            print("Error occurred at line: %d" % n)
            print("Try `iptables-restore -h' or 'iptables-restore --help' for more information.")
            return e.code
        t["chains"][tokens[1]].rules.append(r)
    if cur is not None:
        print("iptables-restore: COMMIT expected at line %d" % (len(lines) + 1))
        return 1
    for k, v in staged.items():
        tables[k] = v
    return 0


# ------------------------------------------------------------ the iptables command

def cmd_iptables(tables, args):
    tname = "filter"
    if args[:1] == ["-t"]:
        tname, args = args[1], args[2:]
    t = tables[tname]
    op = args[0]
    rest = args[1:]
    chains = t["chains"]
    if op in ("-S", "--list-rules"):
        names = rest[:1] or t["order"]
        for c in names:
            ch = chains[c]
            print("-P %s %s" % (c, ch.policy) if ch.policy else "-N " + c)
        for c in names:
            for r in chains[c].rules:
                print("-A %s %s" % (c, spec(r)))
        return 0
    if op in ("-L", "--list"):
        names = [rest[0]] if rest and not rest[0].startswith("-") else t["order"]
        numbers = "--line-numbers" in rest
        for i, c in enumerate(names):
            if i:
                print()
            list_chain(t, chains[c], numbers)
        return 0
    if op in ("-A", "-I", "-D"):
        if rest[0] not in chains:
            raise Fail("No chain/target/match by that name.", 1)
        ch = chains[rest[0]]
        if op == "-D" and len(rest) == 2 and rest[1].isdigit():
            n = int(rest[1])
            if n > len(ch.rules):
                raise Fail("Index of deletion too big.", 1)
            del ch.rules[n - 1]
            return 0
        pos = len(ch.rules)
        tokens = rest[1:]
        if op == "-I":
            pos = 0
            if tokens and tokens[0].isdigit():
                pos = int(tokens[0]) - 1
                tokens = tokens[1:]
        r = parse_rule(tokens, chains)
        if op == "-D":
            for k, old in enumerate(ch.rules):
                if spec(old) == spec(r):
                    del ch.rules[k]
                    return 0
            raise Fail("Bad rule (does a matching rule exist in that chain?).", 1)
        ch.rules.insert(pos, r)
        return 0
    name = rest[0] if rest else ""
    if op == "-N":
        if name in chains:
            raise Fail("Chain already exists.", 1)
        chains[name] = Chain(name, "")
        t["order"].append(name)
    elif op == "-X":
        if name not in chains:
            raise Fail("No chain/target/match by that name.", 1)
        if chains[name].policy or references(t, name):
            raise Fail("Too many links.", 1)
        del chains[name]
        t["order"].remove(name)
    elif op == "-F":
        for c in ([name] if name else t["order"]):
            chains[c].rules = []
    elif op == "-Z":
        for c in ([name] if name else t["order"]):
            chains[c].pkts = chains[c].bytes = 0
            for r in chains[c].rules:
                r.pkts = r.bytes = 0
    elif op == "-P":
        if name not in chains or not chains[name].policy:
            raise Fail("Bad built-in chain name.", 1)
        chains[name].policy = rest[1]
    return 0


def list_chain(t, ch, numbers):
    if ch.policy:
        print("Chain %s (policy %s %d packets, %d bytes)" % (ch.name, ch.policy, ch.pkts, ch.bytes))
    else:
        print("Chain %s (%d references)" % (ch.name, references(t, ch.name)))
    cols = ["pkts", "bytes", "target", "prot", "opt", "in", "out", "source", "destination"]
    print(row(numbers, "num", cols))
    for k, r in enumerate(ch.rules, 1):
        src = ("!" if r.src_neg else "") + net_str(r.src) if r.src else "0.0.0.0/0"
        dst = ("!" if r.dst_neg else "") + net_str(r.dst) if r.dst else "0.0.0.0/0"
        cells = [str(r.pkts), str(r.bytes), r.target, r.proto or "all", "--",
                 r.in_if or "*", r.out_if or "*", src, dst]
        print((row(numbers, str(k), cells) + " " + extras(r)).rstrip())


def row(numbers, num, c):
    s = "%-4s " % num if numbers else ""
    return (s + "%5s %5s %-10s %-4s %-3s %-6s %-6s %-20s %-20s" % tuple(c)).rstrip()


# ------------------------------------------------------------ packets

class Packet:
    pass


def parse_packet(tokens):
    p = Packet()
    p.time = int(tokens[0][1:])
    p.iface = tokens[1]
    p.proto = tokens[2]
    p.src, p.sport = split_addr(tokens[3])
    p.dst, p.dport = split_addr(tokens[5])
    p.flags = []
    p.icmp = -1
    p.length = 0
    for t in tokens[6:]:
        k, v = t.split("=", 1)
        if k == "flags":
            p.flags = v.split(",")
        elif k == "type":
            p.icmp = int(v)
        elif k == "len":
            p.length = int(v)
    return p


def split_addr(s):
    if ":" in s:
        a, b = s.split(":", 1)
        return ip_num(a), int(b)
    return ip_num(s), 0


def addr(ip, port):
    return ip_str(ip) + (":%d" % port if port else "")


def route(ip):
    if in_net(ip, (ip_num("127.0.0.0"), 8)):
        return "lo"
    if in_net(ip, (ip_num("10.0.0.0"), 24)):
        return "eth1"
    return "eth0"


def if_match(want, have):
    if want.endswith("+"):
        return have.startswith(want[:-1])
    return want == have


def matches(r, p, ctx):
    if r.src and in_net(p.src, r.src) == r.src_neg:
        return False
    if r.dst and in_net(p.dst, r.dst) == r.dst_neg:
        return False
    if r.in_if and not if_match(r.in_if, ctx["in"]):
        return False
    if r.out_if and not if_match(r.out_if, ctx["out"]):
        return False
    if r.proto and r.proto != "all" and r.proto != p.proto:
        return False
    for m in r.matches:
        k = m["kind"]
        if k in ("tcp", "udp"):
            if m["sport"] and not m["sport"][0] <= p.sport <= m["sport"][1]:
                return False
            if m["dport"] and not m["dport"][0] <= p.dport <= m["dport"][1]:
                return False
            if m["syn"] and not ("SYN" in p.flags and not any(f in p.flags for f in ("ACK", "RST", "FIN"))):
                return False
        elif k == "icmp":
            if m["icmp"] != p.icmp:
                return False
        elif k in ("conntrack", "state"):
            if ctx["state"] not in m["states"]:
                return False
        elif k == "multiport":
            if not any(a <= p.dport <= b for a, b in m["ports"]):
                return False
        elif k == "limit":
            cap = m["burst"] * m["interval"]
            m["credit"] = min(cap, m["credit"] + p.time - m["prev"])
            m["prev"] = p.time
            if m["credit"] < m["interval"]:
                return False
            m["credit"] -= m["interval"]
    return True


def log_line(r, p, ctx):
    s = "%sIN=%s OUT=%s SRC=%s DST=%s LEN=%d PROTO=%s" % (
        r.log_prefix, ctx["in"], ctx["out"], ip_str(p.src), ip_str(p.dst), p.length, p.proto.upper())
    if p.proto in ("tcp", "udp"):
        s += " SPT=%d DPT=%d" % (p.sport, p.dport)
    if p.proto == "tcp":
        s += " " + " ".join(p.flags)
    if p.proto == "icmp":
        s += " TYPE=%d" % p.icmp
    return s


def traverse(tname, t, chain, p, ctx):
    """Runs p through a chain; returns (verdict, reject-with)."""
    stack = []
    cur, i = chain, 0
    while True:
        ch = t["chains"][cur]
        if i >= len(ch.rules):
            if ch.policy:
                ch.pkts += 1
                ch.bytes += p.length
                if tname == "filter":
                    print("  filter %s policy %s" % (cur, ch.policy))
                return ch.policy, ""
            cur, i = stack.pop()
            continue
        r = ch.rules[i]
        i += 1
        if not matches(r, p, ctx):
            continue
        r.pkts += 1
        r.bytes += p.length
        where = "  %s %s #%d" % (tname, cur, i)
        tg = r.target
        if tg == "":
            continue
        if tg in ("ACCEPT", "DROP"):
            print("%s %s" % (where, tg))
            return tg, ""
        if tg == "REJECT":
            print("%s REJECT (%s)" % (where, r.reject_with))
            return "REJECT", r.reject_with
        if tg == "LOG":
            print("%s LOG" % where)
            print("  kernel: " + log_line(r, p, ctx))
            continue
        if tg == "RETURN":
            print("%s RETURN" % where)
            if stack:
                cur, i = stack.pop()
            else:
                i = len(ch.rules)
            continue
        if tg == "DNAT":
            p.dst = ip_num(r.to_addr)
            if r.to_port:
                p.dport = r.to_port
            print("%s DNAT to %s" % (where, addr(p.dst, p.dport)))
            return "ACCEPT", ""
        if tg in ("SNAT", "MASQUERADE"):
            p.src = ip_num(r.to_addr if tg == "SNAT" else IFADDR[ctx["out"]])
            print("%s %s to %s" % (where, tg, ip_str(p.src)))
            return "ACCEPT", ""
        print("%s -> %s" % (where, tg))
        if not r.goto:
            stack.append((cur, i))
        cur, i = tg, 0


class Conn:
    def __init__(self, orig, reply):
        self.orig = orig
        self.reply = reply
        self.replied = False


def tuple_of(p):
    return (p.proto, p.src, p.sport, p.dst, p.dport)


def packet(tables, conns, line):
    p = parse_packet(line.split()[1:])
    tup = tuple_of(p)
    conn, direction = None, ""
    for c in conns:
        if c.orig == tup:
            conn, direction = c, "orig"
        elif c.reply == tup:
            conn, direction = c, "reply"
    if conn:
        state = "ESTABLISHED" if direction == "reply" or conn.replied else "NEW"
        print("  conntrack: %s, %s direction" % (state, "reply" if direction == "reply" else "original"))
        target = conn.reply if direction == "orig" else conn.orig
        _, p.dst, p.dport, p.src, p.sport = target
        if tuple_of(p) != tup:
            print("  conntrack: rewritten to %s > %s" % (addr(p.src, p.sport), addr(p.dst, p.dport)))
    else:
        tcp_bad = p.proto == "tcp" and ("SYN" not in p.flags or "ACK" in p.flags)
        state = "INVALID" if tcp_bad or (p.proto == "icmp" and p.icmp == 0) else "NEW"
        print("  conntrack: " + state)
    fresh = conn is None and state == "NEW"
    local = p.iface == "local"
    ctx = {"in": "" if local else p.iface, "out": "", "state": state}
    if fresh:
        traverse("nat", tables["nat"], "OUTPUT" if local else "PREROUTING", p, ctx)
    if local:
        ctx["out"] = route(p.dst)
        chain = "OUTPUT"
        print("  route: out %s" % ctx["out"])
    elif ip_str(p.dst) in LOCAL:
        chain = "INPUT"
        print("  route: local delivery")
    else:
        ctx["out"] = route(p.dst)
        chain = "FORWARD"
        print("  route: forward via %s" % ctx["out"])
    verdict, why = traverse("filter", tables["filter"], chain, p, ctx)
    if verdict == "ACCEPT" and fresh:
        traverse("nat", tables["nat"], "INPUT" if chain == "INPUT" else "POSTROUTING", p, ctx)
    if verdict == "ACCEPT":
        if chain == "INPUT":
            print("  => ACCEPT, delivered as %s > %s" % (addr(p.src, p.sport), addr(p.dst, p.dport)))
        else:
            print("  => ACCEPT, sent out %s as %s > %s" % (ctx["out"], addr(p.src, p.sport), addr(p.dst, p.dport)))
        if fresh:
            conns.append(Conn(tup, (p.proto, p.dst, p.dport, p.src, p.sport)))
            print("  conntrack: new entry")
        elif conn and direction == "reply":
            conn.replied = True
        if conn and "RST" in p.flags:
            conns.remove(conn)
            print("  conntrack: entry destroyed")
    elif verdict == "REJECT":
        print("  => REJECT, answered with %s" % why)
    else:
        print("  => DROP")


def conntrack_list(conns):
    nums = {"tcp": 6, "udp": 17, "icmp": 1}
    for c in conns:
        proto = c.orig[0]
        s = "%-8s %d " % (proto, nums[proto])
        if proto == "tcp":
            s += ("ESTABLISHED " if c.replied else "SYN_SENT ")
        s += side(c.orig, 8)
        if not c.replied:
            s += " [UNREPLIED]"
        s += " " + side(c.reply, 0)
        if c.replied:
            s += " [ASSURED]"
        print(s)
    print("conntrack v1.4.8 (conntrack-tools): %d flow entries have been shown." % len(conns))


def side(t, icmp_type):
    proto, src, sport, dst, dport = t
    s = "src=%s dst=%s" % (ip_str(src), ip_str(dst))
    if proto == "icmp":
        return s + " type=%d code=0 id=0" % icmp_type
    return s + " sport=%d dport=%d" % (sport, dport)


def save(tables):
    print("# Generated by iptables-save %s on %s" % ("v1.8.10", DATE))
    for tname in TABLE_ORDER:
        t = tables[tname]
        print("*" + tname)
        for c in t["order"]:
            ch = t["chains"][c]
            print(":%s %s [%d:%d]" % (c, ch.policy or "-", ch.pkts, ch.bytes))
        for c in t["order"]:
            for r in t["chains"][c].rules:
                print("[%d:%d] -A %s %s" % (r.pkts, r.bytes, c, spec(r)))
        print("COMMIT")
        print("# Completed on " + DATE)


def main():
    shutil.rmtree("data", ignore_errors=True)
    mk.write_all()
    tables = {n: new_table(n) for n in TABLE_ORDER}
    conns = []
    for line in open("data/jobs.txt").read().split("\n"):
        if not line or line.startswith("#"):
            continue
        print("$ " + line)
        words = tokenize(line)
        code = 0
        if words[0] == "iptables-restore":
            code = restore(tables, "data/" + words[1])
        elif words[0] == "iptables":
            try:
                code = cmd_iptables(tables, words[1:])
            except Fail as e:
                if e.code == 2:
                    print("iptables %s: %s" % (VERSION, e.msg))
                    print()
                    print("Try `iptables -h' or 'iptables --help' for more information.")
                else:
                    print("iptables: " + e.msg)
                code = e.code
        elif words[0] == "iptables-save":
            save(tables)
        elif words[0] == "conntrack":
            conntrack_list(conns)
        elif words[0] == "packet":
            packet(tables, conns, line)
        if code:
            print("(exit status %d)" % code)


main()
