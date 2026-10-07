"""Python mirror of source.orig.kara: same limiter algorithm, same log, same output."""
from collections import deque

CLIENT_CAPACITY = 6
CLIENT_REFILL_MILLI = 400
BURST_WINDOW = 3
BURST_THRESHOLD = 4
PENALTY_STRIKES = 3
PENALTY_TICKS = 8
LOG_PATH = "requests.log"

# Policies: ("open",), ("bucket", capacity, refill_milli), ("window", window, limit)
# Decisions: ("allow", tokens_left) or ("route-bucket",), ("route-window",), ("client-quota",), ("penalty",)


class ParseError(Exception):
    pass


class LimitError(Exception):
    pass


class Request:
    def __init__(self, number, time, client, route, cost):
        self.number, self.time, self.client, self.route, self.cost = number, time, client, route, cost


class TokenBucket:
    def __init__(self, capacity, refill_milli):
        self.capacity_milli = capacity * 1000
        self.refill_milli = refill_milli
        self.tokens_milli = capacity * 1000
        self.last_tick = 0

    def refill(self, now):
        if now > self.last_tick:
            gained = (now - self.last_tick) * self.refill_milli
            self.tokens_milli += gained
            if self.tokens_milli > self.capacity_milli:
                self.tokens_milli = self.capacity_milli
            self.last_tick = now

    def can_take(self, now, cost):
        self.refill(now)
        return self.tokens_milli >= cost * 1000

    def take(self, cost):
        self.tokens_milli -= cost * 1000


class SlidingWindow:
    def __init__(self, window, limit):
        self.window, self.limit, self.stamps = window, limit, deque()

    def evict(self, now):
        while self.stamps:
            ts = self.stamps.popleft()
            if ts > now - self.window:
                self.stamps.appendleft(ts)
                break

    def has_room(self, now):
        self.evict(now)
        return len(self.stamps) < self.limit

    def record(self, now):
        self.evict(now)
        self.stamps.append(now)
        return len(self.stamps)


class ClientStats:
    def __init__(self):
        self.total = self.allowed = self.route_throttled = self.quota_throttled = self.penalized = 0
        self.bursts = self.peak = self.strikes = 0
        self.blocked_until = -1
        self.recent = SlidingWindow(BURST_WINDOW, 1000000)


class RouteStats:
    def __init__(self):
        self.allowed = self.throttled = self.units = 0


def admit(quota, req):
    if quota.can_take(req.time, req.cost):
        quota.take(req.cost)
        return ("allow", quota.tokens_milli)
    return ("client-quota",)


class Limiter:
    def __init__(self):
        self.routes = {
            "/login": ("window", 20, 3),
            "/search": ("bucket", 4, 500),
            "/upload": ("window", 30, 2),
            "/health": ("open",),
        }
        self.client_buckets = {}
        self.route_buckets = {}
        self.route_windows = {}
        self.clients = {}
        self.route_stats = {}

    def handle(self, req):
        if req.route not in self.routes:
            raise LimitError(f"unknown route {req.route}")
        policy = self.routes[req.route]
        decision = self.decide(req, policy)
        self.record(req, decision)
        return decision

    def decide(self, req, policy):
        stats = self.clients.get(req.client)
        blocked_until = stats.blocked_until if stats is not None else -1
        if req.time < blocked_until:
            return ("penalty",)
        key = f"{req.client} {req.route}"
        quota = self.client_buckets.pop(req.client, None) or TokenBucket(CLIENT_CAPACITY, CLIENT_REFILL_MILLI)
        if policy[0] == "open":
            decision = admit(quota, req)
        elif policy[0] == "bucket":
            bucket = self.route_buckets.pop(key, None) or TokenBucket(policy[1], policy[2])
            d = admit(quota, req) if bucket.can_take(req.time, req.cost) else ("route-bucket",)
            if d[0] == "allow":
                bucket.take(req.cost)
            self.route_buckets[key] = bucket
            decision = d
        else:
            win = self.route_windows.pop(key, None) or SlidingWindow(policy[1], policy[2])
            d = admit(quota, req) if win.has_room(req.time) else ("route-window",)
            if d[0] == "allow":
                win.record(req.time)
            self.route_windows[key] = win
            decision = d
        self.client_buckets[req.client] = quota
        return decision

    def record(self, req, decision):
        stats = self.clients.pop(req.client, None) or ClientStats()
        route = self.route_stats.pop(req.route, None) or RouteStats()
        stats.total += 1
        in_window = stats.recent.record(req.time)
        if in_window > stats.peak:
            stats.peak = in_window
        if in_window == BURST_THRESHOLD:
            stats.bursts += 1
            print(f"  ! burst: {req.client} sent {in_window} requests within {BURST_WINDOW} ticks")
        kind = decision[0]
        if kind == "allow":
            stats.allowed += 1
            stats.strikes = 0
            route.allowed += 1
            route.units += req.cost
        elif kind == "penalty":
            route.throttled += 1
            stats.penalized += 1
        else:
            route.throttled += 1
            if kind == "client-quota":
                stats.quota_throttled += 1
            else:
                stats.route_throttled += 1
            stats.strikes += 1
            if stats.strikes >= PENALTY_STRIKES:
                stats.blocked_until = req.time + PENALTY_TICKS
                stats.strikes = 0
                print(f"  ! penalty: {req.client} blocked until t={stats.blocked_until}")
        self.clients[req.client] = stats
        self.route_stats[req.route] = route

    def print_summary(self):
        print("== clients ==")
        for cid in sorted(self.clients):
            s = self.clients[cid]
            b = self.client_buckets.get(cid)
            tokens = fmt_milli(b.tokens_milli) if b is not None else "n/a"
            print(f"{cid}: total={s.total} allowed={s.allowed} route={s.route_throttled} "
                  f"quota={s.quota_throttled} penalty={s.penalized} bursts={s.bursts} "
                  f"peak={s.peak} tokens={tokens}")
        print("== routes ==")
        for name in sorted(self.route_stats):
            r = self.route_stats[name]
            print(f"{name}: allowed={r.allowed} throttled={r.throttled} units={r.units}")


