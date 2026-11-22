#!/usr/bin/env python3
"""docs/library/io.md "HTTP client" / "HTTPS": the client's TLS against a
local test PKI, with Python's `ssl` module as the oracle.

A root, an intermediate and a set of leaf certificates are made here with
`cryptography` (so a certificate can be expired, not yet valid, signed with
SHA-1, carry a 1024-bit key, or have its signature broken). Each case is a
Python `ssl` server on 127.0.0.1 presenting one chain. The Kāra program
fetches every case's URL with `Client.new().get(..)` under
`KARAC_HTTP_EXTRA_CA=<root.pem>`; a DNS name such as `good.test` is reached
through a local CONNECT proxy (`https_proxy`), which is how io.md routes an
`https://` URL and still checks the certificate against the target host.

The oracle for each verification case is a Python client
(`ssl.create_default_context(cafile=root.pem)`, Python 3.13's defaults:
`VERIFY_X509_STRICT`, `VERIFY_X509_PARTIAL_CHAIN`) connecting to the same
server with the same `server_hostname`, with `hostname_checks_common_name`
off: RFC 9525 §6.3 and the CA/Browser Forum baseline requirements match a
host against the subjectAltName only. Kāra's verdict (accept or refuse) must
equal Python's, a refusal must be a TLS-handshake `Err` whose message names
the reason, and an accepted fetch must return the body.

Besides verification: the SNI and ALPN the client sends (read off the raw
ClientHello), the h2 and HTTP/1.1 answers to ALPN, a server capped at
TLS 1.1 (refused), `KARAC_HTTP_EXTRA_CA` naming a missing or certificate-less
file, and a close-delimited body ended with `close_notify` against one cut
off without it (a truncation attack, RFC 8446 §6.1).

    python3 conformance/tls_client.py
"""

import datetime
import ipaddress
import re
import os
import socket
import ssl
import struct
import sys
import tempfile
import threading
from pathlib import Path

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec, rsa
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import Report, kara_str, run  # noqa: E402

NOW = datetime.datetime.now(datetime.timezone.utc)
DAY = datetime.timedelta(days=1)


# ---------------------------------------------------------------- the PKI

def key_ec():
    return ec.generate_private_key(ec.SECP256R1())


def name(cn):
    return x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, cn)])


def make_cert(subject_cn, key, issuer_cn, issuer_key, *, ca=False, dns=(), ips=(), not_before=None,
              not_after=None, hash_alg=None, eku=(ExtendedKeyUsageOID.SERVER_AUTH,), bc=True, path_len=None):
    b = (x509.CertificateBuilder()
         .subject_name(name(subject_cn))
         .issuer_name(name(issuer_cn))
         .public_key(key.public_key())
         .serial_number(x509.random_serial_number())
         .not_valid_before(not_before or NOW - DAY)
         .not_valid_after(not_after or NOW + 30 * DAY))
    if bc:
        b = b.add_extension(x509.BasicConstraints(ca=ca, path_length=path_len if ca else None), critical=True)
    if ca:
        b = b.add_extension(x509.KeyUsage(digital_signature=True, content_commitment=False, key_encipherment=False,
                                          data_encipherment=False, key_agreement=False, key_cert_sign=True,
                                          crl_sign=True, encipher_only=False, decipher_only=False), critical=True)
    else:
        if eku:
            b = b.add_extension(x509.ExtendedKeyUsage(list(eku)), critical=False)
        sans = [x509.DNSName(d) for d in dns] + [x509.IPAddress(ipaddress.ip_address(i)) for i in ips]
        if sans:
            b = b.add_extension(x509.SubjectAlternativeName(sans), critical=False)
    b = b.add_extension(x509.SubjectKeyIdentifier.from_public_key(key.public_key()), critical=False)
    b = b.add_extension(x509.AuthorityKeyIdentifier.from_issuer_public_key(issuer_key.public_key()), critical=False)
    return b.sign(issuer_key, hash_alg or hashes.SHA256())


