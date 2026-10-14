# Python mirror of smtp_sink.kara: the same SMTP state machine, the same
# client sessions, the same maildir and the same report. The server and the
# client share one thread over loopback, as in the Kara program.
import os
import shutil
import socket
import subprocess

ADDR = ("127.0.0.1", 47735)
DOMAIN = "example.test"
MAILDIR = "maildir"
MAX_RCPT = 3
MAX_SIZE = 600

CONNECTED, GREETED, IN_MAIL, IN_RCPT = range(4)


def path_arg(arg, key):
    if len(arg) < len(key) or arg[:len(key)].upper() != key:
        return None
    rest = arg[len(key):].strip()
    if not rest.startswith("<"):
        return None
    close = rest.find(">")
    if close < 0:
        return None
    addr = rest[1:close]
    if not addr or "@" not in addr:
        return None
    return addr.lower()


class Server:
    def __init__(self):
        self.phase = CONNECTED
        self.helo = ""
        self.frm, self.to = "", []
        self.next_id = 1
        self.stored = []

    def reset(self):
        self.frm, self.to = "", []
        if self.phase != CONNECTED:
            self.phase = GREETED

    def command(self, line):
        i = line.find(" ")
        if i >= 0:
            verb, arg = line[:i].upper(), line[i + 1:].strip()
        else:
            verb, arg = line.upper(), ""
        if verb in ("EHLO", "HELO"):
            if not arg:
                return "501 5.5.4 Syntax: EHLO hostname\r\n"
            self.helo = arg
            self.phase = GREETED
            self.frm, self.to = "", []
            if verb == "HELO":
                return f"250 {DOMAIN}\r\n"
            return f"250-{DOMAIN} greets {arg}\r\n250-SIZE {MAX_SIZE}\r\n250-PIPELINING\r\n250 8BITMIME\r\n"
        if verb == "MAIL":
            if self.phase == CONNECTED:
                return "503 5.5.1 Error: send HELO/EHLO first\r\n"
            if self.phase != GREETED:
                return "503 5.5.1 Error: nested MAIL command\r\n"
            addr = path_arg(arg, "FROM:")
            if addr is None:
                return "501 5.5.4 Syntax: MAIL FROM:<address>\r\n"
            self.frm = addr
            self.phase = IN_MAIL
            return "250 2.1.0 Ok\r\n"
        if verb == "RCPT":
            if self.phase not in (IN_MAIL, IN_RCPT):
                return "503 5.5.1 Error: need MAIL command\r\n"
            addr = path_arg(arg, "TO:")
            if addr is None:
                return "501 5.5.4 Syntax: RCPT TO:<address>\r\n"
            if not addr.endswith("@" + DOMAIN):
                return f"550 5.1.1 <{addr}>: Recipient address rejected: not local\r\n"
            if len(self.to) >= MAX_RCPT:
                return "452 4.5.3 Error: too many recipients\r\n"
            self.to.append(addr)
            self.phase = IN_RCPT
            return "250 2.1.5 Ok\r\n"
        if verb == "DATA":
            if self.phase != IN_RCPT:
                if self.phase == IN_MAIL:
                    return "554 5.5.1 Error: no valid recipients\r\n"
                return "503 5.5.1 Error: need RCPT command\r\n"
            return "354 End data with <CR><LF>.<CR><LF>\r\n"
        if verb == "RSET":
            self.reset()
            return "250 2.0.0 Ok\r\n"
        if verb == "NOOP":
            return "250 2.0.0 Ok\r\n"
        if verb == "VRFY":
            return "252 2.0.0 Cannot VRFY user, but will accept message\r\n"
        if verb == "QUIT":
            return "221 2.0.0 Bye\r\n"
        return "500 5.5.2 Error: command not recognized\r\n"

    def message(self, body):
        if len(body.encode()) > MAX_SIZE:
            self.reset()
            return "552 5.3.4 Error: message file too big\r\n"
        mid = "Q" + str(self.next_id).rjust(4, "0")
        self.next_id += 1
        text = f"Return-Path: <{self.frm}>\r\n"
        text += f"Received: from {self.helo} by {DOMAIN} with ESMTP id {mid}\r\n"
        text += "Delivered-To: " + ", ".join(self.to) + "\r\n"
        text += body
        with open(f"{MAILDIR}/new/{mid}.eml", "w", newline="") as f:
            f.write(text)
        self.stored.append(mid)
        self.reset()
        return f"250 2.0.0 Ok: queued as {mid}\r\n"


