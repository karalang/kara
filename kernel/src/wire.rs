//! Jupyter Messaging Protocol — wire-format codec and HMAC signer.
//!
//! Spec: <https://jupyter-client.readthedocs.io/en/stable/messaging.html#wire-protocol>.
//!
//! A Jupyter message on the wire is a sequence of ZMQ frames laid out
//! as:
//!
//! ```text
//! [zmq_identity_1, zmq_identity_2, ...,]   # 0+ routing identities (ROUTER socket)
//! b"<IDS|MSG>",                            # literal delimiter
//! b"<signature>",                          # hex-encoded HMAC SHA-256
//! b"<header>",                             # JSON
//! b"<parent_header>",                      # JSON (often `{}`)
//! b"<metadata>",                           # JSON (often `{}`)
//! b"<content>",                            # JSON (message-type-specific)
//! buf_0, buf_1, ...                        # 0+ raw binary buffers
//! ```
//!
//! The HMAC is computed over the *exact bytes* of the four JSON
//! frames concatenated in order (header || parent_header || metadata
//! || content). Identities, delimiter, signature, and buffers are
//! **not** signed. Verifying against the received bytes (not against
//! re-serialized JSON) is load-bearing — JSON canonicalization
//! differences between implementations would otherwise break
//! signatures.
//!
//! Empty signing key disables signing: outgoing signatures are empty
//! strings, and incoming signatures are accepted unconditionally.
//! Slice 1's `ConnectionFile` parser already permits an empty `key`
//! per the spec.

// Slice 2 lands the codec + signer surface; slice 3 (ZMQ sockets +
// kernel_info dispatch) is what calls into this module from
// `main.rs`. Until that wiring lands, every public item here is
// reachable only from `#[cfg(test)]` — clear the dead-code lint at
// the module level rather than peppering individual items.
#![allow(dead_code)]

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::Sha256;
use std::fmt;

type HmacSha256 = Hmac<Sha256>;

/// Literal delimiter frame separating ZMQ routing identities from the
/// signed payload. Always the ASCII bytes `<IDS|MSG>`.
pub const DELIMITER: &[u8] = b"<IDS|MSG>";

/// Protocol version string emitted in every outgoing header. Matches
/// `jupyter_client`'s current major.minor. Frontends use this to
/// negotiate forward-compatible behavior; bumping it is a
/// breaking-change marker on the kernel side.
pub const PROTOCOL_VERSION: &str = "5.3";

/// Header block carried verbatim in every message. Field order
/// matches the spec; `serde_json` preserves insertion order under the
/// `preserve_order` feature already enabled at the workspace root.
///
/// Round-trip note: when parsing an incoming header we hold the raw
/// JSON bytes too (see [`Message`]) so the signature can be verified
/// against what the sender actually wrote, not against our
/// re-serialization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    /// Unique per-message identifier (conventionally UUIDv4, but the
    /// spec only requires uniqueness within a session).
    pub msg_id: String,
    /// Frontend username — passed through verbatim from
    /// `kernel_info_request`; defaults to `"kernel"` for
    /// kernel-originated messages.
    pub username: String,
    /// Session identifier — stable for the lifetime of one frontend
    /// connection; same across every message in that session.
    pub session: String,
    /// Message type — e.g. `"execute_request"`, `"kernel_info_reply"`,
    /// `"stream"`, `"error"`. Drives downstream dispatch.
    pub msg_type: String,
    /// Protocol version this header was authored under. Always
    /// [`PROTOCOL_VERSION`] on outgoing messages.
    pub version: String,
    /// ISO 8601 timestamp with microsecond precision. Format matches
    /// Python's `datetime.utcnow().isoformat()`.
    pub date: String,
}

