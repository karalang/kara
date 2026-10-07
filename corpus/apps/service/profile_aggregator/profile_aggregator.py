"""Python mirror of source.orig.kara (profile aggregator service)."""

import time

BACKEND_LATENCY_MS = 3
MAX_RECOMMENDATIONS = 3

TIER_LABEL = {"Free": "free", "Silver": "silver", "Gold": "gold"}
TIER_DISCOUNT = {"Free": 0, "Silver": 5, "Gold": 10}
STATUS_LABEL = {
    "Pending": "pending",
    "Shipped": "shipped",
    "Delivered": "delivered",
    "Cancelled": "cancelled",
}


def status_is_open(status):
    return status in ("Pending", "Shipped")


class Account:
    def __init__(self, user_id, name, email, tier):
        self.user_id = user_id
        self.name = name
        self.email = email
        self.tier = tier


class Order:
    def __init__(self, order_id, item, amount_cents, status):
        self.order_id = order_id
        self.item = item
        self.amount_cents = amount_cents
        self.status = status


class FetchError(Exception):
    def __init__(self, kind, backend, user_id=0, reason=""):
        self.kind = kind
        self.backend = backend
        self.user_id = user_id
        self.reason = reason

    def describe(self):
        if self.kind == "NotFound":
            return f"{self.backend}: no record for user {self.user_id}"
        return f"{self.backend}: unavailable ({self.reason})"


class RequestError:
    def __init__(self, kind, id_):
        self.kind = kind
        self.id = id_

    def describe(self):
        if self.kind == "InvalidId":
            return f"invalid id {self.id}"
        return f"duplicate id {self.id}"


def sleep_ms(ms):
    time.sleep(ms / 1000.0)


class AccountsBackend:
    def __init__(self):
        self.table = {}
        rows = [
            (101, "Ada Lovelace", "ada@example.com", "Gold"),
            (102, "Grace Hopper", "grace@example.com", "Silver"),
            (103, "Alan Turing", "alan@example.com", "Free"),
            (104, "Katherine Johnson", "kj@example.com", "Gold"),
            (106, "Edsger Dijkstra", "edsger@example.com", "Silver"),
            (107, "Barbara Liskov", "barbara@example.com", "Free"),
            (108, "Donald Knuth", "don@example.com", "Silver"),
        ]
        for id_, name, email, tier in rows:
            self.table[id_] = Account(id_, name, email, tier)

    def fetch(self, user_id):
        sleep_ms(BACKEND_LATENCY_MS)
        if user_id in self.table:
            a = self.table[user_id]
            return ("ok", Account(a.user_id, a.name, a.email, a.tier))
        return ("err", FetchError("NotFound", "accounts", user_id=user_id))


class OrdersBackend:
    def __init__(self):
        self.table = {}
        self.add(101, 5001, "mechanical keyboard", 8999, "Shipped")
        self.add(101, 5002, "usb hub", 2450, "Delivered")
        self.add(101, 5003, "desk lamp", 3999, "Cancelled")
        self.add(102, 5004, "monitor arm", 6500, "Pending")
        self.add(103, 5005, "notebook", 1200, "Delivered")
        self.add(103, 5006, "fountain pen", 4500, "Delivered")
        self.add(104, 5007, "standing desk", 45000, "Pending")
        self.add(104, 5008, "monitor", 21999, "Shipped")
        self.add(105, 5009, "webcam", 5999, "Delivered")
        self.add(106, 5010, "headphones", 15000, "Delivered")
        self.add(108, 5011, "ergonomic chair", 32000, "Shipped")
        self.add(108, 5012, "notebook", 1200, "Pending")

    def add(self, user_id, order_id, item, amount_cents, status):
        self.table.setdefault(user_id, []).append(Order(order_id, item, amount_cents, status))

    def fetch(self, user_id):
        sleep_ms(BACKEND_LATENCY_MS)
        if user_id in self.table:
            return ("ok", list(self.table[user_id]))
        return ("err", FetchError("NotFound", "orders", user_id=user_id))


class RecsBackend:
    def __init__(self):
        self.table = {
            101: ["monitor", "usb hub", "desk lamp", "webcam"],
            102: ["monitor", "standing desk", "headphones"],
            103: ["fountain pen", "ink set", "notebook", "desk lamp"],
            104: ["ergonomic chair", "monitor", "webcam"],
            105: ["headphones", "webcam"],
            106: ["usb hub", "notebook", "headphones"],
            108: ["standing desk", "monitor arm"],
        }
        self.outage = {108}

    def fetch(self, user_id):
        sleep_ms(BACKEND_LATENCY_MS)
        if user_id in self.outage:
            return ("err", FetchError("Unavailable", "recommendations", reason="shard timeout"))
        if user_id in self.table:
            return ("ok", list(self.table[user_id]))
        return ("err", FetchError("NotFound", "recommendations", user_id=user_id))