def pem(cert):
    return cert.public_bytes(serialization.Encoding.PEM)


def key_pem(key):
    return key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
                             serialization.NoEncryption())


def break_signature(cert):
    """`cert`'s DER with the last byte of its signature flipped."""
    der = bytearray(cert.public_bytes(serialization.Encoding.DER))
    der[-1] ^= 0x01
    return x509.load_der_x509_certificate(bytes(der))


class Pki:
    def __init__(self, d):
        self.d = Path(d)
        self.root_key = key_ec()
        self.root = make_cert("Conformance Root", self.root_key, "Conformance Root", self.root_key, ca=True)
        self.int_key = key_ec()
        self.inter = make_cert("Conformance Intermediate", self.int_key, "Conformance Root", self.root_key, ca=True)
        self.other_key = key_ec()
        self.other_root = make_cert("Other Root", self.other_key, "Other Root", self.other_key, ca=True)
        self.root_pem = self.d / "root.pem"
        self.root_pem.write_bytes(pem(self.root))

    def leaf(self, cn="leaf", key=None, issuer=None, **kw):
        key = key or key_ec()
        icn, ikey = issuer or ("Conformance Intermediate", self.int_key)
        return key, make_cert(cn, key, icn, ikey, **kw)

    def write(self, label, key, chain):
        cp, kp = self.d / f"{label}.chain.pem", self.d / f"{label}.key.pem"
        cp.write_bytes(b"".join(pem(c) for c in chain))
        kp.write_bytes(key_pem(key))
        return cp, kp


# ------------------------------------------------------- the ClientHello

def parse_client_hello(buf):
    """(sni, [alpn], [versions]) from the first TLS record of a ClientHello."""
    if len(buf) < 5 or buf[0] != 0x16:
        return None
    hs = buf[5:]
    if hs[0] != 1:
        return None
    p = 4 + 2 + 32
    p += 1 + hs[p]                                   # session id
    p += 2 + struct.unpack(">H", hs[p:p + 2])[0]     # cipher suites
    p += 1 + hs[p]                                   # compression
    end = p + 2 + struct.unpack(">H", hs[p:p + 2])[0]
    p += 2
    sni, alpn, versions = None, [], []
    while p + 4 <= end:
        t, n = struct.unpack(">HH", hs[p:p + 4])
        body = hs[p + 4:p + 4 + n]
        if t == 0:  # server_name
            q = 2
            while q < len(body):
                kind, ln = body[q], struct.unpack(">H", body[q + 1:q + 3])[0]
                if kind == 0:
                    sni = body[q + 3:q + 3 + ln].decode()
                q += 3 + ln
        elif t == 16:  # ALPN
            q = 2
            while q < len(body):
                ln = body[q]
                alpn.append(body[q + 1:q + 1 + ln].decode())
                q += 1 + ln
        elif t == 43:  # supported_versions
            versions = [body[1 + i:3 + i].hex() for i in range(0, body[0], 2)]
        p += 4 + n
    return sni, alpn, versions


def peek_hello(conn):
    conn.settimeout(10)
    buf = b""
    while True:
        buf = conn.recv(65536, socket.MSG_PEEK)
        if len(buf) >= 5 and len(buf) >= 5 + struct.unpack(">H", buf[3:5])[0]:
            return buf
        if not buf:
            return b""


# ------------------------------------------------------------ the servers

H2_PREFACE = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"


def h2_frame(kind, flags, stream, payload):
    return struct.pack(">I", len(payload))[1:] + bytes([kind, flags]) + struct.pack(">I", stream) + payload


