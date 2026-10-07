#!/usr/bin/env python3
"""Python mirror of source.orig.kara: hand-written JSON parser, v1 -> v2 migration,
sorted-key serialization. Same algorithm, same data, byte-identical stdout."""
import re
import sys


class ParseError(Exception):
    def __init__(self, pos, msg):
        super().__init__(msg)
        self.pos = pos
        self.msg = msg


def hex_val(c):
    if '0' <= c <= '9':
        return ord(c) - ord('0')
    if 'a' <= c <= 'f':
        return ord(c) - ord('a') + 10
    if 'A' <= c <= 'F':
        return ord(c) - ord('A') + 10
    return None


# JSON values: None, bool, Num (raw source text), str, list, dict
class Num:
    def __init__(self, text):
        self.text = text


class JsonParser:
    def __init__(self, text):
        self.chars = list(text)
        self.pos = 0

    def peek(self):
        return self.chars[self.pos] if self.pos < len(self.chars) else None

    def fail(self, msg):
        return ParseError(self.pos, msg)

    def eat(self, c):
        if self.peek() != c:
            return False
        self.pos += 1
        return True

    def skip_ws(self):
        while self.peek() is not None:
            c = self.peek()
            if c not in (' ', '\n', '\t', '\r'):
                break
            self.pos += 1

    def expect(self, want):
        if self.eat(want):
            return
        c = self.peek()
        found = f"'{c}'" if c is not None else "end of input"
        raise self.fail(f"expected '{want}', found {found}")

    def parse_value(self):
        self.skip_ws()
        c = self.peek()
        if c is None:
            raise self.fail("unexpected end of input")
        if c == '{':
            return self.parse_object()
        if c == '[':
            return self.parse_array()
        if c == '"':
            return self.parse_string()
        if c == 't':
            return self.parse_word("true", True)
        if c == 'f':
            return self.parse_word("false", False)
        if c == 'n':
            return self.parse_word("null", None)
        if c == '-' or '0' <= c <= '9':
            return self.parse_number()
        raise self.fail(f"unexpected character '{c}'")

    def parse_word(self, word, value):
        for w in word:
            if not self.eat(w):
                raise self.fail(f"invalid literal, expected '{word}'")
        return value

    def take_digits(self, out):
        start = self.pos
        while self.peek() is not None:
            c = self.peek()
            if c < '0' or c > '9':
                break
            out.append(c)
            self.pos += 1
        if self.pos == start:
            raise self.fail("expected digit")

    def parse_number(self):
        text = []
        if self.eat('-'):
            text.append('-')
        self.take_digits(text)
        if self.eat('.'):
            text.append('.')
            self.take_digits(text)
        if self.eat('e') or self.eat('E'):
            text.append('e')
            if self.eat('+'):
                text.append('+')
            elif self.eat('-'):
                text.append('-')
            self.take_digits(text)
        return Num(''.join(text))

    def parse_unicode_escape(self, out):
        code = 0
        for _ in range(4):
            c = self.peek()
            d = hex_val(c) if c is not None else None
            if d is None:
                raise self.fail("bad \\u escape")
            code = code * 16 + d
            self.pos += 1
        if 0xD800 <= code <= 0xDFFF:
            raise self.fail("invalid code point")
        out.append(chr(code))

    def parse_string(self):
        self.expect('"')
        out = []
        while True:
            c = self.peek()
            if c is None:
                raise self.fail("unterminated string")
            self.pos += 1
            if c == '"':
                break
            if c != '\\':
                if ord(c) < 32:
                    raise self.fail("control character in string")
                out.append(c)
                continue
            esc = self.peek()
            if esc is None:
                raise self.fail("unterminated escape")
            self.pos += 1
            if esc in ('"', '\\', '/'):
                out.append(esc)
            elif esc == 'n':
                out.append('\n')
            elif esc == 't':
                out.append('\t')
            elif esc == 'r':
                out.append('\r')
            elif esc == 'u':
                self.parse_unicode_escape(out)
            else:
                raise self.fail(f"unknown escape '\\{esc}'")
        return ''.join(out)

    def parse_array(self):
        self.expect('[')
        items = []
        self.skip_ws()
        if self.eat(']'):
            return items
        while True:
            items.append(self.parse_value())
            self.skip_ws()
            if self.eat(']'):
                break
            if not self.eat(','):
                raise self.fail("expected ',' or ']'")
        return items

    def parse_object(self):
        self.expect('{')
        fields = {}
        self.skip_ws()
        if self.eat('}'):
            return fields
        while True:
            self.skip_ws()
            if self.peek() != '"':
                raise self.fail("expected string key")
            key_pos = self.pos
            key = self.parse_string()
            self.skip_ws()
            self.expect(':')
            value = self.parse_value()
            if key in fields:
                raise ParseError(key_pos, f"duplicate key '{key}'")
            fields[key] = value
            self.skip_ws()
            if self.eat('}'):
                break
            if not self.eat(','):
                raise self.fail("expected ',' or '}'")
        return fields