def fmt_milli(m):
    padded = f"{1000 + m % 1000}"
    return f"{m // 1000}.{padded[1:]}"


def describe(decision):
    if decision[0] == "allow":
        return f"allow (quota {fmt_milli(decision[1])})"
    return f"throttle {decision[0]}"


def parse_num(text, field):
    try:
        return int(text)
    except ValueError:
        raise ParseError(f"bad {field} '{text}'")


def parse_line(line, number):
    parts = line.strip().split(" ")
    if len(parts) != 4:
        raise ParseError(f"expected 4 fields, found {len(parts)}")
    time = parse_num(parts[0], "time")
    cost = parse_num(parts[3], "cost")
    if cost <= 0:
        raise ParseError(f"cost must be positive, got {cost}")
    return Request(number, time, parts[1], parts[2], cost)


def build_log():
    clients = ["acme", "bolt", "cyan", "delta", "echo"]
    routes = ["/login", "/search", "/upload", "/health"]
    out = []
    for t in range(60):
        if t % 2 == 0:
            k = t // 2
            client = clients[(k * 3) % 5]
            route = routes[k % 4]
            cost = 3 if route == "/upload" else 1
            out.append(f"{t} {client} {route} {cost}\n")
        if 30 <= t < 33:
            for _ in range(3):
                out.append(f"{t} bolt /search 1\n")
        if 40 <= t <= 45:
            out.append(f"{t} delta /login 1\n")
        if t % 7 == 0:
            out.append(f"{t} cyan /login 1\n")
        if t == 13:
            out.append("13 acme /login\n")
        if t == 27:
            out.append("27 delta /search x\n")
        if t == 44:
            out.append("44 echo /health 0\n")
        if t == 50:
            out.append("50 zulu /admin 1\n")
        if 50 <= t < 53:
            for _ in range(3):
                out.append(f"{t} echo /health 1\n")
    return "".join(out)


def main():
    with open(LOG_PATH, "w") as f:
        f.write(build_log())
    with open(LOG_PATH) as f:
        text = f.read()
    requests = []
    line_no = 0
    malformed = 0
    for line in text.split("\n"):
        line_no += 1
        if line.strip() == "":
            continue
        try:
            requests.append(parse_line(line, len(requests) + 1))
        except ParseError as e:
            malformed += 1
            print(f"line {line_no}: skipped ({e})")
    print(f"parsed {len(requests)} requests, {malformed} malformed lines")
    limiter = Limiter()
    rejected = 0
    for req in requests:
        head = f"#{req.number} t={req.time} {req.client} {req.route} cost={req.cost}"
        try:
            decision = limiter.handle(req)
            print(f"{head} -> {describe(decision)}")
        except LimitError as e:
            rejected += 1
            print(f"{head} -> rejected ({e})")
    print(f"rejected {rejected} requests")
    limiter.print_summary()


if __name__ == "__main__":
    main()
