//! HTTP/1.1 requests and responses for the servers of the MIR backends
//! (runtime/stdlib/source/http_server.kara): parsing a request off the
//! bytes a connection has sent, and a response's bytes. The MIR
//! interpreter (`src/mir/interp.rs`) and the compiled backend's runtime
//! (`runtime/src/mir_io.rs`, which includes this file by `#[path]`, as it
//! does `format_spec.rs`) share it, so a request parses and a response is
//! written the same way on both. Plain `std`, no `karac` types.
//!
//! The semantics follow the legacy runtime's server (`serve_request` in
//! `runtime/src/lib.rs`): the query is split on `&` and each part on its
//! first `=`, form-decoded. A response without a `content-type` gets
//! [`DEFAULT_CONTENT_TYPE`]: its body is a `String` (review-thread ruling
//! 2026-11-17 in docs/library/io.md, B-2026-11-16-275). A connection is kept for the next request until the
//! client closes it or asks to (`Connection: close`, or HTTP/1.0 without
//! `keep-alive`).

/// Header (or query) pairs, in the order they came.
pub type Headers = Vec<(String, String)>;

/// The `Content-Type` of a response whose handler sets none: its body is
/// a `String`, so text; a JSON API sets `application/json` itself
/// (B-2026-11-16-275).
pub const DEFAULT_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

/// One request, as read off the wire.
/// A request target in origin form: an absolute URL's path and query, `/`
/// when it has neither, and any other target itself. A server reads an
/// absolute-form target (`GET http://example.com/z?k=v`, RFC 9112 §3.2.2)
/// this way too, so `path()` is `/z` (docs/library/io.md, B-2026-11-07-26).
fn origin_target(url: &str) -> String {
    match url.split_once("://") {
        Some((_, rest)) => match rest.find(['/', '?']) {
            Some(i) if rest[i..].starts_with('/') => rest[i..].to_string(),
            Some(i) => format!("/{}", &rest[i..]),
            None => "/".to_string(),
        },
        None => url.to_string(),
    }
}

#[derive(Default, Clone)]
pub struct Request {
    pub method: String,
    /// The request target: the path, then `?` and the query if there is one.
    pub target: String,
    pub headers: Headers,
    pub body: Vec<u8>,
    /// Whether the connection stays open after the response.
    keep_alive: bool,
    /// An HTTP/1.0 request, whose connection stays open only when the
    /// response says `connection: keep-alive`.
    http10: bool,
}

impl Request {
    /// The request as a value of its own, for `Server.__served`
    /// (B-2026-11-20-54): its target, headers and body move to it, and
    /// `self` keeps what the response to it reads (`response_bytes`,
    /// `keep_alive`): its method and how it keeps the connection.
    pub fn detach(&mut self) -> Request {
        Request {
            method: self.method.clone(),
            target: std::mem::take(&mut self.target),
            headers: std::mem::take(&mut self.headers),
            body: std::mem::take(&mut self.body),
            keep_alive: self.keep_alive,
            http10: self.http10,
        }
    }

    /// A request a program builds (`Request.new(method, url)`, core
    /// semantics §14.12): an absolute URL's target is its path and query,
    /// `/` when it has neither, and any other URL is the target itself.
    pub fn built(method: String, url: &str) -> Request {
        Request {
            method,
            target: origin_target(url),
            ..Request::default()
        }
    }

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

    /// Whether the connection stays open for the next request.
    pub fn keep_alive(&self) -> bool {
        self.keep_alive
    }

    /// Whether the request is HTTP/1.0.
    pub fn http10(&self) -> bool {
        self.http10
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

/// The largest request head the server reads, request line and headers
/// together (and a chunked body's trailers): a bigger one is answered
/// `431` (docs/library/io.md, B-2026-11-17-50).
pub const HEAD_LIMIT: usize = 64 * 1024;
/// The largest request body the server reads: a bigger one is answered
/// `413`.
pub const BODY_LIMIT: usize = 16 * 1024 * 1024;
/// How long after its first byte a request head may take to arrive before
/// it is answered `408`.
pub const HEAD_TIMEOUT_MS: i64 = 30_000;
/// How long a connection may wait between requests, or a body between its
/// reads, before it is closed.
pub const IDLE_TIMEOUT_MS: i64 = 60_000;

/// Whether `s` is an RFC 9110 token (a method or a header name).
fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// Whether a header value holds a byte that would end or split the line
/// it is written on (CR, LF) or that a peer may read as its end (NUL).
fn breaks_line(s: &str) -> bool {
    s.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0))
}

/// Whether `status` is sent without a body: every `1xx`, `204` and `304`
/// (RFC 9110 §6.4.1).
fn bodiless(status: i64) -> bool {
    (100..200).contains(&status) || status == 204 || status == 304
}

/// Whether the comma-separated `Connection` value `v` lists `token`,
/// without case (RFC 9110 §7.6.1, B-2026-11-17-52).
fn lists(v: &str, token: &str) -> bool {
    v.split(',').any(|t| t.trim().eq_ignore_ascii_case(token))
}