class Profile:
    def __init__(self, user_id, display_name, email, tier, orders, recommendations, fallbacks):
        self.user_id = user_id
        self.display_name = display_name
        self.email = email
        self.tier = tier
        self.orders = orders
        self.recommendations = recommendations
        self.fallbacks = fallbacks

    def total_spent(self):
        total = 0
        for order in self.orders:
            if order.status != "Cancelled":
                total += order.amount_cents
        return total

    def open_orders(self):
        return sum(1 for o in self.orders if status_is_open(o.status))

    def discounted_spend(self):
        # all operands non-negative, so floor division == truncating division
        return self.total_spent() * (100 - TIER_DISCOUNT[self.tier]) // 100

    def is_degraded(self):
        return len(self.fallbacks) > 0


def guest_account(user_id):
    return Account(user_id, "Guest", "unknown", "Free")


def popular_items():
    return ["monitor", "headphones", "notebook", "webcam"]


def already_ordered(orders, item):
    for order in orders:
        if order.status != "Cancelled" and order.item == item:
            return True
    return False


def pick_recommendations(candidates, orders):
    picked = []
    for item in candidates:
        if len(picked) >= MAX_RECOMMENDATIONS:
            break
        if not already_ordered(orders, item):
            picked.append(item)
    return picked


def build_profile(user_id, accounts, orders, recs):
    account_res = accounts.fetch(user_id)
    orders_res = orders.fetch(user_id)
    recs_res = recs.fetch(user_id)

    fallbacks = []
    if account_res[0] == "ok":
        account = account_res[1]
    else:
        fallbacks.append(account_res[1].describe())
        account = guest_account(user_id)
    if orders_res[0] == "ok":
        order_list = orders_res[1]
    else:
        fallbacks.append(orders_res[1].describe())
        order_list = []
    if recs_res[0] == "ok":
        candidates = recs_res[1]
    else:
        fallbacks.append(recs_res[1].describe())
        candidates = popular_items()
    recommendations = pick_recommendations(candidates, order_list)
    return Profile(user_id, account.name, account.email, account.tier,
                   order_list, recommendations, fallbacks)


def format_cents(cents):
    dollars = cents // 100
    rem = cents % 100
    if rem < 10:
        return f"${dollars}.0{rem}"
    return f"${dollars}.{rem}"


def render(p):
    print(f"== profile {p.user_id} ==")
    print(f"name: {p.display_name} <{p.email}>")
    print(f"tier: {TIER_LABEL[p.tier]} ({TIER_DISCOUNT[p.tier]}% off)")
    spent = format_cents(p.total_spent())
    paid = format_cents(p.discounted_spend())
    print(f"orders: {len(p.orders)} total, {p.open_orders()} open, spent {spent}, paid {paid}")
    for o in p.orders:
        print(f"  #{o.order_id} {o.item} {format_cents(o.amount_cents)} {STATUS_LABEL[o.status]}")
    if not p.recommendations:
        print("recs: (none)")
    else:
        print(f"recs: {', '.join(p.recommendations)}")
    if p.is_degraded():
        for note in p.fallbacks:
            print(f"fallback: {note}")
    else:
        print("fallback: none")


def check_id(id_, seen):
    if id_ <= 0:
        return ("err", RequestError("InvalidId", id_))
    if id_ in seen:
        return ("err", RequestError("Duplicate", id_))
    return ("ok", id_)


def main():
    accounts = AccountsBackend()
    orders = OrdersBackend()
    recs = RecsBackend()

    requests = [101, 102, 103, 0, 104, 105, 106, 103, 107, 108, -7]

    seen = set()
    rejected = []
    profiles = []
    for id_ in requests:
        tag, val = check_id(id_, seen)
        if tag == "ok":
            seen.add(val)
            profile = build_profile(val, accounts, orders, recs)
            render(profile)
            profiles.append(profile)
        else:
            rejected.append(val)

    degraded = 0
    revenue = 0
    fallback_counts = {}
    tier_counts = {}
    for p in profiles:
        if p.is_degraded():
            degraded += 1
        revenue += p.discounted_spend()
        for note in p.fallbacks:
            backend = note.split(":")[0]
            fallback_counts[backend] = fallback_counts.get(backend, 0) + 1
        label = TIER_LABEL[p.tier]
        tier_counts[label] = tier_counts.get(label, 0) + 1

    print("== summary ==")
    print(f"requests: {len(requests)}, served: {len(profiles)}, rejected: {len(rejected)}")
    for err in rejected:
        print(f"rejected: {err.describe()}")
    print(f"degraded profiles: {degraded}")
    for backend in sorted(fallback_counts):
        print(f"fallbacks from {backend}: {fallback_counts[backend]}")
    for tier in sorted(tier_counts):
        print(f"tier {tier}: {tier_counts[tier]}")
    print(f"revenue after discounts: {format_cents(revenue)}")


if __name__ == "__main__":
    main()