def serve(srv, data):
    srv.phase = CONNECTED
    srv.helo = ""
    srv.frm, srv.to = "", []
    out = f"220 {DOMAIN} ESMTP sink\r\n"
    lines = data.split("\r\n")
    i = 0
    while i < len(lines):
        line = lines[i]
        i += 1
        if line == "" and i == len(lines):
            break
        reply = srv.command(line)
        out += reply
        if reply.startswith("221"):
            break
        if reply.startswith("354"):
            body = ""
            ended = False
            while i < len(lines):
                l = lines[i]
                i += 1
                if l == ".":
                    ended = True
                    break
                if l.startswith(".."):
                    l = l[1:]
                body += l + "\r\n"
            if not ended:
                out += "451 4.3.0 Error: connection lost in DATA\r\n"
                break
            out += srv.message(body)
    return out


def read_all(sock):
    chunks = []
    while True:
        b = sock.recv(64)
        if not b:
            break
        chunks.append(b)
    return b"".join(chunks).decode()


def wire(steps):
    w = ""
    for kind, s in steps:
        if kind == "cmd":
            w += s + "\r\n"
        else:
            for l in s.split("\n"):
                if l.startswith("."):
                    w += "."
                w += l + "\r\n"
            w += ".\r\n"
    return w


def replies(text):
    out, cur = [], []
    for l in text.split("\r\n"):
        if not l:
            continue
        cur.append(l)
        if len(l) < 4 or l[3:4] == " ":
            out.append(cur)
            cur = []
    return out


def session(listener, srv, label, steps):
    print(f"== {label}")
    client = socket.create_connection(ADDR)
    client.sendall(wire(steps).encode())
    client.shutdown(socket.SHUT_WR)
    conn, _ = listener.accept()
    conn.sendall(serve(srv, read_all(conn)).encode())
    conn.close()
    rs = replies(read_all(client))
    client.close()
    print("S: " + " | ".join(rs[0]))
    k = 1
    for kind, s in steps:
        if kind == "cmd":
            print(f"C: {s}")
        else:
            print(f"C: <message, {len(s.split(chr(10)))} lines>")
        if k < len(rs):
            print("S: " + " | ".join(rs[k]))
        else:
            print("S: (no reply)")
        k += 1


def parse(text):
    hs = []
    lines = text.split("\r\n")
    i = 0
    while i < len(lines):
        l = lines[i]
        i += 1
        if l == "":
            break
        if (l.startswith(" ") or l.startswith("\t")) and hs:
            hs[-1][1] += " " + l.strip()
            continue
        c = l.find(":")
        if c >= 0:
            hs.append([l[:c].lower(), l[c + 1:].strip()])
    body = ""
    while i < len(lines):
        body += lines[i]
        if i + 1 < len(lines):
            body += "\n"
        i += 1
    return hs, body


def header(hs, name):
    for n, v in hs:
        if n == name:
            return v
    return ""


def boundary(ct):
    at = ct.find("boundary=")
    if at < 0:
        return None
    rest = ct[at + 9:]
    if rest.startswith('"'):
        end = rest[1:].find('"')
        if end < 0:
            return None
        return rest[1:end + 1]
    end = rest.find(";")
    if end < 0:
        end = len(rest)
    return rest[:end].strip()


def b64_value(c):
    if 65 <= c <= 90:
        return c - 65
    if 97 <= c <= 122:
        return c - 97 + 26
    if 48 <= c <= 57:
        return c - 48 + 52
    if c == 43:
        return 62
    if c == 47:
        return 63
    return -1


def b64_decode(text):
    out = bytearray()
    acc = bits = 0
    for c in text.encode():
        if c in (61, 10, 13, 32):
            continue
        v = b64_value(c)
        if v < 0:
            return None, f"bad base64 byte {c}"
        acc = (acc << 6) | v
        bits += 6
        if bits >= 8:
            bits -= 8
            out.append((acc >> bits) & 255)
    return bytes(out), None


