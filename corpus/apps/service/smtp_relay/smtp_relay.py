# Oracle for smtp_relay.kara: an SMTP relay (RFC 5321) with the downstream
# mail exchangers it delivers to, written with socket, base64 and threads.
# Every socket binds 127.0.0.1 port 0; a lab table maps each exchanger's
# documentation address (192.0.2.x) to the socket it got.
# Run: python3 -I -B smtp_relay.py > expected.out
import base64
import socket
import threading

LOCAL_DOMAIN = "relay.test"
MAX_SIZE = 10240
RETRY = [300, 900, 1800, 3600]
MAX_AGE = 5400
USERS = {"alice": "secret"}
LOCAL_BOXES = ["alice", "carol"]

# domain -> [(preference, exchanger name, address)]
MX = {
    "example.org": [(10, "mx1.example.org", "192.0.2.10")],
    "flaky.test": [(20, "mx2.flaky.test", "192.0.2.21"), (10, "mx1.flaky.test", "192.0.2.20")],
    "strict.test": [(10, "mx.strict.test", "192.0.2.30")],
    "slow.test": [(10, "mx.slow.test", "192.0.2.40")],
    "nowhere.test": [(10, "mx.nowhere.test", "192.0.2.99")],
    "big.test": [(10, "mx.big.test", "192.0.2.50")],
}


def crlf_lines(data):
    return data.split("\r\n")


# ------------------------------------------------------------ line I/O

class Conn:
    def __init__(self, sock):
        self.sock = sock
        self.buf = b""

    def line(self):
        while b"\r\n" not in self.buf:
            chunk = self.sock.recv(4096)
            if not chunk:
                return None
            self.buf += chunk
        line, self.buf = self.buf.split(b"\r\n", 1)
        return line.decode()

    def send(self, text):
        self.sock.sendall(text.encode())

    def reply(self):
        """A whole reply, its lines joined by ' | '; (code, text)."""
        lines = []
        while True:
            line = self.line()
            if line is None:
                return 0, "(closed)"
            lines.append(line)
            if len(line) < 4 or line[3] != "-":
                break
        return int(lines[-1][:3]), " | ".join(lines)

    def read_data(self):
        """The DATA lines up to the lone dot, unstuffed, joined by CRLF."""
        out = []
        while True:
            line = self.line()
            if line is None or line == ".":
                break
            out.append(line[1:] if line.startswith(".") else line)
        return "\r\n".join(out)


def stuff(data):
    return "".join(("." + l if l.startswith(".") else l) + "\r\n" for l in crlf_lines(data)) + ".\r\n"


def addr_of(arg, prefix):
    """`FROM:<a@b> SIZE=9` -> ('a@b', {'SIZE': '9'}), or None."""
    if not arg.upper().startswith(prefix):
        return None
    rest = arg[len(prefix):].strip()
    if not rest.startswith("<") or ">" not in rest:
        return None
    addr = rest[1:rest.index(">")]
    params = {}
    for p in rest[rest.index(">") + 1:].split():
        k, _, v = p.partition("=")
        params[k.upper()] = v
    return addr, params


def domain_of(addr):
    return addr.rsplit("@", 1)[1].lower() if "@" in addr else ""


# ------------------------------------------------------------ downstream exchangers

