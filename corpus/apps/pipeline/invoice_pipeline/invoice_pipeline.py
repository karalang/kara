#!/usr/bin/env python3
"""Python mirror of source.orig.kara: line items -> invoices -> ledger summary."""

CATEGORIES = ["grocery", "clothing", "electronics", "books", "services"]
REGIONS = ["NE", "WEST", "SOUTH", "EXPORT"]
CURRENCIES = ["USD", "EUR", "JPY"]


class PipelineError(Exception):
    pass


def parse_category(line, s):
    if s in CATEGORIES:
        return s
    raise PipelineError(f"line {line}: unknown category '{s}'")


def parse_region(line, s):
    if s in REGIONS:
        return s
    raise PipelineError(f"line {line}: unknown region '{s}'")


def parse_currency(line, s):
    if s in CURRENCIES:
        return s
    raise PipelineError(f"line {line}: unknown currency '{s}'")


def parse_int(line, s):
    t = s.strip()
    try:
        n = int(t)
    except ValueError:
        n = None
    if n is not None and n >= 0 and (t.isdigit() or (t.startswith("+") and t[1:].isdigit())):
        return n
    raise PipelineError(f"line {line}: '{s}' is not a valid number")


def parse_discount(line, s):
    if s == "none":
        return ("none", 0)
    parts = s.split(":")
    if len(parts) != 2:
        raise PipelineError(f"line {line}: malformed discount record")
    amount = parse_int(line, parts[1])
    if parts[0] == "pct":
        return ("pct", amount)
    if parts[0] == "bogo":
        return ("bogo", amount)
    raise PipelineError(f"line {line}: malformed discount record")


def tax_rate_bps(cat, region):
    if region == "EXPORT":
        if cat == "services":
            raise PipelineError(f"no tax rule for {cat} shipped to {region}")
        return 0
    if region == "NE":
        return {"grocery": 0, "clothing": 400, "books": 250}.get(cat, 888)
    if region == "WEST":
        return {"grocery": 125, "services": 500}.get(cat, 925)
    return {"grocery": 300, "books": 0}.get(cat, 700)


def round_div(numer, denom):
    return (numer * 2 + denom) // (denom * 2)