/// A response's bytes, for the request `req` is answering: the status
/// line, `headers` (with a JSON `content-type` when they give none, and
/// the body's `content-length` in place of any they give), `connection:
/// close` unless the connection is kept, and the body. An HTTP/1.0
/// connection kept open says so, `connection: keep-alive`, since a 1.0
/// client otherwise reads the response as the last (review-thread ruling
/// 2026-11-16, B-2026-11-16-210).
///
/// What the handler cannot be allowed to send is the server's to fix
/// (docs/library/io.md, B-2026-11-17-45, -47, -48): a status outside
/// 100..=999, or a header whose name is not a token or whose value holds
/// CR, LF or NUL, is answered `500` instead; the handler's
/// `transfer-encoding` is dropped (the body goes out whole, framed by its
/// length) and its `connection: close` closes the connection; a response
/// to `HEAD`, and every `1xx`, `204` and `304`, goes without a body. A
/// `1xx` from a handler is no interim response, so the connection closes
/// after it. `req`'s `keep_alive` is updated to what the response does.
pub fn response_bytes(
    status: i64,
    body: &[u8],
    headers: &[(String, String)],
    req: &mut Request,
) -> Vec<u8> {
    let refused = !(100..=999).contains(&status)
        || headers.iter().any(|(k, v)| !is_token(k) || breaks_line(v));
    if refused {
        return response_bytes(500, b"", &[], req);
    }
    let mut head = format!("HTTP/1.1 {status} {}\r\n", reason(status));
    let mut typed = false;
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("content-length") || k.eq_ignore_ascii_case("transfer-encoding") {
            continue;
        }
        if k.eq_ignore_ascii_case("connection") {
            if lists(v, "close") {
                req.keep_alive = false;
            }
            continue;
        }
        typed |= k.eq_ignore_ascii_case("content-type");
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    let bodiless = bodiless(status);
    if (100..200).contains(&status) {
        req.keep_alive = false;
    }
    if !typed && !bodiless {
        head.push_str(&format!("content-type: {DEFAULT_CONTENT_TYPE}\r\n"));
    }
    // A HEAD response's length is the one a GET would get (RFC 9110
    // §9.3.2); a 1xx, 204 or 304 has none.
    if !bodiless {
        head.push_str(&format!("content-length: {}\r\n", body.len()));
    }
    if !req.keep_alive {
        head.push_str("connection: close\r\n");
    } else if req.http10 {
        head.push_str("connection: keep-alive\r\n");
    }
    head.push_str("\r\n");
    let mut wire = head.into_bytes();
    if !bodiless && req.method != "HEAD" {
        wire.extend_from_slice(body);
    }
    wire
}

/// The bytes of the answer to a request the server refuses with `status`
/// (400, 408, 413, 431 or 501) before any handler sees it; its connection
/// closes.
pub fn refusal_bytes(status: i64) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} {}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        reason(status)
    )
    .into_bytes()
}

/// What the bytes read so far off a connection hold of its next request.
pub enum Parse {
    /// A whole request, and how many bytes it took.
    Done(Request, usize),
    /// Not a whole request yet: read more and ask again.
    More(Pending),
    /// Nothing but blank lines, and the client closed the connection.
    Closed,
    /// Bytes the server refuses, with the status it answers (400, 413,
    /// 431 or 501) and why; the connection closes.
    Bad(u16, &'static str),
}

/// What a request that is not whole yet is waiting for.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Pending {
    /// How many bytes the buffer must hold before asking again can give
    /// a different answer: the end of the body or of the chunk being read,
    /// or one more byte (B-2026-11-17-51).
    pub need: usize,
    /// Whether any byte of the request has come (more than blank lines).
    pub started: bool,
    /// Whether its head is whole and its body is being read.
    pub in_body: bool,
    /// Whether the head asks `Expect: 100-continue` (B-2026-11-17-53).
    pub expect_continue: bool,
}

/// The answer for a request that needs more than `buf` holds: `More` with
/// `pending`, or, at the end of the input, a 400 for `what`.
fn short(eof: bool, pending: Pending, what: &'static str) -> Parse {
    if eof {
        Parse::Bad(400, what)
    } else {
        Parse::More(pending)
    }
}

/// Parses the next request at the start of `buf`, without blocking: a
/// reader that may not have the whole request yet (a non-blocking socket)
/// asks again once more bytes arrive. `eof` says no more will, so a request
/// that needs more is a bad one.
///
/// A request is refused unless it is unambiguous (RFC 9112, review-thread
/// ruling 2026-11-17 in docs/library/io.md, B-2026-11-17-44, -46): `400`
/// for a malformed request line or header (no colon, whitespace before
/// the colon, an empty or non-token name, CR, LF or NUL in a value), a
/// version other than HTTP/1.0 and HTTP/1.1, an HTTP/1.1 request without
/// exactly one `Host`, and ambiguous framing (`Content-Length` with
/// `Transfer-Encoding`, two `Content-Length`s, one that is not plain
/// digits, `chunked` that is not the last coding); `501` for a transfer
/// coding other than `chunked`; `431` for a head over [`HEAD_LIMIT`];
/// `413` for a body over [`BODY_LIMIT`].
pub fn parse_request(buf: &[u8], eof: bool) -> Parse {
    Parser::default().advance(buf, eof)
}