class Exchanger:
    def __init__(self, name, kind):
        self.name = name
        self.kind = kind
        self.connections = 0
        self.accepted = 0
        self.inbox = []  # (from, [rcpts], data)
        self.size_limit = 1000 if kind == "big" else 0

    def session(self, conn):
        self.connections += 1
        if self.kind == "busy":
            conn.send(f"421 4.3.2 {self.name} busy, try another\r\n")
            line = conn.line()
            if line is not None and line.upper() == "STOP":
                self.connections -= 1  # the shutdown connection is not a delivery
                return "stop"
            return None
        conn.send(f"220 {self.name} ESMTP\r\n")
        sender = None
        rcpts = []
        while True:
            line = conn.line()
            if line is None:
                return
            verb, _, arg = line.partition(" ")
            verb = verb.upper()
            if verb == "STOP":
                self.connections -= 1
                return "stop"
            if verb == "EHLO":
                ext = [f"250-{self.name}", "250-8BITMIME"]
                if self.size_limit:
                    ext.append(f"250-SIZE {self.size_limit}")
                ext.append("250 PIPELINING")
                conn.send("\r\n".join(ext) + "\r\n")
            elif verb == "MAIL":
                parsed = addr_of(arg, "FROM:")
                size = int(parsed[1].get("SIZE", "0"))
                if self.size_limit and size > self.size_limit:
                    conn.send(f"552 5.3.4 message size {size} exceeds fixed maximum {self.size_limit}\r\n")
                    continue
                sender, rcpts = parsed[0], []
                conn.send("250 2.1.0 ok\r\n")
            elif verb == "RCPT":
                rcpt = addr_of(arg, "TO:")[0]
                if self.kind == "strict" and not rcpt.startswith("frank@"):
                    conn.send(f"550 5.1.1 <{rcpt}>: user unknown\r\n")
                elif self.kind == "greylist" and self.connections <= 2:
                    conn.send(f"451 4.7.1 <{rcpt}>: greylisted, try again later\r\n")
                else:
                    rcpts.append(rcpt)
                    conn.send("250 2.1.5 ok\r\n")
            elif verb == "DATA":
                conn.send("354 end data with <CR><LF>.<CR><LF>\r\n")
                data = conn.read_data()
                self.accepted += 1
                self.inbox.append((sender, rcpts, data))
                conn.send(f"250 2.0.0 ok: queued as {self.name.split('.')[0].upper()}-{self.accepted}\r\n")
            elif verb == "QUIT":
                conn.send("221 2.0.0 bye\r\n")
                return
            else:
                conn.send("502 5.5.2 unknown command\r\n")


def serve_exchanger(listener, mx):
    while True:
        sock, _ = listener.accept()
        outcome = mx.session(Conn(sock))
        sock.close()
        if outcome == "stop":
            break
    listener.close()


# ------------------------------------------------------------ the relay

class Recipient:
    def __init__(self, addr):
        self.addr = addr
        self.status = "pending"
        self.attempts = 0
        self.next = 0
        self.last = ""


class Message:
    def __init__(self, qid, sender, rcpts, data, at):
        self.qid = qid
        self.sender = sender
        self.rcpts = [Recipient(r) for r in rcpts]
        self.data = data
        self.queued_at = at


