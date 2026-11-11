"""Oracle for syslog_collector.kara: a syslog collector on loopback. Datagrams
on UDP and connections on TCP (RFC 6587: octet-counted or newline-ended
frames), each message parsed as RFC 5424 or, failing its version, as the BSD
format of RFC 3164. A thread reads the UDP socket, and each TCP connection
gets a thread of its own; records and rejects go behind one lock. Scripted
senders on the main thread, then the collector prints what it kept."""

import socket
import threading

UDP_ADDR = ("127.0.0.1", 47767)
TCP_ADDR = ("127.0.0.1", 47768)
DATAGRAMS = 4
CONNECTIONS = 3
MAX_FRAME = 8192

FACILITIES = ["kern", "user", "mail", "daemon", "auth", "syslog", "lpr", "news",
              "uucp", "cron", "authpriv", "ftp", "ntp", "audit", "alert", "clock",
              "local0", "local1", "local2", "local3", "local4", "local5", "local6",
              "local7"]
SEVERITIES = ["emerg", "alert", "crit", "err", "warning", "notice", "info", "debug"]
MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct",
          "Nov", "Dec"]


class Reject(Exception):
    pass


def take_field(rest):
    """The text up to the next space, and what follows the space."""
    at = rest.find(" ")
    if at < 0:
        raise Reject("truncated")
    if at == 0:
        raise Reject("empty field")
    return rest[:at], rest[at + 1:]


def parse_pri(text):
    if not text.startswith("<"):
        raise Reject("no pri")
    end = text.find(">")
    if end < 2 or end > 4:
        raise Reject("bad pri")
    digits = text[1:end]
    if not digits.isdigit() or (len(digits) > 1 and digits[0] == "0"):
        raise Reject("bad pri")
    pri = int(digits)
    if pri > 191:
        raise Reject("bad pri")
    return pri, text[end + 1:]


def parse_sd(rest):
    """([(id, [(name, value)])], what follows the structured data)."""
    if rest.startswith("-"):
        return [], rest[1:]
    if not rest.startswith("["):
        raise Reject("bad structured data")
    elements = []
    while rest.startswith("["):
        i = 1
        while i < len(rest) and rest[i] not in " ]":
            i += 1
        if i == 1 or i == len(rest):
            raise Reject("bad structured data")
        sd_id = rest[1:i]
        params = []
        while rest[i] == " ":
            j = i + 1
            while j < len(rest) and rest[j] != "=":
                j += 1
            if j == i + 1 or j + 1 >= len(rest) or rest[j + 1] != '"':
                raise Reject("bad structured data")
            name = rest[i + 1:j]
            k = j + 2
            value = ""
            while True:
                if k >= len(rest):
                    raise Reject("bad structured data")
                c = rest[k]
                if c == "\\" and k + 1 < len(rest) and rest[k + 1] in '"\\]':
                    value += rest[k + 1]
                    k += 2
                elif c == '"':
                    k += 1
                    break
                else:
                    value += c
                    k += 1
            params.append((name, value))
            i = k
            if i >= len(rest):
                raise Reject("bad structured data")
        if rest[i] != "]":
            raise Reject("bad structured data")
        elements.append((sd_id, params))
        rest = rest[i + 1:]
    return elements, rest


def parse_5424(pri, rest):
    rest = rest[2:]
    stamp, rest = take_field(rest)
    if stamp != "-" and (len(stamp) < 20 or stamp[10] != "T"):
        raise Reject("bad timestamp")
    host, rest = take_field(rest)
    app, rest = take_field(rest)
    pid, rest = take_field(rest)
    msgid, rest = take_field(rest)
    sd, rest = parse_sd(rest)
    if rest == "":
        msg = ""
    elif rest.startswith(" "):
        msg = rest[1:]
    else:
        raise Reject("bad structured data")
    if msg.startswith("﻿"):
        msg = msg[1:]
    return dict(pri=pri, version=1, stamp=stamp, host=host, app=app, pid=pid,
                msgid=msgid, sd=sd, msg=msg)


def parse_3164(pri, rest):
    if len(rest) < 16 or rest[:3] not in MONTHS or rest[15] != " ":
        raise Reject("bad timestamp")
    stamp = rest[:15]
    host, rest = take_field(rest[16:])
    colon = rest.find(":")
    if colon <= 0:
        raise Reject("bad tag")
    tag = rest[:colon]
    pid = "-"
    if tag.endswith("]") and "[" in tag:
        at = tag.index("[")
        pid = tag[at + 1:-1]
        tag = tag[:at]
    msg = rest[colon + 1:]
    if msg.startswith(" "):
        msg = msg[1:]
    return dict(pri=pri, version=0, stamp=stamp, host=host, app=tag, pid=pid,
                msgid="-", sd=[], msg=msg)


def parse(frame):
    try:
        text = frame.decode("utf-8")
    except UnicodeDecodeError:
        raise Reject("bad utf-8")
    pri, rest = parse_pri(text)
    if rest.startswith("1 "):
        return parse_5424(pri, rest)
    return parse_3164(pri, rest)


def show_sd(sd):
    if not sd:
        return "-"
    out = ""
    for sd_id, params in sd:
        out += "[" + sd_id
        for name, value in params:
            out += f" {name}={value}"
        out += "]"
    return out


