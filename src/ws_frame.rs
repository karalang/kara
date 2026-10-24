//! WebSocket framing (RFC 6455) for the MIR backends: the opening
//! handshake's accept key, and a frame encoder and incremental decoder.
//! The MIR interpreter (`src/mir/interp.rs`) and the compiled backend's
//! runtime share it by `#[path]`, as they do `http1.rs`, so a connection
//! reads and writes the same bytes on both. Plain `std`, no `karac` types,
//! and no crypto crate: the handshake's SHA-1 is here, so a program that
//! links the lean (rustls-free) runtime archive can speak WebSocket over
//! plain TCP.

/// The GUID RFC 6455 §1.3 appends to the client's key.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// The `Sec-WebSocket-Accept` value answering a client's
/// `Sec-WebSocket-Key`: base64 of the SHA-1 of the key and the GUID.
pub fn accept_key(client_key: &str) -> String {
    let mut text = client_key.trim().as_bytes().to_vec();
    text.extend_from_slice(GUID.as_bytes());
    base64(&sha1(&text))
}

/// A frame's opcode (RFC 6455 §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opcode {
    Continuation,
    Text,
    Binary,
    Close,
    Ping,
    Pong,
}

impl Opcode {
    fn bits(self) -> u8 {
        match self {
            Opcode::Continuation => 0x0,
            Opcode::Text => 0x1,
            Opcode::Binary => 0x2,
            Opcode::Close => 0x8,
            Opcode::Ping => 0x9,
            Opcode::Pong => 0xA,
        }
    }

    fn from_bits(b: u8) -> Option<Opcode> {
        Some(match b {
            0x0 => Opcode::Continuation,
            0x1 => Opcode::Text,
            0x2 => Opcode::Binary,
            0x8 => Opcode::Close,
            0x9 => Opcode::Ping,
            0xA => Opcode::Pong,
            _ => return None,
        })
    }

    /// Close, Ping and Pong: never fragmented, at most 125 bytes.
    fn is_control(self) -> bool {
        matches!(self, Opcode::Close | Opcode::Ping | Opcode::Pong)
    }
}

/// One frame: whether it ends its message, its opcode, and its payload,
/// unmasked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub fin: bool,
    pub opcode: Opcode,
    pub payload: Vec<u8>,
}

/// The bytes of a frame with `payload`, masked with `mask` when given (a
/// client's frames are masked, a server's are not, §5.1).
pub fn encode(fin: bool, opcode: Opcode, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(if fin { 0x80 } else { 0 } | opcode.bits());
    let masked = if mask.is_some() { 0x80 } else { 0 };
    let n = payload.len();
    if n < 126 {
        out.push(masked | n as u8);
    } else if n <= usize::from(u16::MAX) {
        out.push(masked | 126);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(masked | 127);
        out.extend_from_slice(&(n as u64).to_be_bytes());
    }
    match mask {
        Some(m) => {
            out.extend_from_slice(&m);
            out.extend(payload.iter().enumerate().map(|(i, b)| b ^ m[i % 4]));
        }
        None => out.extend_from_slice(payload),
    }
    out
}

/// Why the bytes a peer sent are not a WebSocket stream (§7.4.1's 1002,
/// protocol error, unless noted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// A reserved bit set with no extension negotiated.
    Reserved,
    /// An opcode §5.2 reserves.
    UnknownOpcode(u8),
    /// A control frame that is fragmented or longer than 125 bytes.
    BadControl,
    /// A client frame not masked, or a server frame masked (§5.1).
    Masking,
    /// A frame longer than the decoder takes (1009, message too big).
    TooBig,
}

/// Reads frames off a byte stream as it arrives: [`Decoder::push`] the
/// bytes read, then take each complete frame with [`Decoder::next`].
pub struct Decoder {
    buf: Vec<u8>,
    /// Whether frames must be masked (a server reading a client) or must
    /// not be (a client reading a server).
    expect_masked: bool,
    /// The longest payload taken.
    limit: u64,
}