class Relay:
    def __init__(self, lab):
        self.lab = lab
        self.now = 0
        self.queue = []
        self.next_qid = 1
        self.boxes = {b: [] for b in LOCAL_BOXES}
        self.log = []

    def deliver_local(self, qid, sender, rcpt, data):
        box = rcpt.split("@")[0].lower()
        self.boxes[box].append((qid, sender, data))
        self.log.append(f"{qid} to=<{rcpt}> relay=local status=sent (delivered to mailbox)")

    def accept(self, helo, user, sender, rcpts, data):
        qid = f"Q{self.next_qid}"
        self.next_qid += 1
        via = f" (authenticated as {user})" if user else ""
        stamped = f"Received: from {helo}{via}\r\n\tby {LOCAL_DOMAIN} with ESMTP id {qid}; t={self.now}\r\n" + data
        remote = []
        for r in rcpts:
            if domain_of(r) == LOCAL_DOMAIN:
                self.deliver_local(qid, sender, r, stamped)
            else:
                remote.append(r)
        if remote:
            self.queue.append(Message(qid, sender, remote, stamped, self.now))
        return qid

    # -- the client side

    def attempt(self, msg, domain, rcpts):
        """Delivers rcpts of one domain; returns (relay label, {addr: (code, text)})."""
        size = len(msg.data.encode())
        last = None
        for _pref, name, ip in sorted(MX[domain]):
            label = f"{name}[{ip}]"
            try:
                sock = socket.create_connection(self.lab[ip])
            except OSError:
                last = (label, 421, "4.4.1 connection refused")
                continue
            conn = Conn(sock)
            code, text = conn.reply()
            if code != 220:
                sock.close()
                last = (label, code, text)
                continue
            conn.send(f"EHLO {LOCAL_DOMAIN}\r\n")
            conn.reply()
            conn.send(f"MAIL FROM:<{msg.sender}> SIZE={size}\r\n")
            code, text = conn.reply()
            results = {}
            if code != 250:
                for r in rcpts:
                    results[r.addr] = (code, text)
            else:
                ok = []
                for r in rcpts:
                    conn.send(f"RCPT TO:<{r.addr}>\r\n")
                    c, t = conn.reply()
                    results[r.addr] = (c, t)
                    if c == 250:
                        ok.append(r)
                if ok:
                    conn.send("DATA\r\n")
                    conn.reply()
                    conn.send(stuff(msg.data))
                    c, t = conn.reply()
                    for r in ok:
                        results[r.addr] = (c, t)
            conn.send("QUIT\r\n")
            conn.reply()
            sock.close()
            return label, results
        label, code, text = last
        return label, {r.addr: (code, text) for r in rcpts}

    def run_queue(self):
        sent = deferred = bounced = 0
        for msg in list(self.queue):
            due = [r for r in msg.rcpts if r.status == "pending" and r.next <= self.now]
            failed = []
            domains = []
            for r in due:
                if domain_of(r.addr) not in domains:
                    domains.append(domain_of(r.addr))
            for d in domains:
                group = [r for r in due if domain_of(r.addr) == d]
                if self.now - msg.queued_at >= MAX_AGE:
                    for r in group:
                        r.status = "bounced"
                        r.last = f"delivery time expired after {r.attempts} attempts: {r.last}"
                        self.log.append(f"{msg.qid} to=<{r.addr}> status=expired ({r.last})")
                        failed.append(r)
                        bounced += 1
                    continue
                label, results = self.attempt(msg, d, group)
                for r in group:
                    code, text = results[r.addr]
                    r.attempts += 1
                    r.last = text
                    delay = self.now - msg.queued_at
                    if code == 250:
                        r.status = "sent"
                        sent += 1
                    elif code >= 500:
                        r.status = "bounced"
                        failed.append(r)
                        bounced += 1
                    else:
                        r.next = self.now + RETRY[min(r.attempts, len(RETRY)) - 1]
                        deferred += 1
                    note = f", retry at {r.next}" if r.status == "pending" else ""
                    self.log.append(f"{msg.qid} to=<{r.addr}> relay={label} delay={delay} "
                                    f"status={'deferred' if r.status == 'pending' else r.status} ({text}){note}")
            if failed and msg.sender:
                self.bounce(msg, failed)
            if all(r.status != "pending" for r in msg.rcpts):
                self.queue.remove(msg)
        return f"250 2.0.0 queue run at t={self.now}: {sent} sent, {deferred} deferred, {bounced} bounced, {len(self.queue)} queued"

    def bounce(self, msg, failed):
        lines = [
            f"From: MAILER-DAEMON@{LOCAL_DOMAIN}",
            f"To: {msg.sender}",
            "Subject: Undelivered Mail Returned to Sender",
            f"X-Original-Queue-Id: {msg.qid}",
            "",
            f"This is the mail system at {LOCAL_DOMAIN}.",
            "Your message could not be delivered to:",
            "",
        ]
        for r in failed:
            lines.append(f"<{r.addr}>: {r.last}")
        lines.append("")
        lines.append("--- original headers ---")
        for l in crlf_lines(msg.data):
            if l == "":
                break
            lines.append(l)
        qid = f"Q{self.next_qid}"
        self.next_qid += 1
        if domain_of(msg.sender) == LOCAL_DOMAIN:
            self.deliver_local(qid, "", msg.sender, "\r\n".join(lines))

    # -- the server side

    def session(self, conn):
        conn.send(f"220 {LOCAL_DOMAIN} ESMTP relay\r\n")
        helo = None
        user = None
        sender = None
        rcpts = []
        while True:
            line = conn.line()
            if line is None:
                return
            verb, _, arg = line.partition(" ")
            verb = verb.upper()
            if verb == "EHLO":
                helo = arg.strip()
                sender, rcpts = None, []
                conn.send(f"250-{LOCAL_DOMAIN} hello {helo}\r\n250-PIPELINING\r\n"
                          f"250-SIZE {MAX_SIZE}\r\n250-8BITMIME\r\n250 AUTH PLAIN\r\n")
            elif verb == "AUTH":
                mech, _, b64 = arg.partition(" ")
                if helo is None:
                    conn.send("503 5.5.1 EHLO first\r\n")
                elif mech.upper() != "PLAIN":
                    conn.send("504 5.5.4 unrecognized authentication type\r\n")
                else:
                    try:
                        parts = base64.b64decode(b64, validate=True).decode().split("\0")
                    except Exception:
                        parts = []
                    if len(parts) != 3:
                        conn.send("501 5.5.2 cannot decode response\r\n")
                    elif USERS.get(parts[1]) == parts[2]:
                        user = parts[1]
                        conn.send("235 2.7.0 authentication successful\r\n")
                    else:
                        conn.send("535 5.7.8 authentication failed\r\n")
            elif verb == "MAIL":
                parsed = addr_of(arg, "FROM:")
                if helo is None:
                    conn.send("503 5.5.1 EHLO first\r\n")
                elif parsed is None:
                    conn.send("501 5.5.4 syntax: MAIL FROM:<address>\r\n")
                elif int(parsed[1].get("SIZE", "0")) > MAX_SIZE:
                    conn.send(f"552 5.3.4 message size exceeds fixed maximum {MAX_SIZE}\r\n")
                else:
                    sender, rcpts = parsed[0], []
                    conn.send("250 2.1.0 ok\r\n")
            elif verb == "RCPT":
                parsed = addr_of(arg, "TO:")
                if sender is None:
                    conn.send("503 5.5.1 need MAIL first\r\n")
                elif parsed is None:
                    conn.send("501 5.5.4 syntax: RCPT TO:<address>\r\n")
                else:
                    rcpt = parsed[0]
                    d = domain_of(rcpt)
                    if d == LOCAL_DOMAIN and rcpt.split("@")[0].lower() not in self.boxes:
                        conn.send(f"550 5.1.1 <{rcpt}>: no such mailbox\r\n")
                    elif d != LOCAL_DOMAIN and user is None:
                        conn.send(f"554 5.7.1 <{rcpt}>: relay access denied\r\n")
                    elif d != LOCAL_DOMAIN and d not in MX:
                        conn.send(f"550 5.1.2 <{rcpt}>: no mail exchanger for {d}\r\n")
                    else:
                        rcpts.append(rcpt)
                        conn.send("250 2.1.5 ok\r\n")
            elif verb == "DATA":
                if not rcpts:
                    conn.send("554 5.5.1 no valid recipients\r\n")
                    continue
                conn.send("354 end data with <CR><LF>.<CR><LF>\r\n")
                data = conn.read_data()
                if len(data.encode()) > MAX_SIZE:
                    conn.send("552 5.3.4 message too big\r\n")
                else:
                    qid = self.accept(helo, user, sender, rcpts, data)
                    conn.send(f"250 2.0.0 ok: queued as {qid}\r\n")
                sender, rcpts = None, []
            elif verb == "RSET":
                sender, rcpts = None, []
                conn.send("250 2.0.0 ok\r\n")
            elif verb == "NOOP":
                conn.send("250 2.0.0 ok\r\n")
            elif verb == "XRUN":
                self.now = int(arg)
                conn.send(self.run_queue() + "\r\n")
            elif verb == "QUIT":
                conn.send("221 2.0.0 bye\r\n")
                return
            elif verb == "XSTOP":
                conn.send("221 2.0.0 stopping\r\n")
                return "stop"
            else:
                conn.send("502 5.5.2 unknown command\r\n")


