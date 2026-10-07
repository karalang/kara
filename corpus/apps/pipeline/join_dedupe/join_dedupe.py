#!/usr/bin/env python3
"""pipeline/join_dedupe -- Python mirror of source.orig.kara."""

from dataclasses import dataclass


class RowError(Exception):
    pass


@dataclass
class Contact:
    id: int
    name: str
    email: str
    company: str
    updated: int


@dataclass
class Signup:
    name: str
    email: str
    channel: str
    signed_up: int


def title_case(word):
    lower = word.lower()
    if not lower:
        return lower
    return lower[0:1].upper() + lower[1:]


def normalize_name(raw):
    out = ""
    for word in raw.strip().split(" "):
        if not word:
            continue
        if out:
            out += " "
        out += title_case(word)
    return out


def canonical_domain(domain):
    if domain == "googlemail.com":
        return "gmail.com"
    return domain


def normalize_email(raw):
    cleaned = raw.strip().lower()
    parts = cleaned.split("@")
    if len(parts) != 2:
        raise RowError(f"invalid email '{raw.strip()}'")
    domain = canonical_domain(parts[1])
    if "." not in domain:
        raise RowError(f"invalid email '{raw.strip()}'")
    local = parts[0].split("+")[0]
    if domain == "gmail.com":
        local = local.replace(".", "")
    if not local:
        raise RowError(f"invalid email '{raw.strip()}'")
    return local + "@" + domain


def parse_int(s):
    try:
        return int(s)
    except ValueError:
        return None


def parse_date(raw):
    text = raw.strip()
    parts = text.split("-")
    if len(parts) != 3:
        raise RowError(f"invalid date '{text}'")
    y = parse_int(parts[0])
    m = parse_int(parts[1])
    d = parse_int(parts[2])
    y = -1 if y is None else y
    m = -1 if m is None else m
    d = -1 if d is None else d
    if y < 1900 or m < 1 or m > 12 or d < 1 or d > 31:
        raise RowError(f"invalid date '{text}'")
    return y * 10000 + m * 100 + d


def format_date(stamp):
    return f"{stamp // 10000}-{(stamp // 100) % 100:02d}-{stamp % 100:02d}"


def parse_channel(raw):
    c = raw.strip().lower()
    if c in ("web", "event", "referral"):
        return c
    raise RowError(f"unknown channel '{c}'")


def split_fields(line, want):
    fields = line.split(",")
    if len(fields) != want:
        raise RowError(f"expected {want} fields, got {len(fields)}")
    return fields


def parse_contact(line):
    f = split_fields(line, 5)
    id_text = f[0].strip()
    n = parse_int(id_text)
    if n is None:
        raise RowError(f"invalid id '{id_text}'")
    email = normalize_email(f[2])
    updated = parse_date(f[4])
    company = f[3].strip()
    return Contact(n, normalize_name(f[1]), email, company if company else "-", updated)


def parse_signup(line):
    f = split_fields(line, 4)
    email = normalize_email(f[0])
    channel = parse_channel(f[2])
    signed_up = parse_date(f[3])
    return Signup(normalize_name(f[1]), email, channel, signed_up)


def data_lines(text):
    out = []
    lineno = 0
    for line in text.split("\n"):
        lineno += 1
        if lineno == 1:
            continue
        t = line.strip()
        if not t or t.startswith("#"):
            continue
        out.append((lineno, line))
    return out


def dedupe(rows, stamp):
    best = {}
    merged = 0
    for row in rows:
        key = row.email
        if key in best:
            merged += 1
            replace = stamp(row) > stamp(best[key])
        else:
            replace = True
        if replace:
            best[key] = row
    return dict(sorted(best.items())), merged


def join_lines(lines):
    return "".join(line + "\n" for line in lines)


