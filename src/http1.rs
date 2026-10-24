//! A blocking HTTP/1.1 server, one request at a time: `Server.serve` on the
//! MIR backends. The MIR interpreter (`src/mir/interp.rs`) and the compiled
//! backend's runtime (`runtime/src/mir_io.rs`, which includes this file by
//! `#[path]`, as it does `format_spec.rs`) share it, so a request parses and
//! a response is written the same way on both. Plain `std`, no `karac`
//! types.
//!
//! The semantics follow the legacy runtime's server (`serve_request` in
//! `runtime/src/lib.rs`): the query is split on `&` and each part on its
//! first `=`, form-decoded; a response without a `content-type` gets
//! `application/json`. A connection is kept for the next request until the
//! client closes it or asks to (`Connection: close`, or HTTP/1.0 without
//! `keep-alive`).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

/// Header (or query) pairs, in the order they came.
pub type Headers = Vec<(String, String)>;

/// One request, as read off the wire.
#[derive(Default)]
pub struct Request {
    pub method: String,
    /// The request target: the path, then `?` and the query if there is one.
    pub target: String,
    pub headers: Headers,
    pub body: Vec<u8>,
    /// Whether the connection stays open after the response.
    keep_alive: bool,
}

impl Request {
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }

    /// The first header named `name`, ignoring case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The query's `(key, value)` pairs in order, form-decoded; a part with
    /// no `=` has an empty value and an empty part is skipped.
    pub fn query(&self) -> Headers {
        let Some((_, q)) = self.target.split_once('?') else {
            return Vec::new();
        };
        q.split('&')
            .filter(|s| !s.is_empty())
            .map(|s| {
                let (k, v) = s.split_once('=').unwrap_or((s, ""));
                (form_decode(k), form_decode(v))
            })
            .collect()
    }

    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// `+` is a space and `%XX` the byte it names; a malformed escape stays as