class Server:
    """One TLS listener on 127.0.0.1 serving `mode` to every connection."""

    def __init__(self, chain, key, *, alpn=None, max_version=None, min_version=None, weak=False, mode="ok",
                 body=b"hello"):
        self.ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        if weak:
            self.ctx.set_ciphers("DEFAULT:@SECLEVEL=0")
        if max_version:
            self.ctx.maximum_version = max_version
        if min_version:
            self.ctx.minimum_version = min_version
        if alpn:
            self.ctx.set_alpn_protocols(alpn)
        self.ctx.load_cert_chain(chain, key)
        self.mode, self.body = mode, body
        self.hellos, self.versions, self.alpn_chosen, self.errors = [], [], [], []
        self.sock = socket.socket()
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(16)
        self.port = self.sock.getsockname()[1]
        threading.Thread(target=self.loop, daemon=True).start()

    def loop(self):
        while True:
            try:
                conn, _ = self.sock.accept()
            except OSError:
                return
            threading.Thread(target=self.one, args=(conn,), daemon=True).start()

    def one(self, conn):
        try:
            self.hellos.append(parse_client_hello(peek_hello(conn)))
            s = self.ctx.wrap_socket(conn, server_side=True)
            self.versions.append(s.version())
            self.alpn_chosen.append(s.selected_alpn_protocol())
            if s.selected_alpn_protocol() == "h2":
                self.serve_h2(s)
                return
            req = b""
            while b"\r\n\r\n" not in req:
                chunk = s.recv(4096)
                if not chunk:
                    return
                req += chunk
            if self.mode == "ok":
                s.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: %d\r\nConnection: close\r\n\r\n%s"
                          % (len(self.body), self.body))
                s = s.unwrap()
            elif self.mode == "close_notify":
                s.sendall(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n" + self.body)
                s = s.unwrap()
            elif self.mode == "truncated":
                s.sendall(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n" + self.body)
                # Close the TCP connection under TLS, with no close_notify.
                raw = socket.socket(fileno=os.dup(s.fileno()))
                raw.shutdown(socket.SHUT_RDWR)
                raw.close()
            elif self.mode == "length_truncated":
                s.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: %d\r\nConnection: close\r\n\r\n%s"
                          % (len(self.body), self.body))
                raw = socket.socket(fileno=os.dup(s.fileno()))
                raw.shutdown(socket.SHUT_RDWR)
                raw.close()
            s.close()
        except (ssl.SSLError, OSError) as e:
            self.errors.append(str(e))
            conn.close()

    def serve_h2(self, s):
        got = b""
        while len(got) < len(H2_PREFACE):
            chunk = s.recv(4096)
            if not chunk:
                return
            got += chunk
        self.h2_preface = got.startswith(H2_PREFACE)
        s.sendall(h2_frame(4, 0, 0, b"")                       # SETTINGS
                  + h2_frame(4, 1, 0, b"")                     # SETTINGS ack
                  + h2_frame(1, 4, 1, b"\x88")                 # HEADERS :status 200, END_HEADERS
                  + h2_frame(0, 1, 1, b"h2 " + self.body))     # DATA, END_STREAM
        try:
            while s.recv(4096):
                pass
        except (ssl.SSLError, OSError):
            pass
        s.close()


class Proxy:
    """A CONNECT proxy that sends every tunnel to 127.0.0.1 at the asked port."""

    def __init__(self):
        self.targets = []
        self.sock = socket.socket()
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(16)
        self.port = self.sock.getsockname()[1]
        threading.Thread(target=self.loop, daemon=True).start()

    def loop(self):
        while True:
            try:
                c, _ = self.sock.accept()
            except OSError:
                return
            threading.Thread(target=self.one, args=(c,), daemon=True).start()

    def one(self, c):
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = c.recv(1)
            if not chunk:
                c.close()
                return
            head += chunk
        target = head.split(b" ")[1].decode()
        self.targets.append(target)
        up = socket.create_connection(("127.0.0.1", int(target.rsplit(":", 1)[1])))
        c.sendall(b"HTTP/1.1 200 Connection established\r\n\r\n")

        def pump(a, b):
            try:
                while True:
                    d = a.recv(65536)
                    if not d:
                        break
                    b.sendall(d)
            except OSError:
                pass
            for x in (a, b):
                try:
                    x.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass

        threading.Thread(target=pump, args=(c, up), daemon=True).start()
        threading.Thread(target=pump, args=(up, c), daemon=True).start()


# ----------------------------------------------------------- the oracle

def python_verdict(root_pem, host, port):
    """'ok' or the Python ssl verification error for `host` at `port`."""
    ctx = ssl.create_default_context(cafile=str(root_pem))
    ctx.hostname_checks_common_name = False
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=10) as raw:
            sni = None if is_ip(host) else host
            with ctx.wrap_socket(raw, server_hostname=host if sni or is_ip(host) else None) as s:
                s.sendall(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
                while s.recv(4096):
                    pass
        return "ok"
    except ssl.SSLCertVerificationError as e:
        return "refused: " + (e.verify_message or str(e))
    except ssl.SSLError as e:
        return "refused: " + str(e)


def python_truncation(port):
    """What Python's client reads from a close-delimited body at `port`."""
    ctx = ssl.create_default_context(cafile=str(PKI.root_pem))
    raw = socket.create_connection(("127.0.0.1", port), timeout=10)
    s = ctx.wrap_socket(raw, server_hostname="127.0.0.1", suppress_ragged_eofs=False)
    s.sendall(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
    got = b""
    try:
        while True:
            d = s.recv(4096)
            if not d:
                return "ok " + got.split(b"\r\n\r\n", 1)[1].decode()
            got += d
    except ssl.SSLEOFError:
        return "truncated"
    finally:
        s.close()


def is_ip(h):
    try:
        ipaddress.ip_address(h)
        return True
    except ValueError:
        return False


# ----------------------------------------------------------- the cases

# A Rust type or variant name (`UnknownIssuer`, `OtherError`) in a message
# meant for a person.
CAMEL = re.compile(r"\b[A-Z][a-z]+[A-Z][A-Za-z]*\b")

TMP = tempfile.mkdtemp(prefix="tls-conf-")
PKI = Pki(TMP)


def verification_cases():
    """(label, host, chain, key, server kwargs, words the refusal must name)."""
    p = PKI
    out = []

    def add(label, host, key, chain, words=(), **kw):
        cp, kp = p.write(label, key, chain)
        out.append((label, host, cp, kp, kw, words))

    k, c = p.leaf(dns=["good.test"])
    add("good chain", "good.test", k, [c, p.inter])
    k, c = p.leaf(dns=["good.test"], issuer=("Conformance Root", p.root_key))
    add("leaf issued by the root", "good.test", k, [c])
    k, c = p.leaf(dns=["good.test"], not_before=NOW - 30 * DAY, not_after=NOW - DAY)
    add("expired leaf", "good.test", k, [c, p.inter], (f"it expired at {NOW - DAY:%Y-%m-%d %H:%M:%S} UTC",))
    k, c = p.leaf(dns=["good.test"], not_before=NOW + DAY, not_after=NOW + 30 * DAY)
    add("not-yet-valid leaf", "good.test", k, [c, p.inter], (f"not valid until {NOW + DAY:%Y-%m-%d %H:%M:%S} UTC",))
    old_int = make_cert("Conformance Intermediate", p.int_key, "Conformance Root", p.root_key, ca=True,
                        not_before=NOW - 30 * DAY, not_after=NOW - DAY)
    k, c = p.leaf(dns=["good.test"])
    add("expired intermediate", "good.test", k, [c, old_int], (f"it expired at {NOW - DAY:%Y-%m-%d %H:%M:%S} UTC",))
    k, c = p.leaf(dns=["other.test"])
    add("wrong hostname", "good.test", k, [c, p.inter], ("is not valid for",))
    k, c = p.leaf(cn="good.test")
    add("CN only, no SAN", "good.test", k, [c, p.inter], ("is not valid for",))
    k, c = p.leaf(cn="good.test", dns=["other.test"])
    add("CN matches but the SAN does not", "good.test", k, [c, p.inter], ("is not valid for",))
    k, c = p.leaf(dns=["*.w.test"])
    add("wildcard, one label", "a.w.test", k, [c, p.inter])
    add("wildcard, two labels", "a.b.w.test", k, [c, p.inter], ("is not valid for",))
    add("wildcard, bare domain", "w.test", k, [c, p.inter], ("is not valid for",))
    k, c = p.leaf(dns=["f*.w.test"])
    add("partial-label wildcard", "foo.w.test", k, [c, p.inter], ("is not valid for",))
    k, c = p.leaf(dns=["a.*.w.test"])
    add("wildcard not leftmost", "a.b.w.test", k, [c, p.inter], ("is not valid for",))
    k, c = p.leaf(dns=["GOOD.Test"])
    add("hostname compared without case", "good.test", k, [c, p.inter])
    k, c = p.leaf(ips=["127.0.0.1"])
    add("IP SAN matches", "127.0.0.1", k, [c, p.inter])
    k, c = p.leaf(ips=["127.0.0.2"])
    add("IP SAN differs", "127.0.0.1", k, [c, p.inter], ("is not valid for",))
    k, c = p.leaf(dns=["127.0.0.1"])
    add("IP only as a DNS SAN", "127.0.0.1", k, [c, p.inter], ("is not valid for",))
    k, c = p.leaf(cn="127.0.0.1")
    add("IP only in the CN", "127.0.0.1", k, [c, p.inter], ("is not valid for",))
    k, c = p.leaf(dns=["good.test"], ips=["127.0.0.1"])
    add("IP host against a DNS+IP cert", "127.0.0.1", k, [c, p.inter])
    sk = key_ec()
    self_signed = make_cert("good.test", sk, "good.test", sk, dns=["good.test"])
    add("self-signed leaf", "good.test", sk, [self_signed], ("trusted root",))
    other_int_key = key_ec()
    other_int = make_cert("Other Intermediate", other_int_key, "Other Root", p.other_key, ca=True)
    k, c = p.leaf(dns=["good.test"], issuer=("Other Intermediate", other_int_key))
    add("untrusted root", "good.test", k, [c, other_int, p.other_root], ("trusted root",))
    k, c = p.leaf(dns=["good.test"])
    add("incomplete chain (no intermediate sent)", "good.test", k, [c], ("trusted root",))
    k, c = p.leaf(dns=["good.test"])
    add("bad leaf signature", "good.test", k, [break_signature(c), p.inter], ("signature",))
    k, c = p.leaf(dns=["good.test"])
    add("bad intermediate signature", "good.test", k, [c, break_signature(p.inter)], ("signature",))
    k, c = p.leaf(dns=["good.test"], eku=(ExtendedKeyUsageOID.CLIENT_AUTH,))
    add("EKU clientAuth only", "good.test", k, [c, p.inter], ("usage", "purpose"))
    nonca_key = key_ec()
    nonca = make_cert("Not A CA", nonca_key, "Conformance Root", p.root_key, dns=["notca.test"])
    k, c = p.leaf(dns=["good.test"], issuer=("Not A CA", nonca_key))
    add("issuer is not a CA", "good.test", k, [c, nonca], ("CA",))
    rk = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    k, c = p.leaf(key=rk, dns=["good.test"])
    add("RSA-2048 leaf", "good.test", rk, [c, p.inter])
    rk1 = rsa.generate_private_key(public_exponent=65537, key_size=1024)
    k, c = p.leaf(key=rk1, dns=["good.test"])
    add("RSA-1024 leaf", "good.test", rk1, [c, p.inter], ("key", "unsupported", "signature"), weak=True)
    k, c = sha1_leaf(p, "good.test")
    add("SHA-1-signed leaf", "good.test", k, [c, p.inter], ("algorithm", "signature"), weak=True)
    return out


def sha1_leaf(p, host):
    """A leaf for `host` the intermediate signs with SHA-1 (`cryptography`
    refuses to make one, so `openssl x509 -req` does)."""
    import subprocess
    key = key_ec()
    d = Path(TMP)
    csr = (x509.CertificateSigningRequestBuilder().subject_name(name("sha1"))
           .sign(key, hashes.SHA256()))
    (d / "sha1.csr").write_bytes(csr.public_bytes(serialization.Encoding.PEM))
    (d / "int.pem").write_bytes(pem(p.inter))
    (d / "int.key").write_bytes(key_pem(p.int_key))
    (d / "sha1.ext").write_text(f"subjectAltName=DNS:{host}\nextendedKeyUsage=serverAuth\n"
                                "basicConstraints=critical,CA:FALSE\n")
    subprocess.run(["openssl", "x509", "-req", "-sha1", "-in", d / "sha1.csr", "-CA", d / "int.pem",
                    "-CAkey", d / "int.key", "-CAcreateserial", "-days", "30", "-extfile", d / "sha1.ext",
                    "-out", d / "sha1.pem"], check=True, capture_output=True)
    return key, x509.load_pem_x509_certificate((d / "sha1.pem").read_bytes())


def main():
    rep = Report("tls_client")
    lanes = tuple(os.environ.get("CONF_LANES", "interp,build").split(","))
    proxy = Proxy()
    p = PKI

    rows = []   # (label, url, expectation-check(line) -> error or None)
    servers = []

    vcases = verification_cases()
    for label, host, cp, kp, kw, words in vcases:
        srv = Server(cp, kp, **kw)
        servers.append((label, host, srv))
        verdict = python_verdict(p.root_pem, host, srv.port)
        url = f"https://{host if not is_ip(host) else host}:{srv.port}/"
        rows.append((label, url, verdict, words))

    # Protocol behaviour, against the good chain.
    gk, gc = p.leaf(dns=["good.test"], ips=["127.0.0.1"])
    gcp, gkp = p.write("proto", gk, [gc, p.inter])
    proto = {
        "ALPN h2": Server(gcp, gkp, alpn=["h2"]),
        "ALPN http/1.1": Server(gcp, gkp, alpn=["http/1.1"]),
        "no ALPN": Server(gcp, gkp),
        "TLS 1.2 only": Server(gcp, gkp, max_version=ssl.TLSVersion.TLSv1_2),
        "TLS 1.3 only": Server(gcp, gkp, min_version=ssl.TLSVersion.TLSv1_3),
        "TLS 1.1 only": Server(gcp, gkp, max_version=ssl.TLSVersion.TLSv1_1, weak=True),
        "close_notify": Server(gcp, gkp, alpn=["http/1.1"], mode="close_notify"),
        "truncated": Server(gcp, gkp, alpn=["http/1.1"], mode="truncated"),
        "length then no close_notify": Server(gcp, gkp, alpn=["http/1.1"], mode="length_truncated"),
    }
    oracle_trunc = {k: python_truncation(proto[k].port) for k in ("close_notify", "truncated")}
    for srv in proto.values():
        srv.hellos.clear()
        srv.versions.clear()
        srv.alpn_chosen.clear()
    proto_url = {k: f"https://good.test:{s.port}/" for k, s in proto.items()}
    proto_url["SNI for an IP host"] = f"https://127.0.0.1:{proto['no ALPN'].port}/"

    all_urls = [u for _, u, _, _ in rows] + list(proto_url.values())
    body = "".join(
        f"    show(c.get({kara_str(u)}));\n" for u in all_urls)
    src = ("fn show(r: Result[Response, HttpError]) {\n"
           "    match r {\n"
           "        Ok(resp) => println(f\"ok {resp.status()} {resp.body()}\"),\n"
           "        Err(e) => println(f\"err {e.message()}\"),\n"
           "    }\n"
           "}\n"
           "fn main() {\n    let c = Client.new();\n" + body + "}\n")
    env = {"KARAC_HTTP_EXTRA_CA": str(p.root_pem), "https_proxy": f"http://127.0.0.1:{proxy.port}",
           "HTTPS_PROXY": "", "no_proxy": "", "NO_PROXY": "", "all_proxy": "", "ALL_PROXY": ""}
    res = run(src, backends=lanes, timeout=900, env=env)
    if res["check"][0] != 0:
        rep.failed.append("check refused the program:\n" + (res["check"][1] + res["check"][2])[-3000:])
        rep.finish()

    report_lines = []
    for backend in lanes:
        for code, out, err in res.get(backend, []):
            if code != 0:
                rep.failed.append(f"[{backend}] exit {code}: {err[-1500:]}")
                continue
            got = out.splitlines()
            if len(got) != len(all_urls):
                rep.failed.append(f"[{backend}] {len(got)} lines for {len(all_urls)} fetches:\n{out[-2000:]}")
                continue
            # Verification cases.
            for (label, url, verdict, words), line in zip(rows, got):
                report_lines.append(f"[{backend}] {label}: python {verdict!r}; kara {line!r}")
                if verdict == "ok":
                    if line != "ok 200 hello":
                        rep.failed.append(f"[{backend}] {label}: Python accepts, Kāra says {line!r}")
                    else:
                        rep.passed += 1
                    continue
                if not line.startswith("err ") or "TLS handshake failed" not in line:
                    rep.failed.append(f"[{backend}] {label}: Python refuses ({verdict}), Kāra says {line!r}")
                    continue
                if CAMEL.search(line.split("TLS handshake failed: ", 1)[1]):
                    rep.failed.append(f"[{backend}] {label}: the message names a Rust type, not the reason: {line!r}")
                    continue
                if words and not any(w.lower() in line.lower() for w in words):
                    rep.failed.append(f"[{backend}] {label}: refused, but the message names none of {words}: {line!r}")
                    continue
                rep.passed += 1
            # Protocol cases.
            pl = dict(zip(proto_url, got[len(rows):]))
            report_lines += [f"[{backend}] {k}: kara {v!r}" for k, v in pl.items()]
            want = {
                "ALPN h2": "ok 200 h2 hello",
                "ALPN http/1.1": "ok 200 hello",
                "no ALPN": "ok 200 hello",
                "TLS 1.2 only": "ok 200 hello",
                "TLS 1.3 only": "ok 200 hello",
                "close_notify": "ok 200 hello",
                "length then no close_notify": "ok 200 hello",
                "SNI for an IP host": "ok 200 hello",
            }
            for k, w in want.items():
                rep.compare(backend, [k], [pl[k]], [w])
            v = pl["TLS 1.1 only"]
            if v.startswith("err ") and "TLS handshake failed" in v and "version" in v.lower():
                rep.passed += 1
            else:
                rep.failed.append(f"[{backend}] TLS 1.1 server: want a handshake Err naming the version, got {v!r}")
            v = pl["truncated"]
            if oracle_trunc["truncated"] == "truncated" and v.startswith("err ") and "close_notify" in v:
                rep.passed += 1
            else:
                rep.failed.append(f"[{backend}] truncated close-delimited body: Python {oracle_trunc['truncated']!r}, "
                                  f"Kāra {v!r}")
            if oracle_trunc["close_notify"] != "ok hello":
                rep.failed.append(f"oracle: close_notify read {oracle_trunc['close_notify']!r}")

    # What the client put in its ClientHellos (both lanes' connections).
    for k, srv in proto.items():
        for hello in srv.hellos:
            if hello is None:
                rep.failed.append(f"{k}: no ClientHello parsed")
                continue
            sni, alpn, versions = hello
            want_sni = None if k == "SNI for an IP host" else "good.test"
            if alpn != ["h2", "http/1.1"]:
                rep.failed.append(f"{k}: ALPN offered {alpn}, want ['h2', 'http/1.1']")
            elif "0302" in versions or "0301" in versions:
                rep.failed.append(f"{k}: offered TLS < 1.2 in supported_versions {versions}")
            else:
                rep.passed += 1
            del want_sni
    # The IP host's hello went to the "no ALPN" server alongside good.test's: one
    # hello per lane per URL, so the SNI set must be exactly {good.test, None}.
    snis = {h[0] for h in proto["no ALPN"].hellos if h}
    if snis != {"good.test", None}:
        rep.failed.append(f"SNI: the no-ALPN server saw {snis}, want good.test for a name and none for an IP")
    else:
        rep.passed += 1
    for k in ("TLS 1.2 only", "TLS 1.3 only"):
        want_v = "TLSv1.2" if "1.2" in k else "TLSv1.3"
        if not proto[k].versions or set(proto[k].versions) != {want_v}:
            rep.failed.append(f"{k}: negotiated {proto[k].versions}")
        else:
            rep.passed += 1
    if set(proto["ALPN h2"].alpn_chosen) != {"h2"} or not getattr(proto["ALPN h2"], "h2_preface", False):
        rep.failed.append(f"ALPN h2: chose {proto['ALPN h2'].alpn_chosen}, preface "
                          f"{getattr(proto['ALPN h2'], 'h2_preface', None)}")
    else:
        rep.passed += 1
    # Every name-addressed fetch went through the proxy, to the right target.
    hosts = {t.rsplit(":", 1)[0] for t in proxy.targets}
    want_hosts = {h for _, h, *_ in vcases if not is_ip(h)} | {"good.test"}
    if hosts != want_hosts:
        rep.failed.append(f"proxy saw CONNECT for {sorted(hosts)}, want {sorted(want_hosts)}")
    else:
        rep.passed += 1

    # KARAC_HTTP_EXTRA_CA that names nothing usable must be an error, not a
    # silent empty trust list.
    good = proto["no ALPN"]
    one = ("fn main() {\n    match Client.new().get(" + kara_str(f"https://127.0.0.1:{good.port}/") + ") {\n"
           "        Ok(resp) => println(f\"ok {resp.status()} {resp.body()}\"),\n"
           "        Err(e) => println(f\"err {e.message()}\"),\n    }\n}\n")
    empty = Path(TMP) / "empty.pem"
    empty.write_text("")
    keyonly = Path(TMP) / "keyonly.pem"
    keyonly.write_bytes(key_pem(gk))
    garbage = Path(TMP) / "garbage.pem"
    garbage.write_text("-----BEGIN CERTIFICATE-----\nnot base64 at all!!\n-----END CERTIFICATE-----\n")
    for label, path, words in [("missing file", Path(TMP) / "nope.pem", ("cannot read",)),
                               ("empty file", empty, ("KARAC_HTTP_EXTRA_CA: no certificate",)),
                               ("private key only", keyonly, ("KARAC_HTTP_EXTRA_CA: no certificate",)),
                               ("garbage PEM", garbage, ("KARAC_HTTP_EXTRA_CA: bad PEM",))]:
        r = run(one, backends=lanes, timeout=600, env=dict(env, KARAC_HTTP_EXTRA_CA=str(path)))
        for backend in lanes:
            for code, out, err in r.get(backend, []):
                line = out.strip()
                report_lines.append(f"[{backend}] extra CA {label}: {line!r}")
                if line.startswith("err ") and any(w in line for w in words) and not CAMEL.search(line):
                    rep.passed += 1
                else:
                    rep.failed.append(f"[{backend}] KARAC_HTTP_EXTRA_CA = {label}: want an Err naming the "
                                      f"variable, got {line!r} (exit {code})")

    out_path = os.environ.get("TLS_REPORT")
    if out_path:
        Path(out_path).write_text("\n".join(report_lines) + "\n")
    rep.finish()


if __name__ == "__main__":
    main()