class Collector:
    def __init__(self):
        self.lock = threading.Lock()
        self.records = []
        self.rejects = []
        self.frames = {}

    def take(self, transport, frame):
        try:
            rec = parse(frame)
        except Reject as e:
            with self.lock:
                self.rejects.append(f"{transport}: {e}")
            return
        with self.lock:
            self.records.append(rec)
            self.frames[transport] = self.frames.get(transport, 0) + 1

    def reject(self, transport, why):
        with self.lock:
            self.rejects.append(f"{transport}: {why}")


def next_frame(pending):
    """(frame, rest), or None when `pending` holds no whole frame yet.
    Raises Reject for a frame that can never be whole."""
    if pending[:1].isdigit():
        sp = pending.find(b" ", 0, 9)
        if sp < 0:
            if len(pending) >= 9:
                raise Reject("bad frame length")
            return None
        digits = pending[:sp]
        if not digits.isdigit():
            raise Reject("bad frame length")
        n = int(digits)
        if n > MAX_FRAME:
            raise Reject("frame too large")
        if len(pending) < sp + 1 + n:
            return None
        return pending[sp + 1:sp + 1 + n], pending[sp + 1 + n:]
    nl = pending.find(b"\n")
    if nl < 0:
        return None
    return pending[:nl], pending[nl + 1:]


def serve_conn(col, conn):
    pending = b""
    try:
        while True:
            chunk = conn.recv(64)
            if chunk:
                pending += chunk
            while pending:
                got = next_frame(pending)
                if got is None:
                    break
                frame, pending = got
                col.take("tcp", frame)
            if not chunk:
                break
        if pending:
            if pending[:1].isdigit():
                col.reject("tcp", "truncated frame")
            else:
                col.take("tcp", pending)
    except Reject as e:
        col.reject("tcp", str(e))
    conn.close()


def serve_udp(col, sock):
    for _ in range(DATAGRAMS):
        pkt, _ = sock.recvfrom(4096)
        col.take("udp", pkt)


def serve_tcp(col, listener):
    threads = []
    for _ in range(CONNECTIONS):
        conn, _ = listener.accept()
        t = threading.Thread(target=serve_conn, args=(col, conn))
        t.start()
        threads.append(t)
    for t in threads:
        t.join()


def octets(msg):
    data = msg.encode()
    return str(len(data)).encode() + b" " + data


def main():
    col = Collector()
    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.bind(UDP_ADDR)
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(TCP_ADDR)
    listener.listen(8)
    tu = threading.Thread(target=serve_udp, args=(col, udp))
    tt = threading.Thread(target=serve_tcp, args=(col, listener))
    tu.start()
    tt.start()

    sender = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    for msg in [
        '<165>1 2026-11-11T22:14:15.003Z web01 nginx 2042 ID47 '
        '[exampleSDID@32473 iut="3" eventSource="App\\"lic\\]ation" eventID="1011"]'
        '[meta seq="7"] ﻿request served',
        "<34>Nov 11 22:14:15 mymachine su[219]: 'su root' failed for lonvick",
        "<192>1 2026-11-11T22:14:16Z web01 app - - - out of range",
        "<13>1 2026-11-11T22:14:17Z web01",
    ]:
        sender.sendto(msg.encode(), UDP_ADDR)
    sender.close()

    c = socket.create_connection(TCP_ADDR)
    first = octets("<86>1 2026-11-11T22:15:00Z db01 sshd 77 LOGIN - accepted key")
    c.sendall(first + octets("<87>1 - db01 cron - - - tick")
              + octets("<11>Nov  1 03:04:05 db01 kernel: oops"))
    split = octets('<190>1 2026-11-11T22:15:01Z db01 app 9 M9 [x@1 k="v w"] split frame')
    c.sendall(split[:20])
    c.sendall(split[20:])
    c.close()

    c = socket.create_connection(TCP_ADDR)
    c.sendall(b"<14>1 2026-11-11T22:16:00Z cache01 redis 5 SAVE - saved\n"
              b"<15>1 2026-11-11T22:16:01Z cache01 redis 5 - [bad no-quote=x] oops\n"
              b"<30>1 2026-11-11T22:16:02Z cache01 redis 5 BG - last line, no newline")
    c.close()

    c = socket.create_connection(TCP_ADDR)
    c.sendall(octets("<31>1 - edge01 proxy - - - ok") + b"99999999 <31>1 too big")
    c.close()

    tu.join()
    tt.join()
    udp.close()
    listener.close()

    def key(r):
        return f"{r['host']} {r['app']} {r['msgid']} {r['pid']} {r['msg']}"

    print(f"== {len(col.records)} records")
    for r in sorted(col.records, key=key):
        fac = FACILITIES[r["pri"] // 8]
        sev = SEVERITIES[r["pri"] % 8]
        print(f"{fac}.{sev} v{r['version']} host={r['host']} app={r['app']} pid={r['pid']} "
              f"msgid={r['msgid']} time={r['stamp']} sd={show_sd(r['sd'])} msg=\"{r['msg']}\"")
    print(f"== {len(col.rejects)} rejected")
    for line in sorted(col.rejects):
        print(line)
    print("== by severity")
    counts = {}
    for r in col.records:
        name = SEVERITIES[r["pri"] % 8]
        counts[name] = counts.get(name, 0) + 1
    for name in sorted(counts):
        print(f"{name}: {counts[name]}")
    for t in sorted(col.frames):
        print(f"{t} frames kept: {col.frames[t]}")


main()
