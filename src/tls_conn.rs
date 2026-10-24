//! The TLS server's connections for the MIR backends (`TlsListener` /
//! `TlsStream`, docs/library/io.md § TLS server): a rustls server
//! connection over a non-blocking `TcpStream`. The MIR interpreter
//! (`src/mir/interp.rs`) and the compiled backend's runtime share it by
//! `#[path]`, as they do `http1.rs`, so a connection behaves the same on
//! both. Plain `std` and rustls, no `karac` types.
//!
//! The handshake runs inside the first `read` or `write`, as Go's
//! `tls.Conn` does, so a slow client never holds up `accept`. Each side
//! supplies the wait for the socket: the executor's readiness check calls
//! [`TlsConn::ready`], which makes progress without waiting, and the call
//! after it then finds its bytes, or a read waits through `wait`.

use std::io::{self, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

/// A TLS failure, `NetError.Tls`'s payload, in `TlsFailure`'s order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The handshake failed: no protocol, cipher or key in common.
    Handshake,
    /// The peer's certificate, or the peer's verdict on ours, refused.
    BadCertificate,
    /// The peer broke the TLS record protocol after the handshake.
    Protocol,
    /// The connection ended without the peer's `close_notify`.
    UnexpectedClose,
}

impl Failure {
    /// Its index in `TlsFailure`.
    pub fn code(self) -> i32 {
        match self {
            Failure::Handshake => 0,
            Failure::BadCertificate => 1,
            Failure::Protocol => 2,
            Failure::UnexpectedClose => 3,
        }
    }
}

/// Why a TLS call failed: the socket's own error, or a TLS one.
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Tls(Failure),
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}

/// A server config from PEM text: the certificate chain, then the
/// private key (PKCS#8, PKCS#1 or SEC1). `None` when either does not
/// parse or they do not match.
pub fn server_config(cert_pem: &[u8], key_pem: &[u8]) -> Option<Arc<rustls::ServerConfig>> {
    let certs: Vec<_> = rustls_pemfile::certs(&mut &cert_pem[..])
        .collect::<Result<_, _>>()
        .ok()?;
    if certs.is_empty() {
        return None;
    }
    let key = rustls_pemfile::private_key(&mut &key_pem[..]).ok()??;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .ok()?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .ok()?;
    Some(Arc::new(config))
}

/// One accepted connection's TLS state.
pub struct TlsConn {
    conn: rustls::ServerConnection,
    /// The peer sent `close_notify`: reads give 0 from now on.
    closed: bool,
    /// A failure already met, which every later call reports.
    failed: Option<Failure>,
}

impl TlsConn {
    pub fn new(config: &Arc<rustls::ServerConfig>) -> Option<TlsConn> {
        Some(TlsConn {
            conn: rustls::ServerConnection::new(config.clone()).ok()?,
            closed: false,
            failed: None,
        })
    }

    /// Moves the connection on without waiting: sends what rustls has to
    /// send, and takes in what the socket has. A socket with nothing to
    /// give is not an error.
    fn progress(&mut self, sock: &mut TcpStream) -> Result<(), Error> {
        if let Some(f) = self.failed {
            return Err(Error::Tls(f));
        }
        self.flush(sock)?;
        if self.closed || !self.conn.wants_read() {
            return Ok(());
        }
        match self.conn.read_tls(sock) {
            // The TCP stream ended. Without `close_notify` that is a
            // truncation, unless the handshake never began.
            Ok(0) => {
                let f = if self.conn.is_handshaking() {
                    Failure::Handshake
                } else {
                    Failure::UnexpectedClose
                };
                self.failed = Some(f);
                return Err(Error::Tls(f));
            }
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
            Err(e) if e.kind() == ErrorKind::Interrupted => return Ok(()),
            Err(e) => return Err(Error::Io(e)),
        }
        match self.conn.process_new_packets() {
            Ok(state) => {
                if state.peer_has_closed() {
                    self.closed = true;
                }
            }
            Err(e) => {
                // An alert for the peer, as far as the socket takes it.
                let _ = self.flush(sock);
                let f = failure(&e, self.conn.is_handshaking());
                self.failed = Some(f);
                return Err(Error::Tls(f));
            }
        }
        self.flush(sock)
    }

