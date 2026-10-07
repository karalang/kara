#!/usr/bin/env python3
"""Mirror of source.orig.kara: parse an embedded sales CSV, group by region and
month, print a fixed-width report with subtotals and a grand total."""

import re

EXPECTED_FIELDS = 6


class CsvError(Exception):
    def __init__(self, kind, line):
        self.kind = kind
        self.line = line

    def __str__(self):
        if self.kind == "unterminated":
            return f"unterminated quoted field starting on line {self.line}"
        return f"unexpected quote inside unquoted field on line {self.line}"


class RowError:
    def __init__(self, kind, line, text=None, got=None):
        self.kind, self.line, self.text, self.got = kind, line, text, got

    def __str__(self):
        if self.kind == "field_count":
            return f"line {self.line}: expected {EXPECTED_FIELDS} fields, found {self.got}"
        if self.kind == "date":
            return f"line {self.line}: invalid date '{self.text}'"
        if self.kind == "quantity":
            return f"line {self.line}: invalid quantity '{self.text}'"
        if self.kind == "price":
            return f"line {self.line}: invalid unit price '{self.text}'"
        return f"line {self.line}: region is empty"


class Sale:
    def __init__(self, region, month, product, quantity, price_cents):
        self.region, self.month, self.product = region, month, product
        self.quantity, self.price_cents = quantity, price_cents

    def revenue_cents(self):
        return self.quantity * self.price_cents


class MonthTotal:
    def __init__(self, region, month):
        self.region, self.month = region, month
        self.orders = self.units = self.revenue_cents = 0

    def add(self, sale):
        self.orders += 1
        self.units += sale.quantity
        self.revenue_cents += sale.revenue_cents()

    def key(self):
        return (self.region, self.month, self.orders, self.units, self.revenue_cents)


class Totals:
    def __init__(self):
        self.orders = self.units = self.revenue_cents = 0

    def absorb(self, m):
        self.orders += m.orders
        self.units += m.units
        self.revenue_cents += m.revenue_cents


def parse_csv(text):
    chars = list(text)
    n = len(chars)
    records, fields, field = [], [], ""
    in_quotes = False
    line = record_line = 1
    quote_line = 0
    i = 0
    while i < n:
        c = chars[i]
        if in_quotes:
            if c == '"':
                if i + 1 < n and chars[i + 1] == '"':
                    field += '"'
                    i += 1
                else:
                    in_quotes = False
            else:
                if c == "\n":
                    line += 1
                field += c
        elif c == '"':
            if field == "":
                in_quotes = True
                quote_line = line
            else:
                raise CsvError("stray", line)
        elif c == ",":
            fields.append(field)
            field = ""
        elif c == "\n":
            fields.append(field)
            field = ""
            blank = len(fields) == 1 and fields[0] == ""
            if not blank:
                records.append((record_line, fields))
            fields = []
            line += 1
            record_line = line
        elif c != "\r":
            field += c
        i += 1
    if in_quotes:
        raise CsvError("unterminated", quote_line)
    if field != "" or fields:
        fields.append(field)
        records.append((record_line, fields))
    return records


def parse_int(s):
    return int(s) if re.fullmatch(r"-?[0-9]+", s) else None


def parse_month(date):
    if len(date) != 10 or date[4] != "-" or date[7] != "-":
        return None
    year, month, day = parse_int(date[0:4]), parse_int(date[5:7]), parse_int(date[8:10])
    if year is None or month is None or day is None:
        return None
    if year < 2000 or month < 1 or month > 12 or day < 1 or day > 31:
        return None
    return date[0:7]


def parse_price(text):
    parts = text.replace(",", "").split(".")
    dollars = parse_int(parts[0])
    if dollars is None or dollars < 0:
        return None
    if len(parts) == 1:
        return dollars * 100
    if len(parts) != 2 or len(parts[1]) != 2:
        return None
    cents = parse_int(parts[1])
    if cents is None or cents < 0:
        return None
    return dollars * 100 + cents


def parse_sale(rec):
    line, fields = rec
    if len(fields) != EXPECTED_FIELDS:
        return RowError("field_count", line, got=len(fields))
    date = fields[0].strip()
    month = parse_month(date)
    if month is None:
        return RowError("date", line, text=date)
    region = fields[1].strip()
    if region == "":
        return RowError("region", line)
    qty_text = fields[3].strip()
    q = parse_int(qty_text)
    if q is None or not q > 0:
        return RowError("quantity", line, text=qty_text)
    price_text = fields[4].strip()
    p = parse_price(price_text)
    if p is None:
        return RowError("price", line, text=price_text)
    return Sale(region, month, fields[2].strip(), q, p)


def aggregate(sales):
    index, totals = {}, []
    for sale in sales:
        key = f"{sale.region}|{sale.month}"
        pos = index.get(key, -1)
        if pos >= 0:
            totals[pos].add(sale)
        else:
            total = MonthTotal(sale.region, sale.month)
            total.add(sale)
            index[key] = len(totals)
            totals.append(total)
    totals.sort(key=MonthTotal.key)
    return totals


def pad_right(s, width):
    return s + " " * max(0, width - len(s))


def pad_left(s, width):
    return " " * max(0, width - len(s)) + s