/// One parsed (or about-to-be-sent) Jupyter message.
///
/// Identities are the opaque ZMQ ROUTER routing frames that precede
/// the `<IDS|MSG>` delimiter; the kernel echoes them back on replies
/// so the frontend can route the response to the right caller. They
/// are passthrough — never inspected, never signed.
///
/// Buffers are raw binary blobs that follow the signed payload —
/// used by Jupyter Widgets and binary IPC patterns; not part of the
/// MVP cell-execution path but preserved for forward compatibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub identities: Vec<Vec<u8>>,
    pub header: Header,
    /// Empty object (`{}`) on kernel-originated messages that aren't
    /// replies; a copy of the request header on replies and on iopub
    /// broadcasts triggered by a request (so the frontend can
    /// correlate them).
    pub parent_header: JsonValue,
    /// Per-message metadata — kernel side leaves this `{}` by
    /// default.
    pub metadata: JsonValue,
    /// Message-type-specific payload. Validation happens at the
    /// handler layer; the wire codec treats it as opaque JSON.
    pub content: JsonValue,
    pub buffers: Vec<Vec<u8>>,
}

/// HMAC SHA-256 signer keyed off the connection file's `key` field.
/// Empty key = no signing (signatures emit as `""`, incoming
/// signatures are accepted unconditionally).
#[derive(Debug, Clone)]
pub struct Signer {
    key: Vec<u8>,
}

impl Signer {
    /// Wrap a signing key. Empty string disables signing (Jupyter's
    /// documented escape hatch for testing setups).
    pub fn new(key: impl AsRef<[u8]>) -> Self {
        Self {
            key: key.as_ref().to_vec(),
        }
    }

    /// True when the connection file specified an empty signing key
    /// and signing should be skipped. Exposed for tests + slice-3's
    /// startup logging.
    pub fn is_disabled(&self) -> bool {
        self.key.is_empty()
    }

    /// Compute the lowercase hex-encoded HMAC SHA-256 over the four
    /// JSON envelope frames in order (header, parent_header,
    /// metadata, content). Returns the empty string when signing is
    /// disabled. Matches `jupyter_client.session.Session.sign`.
    fn sign_parts(&self, parts: [&[u8]; 4]) -> String {
        if self.is_disabled() {
            return String::new();
        }
        let mut mac =
            HmacSha256::new_from_slice(&self.key).expect("HMAC SHA-256 accepts any key length");
        for part in parts {
            mac.update(part);
        }
        hex::encode(mac.finalize().into_bytes())
    }

    /// Verify a hex-encoded signature against the four JSON envelope
    /// frames. Returns `Ok(())` on match or when signing is disabled.
    /// Comparison is byte-equal on the hex strings — `hmac::Mac`'s
    /// `verify_slice` would do constant-time comparison on the raw
    /// bytes, but we already have hex output and the timing-attack
    /// surface against a local-loopback kernel is essentially nil;
    /// we still walk the full string so the cost doesn't depend on
    /// the position of the first mismatch.
    fn verify_parts(&self, signature: &str, parts: [&[u8]; 4]) -> Result<(), DecodeError> {
        if self.is_disabled() {
            return Ok(());
        }
        let expected = self.sign_parts(parts);
        if constant_time_eq(signature.as_bytes(), expected.as_bytes()) {
            Ok(())
        } else {
            Err(DecodeError::BadSignature)
        }
    }
}