def price_line(item, region):
    gross = item["unit_price"] * item["quantity"]
    kind, amt = item["discount"]
    if kind == "none":
        discount = 0
    elif kind == "pct":
        discount = round_div(gross * amt, 100)
    else:
        discount = (item["quantity"] // (amt + 1)) * item["unit_price"]
    net = gross - discount
    bps = tax_rate_bps(item["category"], region)
    tax = round_div(net * bps, 10000)
    return dict(sku=item["sku"], description=item["description"], quantity=item["quantity"],
                gross=gross, discount=discount, net=net, tax=tax, tax_bps=bps)


def build_invoice(order):
    lines = []
    subtotal = discount_total = tax_total = 0
    for item in order["items"]:
        priced = price_line(item, order["region"])
        subtotal += priced["gross"]
        discount_total += priced["discount"]
        tax_total += priced["tax"]
        lines.append(priced)
    return dict(order_id=order["id"], customer=order["customer"], region_name=order["region"],
                currency=order["currency"], lines=lines, subtotal=subtotal,
                discount_total=discount_total, tax_total=tax_total,
                total=subtotal - discount_total + tax_total)


def pad3(n):
    return f"{n:03d}"


def group_thousands(n, sep):
    if n < 1000:
        return str(n)
    return group_thousands(n // 1000, sep) + sep + pad3(n % 1000)


def format_money(amount, currency):
    sign = "-" if amount < 0 else ""
    a = -amount if amount < 0 else amount
    if currency == "USD":
        return f"{sign}${group_thousands(a // 100, ',')}.{a % 100:02d}"
    if currency == "EUR":
        return f"{sign}{group_thousands(a // 100, '.')},{a % 100:02d} EUR"
    return f"{sign}JPY {group_thousands(a, ',')}"


def format_rate(bps):
    return f"{bps // 100}.{bps % 100:02d}%"


def pad_left(s, width):
    return s.rjust(width)


def pad_right(s, width):
    return s.ljust(width)


def print_invoice(inv):
    cur = inv["currency"]
    print(f"=== Invoice {inv['order_id']} ===")
    print(f"Customer: {inv['customer']}   Region: {inv['region_name']}   Currency: {cur}")
    for line in inv["lines"]:
        label = pad_right(f"{line['sku']} {line['description']}", 28)
        qty = pad_left(f"x{line['quantity']}", 5)
        net = pad_left(format_money(line["net"], cur), 16)
        tax = pad_left(format_money(line["tax"], cur), 14)
        print(f"  {label}{qty}{net}  tax {format_rate(line['tax_bps'])}{tax}")
        if line["discount"] > 0:
            print(f"      discount {format_money(-line['discount'], cur)}")
    print(f"  Subtotal: {pad_left(format_money(inv['subtotal'], cur), 18)}")
    print(f"  Discount: {pad_left(format_money(-inv['discount_total'], cur), 18)}")
    print(f"  Tax:      {pad_left(format_money(inv['tax_total'], cur), 18)}")
    print(f"  TOTAL:    {pad_left(format_money(inv['total'], cur), 18)}")
    print("")


def parse_record(line_no, line, orders):
    f = line.split("|")
    if f[0] == "ORDER":
        if len(f) != 5:
            raise PipelineError(f"line {line_no}: malformed ORDER record")
        region = parse_region(line_no, f[3])
        currency = parse_currency(line_no, f[4])
        orders.append(dict(id=f[1], customer=f[2], region=region, currency=currency, items=[]))
        return
    if f[0] != "ITEM" or len(f) != 7:
        raise PipelineError(f"line {line_no}: malformed {f[0]} record")
    if not orders:
        raise PipelineError(f"line {line_no}: ITEM before any ORDER")
    item = dict(sku=f[1], description=f[2],
                category=parse_category(line_no, f[3]),
                unit_price=parse_int(line_no, f[4]),
                quantity=parse_int(line_no, f[5]),
                discount=parse_discount(line_no, f[6]))
    orders[-1]["items"].append(item)


def parse_orders(text, rejects):
    orders = []
    line_no = 0
    for raw in text.split("\n"):
        line_no += 1
        line = raw.strip()
        if line == "" or line.startswith("#"):
            continue
        try:
            parse_record(line_no, line, orders)
        except PipelineError as e:
            rejects.append(str(e))
    return orders


INPUT = (
    "# kind|fields...\n"
    "ITEM|X-0|orphan|books|100|1|none\n"
    "ORDER|INV-1001|Acme Corp|NE|USD\n"
    "ITEM|GR-101|Organic coffee|grocery|1299|3|none\n"
    "ITEM|CL-220|Rain jacket|clothing|8950|1|pct:15\n"
    "ITEM|EL-310|USB-C hub|electronics|3499|2|none\n"
    "ITEM|BK-007|Field guide|books|2450|4|bogo:3\n"
    "ORDER|INV-1002|Bluebird Cafe|WEST|USD\n"
    "ITEM|GR-140|Oat milk case|grocery|2199|12|bogo:5\n"
    "ITEM|SV-001|Espresso machine service|services|15000|1|pct:10\n"
    "ITEM|EL-415|Card reader|electronics|5999|two|none\n"
    "ITEM|EL-416|Receipt printer|electronics|12999|1|pct:5\n"
    "ORDER|INV-1003|Lindqvist AB|EXPORT|EUR\n"
    "ITEM|EL-501|Label printer|electronics|24900|3|pct:12\n"
    "ITEM|BK-120|Manual set|books|1875|10|bogo:4\n"
    "ORDER|INV-1004|Sakura Trading|EXPORT|JPY\n"
    "ITEM|CL-900|Linen shirts|clothing|4800|25|pct:20\n"
    "ITEM|SV-020|Onsite setup|services|90000|1|none\n"
    "ORDER|INV-1005|Magnolia Books|SOUTH|USD\n"
    "ITEM|BK-310|Atlas|books|6500|2|none\n"
    "ITEM|GR-077|Pecan tin|grocery|1650|7|bogo:2\n"
    "ITEM|CL-404|Tote bag|clothing|1225|9|pct:33\n"
    "ITEM|ZZ-999|Mystery box|toys|999|1|none\n"
    "ORDER|INV-1006|Harbor Supply|MARS|USD\n"
    "ORDER|INV-1007|Ridge Outfitters|WEST|EUR\n"
    "ITEM|CL-610|Wool socks|clothing|1499|6|bogo:2\n"
    "ITEM|EL-611|Headlamp|electronics|3995|2|pct:7\n"
    "ORDER|INV-1008|Kobe Gear|EXPORT|JPY\n"
    "ITEM|CL-901|Denim jackets|clothing|12800|15|pct:17\n"
    "ITEM|BK-902|Catalog|books|1500|8|bogo:3\n"
)


def main():
    with open("orders.txt", "w") as fh:
        fh.write(INPUT)
    with open("orders.txt") as fh:
        text = fh.read()

    rejects = []
    orders = parse_orders(text, rejects)

    invoices = []
    for order in orders:
        try:
            invoices.append(build_invoice(order))
        except PipelineError as e:
            rejects.append(f"order {order['id']}: {e}")

    for inv in invoices:
        print_invoice(inv)

    totals, tax_by_region, counts = {}, {}, {}
    for inv in invoices:
        code = inv["currency"]
        key = f"{code}|{inv['region_name']}"
        totals[code] = totals.get(code, 0) + inv["total"]
        tax_by_region[key] = tax_by_region.get(key, 0) + inv["tax_total"]
        counts[code] = counts.get(code, 0) + 1

    print("=== Ledger summary ===")
    print(f"Invoices issued: {len(invoices)}")
    for code in sorted(totals):
        n = counts.get(code, 0)
        print(f"  {pad_right(code, 4)} invoices={n}  billed {pad_left(format_money(totals[code], code), 16)}")
    print("Tax collected by currency/region:")
    for key in sorted(tax_by_region):
        cur = key.split("|")[0]
        print(f"  {pad_right(key, 12)}{pad_left(format_money(tax_by_region[key], cur), 16)}")
    print(f"Rejected records: {len(rejects)}")
    for r in rejects:
        print(f"  - {r}")


if __name__ == "__main__":
    main()