def report_parts(body, bnd):
    opn, close = f"--{bnd}", f"--{bnd}--"
    chunks, cur, inside = [], "", False
    for l in body.split("\n"):
        if l == close:
            if inside:
                chunks.append(cur)
            inside = False
            break
        if l == opn:
            if inside:
                chunks.append(cur)
            cur = ""
            inside = True
            continue
        if inside:
            cur += l + "\r\n"
    print(f"    {len(chunks)} parts")
    for n, c in enumerate(chunks, 1):
        hs, pbody = parse(c)
        ct = header(hs, "content-type")
        enc = header(hs, "content-transfer-encoding").lower()
        kind = ct if ct else "text/plain"
        if enc == "base64":
            data, err = b64_decode(pbody)
            if err:
                print(f"    part {n}: {kind}, base64 error: {err}")
            else:
                try:
                    s = data.decode()
                    first = s.splitlines()[0] if s.splitlines() else ""
                except UnicodeDecodeError:
                    first = "(binary)"
                print(f"    part {n}: {kind}, base64, {len(data)} bytes decoded, first line: {first}")
        else:
            t = pbody.strip()
            print(f"    part {n}: {kind}, {len(t)} chars: {t}")


def report():
    print("== maildir")
    names = sorted(os.listdir(f"{MAILDIR}/new"))
    for name in names:
        with open(f"{MAILDIR}/new/{name}", newline="") as f:
            text = f.read()
        hs, body = parse(text)
        print(f"{name} {len(text.encode())} bytes")
        print(f"    from {header(hs, 'return-path')} to {header(hs, 'delivered-to')}")
        print(f"    subject: {header(hs, 'subject')}")
        print(f"    received: {header(hs, 'received')}")
        b = boundary(header(hs, "content-type"))
        if b is not None:
            report_parts(body, b)
        else:
            ls = body.splitlines()
            print(f"    {len(ls)} body lines, last: {ls[-1]}")


def cmd(s):
    return ("cmd", s)


def body(s):
    return ("body", s)


def main():
    shutil.rmtree(MAILDIR, ignore_errors=True)
    for d in ("new", "tmp", "cur"):
        os.makedirs(f"{MAILDIR}/{d}")
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(ADDR)
    listener.listen(4)
    srv = Server()

    session(listener, srv, "pipelined delivery", [
        cmd("EHLO client.example.org"),
        cmd("MAIL FROM:<Alice@Example.org>"),
        cmd("RCPT TO:<bob@example.test>"),
        cmd("RCPT TO:<carol@example.test>"),
        cmd("DATA"),
        body("From: Alice <alice@example.org>\nTo: bob@example.test\nSubject: lunch\n  on friday?\n\nHi Bob,\n.hidden line starts with a dot\nsee you."),
        cmd("MAIL FROM: <alice@example.org>"),
        cmd("RCPT TO:<dave@example.test>"),
        cmd("DATA"),
        body("Subject: second\n\none line"),
        cmd("QUIT"),
    ])

    big = "Subject: too big\n\n" + "".join(f"line {i} of a long message body\n" for i in range(40))
    session(listener, srv, "errors and limits", [
        cmd("MAIL FROM:<x@example.org>"),
        cmd("HELO"),
        cmd("helo old.example.org"),
        cmd("RCPT TO:<bob@example.test>"),
        cmd("MAIL FROM:alice@example.org"),
        cmd("mail from:<eve@example.org>"),
        cmd("MAIL FROM:<eve@example.org>"),
        cmd("RCPT TO:<root@elsewhere.org>"),
        cmd("DATA"),
        cmd("RCPT TO:<a@example.test>"),
        cmd("RCPT TO:<b@example.test>"),
        cmd("RCPT TO:<c@example.test>"),
        cmd("RCPT TO:<d@example.test>"),
        cmd("VRFY bob"),
        cmd("NOOP"),
        cmd("STARTTLS"),
        cmd("DATA"),
        body(big),
        cmd("RSET"),
        cmd("DATA"),
        cmd("QUIT"),
        cmd("NOOP"),
    ])

    session(listener, srv, "mime message", [
        cmd("EHLO mua.example.org"),
        cmd("MAIL FROM:<carol@example.test>"),
        cmd("RCPT TO:<bob@example.test>"),
        cmd("DATA"),
        body("Subject: report attached\nMIME-Version: 1.0\nContent-Type: multipart/mixed;\n boundary=\"=_b1\"\n\nThis is a MIME message.\n--=_b1\nContent-Type: text/plain\n\nNumbers attached.\n--=_b1\nContent-Type: text/csv\nContent-Transfer-Encoding: base64\n\ncmVnaW9uLHRvdGFsCm5vcnRoLDEyMApzb3V0aCw4NQo=\n--=_b1--\n"),
        cmd("QUIT"),
    ])

    print(f"stored {len(srv.stored)}: " + " ".join(srv.stored))
    report()


main()
