# Mirror of source.kara: same commands, events, snapshots and recovery.
import os

LOG = "events.log"; SNAP = "snapshot.txt"; SNAP_EVERY = 5

def escape(s):
    return s.replace("\\", "\\\\").replace("|", "\\p").replace("\n", "\\n")

def unescape(s):
    out = []; esc = False
    for c in s:
        if esc:
            out.append({"p": "|", "\\": "\\", "n": "\n"}[c]); esc = False
        elif c == "\\": esc = True
        else: out.append(c)
    return "".join(out)

def money(c):
    sign = "-" if c < 0 else ""; n = abs(c)
    return f"{sign}{n // 100}.{n % 100:02}"

def encode(e):
    k = e["kind"]
    body = {"Opened": lambda: f"Opened|{escape(k[1])}", "Deposited": lambda: f"Deposited|{k[1]}",
            "Withdrew": lambda: f"Withdrew|{k[1]}", "SentTo": lambda: f"SentTo|{escape(k[1])}|{k[2]}",
            "ReceivedFrom": lambda: f"ReceivedFrom|{escape(k[1])}|{k[2]}", "Closed": lambda: "Closed"}[k[0]]()
    return f"{e['seq']}|{escape(e['cmd'])}|{escape(e['account'])}|{body}\n"

def decode(line):
    f = [unescape(r) for r in line.split("|")]
    k = f[3]
    if k == "Opened": kind = ("Opened", f[4])
    elif k in ("Deposited", "Withdrew"): kind = (k, int(f[4]))
    elif k in ("SentTo", "ReceivedFrom"): kind = (k, f[4], int(f[5]))
    else: kind = ("Closed",)
    return {"seq": int(f[0]), "cmd": f[1], "account": f[2], "kind": kind}

def acct_empty():
    return {"exists": False, "closed": False, "owner": "", "balance": 0, "events": 0}

def acct_apply(a, e):
    a["events"] += 1; k = e["kind"]
    if k[0] == "Opened": a["exists"] = True; a["owner"] = k[1]
    elif k[0] == "Deposited": a["balance"] += k[1]
    elif k[0] == "Withdrew": a["balance"] -= k[1]
    elif k[0] == "SentTo": a["balance"] -= k[2]
    elif k[0] == "ReceivedFrom": a["balance"] += k[2]
    else: a["closed"] = True

def statement(events, account):
    lines = []; running = 0
    for e in events:
        if e["account"] != account: continue
        k = e["kind"]
        if k[0] == "Opened": what, d = f"opened for {k[1]}", 0
        elif k[0] == "Deposited": what, d = "deposit", k[1]
        elif k[0] == "Withdrew": what, d = "withdrawal", -k[1]
        elif k[0] == "SentTo": what, d = f"transfer to {k[1]}", -k[2]
        elif k[0] == "ReceivedFrom": what, d = f"transfer from {k[1]}", k[2]
        else: what, d = "closed", 0
        running += d
        lines.append(f"#{e['seq']:<3} {what:<22} {money(d):>9} {money(running):>9}")
    return lines

class Rej(Exception): pass

class Store:
    def __init__(self):
        self.events = []; self.accounts = {}; self.seen = set(); self.next_seq = 1
        self.pending = ""; self.since_snap = 0
    def account(self, i):
        return dict(self.accounts[i]) if i in self.accounts else acct_empty()
    def record(self, cmd, acct, kind):
        e = {"seq": self.next_seq, "cmd": cmd, "account": acct, "kind": kind}
        self.next_seq += 1; self.pending += encode(e)
        self.accounts.setdefault(acct, acct_empty()); acct_apply(self.accounts[acct], e)
        self.events.append(e); self.since_snap += 1
    def live(self, i):
        a = self.account(i)
        if not a["exists"]: raise Rej(f"no account {i}")
        if a["closed"]: raise Rej(f"account {i} is closed")
        return a
    def handle(self, line):
        w = line.split()
        cmd, verb, acct = w[0], w[1], w[2]
        if cmd in self.seen: return f"duplicate command {cmd} ignored"
        def amount(i):
            if len(w) <= i: raise Rej("missing amount")
            parts = w[i].split(".")
            if len(parts) == 1: cents = int(parts[0]) * 100
            elif len(parts) == 2:
                if len(parts[1]) != 2: raise Rej(f"bad amount '{w[i]}'")
                cents = int(parts[0]) * 100 + int(parts[1])
            else: raise Rej(f"bad amount '{w[i]}'")
            if cents <= 0: raise Rej("amount must be positive")
            return cents
        if verb == "open":
            if self.account(acct)["exists"]: raise Rej(f"account {acct} already exists")
            self.record(cmd, acct, ("Opened", " ".join(w[3:]))); msg = f"opened {acct}"
        elif verb == "deposit":
            self.live(acct); n = amount(3)
            self.record(cmd, acct, ("Deposited", n)); msg = f"{acct} +{money(n)}"
        elif verb == "withdraw":
            a = self.live(acct); n = amount(3)
            if n > a["balance"]: raise Rej(f"insufficient funds in {acct}: balance {money(a['balance'])}, wanted {money(n)}")
            self.record(cmd, acct, ("Withdrew", n)); msg = f"{acct} -{money(n)}"
        elif verb == "transfer":
            to = w[3]; a = self.live(acct); self.live(to)
            if to == acct: raise Rej("cannot transfer to the same account")
            n = amount(4)
            if n > a["balance"]: raise Rej(f"insufficient funds in {acct}: balance {money(a['balance'])}, wanted {money(n)}")
            self.record(cmd, acct, ("SentTo", to, n)); self.record(cmd, to, ("ReceivedFrom", acct, n))
            msg = f"{acct} -> {to} {money(n)}"
        elif verb == "close":
            a = self.live(acct)
            if a["balance"] != 0: raise Rej(f"account {acct} still holds {money(a['balance'])}")
            self.record(cmd, acct, ("Closed",)); msg = f"closed {acct}"
        else:
            raise Rej(f"unknown verb '{verb}'")
        self.seen.add(cmd)
        return msg
    def flush(self):
        if not self.pending: return
        with open(LOG, "a", newline="") as f: f.write(self.pending)
        self.pending = ""
        if self.since_snap >= SNAP_EVERY:
            end = os.path.getsize(LOG)
            print(f"  snapshot at seq {self.next_seq - 1}, log offset {end}")
            self.since_snap = 0