def serve_relay(listener, relay):
    while True:
        sock, _ = listener.accept()
        outcome = relay.session(Conn(sock))
        sock.close()
        if outcome == "stop":
            break
    listener.close()


# ------------------------------------------------------------ the client

class Client:
    def __init__(self, name, addr):
        self.name = name
        self.sock = socket.create_connection(addr)
        self.conn = Conn(self.sock)
        print(f"{name} S: {self.conn.reply()[1]}")

    def cmd(self, line):
        self.conn.send(line + "\r\n")
        print(f"{self.name} C: {line}")
        print(f"{self.name} S: {self.conn.reply()[1]}")

    def pipeline(self, lines):
        self.conn.send("".join(l + "\r\n" for l in lines))
        for l in lines:
            print(f"{self.name} C: {l}")
            print(f"{self.name} S: {self.conn.reply()[1]}")

    def data(self, text):
        self.conn.send(stuff(text))
        print(f"{self.name} C: ({len(crlf_lines(text))} lines of data)")
        print(f"{self.name} S: {self.conn.reply()[1]}")

    def close(self):
        self.cmd("QUIT")
        self.sock.close()


def plain(user, password):
    return base64.b64encode(f"\0{user}\0{password}".encode()).decode()


def message(subject, to, body_lines):
    head = ["From: alice@relay.test", f"To: {to}", f"Subject: {subject}", ""]
    return "\r\n".join(head + body_lines)