    /// Writes out what rustls holds for the peer, as far as the socket
    /// takes it now.
    fn flush(&mut self, sock: &mut TcpStream) -> Result<(), Error> {
        while self.conn.wants_write() {
            match self.conn.write_tls(sock) {
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(Error::Io(e)),
            }
        }
        Ok(())
    }

    /// Whether a read (`write` false) or a write would complete now,
    /// making progress first: for a read, plaintext to give, the peer's
    /// close, or a failure to report; for a write, the handshake done and
    /// the earlier output gone. Also which way the socket must turn ready
    /// for that to change: `true` for writable.
    pub fn ready(&mut self, sock: &mut TcpStream, write: bool) -> (bool, bool) {
        if self.progress(sock).is_err() {
            return (true, false);
        }
        let wants_out = self.conn.wants_write();
        if write {
            let ready = !self.conn.is_handshaking() && !wants_out;
            return (ready, wants_out);
        }
        let ready = self.closed || self.has_plaintext();
        (ready, wants_out)
    }

    /// Whether rustls holds plaintext the program has not read. With no
    /// new records, `process_new_packets` only reports the state.
    fn has_plaintext(&mut self) -> bool {
        self.conn
            .process_new_packets()
            .map(|s| s.plaintext_bytes_to_read() > 0)
            .unwrap_or(true)
    }

    /// Reads plaintext into `buf`: the count, 0 once the peer has closed,
    /// or the failure. Waits through `wait(writable)` while there is none,
    /// which answers `Ok(false)` when it gave up (the read timeout).
    pub fn read(
        &mut self,
        sock: &mut TcpStream,
        buf: &mut [u8],
        wait: &mut dyn FnMut(bool) -> io::Result<bool>,
    ) -> Result<usize, Error> {
        loop {
            self.progress(sock)?;
            // 0 only after the peer's `close_notify`.
            match self.conn.reader().read(buf) {
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
                    self.failed = Some(Failure::UnexpectedClose);
                    return Err(Error::Tls(Failure::UnexpectedClose));
                }
                Err(e) => return Err(Error::Io(e)),
            }
            if self.closed {
                return Ok(0);
            }
            if !wait(self.conn.wants_write())? {
                return Err(Error::Io(ErrorKind::TimedOut.into()));
            }
        }
    }

    /// Writes `buf` as plaintext: the count rustls took, which is all of
    /// it once the handshake is done, sent as far as the socket takes it
    /// now; the rest goes with the next call or the close. Waits through
    /// `wait` for the handshake.
    pub fn write(
        &mut self,
        sock: &mut TcpStream,
        buf: &[u8],
        wait: &mut dyn FnMut(bool) -> io::Result<bool>,
    ) -> Result<usize, Error> {
        loop {
            self.progress(sock)?;
            if !self.conn.is_handshaking() {
                break;
            }
            if !wait(self.conn.wants_write())? {
                return Err(Error::Io(ErrorKind::TimedOut.into()));
            }
        }
        let n = self.conn.writer().write(buf)?;
        self.flush(sock)?;
        // What the socket did not take yet is sent before the call returns
        // when it is all that is left, so a reply is not held back.
        while self.conn.wants_write() {
            if !wait(true)? {
                break;
            }
            self.flush(sock)?;
        }
        Ok(n)
    }

    /// Sends `close_notify`, as far as the socket takes it now.
    pub fn close(&mut self, sock: &mut TcpStream) {
        self.conn.send_close_notify();
        let _ = self.flush(sock);
    }
}

/// The `TlsFailure` a rustls error is.
fn failure(e: &rustls::Error, handshaking: bool) -> Failure {
    use rustls::Error as E;
    match e {
        E::InvalidCertificate(_) | E::NoCertificatesPresented => Failure::BadCertificate,
        E::AlertReceived(
            rustls::AlertDescription::BadCertificate
            | rustls::AlertDescription::UnknownCA
            | rustls::AlertDescription::CertificateExpired
            | rustls::AlertDescription::CertificateUnknown
            | rustls::AlertDescription::CertificateRevoked
            | rustls::AlertDescription::UnsupportedCertificate,
        ) => Failure::BadCertificate,
        _ if handshaking => Failure::Handshake,
        _ => Failure::Protocol,
    }
}
