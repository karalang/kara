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

use std::io::{BufRead, BufReader, Write};
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
    conn: Option<(BufReader<TcpStream>, TcpStream)>,
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
                let w = s.try_clone().map_err(fail)?;
                self.conn = Some((BufReader::new(s), w));
            }
            let Some((r, _)) = self.conn.as_mut() else {
                continue;
            };
            match read_request(r) {
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
        let Some((_, w)) = self.conn.as_mut() else {
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

/// Reads one request; `None` when the client closed the connection first.
fn read_request(r: &mut impl BufRead) -> std::io::Result<Option<Request>> {
    let bad = |what: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_string());
    let mut line = String::new();
    // Blank lines before a request line are skipped (RFC 9112 §2.2).
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        if !line.trim().is_empty() {
            break;
        }
    }
    let mut words = line.split_whitespace();
    let (Some(method), Some(target), version) = (words.next(), words.next(), words.next()) else {
        return Err(bad("a malformed request line"));
    };
    let http10 = version == Some("HTTP/1.0");
    let mut req = Request {
        method: method.to_string(),
        target: target.to_string(),
        ..Request::default()
    };
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Err(bad("the headers end early"));
        }
        let h = line.trim_end_matches(['\r', '\n']);
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':').ok_or_else(|| bad("a malformed header"))?;
        req.headers
            .push((k.trim().to_string(), v.trim().to_string()));
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
            line.clear();
            r.read_line(&mut line)?;
            let size = line.trim().split(';').next().unwrap_or_default();
            let n = usize::from_str_radix(size, 16).map_err(|_| bad("a malformed chunk size"))?;
            if n == 0 {
                // Trailers, up to the blank line.
                loop {
                    line.clear();
                    if r.read_line(&mut line)? == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                break;
            }
            let at = req.body.len();
            req.body.resize(at + n, 0);
            r.read_exact(&mut req.body[at..])?;
            line.clear();
            r.read_line(&mut line)?;
        }
    } else if let Some(n) = req.header("content-length") {
        let n: usize = n.parse().map_err(|_| bad("a malformed content-length"))?;
        req.body.resize(n, 0);
        r.read_exact(&mut req.body)?;
    }
    Ok(Some(req))
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
        read_request(&mut raw.as_bytes()).unwrap().unwrap()
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
        assert!(read_request(&mut "".as_bytes()).unwrap().is_none());
    }
}