def write_exports():
    crm = join_lines([
        "id,name,email,company,updated",
        "1,  alice   smith ,Alice.Smith@Example.com,Acme,2024-01-10",
        "2,Alice Smith,alice.smith+crm@example.com,Acme Corp,2024-03-02",
        "3,BOB JONES,bob@jones.io,Jones LLC,2023-11-20",
        "# imported from legacy system",
        "4,Carol  White,carol.white@gmail.com,Initech,2024-02-14",
        "5,carol white,CarolWhite+news@googlemail.com,Initech,2024-01-05",
        "6,Dan Brown,dan@brown.org,,2024-02-01",
        "7,Eve Adams,eve(at)adams.net,Hooli,2024-01-01",
        "",
        "8,Frank Moore,frank@moore.dev,Moore Dev,2024-13-01",
        "9,Grace Lee,grace@lee.co,Lee & Co,2024-02-20",
        "10,grace lee, GRACE@LEE.CO ,Lee and Co,2024-02-20",
        "x,Henry Ford,henry@ford.com,Ford,2024-01-01",
        "11,Ivan Petrov,ivan@petrov.ru,Petrov Ltd,2023-12-31",
    ])
    signups = join_lines([
        "email,name,channel,signed_up",
        "alice.smith+newsletter@example.com,Alice S.,web,2024-02-11",
        "ALICE.SMITH@example.com, alice  smith ,Event,2024-03-15",
        "carol.white@gmail.com,Carol W,referral,2024-01-20",
        "c.a.r.o.l.white@gmail.com,carol white,web,2024-02-25",
        "dan@brown.org,Daniel Brown,event,2023-10-10",
        "heidi@klum.de,Heidi K,web,2024-03-01",
        "heidi+promo@klum.de,heidi  k,web,2024-03-03",
        "ivan@petrov.ru,Ivan,podcast,2024-01-01",
        "judy@hops.com,Judy Hopps,referral,2024-02-29",
        "judy@hops.com,Judy Hopps,web",
        "zed@example.com,Zed,web,2024-04-04",
    ])
    with open("crm_contacts.csv", "w") as fh:
        fh.write(crm)
    with open("newsletter_signups.csv", "w") as fh:
        fh.write(signups)


def main():
    write_exports()
    with open("crm_contacts.csv") as fh:
        crm_text = fh.read()
    with open("newsletter_signups.csv") as fh:
        signup_text = fh.read()

    print("== load ==")
    contacts, crm_rejects = [], []
    for lineno, line in data_lines(crm_text):
        try:
            contacts.append(parse_contact(line))
        except RowError as e:
            crm_rejects.append(f"  crm line {lineno}: {e}")
    print(f"crm: {len(contacts)} parsed, {len(crm_rejects)} rejected")
    for r in crm_rejects:
        print(r)

    signups, signup_rejects = [], []
    for lineno, line in data_lines(signup_text):
        try:
            signups.append(parse_signup(line))
        except RowError as e:
            signup_rejects.append(f"  signups line {lineno}: {e}")
    print(f"signups: {len(signups)} parsed, {len(signup_rejects)} rejected")
    for r in signup_rejects:
        print(r)

    print("== dedupe ==")
    crm, crm_merged = dedupe(contacts, lambda c: c.updated)
    subs, sub_merged = dedupe(signups, lambda s: s.signed_up)
    print(f"crm: {len(contacts)} -> {len(crm)} unique ({crm_merged} merged)")
    print(f"signups: {len(signups)} -> {len(subs)} unique ({sub_merged} merged)")

    matched, left_only = [], []
    for email, c in crm.items():
        crm_part = f"#{c.id} {c.name} ({c.company}, {format_date(c.updated)})"
        s = subs.get(email)
        if s is not None:
            matched.append(f"  {email}: {crm_part} <-> {s.name} via {s.channel} on {format_date(s.signed_up)}")
        else:
            left_only.append(f"  {email}: {crm_part}")
    right_only = []
    for email, s in subs.items():
        if email not in crm:
            right_only.append(f"  {email}: {s.name} via {s.channel} on {format_date(s.signed_up)}")

    print(f"== matched ({len(matched)}) ==")
    for line in matched:
        print(line)
    print(f"== crm only ({len(left_only)}) ==")
    for line in left_only:
        print(line)
    print(f"== signups only ({len(right_only)}) ==")
    for line in right_only:
        print(line)

    coverage = len(matched) * 100 // len(crm)
    print(f"newsletter coverage of crm: {coverage}%")


if __name__ == "__main__":
    main()