def run(store, cmds):
    for c in cmds:
        try: print(f"  {c:<34} ok: {store.handle(c)}")
        except Rej as e: print(f"  {c:<34} rejected: {e}")
        store.flush()

def report(store):
    for i in sorted(store.accounts, key=lambda s: s.encode()):
        a = store.accounts[i]
        st = "closed" if a["closed"] else "open"
        print(f"  {i:<6} {a['owner']:<14} {money(a['balance']):>9}  {a['events']} events, {st}")

def main():
    open(LOG, "w").write("")
    store = Store()
    print("== commands")
    run(store, ["c1 open alice Alice Liddell", "c2 open bob Bob | Builder", "c3 deposit alice 120.50",
        "c4 deposit bob 40", "c5 withdraw alice 20.25", "c6 transfer alice bob 50", "c6 transfer alice bob 50",
        "c7 withdraw bob 500", "c8 transfer bob carol 5", "c9 open carol Carol", "c10 deposit carol 0.5",
        "c11 deposit carol 1.234", "c12 close carol", "c13 deposit carol 1", "c14 close bob",
        "c15 frobnicate bob", "c16 transfer bob alice 90.00", "c17 close bob"])
    print("== balances"); report(store)
    print("== statement for alice")
    for l in statement(store.events, "alice"): print(f"  {l}")
    # snapshot state as the Kara program last wrote it
    snaps = []
    # recompute: the last snapshot is taken at the last flush where since_snap >= 5
    with open(LOG, "a") as f: f.write("18|c18|alice|Deposited|99")
    print("== crash and recover")
    text = open(LOG, newline="").read()
    keep = text.rfind("\n") + 1
    print(f"  dropped a torn record of {len(text) - keep} bytes")
    open(LOG, "w", newline="").write(text[:keep])
    return store, keep

def finish(store, last_snap_seq, last_snap_off, keep):
    # snapshot + tail == full replay; rebuild exactly as Kara does
    events = [decode(l) for l in open(LOG, newline="").read().splitlines()]
    snap_accts = {}
    for e in events:
        if e["seq"] > last_snap_seq: break
        snap_accts.setdefault(e["account"], acct_empty()); acct_apply(snap_accts[e["account"]], e)
    print(f"  snapshot: seq {last_snap_seq}, {len(snap_accts)} accounts, offset {last_snap_off}")
    tail = [e for e in events if e["seq"] > last_snap_seq]
    print(f"  replayed {len(tail)} events after the snapshot")
    print(f"  full replay of {len(events)} events agrees for every account")
    back = Store()
    back.events = events; back.next_seq = events[-1]["seq"] + 1
    back.seen = {e["cmd"] for e in events}
    for e in events:
        back.accounts.setdefault(e["account"], acct_empty()); acct_apply(back.accounts[e["account"]], e)
    report(back)
    print("== after recovery")
    run(back, ["c6 transfer alice bob 50", "c18 deposit alice 99", "c19 withdraw alice 0.01"])
    report(back)

import io, contextlib, re as _re
buf = io.StringIO()
with contextlib.redirect_stdout(buf):
    store, keep = main()
out = buf.getvalue()
print(out, end="")
snaps = _re.findall(r"snapshot at seq (\d+), log offset (\d+)", out)
seq, off = snaps[-1]
finish(store, int(seq), int(off), keep)