/// Where a [`Parser`] is in the request it is reading.
#[derive(Default)]
enum Stage {
    /// Before the request line: blank lines are skipped.
    #[default]
    Start,
    /// Reading header lines.
    Headers,
    /// The head is whole; a `Content-Length` body of this many bytes.
    Length(usize),
    /// The next line is a chunk's size.
    ChunkSize,
    /// A chunk of this many bytes is next.
    ChunkData(usize),
    /// The line end after a chunk's data is next.
    ChunkEnd,
    /// The trailers after the last chunk, and how many bytes they took.
    Trailers(usize),
}

/// [`parse_request`] across reads (B-2026-11-17-93): it keeps what it has
/// parsed of a request that is not whole yet, so a connection's bytes are
/// read once however many reads bring them, instead of the whole request
/// being parsed again at each one (a chunked body of many small chunks
/// was quadratic in its size). `advance` must be given the same bytes
/// again each time, with more after them; its answers are
/// [`parse_request`]'s.
#[derive(Default)]
pub struct Parser {
    stage: Stage,
    /// Where the next line or body byte starts.
    pos: usize,
    /// How far past `pos` the buffer is known to hold no line end.
    scanned: usize,
    /// How far the buffer is known to be whitespace, before the request
    /// line, and whether a byte that is not has come.
    blank_upto: usize,
    started: bool,
    req: Request,
    head_end: usize,
    expect_continue: bool,
}

impl Parser {
    /// What `buf`, the bytes read so far, holds of the request: the same
    /// answer as `parse_request(buf, eof)`, reading only the bytes that
    /// came since the last call.
    pub fn advance(&mut self, buf: &[u8], eof: bool) -> Parse {
        let mut p = Lines {
            buf,
            pos: self.pos,
            scanned: self.scanned.max(self.pos),
        };
        let answer = self.step(&mut p, eof);
        self.pos = p.pos;
        self.scanned = p.scanned;
        answer
    }

    fn body_more(&self, need: usize) -> Pending {
        Pending {
            need,
            started: true,
            in_body: true,
            expect_continue: self.expect_continue,
        }
    }