impl Decoder {
    pub fn new(expect_masked: bool, limit: u64) -> Decoder {
        Decoder {
            buf: Vec::new(),
            expect_masked,
            limit,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next complete frame, `Ok(None)` until its bytes have all
    /// arrived, or why the stream is not a WebSocket one.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<Frame>, FrameError> {
        let b = &self.buf;
        if b.len() < 2 {
            return Ok(None);
        }
        if b[0] & 0x70 != 0 {
            return Err(FrameError::Reserved);
        }
        let fin = b[0] & 0x80 != 0;
        let opcode =
            Opcode::from_bits(b[0] & 0x0F).ok_or(FrameError::UnknownOpcode(b[0] & 0x0F))?;
        let masked = b[1] & 0x80 != 0;
        if masked != self.expect_masked {
            return Err(FrameError::Masking);
        }
        let (len, mut at) = match b[1] & 0x7F {
            126 if b.len() >= 4 => (u64::from(u16::from_be_bytes([b[2], b[3]])), 4),
            127 if b.len() >= 10 => {
                let mut n = [0u8; 8];
                n.copy_from_slice(&b[2..10]);
                (u64::from_be_bytes(n), 10)
            }
            126 | 127 => return Ok(None),
            n => (u64::from(n), 2),
        };
        if opcode.is_control() && (!fin || len > 125) {
            return Err(FrameError::BadControl);
        }
        if len > self.limit {
            return Err(FrameError::TooBig);
        }
        let mask = if masked {
            if b.len() < at + 4 {
                return Ok(None);
            }
            let m = [b[at], b[at + 1], b[at + 2], b[at + 3]];
            at += 4;
            Some(m)
        } else {
            None
        };
        let end = at + len as usize;
        if b.len() < end {
            return Ok(None);
        }
        let mut payload = b[at..end].to_vec();
        if let Some(m) = mask {
            for (i, p) in payload.iter_mut().enumerate() {
                *p ^= m[i % 4];
            }
        }
        self.buf.drain(..end);
        Ok(Some(Frame {
            fin,
            opcode,
            payload,
        }))
    }
}

/// SHA-1 (FIPS 180-4), for the handshake only: RFC 6455 fixes it, and it
/// is not used for anything that relies on collision resistance.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

/// Standard base64 with padding (RFC 4648 §4).
fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_frame_accept_key_matches_rfc_6455_example() {
        // RFC 6455 §1.3's worked example.
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn ws_frame_sha1_and_base64_known_answers() {
        assert_eq!(base64(&sha1(b"abc")), "qZk+NkcGgWq6PiVxeFDCbJzQ2J0=");
        assert_eq!(base64(&sha1(b"")), "2jmj7l5rSw0yVb/vlWAYkK/YBwk=");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
    }

    #[test]
    fn ws_frame_round_trips_masked_and_unmasked_frames() {
        // RFC 6455 §5.7: a masked "Hello" from a client.
        let masked = encode(true, Opcode::Text, b"Hello", Some([0x37, 0xfa, 0x21, 0x3d]));
        assert_eq!(
            masked,
            [0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58]
        );
        let mut d = Decoder::new(true, 1 << 20);
        // Byte by byte: nothing until the frame is whole.
        for (i, b) in masked.iter().enumerate() {
            d.push(&[*b]);
            let got = d.next().unwrap();
            assert_eq!(got.is_some(), i == masked.len() - 1);
            if let Some(f) = got {
                assert_eq!(
                    (f.fin, f.opcode, f.payload),
                    (true, Opcode::Text, b"Hello".to_vec())
                );
            }
        }
        // 256 and 65536 bytes take the 16- and 64-bit lengths.
        for n in [125usize, 126, 256, 65536] {
            let payload = vec![7u8; n];
            let mut d = Decoder::new(false, 1 << 20);
            d.push(&encode(false, Opcode::Binary, &payload, None));
            let f = d.next().unwrap().unwrap();
            assert_eq!(
                (f.fin, f.opcode, f.payload.len()),
                (false, Opcode::Binary, n)
            );
            assert_eq!(d.next().unwrap(), None);
        }
    }

    #[test]
    fn ws_frame_decoder_refuses_what_rfc_6455_forbids() {
        let refuse = |bytes: &[u8], masked: bool| {
            let mut d = Decoder::new(masked, 1000);
            d.push(bytes);
            d.next().unwrap_err()
        };
        assert_eq!(refuse(&[0x81, 0x00], true), FrameError::Masking);
        assert_eq!(
            refuse(&[0x81, 0x80, 0, 0, 0, 0], false),
            FrameError::Masking
        );
        assert_eq!(refuse(&[0xC1, 0x00], false), FrameError::Reserved);
        assert_eq!(refuse(&[0x83, 0x00], false), FrameError::UnknownOpcode(3));
        assert_eq!(refuse(&[0x09, 0x00], false), FrameError::BadControl);
        assert_eq!(refuse(&[0x89, 126, 0, 200], false), FrameError::BadControl);
        assert_eq!(refuse(&[0x82, 126, 0x10, 0], false), FrameError::TooBig);
    }
}