def format_money(cents):
    digits = str(cents // 100)
    n = len(digits)
    out = ""
    for i in range(n):
        if i > 0 and (n - i) % 3 == 0:
            out += ","
        out += digits[i]
    rem = cents % 100
    out += "."
    if rem < 10:
        out += "0"
    return out + str(rem)


MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun",
          "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]


def month_label(key):
    m = parse_int(key[5:7])
    name = MONTHS[m - 1] if m is not None and 1 <= m <= 12 else "???"
    return f"{name} {key[0:4]}"


def row(region, month, orders, units, revenue):
    avg = revenue // orders if orders > 0 else 0
    return (pad_right(region, 11) + pad_right(month, 11) + pad_left(str(orders), 6)
            + pad_left(str(units), 8) + pad_left(format_money(revenue), 15)
            + pad_left(format_money(avg), 13))


def print_report(totals):
    width = 64
    print(pad_right("REGION", 11) + pad_right("MONTH", 11) + pad_left("ORDERS", 6)
          + pad_left("UNITS", 8) + pad_left("REVENUE", 15) + pad_left("AVG ORDER", 13))
    print("-" * width)
    grand, sub = Totals(), Totals()
    current = ""
    for t in totals:
        if t.region != current:
            if current != "":
                print(row("", "subtotal", sub.orders, sub.units, sub.revenue_cents))
                print("")
            current = t.region
            sub = Totals()
            print(row(t.region, month_label(t.month), t.orders, t.units, t.revenue_cents))
        else:
            print(row("", month_label(t.month), t.orders, t.units, t.revenue_cents))
        sub.absorb(t)
        grand.absorb(t)
    if current != "":
        print(row("", "subtotal", sub.orders, sub.units, sub.revenue_cents))
    print("=" * width)
    print(row("GRAND TOTAL", "", grand.orders, grand.units, grand.revenue_cents))


def embedded_csv():
    lines = [
        'date,region,product,quantity,unit_price,customer',
        '2024-01-05,North,Widget,10,12.50,"Acme, Inc."',
        '2024-01-17,South,Gadget,3,"1,250.00",Initech',
        '2024-01-20,North,Gizmo,5,7.25,"The ""Best"" Shop"',
        '2024-02-02,East,Widget,8,12.50,Globex',
        '2024-02-11,North,Widget,12,12.00,"Acme, Inc."',
        '2024-02-14,South,Doohickey,20,3.10,"Smith & Sons, LLC"',
        '2024-02-29,East,Gadget,1,"1,199.99","Umbrella ""Corp"""',
        '',
        '2024-03-03,West,Widget,7,12.50,Hooli',
        '2024-03-09,North,Gizmo,ten,7.25,Initech',
        '2024-03-15,West,Gadget,2,"1,250.00","Pied Piper, ""Compression"" Div."',
        '2024-03-21,East,Gizmo,15,7.00,Globex',
        '2024-13-02,South,Widget,4,12.50,Initech',
        '2024-03-28,South,Widget,6,12.50',
        '2024-01-30, North ,Doohickey,40,2.95,"Vandelay Industries"',
        '2024-02-20,West,Gizmo,9,7.25,"Bluth Company',
        'Banana Stand"',
        '2024-03-30,North,Gadget,1,"1,250.00","Acme, Inc."',
        '2024-02-08,East,Doohickey,25,2.95,Kramerica',
        '2024-01-12,West,Widget,3,12.5,Hooli',
        '2024-03-11,,Widget,2,12.50,Nobody',
        '2024-01-25,South,Gizmo,4,7.25,"Initech"',
        '2024-03-17,East,Widget,0,12.50,Globex',
        '2024-03-19,West,Doohickey,30,"2.95","Hooli, ""XYZ"""',
    ]
    return "".join(line + "\n" for line in lines)


def load_sales(text):
    records = parse_csv(text)
    sales, rejected = [], []
    for rec in records[1:]:
        r = parse_sale(rec)
        (sales if isinstance(r, Sale) else rejected).append(r)
    return sales, rejected


def validate_upload(name, text):
    try:
        records = parse_csv(text)
        print(f"  {name}: ok ({len(records)} records)")
    except CsvError as e:
        print(f"  {name}: error: {e}")


def main():
    path = "sales.csv"
    with open(path, "w") as f:
        f.write(embedded_csv())
    with open(path) as f:
        text = f.read()
    try:
        sales, rejected = load_sales(text)
    except CsvError as e:
        print(f"fatal: {e}")
        return
    print("SALES REPORT BY REGION AND MONTH")
    print(f"source: {path} ({len(sales) + len(rejected)} data rows, "
          f"{len(sales)} accepted, {len(rejected)} rejected)")
    print("")
    print_report(aggregate(sales))
    print("")
    print("Rejected rows:")
    for e in rejected:
        print(f"  {e}")
    print("")
    print("Upload validation:")
    validate_upload("good.csv", 'a,b\n"x, y","say ""hi"""\n')
    validate_upload("truncated.csv", 'date,region\n2024-01-01,"North\n')
    validate_upload("stray.csv", 'a,b\nfoo,ba"r\n')


if __name__ == "__main__":
    main()