    fn step(&mut self, p: &mut Lines<'_>, eof: bool) -> Parse {
        let buf = p.buf;
        if matches!(self.stage, Stage::Start) {
            // Blank lines before a request line are skipped (RFC 9112 §2.2).
            loop {
                match p.line() {
                    None if eof && p.rest_is_blank() => return Parse::Closed,
                    None => {
                        let from = self.blank_upto.max(p.pos);
                        if buf[from..].iter().any(|b| !b.is_ascii_whitespace()) {
                            self.started = true;
                        }
                        self.blank_upto = buf.len();
                        return self.head_short(buf, eof, "the request line ends early");
                    }
                    Some(Err(e)) => return Parse::Bad(400, e),
                    Some(Ok(l)) if l.trim().is_empty() => {
                        if l.bytes().any(|b| !b.is_ascii_whitespace()) {
                            self.started = true;
                        }
                    }
                    Some(Ok(l)) => {
                        self.started = true;
                        if let Err(bad) = self.request_line(l) {
                            return bad;
                        }
                        self.stage = Stage::Headers;
                        break;
                    }
                }
            }
        }
        if matches!(self.stage, Stage::Headers) {
            loop {
                let h = match p.line() {
                    None => return self.head_short(buf, eof, "the headers end early"),
                    Some(Err(e)) => return Parse::Bad(400, e),
                    Some(Ok(h)) => h,
                };
                if h.is_empty() {
                    break;
                }
                let Some((k, v)) = h.split_once(':') else {
                    return Parse::Bad(400, "a malformed header");
                };
                // No whitespace before the colon (RFC 9112 §5.1), which is
                // also how an obs-fold continuation line is refused; a
                // value is trimmed of the spaces and tabs around it (OWS),
                // and may hold neither CR nor NUL (RFC 9110 §5.5).
                let v = v.trim_matches([' ', '\t']);
                if !is_token(k) || breaks_line(v) {
                    return Parse::Bad(400, "a malformed header");
                }
                // Names lowercased, as hyper gives them to legacy's
                // handlers; a lookup by `header` ignores case either way.
                self.req
                    .headers
                    .push((k.to_ascii_lowercase(), v.to_string()));
            }
            self.head_end = p.pos;
            if let Err(bad) = self.framing() {
                return bad;
            }
        }
        loop {
            match self.stage {
                Stage::Start | Stage::Headers => unreachable!("the head is whole"),
                Stage::Length(n) => {
                    let Some(data) = p.take(n) else {
                        return short(
                            eof,
                            self.body_more(self.head_end + n),
                            "the body ends early",
                        );
                    };
                    self.req.body = data.to_vec();
                    break;
                }
                Stage::ChunkSize => {
                    let size = match p.line() {
                        None => {
                            return short(
                                eof,
                                self.body_more(buf.len() + 1),
                                "a malformed chunk size",
                            )
                        }
                        Some(Err(e)) => return Parse::Bad(400, e),
                        Some(Ok(l)) => l,
                    };
                    let size = size
                        .split(';')
                        .next()
                        .unwrap_or_default()
                        .trim_matches([' ', '\t']);
                    if size.is_empty() || !size.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Parse::Bad(400, "a malformed chunk size");
                    }
                    let n = match usize::from_str_radix(size, 16) {
                        Ok(n) if self.req.body.len().saturating_add(n) <= BODY_LIMIT => n,
                        _ => return Parse::Bad(413, "the body is too large"),
                    };
                    self.stage = if n == 0 {
                        Stage::Trailers(0)
                    } else {
                        Stage::ChunkData(n)
                    };
                }
                Stage::ChunkData(n) => {
                    let at = p.pos;
                    let Some(data) = p.take(n) else {
                        return short(eof, self.body_more(at + n + 2), "a chunk ends early");
                    };
                    self.req.body.extend_from_slice(data);
                    self.stage = Stage::ChunkEnd;
                }
                Stage::ChunkEnd => {
                    // The chunk's line end, and nothing before it; the end
                    // of the input stands for it.
                    let at = p.pos;
                    match p.line() {
                        None if eof => p.pos = buf.len(),
                        None => return Parse::More(self.body_more(at + 2)),
                        Some(Ok("")) => {}
                        Some(_) => return Parse::Bad(400, "a chunk longer than its size"),
                    }
                    self.stage = Stage::ChunkSize;
                }
                Stage::Trailers(mut trailers) => {
                    // Up to the blank line or the end.
                    loop {
                        let at = p.pos;
                        match p.line() {
                            None if eof => {
                                p.pos = buf.len();
                                break;
                            }
                            None if buf.len() - at > HEAD_LIMIT => {
                                return Parse::Bad(431, "the trailers are too large")
                            }
                            None => {
                                self.stage = Stage::Trailers(trailers);
                                return Parse::More(self.body_more(buf.len() + 1));
                            }
                            Some(Err(e)) => return Parse::Bad(400, e),
                            Some(Ok(l)) if l.trim().is_empty() => break,
                            Some(Ok(_)) => {
                                trailers += p.pos - at;
                                if trailers > HEAD_LIMIT {
                                    return Parse::Bad(431, "the trailers are too large");
                                }
                            }
                        }
                    }
                    break;
                }
            }
        }
        let req = std::mem::take(&mut self.req);
        let used = p.pos;
        *self = Parser::default();
        Parse::Done(req, used)
    }

    /// The answer for a head that needs more than `buf` holds.
    fn head_short(&self, buf: &[u8], eof: bool, what: &'static str) -> Parse {
        if !eof && buf.len() > HEAD_LIMIT {
            Parse::Bad(431, "the request head is too large")
        } else {
            let more = Pending {
                need: buf.len() + 1,
                started: self.started,
                ..Pending::default()
            };
            short(eof, more, what)
        }
    }

    /// `method SP target SP HTTP/x.y` (RFC 9112 §3); anything else is
    /// answered 400 without reaching the handler (docs/library/io.md).
    fn request_line(&mut self, first: &str) -> Result<(), Parse> {
        let mut words = first.split(' ');
        let (Some(method), Some(target), Some(version), None) =
            (words.next(), words.next(), words.next(), words.next())
        else {
            return Err(Parse::Bad(400, "a malformed request line"));
        };
        if !is_token(method) || target.is_empty() || target.bytes().any(|b| b <= b' ' || b == 0x7f)
        {
            return Err(Parse::Bad(400, "a malformed request line"));
        }
        let http10 = match version {
            "HTTP/1.1" => false,
            "HTTP/1.0" => true,
            _ => return Err(Parse::Bad(400, "an HTTP version other than 1.0 and 1.1")),
        };
        self.req = Request {
            method: method.to_string(),
            target: origin_target(target),
            http10,
            ..Request::default()
        };
        Ok(())
    }

    /// The checks on a whole head, and the stage its framing reads the
    /// body in.
    fn framing(&mut self) -> Result<(), Parse> {
        if self.head_end > HEAD_LIMIT {
            return Err(Parse::Bad(431, "the request head is too large"));
        }
        fn all<'a>(h: &'a Headers, name: &'a str) -> impl Iterator<Item = &'a (String, String)> {
            h.iter().filter(move |(k, _)| k == name)
        }
        let req = &mut self.req;
        let http10 = req.http10;
        if !http10 && all(&req.headers, "host").count() != 1 {
            return Err(Parse::Bad(
                400,
                "an HTTP/1.1 request without exactly one Host",
            ));
        }
        let keep_alive = {
            let mut connection = all(&req.headers, "connection").map(|(_, v)| v.as_str());
            match http10 {
                true => connection.any(|v| lists(v, "keep-alive")),
                false => !connection.any(|v| lists(v, "close")),
            }
        };
        req.keep_alive = keep_alive;
        // Framing (RFC 9112 §6): the codings of every Transfer-Encoding
        // header in order, and every Content-Length.
        let codings: Vec<String> = all(&req.headers, "transfer-encoding")
            .flat_map(|(_, v)| v.split(','))
            .map(|c| c.trim().to_ascii_lowercase())
            .collect();
        let lengths: Vec<&str> = all(&req.headers, "content-length")
            .map(|(_, v)| v.as_str())
            .collect();
        if !codings.is_empty() && !lengths.is_empty() {
            return Err(Parse::Bad(400, "both Content-Length and Transfer-Encoding"));
        }
        if lengths.len() > 1 {
            return Err(Parse::Bad(400, "two Content-Length headers"));
        }
        self.expect_continue = !http10
            && req
                .header("expect")
                .is_some_and(|e| e.eq_ignore_ascii_case("100-continue"));
        self.stage = if !codings.is_empty() {
            if codings.last().map(String::as_str) != Some("chunked") {
                return Err(Parse::Bad(
                    400,
                    "a Transfer-Encoding whose last coding is not chunked",
                ));
            }
            if codings[..codings.len() - 1].iter().any(|c| c == "chunked") {
                return Err(Parse::Bad(400, "chunked applied twice"));
            }
            if codings.len() > 1 {
                return Err(Parse::Bad(501, "a transfer coding other than chunked"));
            }
            Stage::ChunkSize
        } else if let Some(n) = lengths.first() {
            // `1*DIGIT` (RFC 9110 §8.6): no sign, no list, no space.
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Parse::Bad(400, "a malformed content-length"));
            }
            match n.parse::<usize>() {
                Ok(n) if n <= BODY_LIMIT => Stage::Length(n),
                _ => return Err(Parse::Bad(413, "the body is too large")),
            }
        } else {
            Stage::Length(0)
        };
        Ok(())
    }
}