/// it is.
fn form_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => match (hex(b[i + 1]), hex(b[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push((h * 16 + l) as u8);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A bound listener and the connection it is serving.
pub struct Server {
    listener: TcpListener,
    /// The connection, and the bytes read off it past the last request.
    conn: Option<(TcpStream, Vec<u8>)>,
    keep_alive: bool,
}

impl Server {
    /// Binds `addr`, an IP literal and a port. Returns the server and the
    /// port it listens on (which the caller prints as `BOUND_PORT=<n>`, as
    /// the legacy runtime does), or the `HttpError` message.
    pub fn bind(addr: &str) -> Result<(Server, u16), String> {
        let fail = |e: &dyn std::fmt::Display| format!("{addr}: bind failed: {e}");
        let at: std::net::SocketAddr = addr.parse().map_err(|e| fail(&e))?;
        let listener = TcpListener::bind(at).map_err(|e| fail(&e))?;
        let port = listener.local_addr().map_err(|e| fail(&e))?.port();
        let server = Server {
            listener,
            conn: None,
            keep_alive: false,
        };
        Ok((server, port))
    }

    /// The next request, from the open connection or, once that ends, from
    /// the next one accepted. A connection that sends something unreadable
    /// is dropped; only a failing `accept` is an error, its `HttpError`
    /// message returned.
    pub fn accept_request(&mut self) -> Result<Request, String> {
        let fail = |e: std::io::Error| format!("accept failed: {e}");
        loop {
            if self.conn.is_none() {
                let (s, _) = self.listener.accept().map_err(fail)?;
                self.conn = Some((s, Vec::new()));
            }
            let Some((s, buf)) = self.conn.as_mut() else {
                continue;
            };
            match read_request(s, buf) {
                Ok(Some(req)) => {
                    self.keep_alive = req.keep_alive;
                    return Ok(req);
                }
                _ => self.conn = None,
            }
        }
    }

    /// Answers the request [`Self::accept_request`] returned last.
    pub fn respond(&mut self, status: i64, body: &[u8], headers: &[(String, String)]) {
        let Some((w, _)) = self.conn.as_mut() else {
            return;
        };
        let mut head = format!("HTTP/1.1 {status} {}\r\n", reason(status));
        let mut typed = false;
        for (k, v) in headers {
            if k.eq_ignore_ascii_case("content-length") {
                continue;
            }
            typed |= k.eq_ignore_ascii_case("content-type");
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        if !typed {
            head.push_str("content-type: application/json\r\n");
        }
        head.push_str(&format!("content-length: {}\r\n", body.len()));
        if !self.keep_alive {
            head.push_str("connection: close\r\n");
        }
        head.push_str("\r\n");
        let sent = w
            .write_all(head.as_bytes())
            .and_then(|()| w.write_all(body))
            .and_then(|()| w.flush());
        if sent.is_err() || !self.keep_alive {
            self.conn = None;
        }
    }
}

/// What the bytes read so far off a connection hold of its next request.
pub enum Parse {
    /// A whole request, and how many bytes it took.
    Done(Request, usize),
    /// Not a whole request yet: read more and ask again.
    More,
    /// Nothing but blank lines, and the client closed the connection.
    Closed,
    /// Bytes that are no request; the connection is dropped.
    Bad(&'static str),
}

/// Parses the next request at the start of `buf`, without blocking: a
/// reader that may not have the whole request yet (a non-blocking socket)
/// asks again once more bytes arrive. `eof` says no more will, so a request
/// that needs more is a bad one.
pub fn parse_request(buf: &[u8], eof: bool) -> Parse {
    let mut p = Lines { buf, pos: 0 };
    // Blank lines before a request line are skipped (RFC 9112 §2.2).
    let first = loop {
        match p.line() {
            None if eof && p.rest_is_blank() => return Parse::Closed,
            None => return p.short(eof, "the request line ends early"),
            Some(Err(e)) => return Parse::Bad(e),
            Some(Ok(l)) if l.trim().is_empty() => continue,
            Some(Ok(l)) => break l,
        }
    };
    let mut words = first.split_whitespace();
    let (Some(method), Some(target), version) = (words.next(), words.next(), words.next()) else {
        return Parse::Bad("a malformed request line");
    };
    let http10 = version == Some("HTTP/1.0");
    let mut req = Request {
        method: method.to_string(),
        target: target.to_string(),
        ..Request::default()
    };
    loop {
        let h = match p.line() {
            None => return p.short(eof, "the headers end early"),
            Some(Err(e)) => return Parse::Bad(e),
            Some(Ok(h)) => h,
        };
        if h.is_empty() {
            break;
        }
        let Some((k, v)) = h.split_once(':') else {
            return Parse::Bad("a malformed header");
        };
        // Names lowercased, as hyper gives them to legacy's handlers; a
        // lookup by `header` ignores case either way.
        req.headers
            .push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
    }
    let connection = req.header("connection").map(str::to_ascii_lowercase);
    req.keep_alive = match http10 {
        true => connection.as_deref() == Some("keep-alive"),
        false => connection.as_deref() != Some("close"),
    };
    let chunked = req
        .header("transfer-encoding")
        .is_some_and(|t| t.to_ascii_lowercase().contains("chunked"));
    if chunked {
        loop {
            let size = match p.line() {
                None if eof => return Parse::Bad("a malformed chunk size"),
                None => return Parse::More,
                Some(Err(e)) => return Parse::Bad(e),
                Some(Ok(l)) => l,
            };
            let size = size.trim().split(';').next().unwrap_or_default();
            let Ok(n) = usize::from_str_radix(size, 16) else {
                return Parse::Bad("a malformed chunk size");
            };
            if n == 0 {
                // Trailers, up to the blank line or the end.
                loop {
                    match p.line() {
                        None if eof => {
                            p.pos = buf.len();
                            break;
                        }
                        None => return Parse::More,
                        Some(Err(e)) => return Parse::Bad(e),
                        Some(Ok(l)) if l.trim().is_empty() => break,
                        Some(Ok(_)) => {}
                    }
                }
                break;
            }
            let Some(data) = p.take(n) else {
                return p.short(eof, "a chunk ends early");
            };
            req.body.extend_from_slice(data);
            // The chunk's line end; the end of the input stands for it.
            match p.line() {
                None if eof => p.pos = buf.len(),
                None => return Parse::More,
                Some(_) => {}
            }
        }
    } else if let Some(n) = req.header("content-length") {
        let Ok(n) = n.parse::<usize>() else {
            return Parse::Bad("a malformed content-length");
        };
        let Some(data) = p.take(n) else {
            return p.short(eof, "the body ends early");
        };
        req.body = data.to_vec();
    }
    Parse::Done(req, p.pos)
}

/// A cursor over the bytes [`parse_request`] reads.
struct Lines<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Lines<'a> {
    /// The next line without its line end, or `None` when no whole line
    /// is left; text that is not UTF-8 is an error.
    fn line(&mut self) -> Option<Result<&'a str, &'static str>> {
        let rest = &self.buf[self.pos..];
        let i = rest.iter().position(|&b| b == b'\n')?;
        self.pos += i + 1;
        let l = rest[..i].strip_suffix(b"\r").unwrap_or(&rest[..i]);
        Some(std::str::from_utf8(l).map_err(|_| "a line that is not UTF-8"))
    }

    /// The next `n` bytes, or `None` when fewer are left.
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let rest = &self.buf[self.pos..];
        let got = rest.get(..n)?;
        self.pos += n;
        Some(got)
    }

    fn rest_is_blank(&self) -> bool {
        self.buf[self.pos..].iter().all(u8::is_ascii_whitespace)
    }

    /// The answer for input that ends inside a request.
    fn short(&self, eof: bool, what: &'static str) -> Parse {
        if eof {
            Parse::Bad(what)
        } else {
            Parse::More
        }
    }
}

/// Reads one request from `r`, blocking, with the bytes after it left in
/// `buf` for the next; `None` when the client closed the connection first.
fn read_request(r: &mut impl Read, buf: &mut Vec<u8>) -> std::io::Result<Option<Request>> {
    let bad = |what: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_string());
    let mut eof = false;
    loop {
        match parse_request(buf, eof) {
            Parse::Done(req, used) => {
                buf.drain(..used);
                return Ok(Some(req));
            }
            Parse::Closed => return Ok(None),
            Parse::Bad(what) => return Err(bad(what)),
            Parse::More => {
                let mut chunk = [0u8; 8192];
                let n = loop {
                    match r.read(&mut chunk) {
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        got => break got?,
                    }
                };
                eof = n == 0;
                buf.extend_from_slice(&chunk[..n]);
            }
        }
    }
}

/// The reason phrase for `status`.
fn reason(status: i64) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> Request {
        read_request(&mut raw.as_bytes(), &mut Vec::new())
            .unwrap()
            .unwrap()
    }

    #[test]
    fn a_request_with_a_query_and_a_body() {
        let r = parse("POST /todos?q=a+b&x=%41&flag&&y=1 HTTP/1.1\r\nContent-Length: 5\r\nX-A: 1\r\n\r\nhello");
        assert_eq!(r.method, "POST");
        assert_eq!(r.path(), "/todos");
        assert_eq!(r.header("x-a"), Some("1"));
        assert_eq!(r.body_text(), "hello");
        let q: Vec<(&str, &str)> = vec![("q", "a b"), ("x", "A"), ("flag", ""), ("y", "1")];
        let got = r.query();
        assert_eq!(
            got.iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect::<Vec<_>>(),
            q
        );
        assert!(r.keep_alive);
    }

    #[test]
    fn a_chunked_body_and_a_closing_request() {
        let r = parse(
            "PUT / HTTP/1.1\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n2;x=1\r\nde\r\n0\r\n\r\n",
        );
        assert_eq!(r.body_text(), "abcde");
        assert!(!r.keep_alive);
        assert!(!parse("GET / HTTP/1.0\r\n\r\n").keep_alive);
    }

    #[test]
    fn a_closed_connection_is_no_request() {
        assert!(read_request(&mut "".as_bytes(), &mut Vec::new())
            .unwrap()
            .is_none());
        assert!(read_request(&mut "\r\n\r\n".as_bytes(), &mut Vec::new())
            .unwrap()
            .is_none());
    }

    /// A request split anywhere is `More` until its last byte, and two
    /// pipelined requests parse one after the other.
    #[test]
    fn a_request_arriving_in_pieces_parses_once_whole() {
        let raw = "POST /a HTTP/1.1\r\nContent-Length: 3\r\n\r\nabcGET /b HTTP/1.1\r\n\r\n";
        let first = raw.find("GET").unwrap();
        for cut in 0..first {
            assert!(
                matches!(parse_request(&raw.as_bytes()[..cut], false), Parse::More),
                "cut at {cut}"
            );
        }
        let Parse::Done(a, used) = parse_request(raw.as_bytes(), false) else {
            panic!("the whole first request")
        };
        assert_eq!(
            (a.target.as_str(), a.body_text().as_str(), used),
            ("/a", "abc", first)
        );
        let Parse::Done(b, _) = parse_request(&raw.as_bytes()[used..], false) else {
            panic!("the second request")
        };
        assert_eq!(b.target, "/b");
        let chunked = "PUT / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n";
        for cut in 0..chunked.len() {
            assert!(
                matches!(
                    parse_request(&chunked.as_bytes()[..cut], false),
                    Parse::More
                ),
                "chunked cut at {cut}"
            );
        }
        assert!(matches!(
            parse_request(&chunked.as_bytes()[..20], true),
            Parse::Bad(_)
        ));
    }
}