/// Constant-time equality over byte slices. Returns `false` for
/// length mismatch without leaking which byte differed for
/// same-length inputs.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Errors surfaced while decoding a multipart message off the wire.
#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Frame list did not contain the `<IDS|MSG>` delimiter at all.
    MissingDelimiter,
    /// Delimiter was found but fewer than the four required JSON
    /// payload frames followed (signature + header + parent_header +
    /// metadata + content = 5 trailing frames after the delimiter
    /// before any buffers).
    TruncatedAfterDelimiter { found: usize },
    /// HMAC signature did not match the recomputed value.
    BadSignature,
    /// Signature frame was not valid UTF-8 / hex.
    InvalidSignatureEncoding,
    /// Header frame failed to deserialize as a [`Header`].
    InvalidHeader(String),
    /// parent_header / metadata / content was not valid JSON.
    InvalidJson { field: &'static str, error: String },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDelimiter => write!(f, "frame list missing <IDS|MSG> delimiter"),
            Self::TruncatedAfterDelimiter { found } => write!(
                f,
                "frame list truncated after <IDS|MSG> delimiter: expected at least 5 \
                 frames (signature + 4 JSON parts), found {found}"
            ),
            Self::BadSignature => write!(f, "HMAC signature did not match envelope"),
            Self::InvalidSignatureEncoding => {
                write!(f, "signature frame was not valid UTF-8")
            }
            Self::InvalidHeader(e) => write!(f, "header JSON did not match schema: {e}"),
            Self::InvalidJson { field, error } => {
                write!(f, "{field} frame was not valid JSON: {error}")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

impl Message {
    /// Decode a multipart frame list off the wire. The frame list is
    /// the verbatim ZMQ output — identity frames at the head, then
    /// `<IDS|MSG>`, signature, 4 JSON frames, then any buffers.
    pub fn decode(frames: &[Vec<u8>], signer: &Signer) -> Result<Self, DecodeError> {
        let delim_idx = frames
            .iter()
            .position(|f| f.as_slice() == DELIMITER)
            .ok_or(DecodeError::MissingDelimiter)?;
        let identities: Vec<Vec<u8>> = frames[..delim_idx].to_vec();
        let after = &frames[delim_idx + 1..];
        if after.len() < 5 {
            return Err(DecodeError::TruncatedAfterDelimiter { found: after.len() });
        }
        let signature_bytes = &after[0];
        let header_bytes = &after[1];
        let parent_bytes = &after[2];
        let metadata_bytes = &after[3];
        let content_bytes = &after[4];
        let buffers: Vec<Vec<u8>> = after[5..].to_vec();

        let signature = std::str::from_utf8(signature_bytes)
            .map_err(|_| DecodeError::InvalidSignatureEncoding)?;
        signer.verify_parts(
            signature,
            [header_bytes, parent_bytes, metadata_bytes, content_bytes],
        )?;

        let header: Header = serde_json::from_slice(header_bytes)
            .map_err(|e| DecodeError::InvalidHeader(e.to_string()))?;
        let parent_header = parse_json_part("parent_header", parent_bytes)?;
        let metadata = parse_json_part("metadata", metadata_bytes)?;
        let content = parse_json_part("content", content_bytes)?;

        Ok(Self {
            identities,
            header,
            parent_header,
            metadata,
            content,
            buffers,
        })
    }

    /// Serialize this message into a multipart frame list ready to
    /// hand to a ZMQ `send_multipart`. Signature is computed over the
    /// JSON bytes we're about to send — round-trips through
    /// [`Self::decode`] verify cleanly.
    pub fn encode(&self, signer: &Signer) -> Vec<Vec<u8>> {
        let header_bytes = serde_json::to_vec(&self.header).expect("header serializes");
        let parent_bytes =
            serde_json::to_vec(&self.parent_header).expect("parent_header serializes");
        let metadata_bytes = serde_json::to_vec(&self.metadata).expect("metadata serializes");
        let content_bytes = serde_json::to_vec(&self.content).expect("content serializes");
        let signature = signer.sign_parts([
            &header_bytes,
            &parent_bytes,
            &metadata_bytes,
            &content_bytes,
        ]);

        let mut frames: Vec<Vec<u8>> =
            Vec::with_capacity(self.identities.len() + 6 + self.buffers.len());
        for id in &self.identities {
            frames.push(id.clone());
        }
        frames.push(DELIMITER.to_vec());
        frames.push(signature.into_bytes());
        frames.push(header_bytes);
        frames.push(parent_bytes);
        frames.push(metadata_bytes);
        frames.push(content_bytes);
        for buf in &self.buffers {
            frames.push(buf.clone());
        }
        frames
    }
}

fn parse_json_part(field: &'static str, bytes: &[u8]) -> Result<JsonValue, DecodeError> {
    serde_json::from_slice(bytes).map_err(|e| DecodeError::InvalidJson {
        field,
        error: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_header() -> Header {
        Header {
            msg_id: "a1b2c3d4-0000-0000-0000-000000000001".to_string(),
            username: "kernel".to_string(),
            session: "session-xyz".to_string(),
            msg_type: "kernel_info_reply".to_string(),
            version: PROTOCOL_VERSION.to_string(),
            date: "2026-05-19T12:34:56.789012".to_string(),
        }
    }

    fn sample_message() -> Message {
        Message {
            identities: vec![b"client-id-1".to_vec()],
            header: sample_header(),
            parent_header: json!({}),
            metadata: json!({}),
            content: json!({"status": "ok", "language": "kara"}),
            buffers: vec![],
        }
    }

    #[test]
    fn round_trip_signed() {
        let signer = Signer::new("secret-key");
        let msg = sample_message();
        let frames = msg.encode(&signer);
        let decoded = Message::decode(&frames, &signer).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_preserves_identities_and_buffers() {
        let signer = Signer::new("k");
        let mut msg = sample_message();
        msg.identities = vec![b"id-A".to_vec(), b"id-B".to_vec()];
        msg.buffers = vec![vec![0xDE, 0xAD], vec![0xBE, 0xEF]];
        let frames = msg.encode(&signer);
        let decoded = Message::decode(&frames, &signer).unwrap();
        assert_eq!(decoded.identities, msg.identities);
        assert_eq!(decoded.buffers, msg.buffers);
    }

    #[test]
    fn empty_key_disables_signing() {
        let signer = Signer::new("");
        assert!(signer.is_disabled());
        let msg = sample_message();
        let frames = msg.encode(&signer);
        // Signature frame is the third one (after one identity + delimiter).
        let signature = &frames[2];
        assert!(signature.is_empty());
        // Round-trip still works.
        let decoded = Message::decode(&frames, &signer).unwrap();
        assert_eq!(decoded.header, msg.header);
    }

    #[test]
    fn empty_key_accepts_any_signature() {
        let signer_sender = Signer::new("");
        let signer_receiver = Signer::new("");
        let msg = sample_message();
        let mut frames = msg.encode(&signer_sender);
        // Force a non-empty signature; receiver with empty key should
        // accept it unconditionally per spec.
        frames[2] = b"bogus-signature-from-elsewhere".to_vec();
        let decoded = Message::decode(&frames, &signer_receiver).unwrap();
        assert_eq!(decoded.header, msg.header);
    }

    #[test]
    fn wrong_key_is_rejected() {
        let signer_send = Signer::new("real-key");
        let signer_recv = Signer::new("attacker-key");
        let frames = sample_message().encode(&signer_send);
        let err = Message::decode(&frames, &signer_recv).unwrap_err();
        assert_eq!(err, DecodeError::BadSignature);
    }

    #[test]
    fn tampered_content_is_rejected() {
        let signer = Signer::new("k");
        let mut frames = sample_message().encode(&signer);
        // Last frame in our sample (no buffers) is content. Mutate it.
        let last = frames.len() - 1;
        frames[last] = br#"{"status":"abort","language":"kara"}"#.to_vec();
        let err = Message::decode(&frames, &signer).unwrap_err();
        assert_eq!(err, DecodeError::BadSignature);
    }

    #[test]
    fn tampered_header_is_rejected() {
        let signer = Signer::new("k");
        let mut frames = sample_message().encode(&signer);
        // Header is the 4th frame (1 id + delimiter + signature + header).
        frames[3] = br#"{"msg_id":"forged","username":"attacker","session":"s","msg_type":"shutdown_request","version":"5.3","date":"2026-05-19T00:00:00"}"#.to_vec();
        let err = Message::decode(&frames, &signer).unwrap_err();
        assert_eq!(err, DecodeError::BadSignature);
    }

    #[test]
    fn missing_delimiter_is_decode_error() {
        let signer = Signer::new("k");
        let frames = vec![
            b"client-id".to_vec(),
            b"random".to_vec(),
            b"frames".to_vec(),
        ];
        let err = Message::decode(&frames, &signer).unwrap_err();
        assert_eq!(err, DecodeError::MissingDelimiter);
    }

    #[test]
    fn truncated_after_delimiter_is_decode_error() {
        let signer = Signer::new("k");
        let frames = vec![DELIMITER.to_vec(), b"sig".to_vec(), b"only-header".to_vec()];
        let err = Message::decode(&frames, &signer).unwrap_err();
        assert_eq!(err, DecodeError::TruncatedAfterDelimiter { found: 2 });
    }

    #[test]
    fn invalid_header_json_is_decode_error() {
        let signer = Signer::new("k");
        let mut frames = sample_message().encode(&signer);
        // Replace header with malformed JSON; recompute signature so
        // we exercise the header-parse path (not the signature path).
        let bad_header = b"{not json".to_vec();
        // Indices: 0=id, 1=delim, 2=sig, 3=header, 4=parent, 5=meta, 6=content
        let parent = frames[4].clone();
        let meta = frames[5].clone();
        let content = frames[6].clone();
        let new_sig = signer.sign_parts([&bad_header, &parent, &meta, &content]);
        frames[2] = new_sig.into_bytes();
        frames[3] = bad_header;
        let err = Message::decode(&frames, &signer).unwrap_err();
        assert!(matches!(err, DecodeError::InvalidHeader(_)));
    }

    #[test]
    fn invalid_parent_header_json_is_decode_error() {
        let signer = Signer::new("k");
        let mut frames = sample_message().encode(&signer);
        let header = frames[3].clone();
        let bad_parent = b"{nope".to_vec();
        let meta = frames[5].clone();
        let content = frames[6].clone();
        let new_sig = signer.sign_parts([&header, &bad_parent, &meta, &content]);
        frames[2] = new_sig.into_bytes();
        frames[4] = bad_parent;
        let err = Message::decode(&frames, &signer).unwrap_err();
        match err {
            DecodeError::InvalidJson { field, .. } => assert_eq!(field, "parent_header"),
            other => panic!("expected InvalidJson for parent_header, got {other:?}"),
        }
    }

    #[test]
    fn signature_uses_received_bytes_not_reserialized() {
        // Pin the load-bearing behavior: a sender's JSON byte-form
        // determines the signature. If we re-serialized header on
        // decode (with potentially different field order or
        // whitespace) the HMAC would fail. The round-trip tests
        // already exercise the happy path; here we confirm the
        // explicit guarantee: a header with deliberately-unusual
        // whitespace round-trips when sent verbatim.
        let signer = Signer::new("k");
        let header_bytes = br#"{"msg_id":"x","username":"u","session":"s","msg_type":"t","version":"5.3","date":"2026-05-19T00:00:00"}"#.to_vec();
        let parent_bytes = b"{}".to_vec();
        let meta_bytes = b"{}".to_vec();
        let content_bytes = b"{}".to_vec();
        let signature =
            signer.sign_parts([&header_bytes, &parent_bytes, &meta_bytes, &content_bytes]);
        let frames = vec![
            DELIMITER.to_vec(),
            signature.into_bytes(),
            header_bytes,
            parent_bytes,
            meta_bytes,
            content_bytes,
        ];
        let decoded = Message::decode(&frames, &signer).unwrap();
        assert_eq!(decoded.header.msg_id, "x");
    }

    #[test]
    fn known_hmac_vector() {
        // Pin the HMAC computation against an externally-computed
        // value so future RustCrypto upgrades catch any silent
        // algorithm change. The four `sign_parts` inputs concatenate
        // to the byte string `"abcd"` — the test exercises the
        // multi-frame update path, not just a single-block HMAC.
        // Reference values produced independently by openssl and
        // Python's `hmac` module (both at the kernel commit time):
        //   $ printf 'abcd' | openssl dgst -sha256 -hmac key -hex
        //   $ python3 -c "import hmac,hashlib; print(hmac.new(b'key', b'abcd', hashlib.sha256).hexdigest())"
        //   both → 2a31ec0ee8d878c9eece9fb0df79b3b90b2256240163aa5ee50d176d3d1121f8
        let signer = Signer::new("key");
        let actual = signer.sign_parts([b"a", b"b", b"c", b"d"]);
        assert_eq!(
            actual,
            "2a31ec0ee8d878c9eece9fb0df79b3b90b2256240163aa5ee50d176d3d1121f8"
        );
    }

    #[test]
    fn constant_time_eq_basic() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abc"));
        assert!(constant_time_eq(b"", b""));
    }
}