/// A connection's progress toward its next request, between the reads of
/// the server's `__next` (runtime/stdlib/source/http_server.kara): what
/// `Server.__install`, `__need` and `__wait_ms` answer. The interpreter
/// and the runtime keep one per task.
#[derive(Default)]
pub struct Progress {
    /// When the first byte of the request came.
    started: Option<std::time::Instant>,
    pending: Pending,
    /// The request read so far, kept across reads (B-2026-11-17-93).
    parser: Parser,
    /// How many bytes the buffer held at the last `install`.
    seen: usize,
}

impl Progress {
    /// `Server.__install(buf, eof)`: the request at the start of `buf`
    /// with the bytes it took; or no request and 0 when more is needed,
    /// -100 when more is needed and the client waits for a `100
    /// Continue` before sending it, -1 when the connection holds no
    /// request, or minus the status a refused one is answered with.
    pub fn install(&mut self, buf: &[u8], eof: bool) -> (i64, Option<Request>) {
        // The server asks again with the same bytes and more after them;
        // a buffer that shrank is a new one, read from its start.
        if buf.len() < self.seen {
            *self = Progress::default();
        }
        self.seen = buf.len();
        match self.parser.advance(buf, eof) {
            Parse::Done(req, used) => {
                *self = Progress::default();
                (used as i64, Some(req))
            }
            Parse::Closed => {
                *self = Progress::default();
                (-1, None)
            }
            Parse::Bad(status, _) => {
                *self = Progress::default();
                (-i64::from(status), None)
            }
            Parse::More(pending) => {
                if pending.started && self.started.is_none() {
                    self.started = Some(std::time::Instant::now());
                }
                self.pending = pending;
                (if pending.expect_continue { -100 } else { 0 }, None)
            }
        }
    }

    /// `Server.__need()`: how many bytes the buffer must hold before
    /// `install` is worth asking again.
    pub fn need(&self) -> i64 {
        self.pending.need as i64
    }

    /// `Server.__wait_ms()`: how long the next read may wait, in
    /// milliseconds: positive between requests (a timeout closes the
    /// connection silently), negative inside one (a timeout is answered
    /// `408`): what is left of [`HEAD_TIMEOUT_MS`] since the head's first
    /// byte, or [`IDLE_TIMEOUT_MS`] between a body's reads.
    pub fn wait_ms(&self) -> i64 {
        match self.started {
            None => IDLE_TIMEOUT_MS,
            Some(_) if self.pending.in_body => -IDLE_TIMEOUT_MS,
            Some(t) => {
                let spent = i64::try_from(t.elapsed().as_millis()).unwrap_or(i64::MAX);
                -(HEAD_TIMEOUT_MS.saturating_sub(spent).max(1))
            }
        }
    }
}

/// A cursor over the bytes [`parse_request`] reads.
struct Lines<'a> {
    buf: &'a [u8],
    pos: usize,
    /// How far the buffer is known to hold no line end after `pos`, so a
    /// line that comes over many reads is searched once.
    scanned: usize,
}