def show_mail(prefix, data):
    for l in crlf_lines(data):
        print(f"{prefix}| {l}")


def main():
    lab = {}
    threads = []
    exchangers = []
    kinds = {"mx1.example.org": "plain", "mx1.flaky.test": "busy", "mx2.flaky.test": "plain",
             "mx.strict.test": "strict", "mx.slow.test": "greylist", "mx.big.test": "big"}
    for domain in MX:
        for _pref, name, ip in MX[domain]:
            listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            listener.bind(("127.0.0.1", 0))
            lab[ip] = listener.getsockname()
            if name == "mx.nowhere.test":
                listener.close()  # nothing listens there: connection refused
                continue
            listener.listen(4)
            mx = Exchanger(name, kinds[name])
            exchangers.append((ip, mx))
            threads.append(threading.Thread(target=serve_exchanger, args=(listener, mx)))
    relay = Relay(lab)
    rl = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    rl.bind(("127.0.0.1", 0))
    rl.listen(4)
    threads.append(threading.Thread(target=serve_relay, args=(rl, relay)))
    for t in threads:
        t.start()
    raddr = rl.getsockname()

    print("== session 1: alice relays")
    a = Client("alice", raddr)
    a.cmd("MAIL FROM:<alice@relay.test>")
    a.cmd("EHLO client.test")
    a.cmd("MAIL FROM:<alice@relay.test>")
    a.cmd("RCPT TO:<bob@example.org>")
    a.cmd("RCPT TO:<zed@relay.test>")
    a.cmd("RSET")
    a.cmd("AUTH PLAIN " + plain("alice", "wrong"))
    a.cmd("AUTH PLAIN !!notbase64")
    a.cmd("AUTH LOGIN")
    a.cmd("AUTH PLAIN " + plain("alice", "secret"))
    body = ["Hello all,", "", ".a line that starts with a dot", "..and one with two", "", "-- alice"]
    rcpts = ["bob@example.org", "dan@example.org", "erin@flaky.test", "frank@strict.test",
             "nobody@strict.test", "gina@slow.test", "hal@nowhere.test", "carol@relay.test",
             "ike@unknown.test"]
    text = message("Quarterly numbers", ", ".join(rcpts[:3]), body)
    a.pipeline([f"MAIL FROM:<alice@relay.test> SIZE={len(text)}"] + [f"RCPT TO:<{r}>" for r in rcpts] + ["DATA"])
    a.data(text)
    big = message("Attachment", "ivy@big.test", ["x" * 70 for _ in range(20)])
    a.pipeline(["MAIL FROM:<alice@relay.test>", "RCPT TO:<ivy@big.test>", "DATA"])
    a.data(big)
    a.cmd("MAIL FROM:<alice@relay.test> SIZE=20000")
    for t in (0, 300, 1200, 3000, 6600):
        a.cmd(f"XRUN {t}")
    a.close()

    print("== session 2: a stranger")
    s = Client("stranger", raddr)
    s.cmd("EHLO evil.test")
    s.cmd("RCPT TO:<carol@relay.test>")
    s.cmd("MAIL FROM:<spam@evil.test>")
    s.cmd("RCPT TO:<x@example.org>")
    s.cmd("DATA")
    s.cmd("RCPT TO:<carol@relay.test>")
    s.cmd("DATA")
    s.data("Subject: hi\r\n\r\nnot spam, promise")
    s.cmd("VRFY carol")
    s.close()

    for ip, _mx in exchangers:
        stop = socket.create_connection(lab[ip])
        Conn(stop).line()
        stop.sendall(b"STOP\r\n")
        stop.close()
    stop = Client("admin", raddr)
    stop.cmd("XSTOP")
    stop.sock.close()
    for t in threads:
        t.join()

    print("== relay log")
    for l in relay.log:
        print(l)
    for box in LOCAL_BOXES:
        print(f"== mailbox {box}: {len(relay.boxes[box])} messages")
        for qid, sender, data in relay.boxes[box]:
            print(f"-- {qid} from=<{sender}>")
            show_mail("  ", data)
    for _ip, mx in exchangers:
        print(f"== {mx.name}: {mx.connections} connections, {len(mx.inbox)} messages")
        for sender, rcpts, data in mx.inbox:
            print(f"-- from=<{sender}> to={','.join('<' + r + '>' for r in rcpts)}")
            show_mail("  ", data)


main()
