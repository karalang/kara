"""Python mirror of source.orig.kara (pipeline/etl_orders)."""


class Reject(Exception):
    def __init__(self, msg):
        self.msg = msg


TIERS = {"gold": "GOLD", "silver": "SILVER", "bronze": "BRONZE"}


def customers_csv():
    names = ["Acme Corp", "Birch & Co", "Cobalt Labs", "Delta Freight", "Ember Foods",
             "Fjord Media", "Granite LLC", "Harbor Tech", "Iris Health"]
    tiers = ["gold", "silver", "bronze", "silver", "gold", "bronze", "bronze", "silver", "platinum"]
    out = "customer_id,name,tier\n"
    for i in range(len(names)):
        out += f"{i + 1},{names[i]},{tiers[i]}\n"
    return out


def orders_csv():
    skus = ["WIDGET", "GADGET", "SPROCKET", "GIZMO"]
    out = "order_id,customer_id,date,sku,quantity,unit_cents\n"
    for i in range(36):
        cust = (i * 7) % 10 + 1
        month = (i * 5) % 6 + 1
        day = (i * 11) % 28 + 1
        qty = (i * 3) % 7 + 1
        cents = 199 + (i * 37) % 900
        out += f"{1000 + i},{cust},2026-{month:02d}-{day:02d},{skus[i % 4]},{qty},{cents}\n"
    out += "1036,2,2026-02-30,WIDGET,3,450\n"
    out += "1037,3,2026-04-11,GADGET,-2,1200\n"
    out += "1038,5,2026-13-01,GIZMO,1,999\n"
    out += "1039,4,2026-05-07,SPROCKET,0,350\n"
    out += "1040,1,2026-03-xx,WIDGET,2,250\n"
    out += "1041,6,2026-06-15,GADGET,4\n"
    out += "1042,7,2026-01-20,GIZMO,five,800\n"
    out += "1043,8,2024-02-29,WIDGET,2,1500\n"
    out += "1044,2,2026-02-29,GIZMO,1,700\n"
    out += "1045,9,2026-06-01,SPROCKET,-10,120\n"
    return out


def to_int(raw):
    s = raw.strip()
    body = s[1:] if s[:1] in "+-" else s
    if not body or not body.isdigit():
        return None
    return int(s)


def parse_int(field, raw):
    n = to_int(raw)
    if n is None:
        raise Reject(f"field {field} is not a number: '{raw}'")
    return n


def is_leap(y):
    return (y % 4 == 0 and y % 100 != 0) or y % 400 == 0


def days_in_month(y, m):
    if m == 2:
        return 29 if is_leap(y) else 28
    if m in (4, 6, 9, 11):
        return 30
    return 31


def parse_date(raw):
    parts = raw.split("-")
    bad = Reject(f"invalid date '{raw}'")
    if len(parts) != 3:
        raise bad
    y, m, d = (to_int(p) for p in parts)
    if y is None or m is None or d is None:
        raise bad
    if m < 1 or m > 12 or d < 1 or d > days_in_month(y, m):
        raise bad
    return (y, m, d)


def parse_order(line, line_no):
    cols = line.split(",")
    if len(cols) != 6:
        raise Reject(f"expected 6 columns, got {len(cols)}")
    oid = parse_int("order_id", cols[0])
    cust = parse_int("customer_id", cols[1])
    date = parse_date(cols[2])
    qty = parse_int("quantity", cols[4])
    cents = parse_int("unit_cents", cols[5])
    if qty < 0:
        raise Reject(f"negative quantity {qty}")
    if qty == 0:
        raise Reject("zero quantity")
    return {"id": oid, "line": line_no, "customer_id": cust, "date": date,
            "sku": cols[3].strip(), "quantity": qty, "unit_cents": cents}


def parse_customer(line):
    cols = line.split(",")
    if len(cols) != 3:
        raise Reject(f"expected 3 columns, got {len(cols)}")
    cid = parse_int("customer_id", cols[0])
    t = cols[2].strip()
    if t not in TIERS:
        raise Reject(f"unknown tier '{t}'")
    return {"id": cid, "name": cols[1].strip(), "tier": TIERS[t]}


def load(path, parser, with_line):
    with open(path) as f:
        text = f.read()
    good, bad = [], []
    for line_no, line in enumerate(text.split("\n"), start=1):
        if line_no == 1 or not line.strip():
            continue
        try:
            good.append(parser(line, line_no) if with_line else parser(line))
        except Reject as r:
            bad.append((path, line_no, r.msg))
    return good, bad


def money(cents):
    return f"{cents // 100}.{cents % 100:02d}"


def add_to(table, key, qty, cents):
    o, u, r = table.get(key, (0, 0, 0))
    table[key] = (o + 1, u + qty, r + cents)


def main():
    with open("customers.csv", "w") as f:
        f.write(customers_csv())
    with open("orders.csv", "w") as f:
        f.write(orders_csv())

    orders, order_rejects = load("orders.csv", parse_order, True)
    customers, customer_rejects = load("customers.csv", parse_customer, False)

    by_id = {c["id"]: c for c in customers}
    rejects = customer_rejects + order_rejects

    by_tier_month, by_tier = {}, {}
    joined = grand = 0
    for o in orders:
        c = by_id.get(o["customer_id"])
        if c is None:
            rejects.append(("orders.csv", o["line"], f"no customer with id {o['customer_id']}"))
            continue
        revenue = o["quantity"] * o["unit_cents"]
        y, m, _ = o["date"]
        add_to(by_tier_month, f"{c['tier']} {y}-{m:02d}", o["quantity"], revenue)
        add_to(by_tier, c["tier"], o["quantity"], revenue)
        joined += 1
        grand += revenue

    print(f"loaded {len(by_id)} customers, {joined} joined orders, {len(rejects)} rejects")
    print()
    print("REJECTS")
    for path, line_no, msg in rejects:
        print(f"  {path}:{line_no}: {msg}")
    print()
    print("REVENUE BY TIER AND MONTH")
    for key in sorted(by_tier_month):
        o, u, r = by_tier_month[key]
        print(f"  {key:<16} orders={o} units={u} revenue={money(r)}")
    print()
    print("REVENUE BY TIER")
    for key in sorted(by_tier):
        o, u, r = by_tier[key]
        print(f"  {key:<8} orders={o} units={u} revenue={money(r)}")
    print(f"TOTAL revenue={money(grand)}")


if __name__ == "__main__":
    main()