impl<'a> Lines<'a> {
    /// The next line without its line end, or `None` when no whole line
    /// is left; text that is not UTF-8 is an error.
    fn line(&mut self) -> Option<Result<&'a str, &'static str>> {
        let rest = &self.buf[self.pos..];
        let from = self.scanned.max(self.pos) - self.pos;
        let Some(i) = rest[from..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| i + from)
        else {
            self.scanned = self.buf.len();
            return None;
        };
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
}

/// Reads one request from `r`, blocking, with the bytes after it left in
/// `buf` for the next; `None` when the client closed the connection first.
#[cfg(test)]
fn read_request(r: &mut impl std::io::Read, buf: &mut Vec<u8>) -> std::io::Result<Option<Request>> {
    let bad = |what: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_string());
    let mut eof = false;
    loop {
        match parse_request(buf, eof) {
            Parse::Done(req, used) => {
                buf.drain(..used);
                return Ok(Some(req));
            }
            Parse::Closed => return Ok(None),
            Parse::Bad(_, what) => return Err(bad(what)),
            Parse::More(_) => {
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
        100 => "Continue",
        101 => "Switching Protocols",
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
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Content Too Large",
        422 => "Unprocessable Entity",
        426 => "Upgrade Required",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
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

    /// A built request's target is its URL's path and query.
    #[test]
    fn a_built_request_takes_its_target_from_the_url() {
        let target = |url: &str| Request::built("GET".to_string(), url).target;
        assert_eq!(target("http://h:8080/a/b?x=1"), "/a/b?x=1");
        assert_eq!(target("https://h?x=1"), "/?x=1");
        assert_eq!(target("http://h"), "/");
        assert_eq!(target("/plain?y=2"), "/plain?y=2");
        assert_eq!(target(""), "");
        let r = Request::built("POST".to_string(), "http://h/p?q=a+b");
        assert_eq!((r.path(), r.query()[0].1.as_str()), ("/p", "a b"));
    }

    #[test]
    fn a_request_with_a_query_and_a_body() {
        let r = parse("POST /todos?q=a+b&x=%41&flag&&y=1 HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\nX-A: 1\r\n\r\nhello");
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
            "PUT / HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n2;x=1\r\nde\r\n0\r\n\r\n",
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
        let raw = "POST /a HTTP/1.1\r\nHost: h\r\nContent-Length: 3\r\n\r\nabcGET /b HTTP/1.1\r\nHost: h\r\n\r\n";
        let first = raw.find("GET").unwrap();
        for cut in 0..first {
            assert!(
                matches!(parse_request(&raw.as_bytes()[..cut], false), Parse::More(_)),
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
        let chunked =
            "PUT / HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n";
        for cut in 0..chunked.len() {
            assert!(
                matches!(
                    parse_request(&chunked.as_bytes()[..cut], false),
                    Parse::More(_)
                ),
                "chunked cut at {cut}"
            );
        }
        assert!(matches!(
            parse_request(&chunked.as_bytes()[..20], true),
            Parse::Bad(..)
        ));
    }

    /// The status a request is refused with, or 0 when it parses.
    fn refused(raw: &[u8]) -> u16 {
        match parse_request(raw, false) {
            Parse::Bad(status, _) => status,
            Parse::Done(..) => 0,
            Parse::More(_) => 1,
            Parse::Closed => 2,
        }
    }

    /// The probe's cells (B-2026-11-17-44, -46): what is ambiguous or
    /// malformed is refused before a handler sees it.
    #[test]
    fn ambiguous_or_malformed_requests_are_refused() {
        let h = |rest: &str| format!("POST /p HTTP/1.1\r\nHost: h\r\n{rest}");
        let bad = [
            // S10: CL and TE both.
            h("Content-Length: 6\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\nGET /smuggled HTTP/1.1\r\n\r\n"),
            // S13 differing, S14 equal Content-Lengths.
            h("Content-Length: 5\r\nContent-Length: 6\r\n\r\nhello!"),
            h("Content-Length: 5\r\nContent-Length: 5\r\n\r\nhello"),
            // S17 a sign, a list, a space, a negative.
            h("Content-Length: +5\r\n\r\nhello"),
            h("Content-Length: 5, 5\r\n\r\nhello"),
            h("Content-Length: -1\r\n\r\n"),
            // chunked not last.
            h("Transfer-Encoding: chunked, gzip\r\n\r\n"),
            // S04 whitespace before the colon, S48 an empty name.
            h("X-A : 1\r\n\r\n"),
            h(": v\r\n\r\n"),
            // S29 a CR or NUL in a value.
            h("X-A: a\rb\r\n\r\n"),
            h("X-A: a\0b\r\n\r\n"),
            // obs-fold.
            h("X-A: a\r\n b\r\n\r\n"),
            // S27 versions.
            "GET / HTTP/2.0\r\nHost: h\r\n\r\n".to_string(),
            "GET / HTTP/1.2\r\nHost: h\r\n\r\n".to_string(),
            "GET / HTTP/1.1x\r\nHost: h\r\n\r\n".to_string(),
            // S28b NUL in the target, two spaces.
            "GET /a\0b HTTP/1.1\r\nHost: h\r\n\r\n".to_string(),
            "GET  / HTTP/1.1\r\nHost: h\r\n\r\n".to_string(),
            // S25 no Host, S26 two.
            "GET / HTTP/1.1\r\n\r\n".to_string(),
            "GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n".to_string(),
            // A chunk size with a sign, a chunk longer than its size.
            h("Transfer-Encoding: chunked\r\n\r\n+3\r\nabc\r\n0\r\n\r\n"),
            h("Transfer-Encoding: chunked\r\n\r\n3\r\nabcdef\r\n0\r\n\r\n"),
        ];
        for raw in &bad {
            assert_eq!(refused(raw.as_bytes()), 400, "{raw:?}");
        }
        // S11 an unknown coding before chunked.
        assert_eq!(
            refused(h("Transfer-Encoding: gzip, chunked\r\n\r\n").as_bytes()),
            501
        );
        // Still right: an HTTP/1.0 request without Host, OWS around a
        // value, any case, a bare LF.
        let ok = [
            "GET / HTTP/1.0\r\n\r\n",
            "GET / HTTP/1.1\r\nhOsT:  h\t\r\n\r\n",
            "GET / HTTP/1.1\nHost: h\n\n",
            "POST / HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: Chunked\r\n\r\n1\r\na\r\n0\r\n\r\n",
        ];
        for raw in ok {
            assert_eq!(refused(raw.as_bytes()), 0, "{raw:?}");
        }
    }

    /// A head over 64 KiB is 431 whether or not it is whole, and a body
    /// over 16 MiB is 413 before it is read (B-2026-11-17-50).
    #[test]
    fn heads_and_bodies_have_limits() {
        let big = format!(
            "GET / HTTP/1.1\r\nHost: h\r\nX-L: {}\r\n",
            "a".repeat(HEAD_LIMIT)
        );
        assert_eq!(refused(big.as_bytes()), 431);
        assert_eq!(refused(format!("{big}\r\n").as_bytes()), 431);
        let target = format!("GET /{} HTTP/1.1\r\n", "a".repeat(HEAD_LIMIT));
        assert_eq!(refused(target.as_bytes()), 431);
        let just = format!(
            "GET / HTTP/1.1\r\nHost: h\r\nX-L: {}\r\n\r\n",
            "a".repeat(60_000)
        );
        assert_eq!(refused(just.as_bytes()), 0);
        let cl = |n: usize| format!("POST / HTTP/1.1\r\nHost: h\r\nContent-Length: {n}\r\n\r\n");
        assert_eq!(refused(cl(BODY_LIMIT + 1).as_bytes()), 413);
        assert_eq!(refused(cl(BODY_LIMIT).as_bytes()), 1);
        assert_eq!(
            refused(format!("{}0", cl(99_999_999_999_999_999)).as_bytes()),
            413
        );
        let chunk = format!(
            "POST / HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
            BODY_LIMIT + 1
        );
        assert_eq!(refused(chunk.as_bytes()), 413);
    }

    /// A body is waited for whole: `need` names its end, so the server
    /// does not parse again after every read (B-2026-11-17-51); a client
    /// that asks for `100-continue` is told so (B-2026-11-17-53).
    #[test]
    fn a_pending_body_names_what_it_needs() {
        let head = "POST / HTTP/1.1\r\nHost: h\r\nContent-Length: 10\r\n\r\n";
        let Parse::More(p) = parse_request(format!("{head}abc").as_bytes(), false) else {
            panic!("a body that is not whole")
        };
        assert_eq!(
            (p.need, p.in_body, p.expect_continue),
            (head.len() + 10, true, false)
        );
        let asks =
            "POST / HTTP/1.1\r\nHost: h\r\nExpect: 100-continue\r\nContent-Length: 5\r\n\r\n";
        let mut progress = Progress::default();
        assert_eq!(progress.install(asks.as_bytes(), false).0, -100);
        assert!(progress.wait_ms() < 0);
        let (used, req) = progress.install(format!("{asks}hello").as_bytes(), false);
        assert_eq!(
            (used as usize, req.unwrap().body_text()),
            (asks.len() + 5, "hello".to_string())
        );
        assert_eq!(progress.wait_ms(), IDLE_TIMEOUT_MS);
        // A head in progress waits what is left of 30 s; nothing yet, 60.
        assert_eq!(progress.install(b"\r\n", false).0, 0);
        assert_eq!(progress.wait_ms(), IDLE_TIMEOUT_MS);
        assert_eq!(progress.install(b"\r\nGET / HT", false).0, 0);
        let w = progress.wait_ms();
        assert!((-HEAD_TIMEOUT_MS..0).contains(&w), "{w}");
        assert_eq!(
            progress.install(b"\r\nGET / HTTP/1.1\r\n\r\n", false).0,
            -400
        );
    }

    /// What a parse answered, to compare two parses.
    fn answer(p: Parse) -> String {
        match p {
            Parse::Done(r, used) => format!(
                "done {used} {} {} {:?} {:?} {} {}",
                r.method,
                r.target,
                r.headers,
                String::from_utf8_lossy(&r.body),
                r.keep_alive,
                r.http10
            ),
            Parse::More(p) => format!("more {p:?}"),
            Parse::Closed => "closed".to_string(),
            Parse::Bad(status, why) => format!("bad {status} {why}"),
        }
    }

    /// A parser kept across reads answers what a fresh parse of the same
    /// bytes does, after every read, whatever the reads' sizes
    /// (B-2026-11-17-93).
    #[test]
    fn a_kept_parser_answers_as_a_fresh_parse() {
        let requests: &[&str] = &[
            "GET /a?x=1 HTTP/1.1\r\nHost: h\r\n\r\n",
            "\r\n\r\nGET / HTTP/1.0\r\nConnection: keep-alive\r\n\r\nNEXT",
            "POST /p HTTP/1.1\r\nHost: h\r\nContent-Length: 11\r\n\r\nhello world",
            "PUT / HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=1\r\nde\r\n0\r\nT: v\r\n\r\n",
            "PUT / HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabcX\r\n",
            "PUT / HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
            "POST / HTTP/1.1\r\nHost: h\r\nExpect: 100-continue\r\nContent-Length: 2\r\n\r\nok",
            "GET / HTTP/1.1\r\nHost: h\r\nBad Name: v\r\n\r\n",
            "GET / HTTP/2.0\r\n\r\n",
            "GET / HTTP/1.1\r\n\r\n",
            "\r\n \r\n",
        ];
        for raw in requests {
            let raw = raw.as_bytes();
            for step in [1, 2, 3, 7, raw.len()] {
                for eof_at_end in [false, true] {
                    let mut kept = Parser::default();
                    let mut end = 0;
                    loop {
                        end = (end + step).min(raw.len());
                        let eof = eof_at_end && end == raw.len();
                        let fresh = answer(parse_request(&raw[..end], eof));
                        let got = answer(kept.advance(&raw[..end], eof));
                        assert_eq!(
                            got,
                            fresh,
                            "{:?} by {step} at {end}",
                            String::from_utf8_lossy(raw)
                        );
                        if !fresh.starts_with("more") || end == raw.len() {
                            break;
                        }
                    }
                }
            }
        }
    }

    /// A chunked body that comes one small chunk per read is read once:
    /// 200 000 one-byte chunks, about 1.2 MB, were about 10^11 byte reads
    /// when each read parsed the request again (B-2026-11-17-93).
    #[test]
    fn a_body_of_many_small_chunks_is_read_once() {
        let mut raw = b"PUT / HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        let mut cuts = vec![raw.len()];
        for _ in 0..200_000 {
            raw.extend_from_slice(b"1\r\nx\r\n");
            cuts.push(raw.len());
        }
        raw.extend_from_slice(b"0\r\n\r\n");
        cuts.push(raw.len());
        let started = std::time::Instant::now();
        let mut progress = Progress::default();
        let mut last = (0, None);
        for &end in &cuts {
            if end >= progress.need() as usize {
                last = progress.install(&raw[..end], false);
            }
        }
        let (used, req) = last;
        assert_eq!(used as usize, raw.len());
        assert_eq!(req.unwrap().body.len(), 200_000);
        let spent = started.elapsed();
        assert!(spent < std::time::Duration::from_secs(10), "{spent:?}");
    }

    /// `Connection` is a token list (B-2026-11-17-52).
    #[test]
    fn connection_is_a_list() {
        assert!(
            !parse("GET / HTTP/1.1\r\nHost: h\r\nConnection: keep-alive, close\r\n\r\n").keep_alive
        );
        assert!(!parse("GET / HTTP/1.1\r\nHost: h\r\nConnection: CLOSE\r\n\r\n").keep_alive);
        assert!(parse("GET / HTTP/1.0\r\nConnection: Keep-Alive, TE\r\n\r\n").keep_alive);
        assert!(parse("GET / HTTP/1.1\r\nHost: h\r\nConnection: Upgrade\r\n\r\n").keep_alive);
    }

    fn wire(method: &str, status: i64, body: &str, headers: &[(&str, &str)]) -> (String, bool) {
        let mut req = parse(&format!("{method} / HTTP/1.1\r\nHost: h\r\n\r\n"));
        let headers: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let bytes = response_bytes(status, body.as_bytes(), &headers, &mut req);
        (String::from_utf8(bytes).unwrap(), req.keep_alive)
    }

    /// What a handler returns is checked and framed by the server
    /// (B-2026-11-17-45, -47, -48).
    #[test]
    fn a_response_is_framed_by_the_server() {
        let server_error = "HTTP/1.1 500 Internal Server Error\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: 0\r\n\r\n";
        for h in [
            ("X-I", "a\r\nSet-Cookie: evil=1"),
            ("X-I: z\r\nX-J", "b"),
            ("X-I", "a\0b"),
            ("X I", "b"),
            ("", "b"),
        ] {
            assert_eq!(wire("GET", 200, "x", &[h]).0, server_error, "{h:?}");
        }
        for status in [99, 1000, -1] {
            assert_eq!(wire("GET", status, "x", &[]).0, server_error, "{status}");
        }
        let (head, _) = wire("HEAD", 200, "hello", &[]);
        assert!(head.ends_with("content-length: 5\r\n\r\n"), "{head:?}");
        for status in [204, 304, 101] {
            let (w, keep) = wire("GET", status, "nobody", &[]);
            assert!(
                w.ends_with("\r\n\r\n") && !w.contains("content-length"),
                "{w:?}"
            );
            assert_eq!(keep, status != 101);
        }
        let (w, keep) = wire(
            "GET",
            200,
            "hello",
            &[("Transfer-Encoding", "chunked"), ("Connection", "close")],
        );
        assert!(!keep);
        assert_eq!(
            w,
            "HTTP/1.1 200 OK\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: 5\r\nconnection: close\r\n\r\nhello"
        );
    }
}