def parse_document(text):
    p = JsonParser(text)
    value = p.parse_value()
    p.skip_ws()
    if p.pos != len(p.chars):
        raise p.fail("trailing characters")
    return value


HEX = "0123456789abcdef"


def write_string(s, out):
    out.append('"')
    for c in s:
        if c == '"':
            out.append('\\"')
        elif c == '\\':
            out.append('\\\\')
        elif c == '\n':
            out.append('\\n')
        elif c == '\t':
            out.append('\\t')
        elif c == '\r':
            out.append('\\r')
        elif ord(c) < 32:
            out.append("\\u00" + HEX[ord(c) // 16] + HEX[ord(c) % 16])
        else:
            out.append(c)
    out.append('"')


def write_json(v, out):
    if v is None:
        out.append("null")
    elif v is True:
        out.append("true")
    elif v is False:
        out.append("false")
    elif isinstance(v, Num):
        out.append(v.text)
    elif isinstance(v, str):
        write_string(v, out)
    elif isinstance(v, list):
        out.append('[')
        for i, item in enumerate(v):
            if i > 0:
                out.append(',')
            write_json(item, out)
        out.append(']')
    else:
        out.append('{')
        for i, k in enumerate(sorted(v)):
            if i > 0:
                out.append(',')
            write_string(k, out)
            out.append(':')
            write_json(v[k], out)
        out.append('}')


def to_json(v):
    out = []
    write_json(v, out)
    return ''.join(out)


STATUS_CODES = {"A": "active", "S": "suspended", "D": "deleted", "P": "pending"}
I64_MIN, I64_MAX = -(1 << 63), (1 << 63) - 1


def parse_i64(text):
    if not re.fullmatch(r"-?[0-9]+", text):
        return None
    n = int(text)
    return n if I64_MIN <= n <= I64_MAX else None


def optional_string(obj, key, errors):
    if key not in obj or obj[key] is None:
        return None
    v = obj[key]
    if isinstance(v, str):
        return v
    errors.append(f"field '{key}' must be a string")
    return None


def split_address(raw):
    parts = raw.split(",")
    if len(parts) < 3 or len(parts) > 4:
        return None, f"field 'addr' must have 3 or 4 comma-separated parts, got {len(parts)}"
    addr = {
        "street": parts[0].strip(),
        "city": parts[1].strip(),
        "postcode": parts[2].strip().upper(),
        "country": parts[3].strip().upper() if len(parts) == 4 else "GB",
    }
    return addr, None


def migrate(record):
    if isinstance(record, dict):
        return transform(record)
    return None, ["record is not an object"]


def transform(obj):
    known = ["id", "full_name", "email", "phone", "addr", "status", "tags", "age", "vip"]
    errors = []
    out = {"schema": Num("2")}
    if "id" not in obj:
        errors.append("missing required field 'id'")
    else:
        v = obj["id"]
        n = parse_i64(v.text) if isinstance(v, Num) else None
        if n is not None and n > 0:
            out["user_id"] = Num(str(n))
        else:
            errors.append("field 'id' must be a positive integer")
    if "full_name" not in obj:
        errors.append("missing required field 'full_name'")
    else:
        v = obj["full_name"]
        if isinstance(v, str) and len(v.strip()) > 0:
            out["display_name"] = v.strip()
        else:
            errors.append("field 'full_name' must be a non-empty string")
    email = optional_string(obj, "email", errors)
    phone = optional_string(obj, "phone", errors)
    out["contact"] = {
        "email": email.lower() if email is not None else None,
        "phone": phone,
    }
    raw = optional_string(obj, "addr", errors)
    if raw is not None:
        addr, err = split_address(raw)
        if err is None:
            out["address"] = addr
        else:
            errors.append(err)
    else:
        out["address"] = None
    code = optional_string(obj, "status", errors)
    if code is not None:
        label = STATUS_CODES.get(code.strip().upper())
        if label is not None:
            out["status"] = label
        else:
            errors.append(f"unknown status code '{code}'")
    else:
        out["status"] = "pending"
    tags = set()
    if "tags" in obj:
        v = obj["tags"]
        if isinstance(v, list):
            for i, item in enumerate(v):
                if isinstance(item, str):
                    tags.add(item.strip().lower())
                else:
                    errors.append(f"tags[{i}] must be a string")
        else:
            errors.append("field 'tags' must be an array")
    out["tags"] = sorted(tags)
    if "age" not in obj or obj["age"] is None:
        out["age"] = None
    else:
        v = obj["age"]
        if isinstance(v, Num):
            n = parse_i64(v.text)
            if n is not None and 0 <= n <= 150:
                out["age"] = Num(str(n))
            else:
                errors.append(f"field 'age' must be an integer in 0..150, got {v.text}")
        else:
            errors.append("field 'age' must be a number")
    vip = False
    if "vip" in obj:
        v = obj["vip"]
        if isinstance(v, bool):
            vip = v
        else:
            errors.append("field 'vip' must be a boolean")
    out["tier"] = "gold" if vip else "standard"
    legacy = {k: v for k, v in obj.items() if k not in known}
    if legacy:
        out["legacy"] = legacy
    if errors:
        return None, errors
    return out, None


V1_RECORDS = [
    "{\"id\": 1, \"full_name\": \"Ada Lovelace\", \"email\": \"ADA@Example.COM\", \"addr\": \"12 St James Sq, London, sw1y 4jh\", \"status\": \"A\", \"tags\": [\"Math\", \"poetry\", \"math \"], \"age\": 36, \"vip\": true}",
    "{\"id\": 2, \"full_name\": \"Zo\\u00eb \\\"Zed\\\" Brandt\", \"phone\": \"+44 20 7946 0018\", \"addr\": \"4 Rue de Rivoli, Paris, 75004, fr\", \"status\": \"s\", \"signup_src\": \"referral\", \"notes\": \"line1\\nline2 \\/ \\u0001 caf\u00e9\"}",
    "{ \"id\": 3, \"full_name\": \"Grace Hopper\", \"email\": \"grace@navy.mil\", \"status\": \"P\", \"age\": 85, \"tags\": [] }",
    "{\"id\": -7, \"full_name\": \"  \", \"status\": \"X\", \"age\": 36.5, \"tags\": [\"ok\", 42], \"addr\": \"Nowhere\", \"vip\": \"yes\"}",
    "{\"id\": 5, \"full_name\": \"Linus T\", \"addr\": \"1 Main St, Helsinki 00100\"",
    "{\"id\": 6, \"full_name\": \"Ken\\tThompson\", \"email\": \"ken@bell-labs.com\", \"addr\": \"Murray Hill, NJ, 07974, us\", \"vip\": false, \"status\": \"D\", \"age\": 79, \"extra\": {\"z\": [true, false, null], \"a\": -0.5E-3, \"score\": 1.5e+3}}",
    "[1, 2, 3]",
    "{\"id\": 8, \"full_name\": \"Edsger\", \"status\": \"a\", \"email\": 12}",
    "{\"id\": 9 \"full_name\": \"x\"}",
    "{\"id\": 10, \"full_name\": \"Barbara Liskov\", \"status\": \"A\", \"addr\": \"  MIT ,Cambridge,02139  \", \"tags\": [\"CS\", \"cs\", \"Theory\"], \"age\": null}",
    "{\"id\": 11} x",
    "{\"id\": 12, \"full_name\": \"Dup\", \"id\": 13}",
    "{\"id\": 14, \"full_name\": \"Bad Escape \\q\"}",
    "{\"id\": 15, \"full_name\": \"Nul\", \"tags\": [tru]}",
]


def main():
    ok = invalid = unparseable = 0
    for i, raw in enumerate(V1_RECORDS):
        n = i + 1
        try:
            doc = parse_document(raw)
        except ParseError as e:
            print(f"record {n}: parse error at {e.pos}: {e.msg}")
            unparseable += 1
            continue
        v2, errors = migrate(doc)
        if errors is None:
            print(f"record {n}: ok {to_json(v2)}")
            ok += 1
        else:
            print(f"record {n}: {len(errors)} transform error(s)")
            for e in errors:
                print(f"  - {e}")
            invalid += 1
    print(f"summary: {ok} ok, {invalid} invalid, {unparseable} unparseable")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    main()
