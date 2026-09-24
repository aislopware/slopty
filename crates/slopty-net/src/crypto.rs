//! A QUIC crypto provider that does no cryptography.
//!
//! Every host sits behind Tailscale, `WireGuard` or a VPN, which already encrypts and
//! authenticates the path; a second layer here would only cost latency and a pairing ceremony
//! (docs/decisions/transport.md). So packets go in the clear: the packet keys copy, the header
//! keys leave the header alone, and the tag is zero bytes long.
//!
//! What is left of a handshake is the exchange QUIC cannot do without, the transport
//! parameters, in one round trip:
//!
//! ```text
//! client  Initial    HELLO(params)      ─▶
//!                                       ◀─  Initial    HELLO(params)   server
//!                                       ◀─  Handshake  FINISHED
//! client  Handshake  FINISHED           ─▶
//! ```
//!
//! `HELLO` is [`MAGIC`], a big-endian `u16` length and the parameters as QUIC encodes them;
//! `FINISHED` is one byte. The FINISHED messages exist because noq moves each side to the next
//! packet space only when it has handshake bytes to send there, and marks the connection
//! established when a Handshake packet arrives after the session stops handshaking. Slopty runs
//! its own QUIC version ([`QUIC_VERSION`]), so a standard QUIC stack gets a version
//! negotiation instead of plaintext it would take for garbage.

use std::any::Any;
use std::hash::Hasher as _;
use std::sync::Arc;

use bytes::BytesMut;
use noq_proto::crypto::{
    self, CryptoError, ExportKeyingMaterialError, HandshakeTokenKey, HeaderKey, HmacKey, KeyPair,
    Keys, PacketKey, UnsupportedVersion,
};
use noq_proto::transport_parameters::TransportParameters;
use noq_proto::{ConnectError, ConnectionId, PathId, Side, TransportError, TransportErrorCode};

/// The QUIC version on the wire: "SLP1". Not a registered version and not of the reserved
/// `0x?a?a?a?a` greasing form.
pub const QUIC_VERSION: u32 = u32::from_be_bytes(*b"SLP1");

/// First bytes of a `HELLO`. The trailing number tracks nothing: protocol compatibility is
/// [`slopty_proto::PROTOCOL_VERSION`] in `Hello`, answered by a readable rejection.
pub const MAGIC: &[u8; 8] = b"slopty\0\x01";

/// The one byte of a `FINISHED`.
const FINISHED: u8 = 0xf1;

/// The largest transport-parameter block accepted (they are ~100 bytes in practice).
const MAX_PARAMS: usize = 4096;

/// Length of the retry integrity tag and of an HMAC signature.
const TAG_LEN: usize = 16;

/// A packet key and a header key that leave every byte as it is.
#[derive(Clone, Copy, Debug)]
struct Plain;

impl PacketKey for Plain {
    fn encrypt(&self, _path: PathId, _packet: u64, _buf: &mut [u8], _header_len: usize) {}

    fn decrypt(
        &self,
        _path: PathId,
        _packet: u64,
        _header: &[u8],
        _payload: &mut BytesMut,
    ) -> Result<(), CryptoError> {
        Ok(())
    }

    fn tag_len(&self) -> usize {
        0
    }

    fn confidentiality_limit(&self) -> u64 {
        u64::MAX
    }

    fn integrity_limit(&self) -> u64 {
        u64::MAX
    }
}

impl HeaderKey for Plain {
    fn decrypt(&self, _: usize, _: &mut [u8]) {}

    fn encrypt(&self, _: usize, _: &mut [u8]) {}

    fn sample_size(&self) -> usize {
        0
    }
}

/// A full set of plain keys for one packet space.
fn keys() -> Keys {
    Keys {
        header: KeyPair { local: Box::new(Plain), remote: Box::new(Plain) },
        packet: KeyPair { local: Box::new(Plain), remote: Box::new(Plain) },
    }
}

/// The client half of the provider.
#[derive(Clone, Copy, Debug, Default)]
pub struct Client;

impl crypto::ClientConfig for Client {
    fn start_session(
        &self,
        version: u32,
        _server_name: &str,
        params: &TransportParameters,
    ) -> Result<Box<dyn crypto::Session>, ConnectError> {
        if version != QUIC_VERSION {
            return Err(ConnectError::UnsupportedVersion);
        }
        Ok(Box::new(Session::new(Side::Client, params)))
    }
}

/// The server half of the provider.
#[derive(Clone, Copy, Debug, Default)]
pub struct Server;

impl crypto::ServerConfig for Server {
    fn initial_keys(
        &self,
        version: u32,
        _dst_cid: ConnectionId,
    ) -> Result<Keys, UnsupportedVersion> {
        if version == QUIC_VERSION { Ok(keys()) } else { Err(UnsupportedVersion) }
    }

    fn retry_tag(&self, _version: u32, _orig_dst_cid: ConnectionId, _packet: &[u8]) -> [u8; 16] {
        [0; TAG_LEN]
    }

    fn start_session(
        &self,
        _version: u32,
        params: &TransportParameters,
    ) -> Box<dyn crypto::Session> {
        Box::new(Session::new(Side::Server, params))
    }
}

/// Where a session stands. Each side sends `HELLO` then `FINISHED`; the server sends its
/// `HELLO` only after reading the client's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    /// Nothing sent yet.
    Start,
    /// Our `HELLO` is out; waiting for the peer's.
    AwaitHello,
    /// The peer's `HELLO` is in and ours is not (server) or its keys are not handed out
    /// (client).
    GotHello,
    /// Handshake keys handed out; `FINISHED` and the 1-RTT keys are next.
    HandshakeKeys,
    /// Our `FINISHED` is out; waiting for the peer's.
    AwaitFinished,
    /// Both `FINISHED`s exchanged.
    Done,
}

/// One connection's handshake.
struct Session {
    side: Side,
    /// Our transport parameters, encoded.
    local: Vec<u8>,
    /// The peer's, once its `HELLO` is in.
    peer: Option<TransportParameters>,
    /// Handshake bytes received and not yet consumed.
    inbox: Vec<u8>,
    stage: Stage,
    /// The peer's `FINISHED` arrived.
    finished: bool,
}

impl Session {
    fn new(side: Side, params: &TransportParameters) -> Self {
        let mut local = Vec::new();
        params.write(&mut local);
        Self { side, local, peer: None, inbox: Vec::new(), stage: Stage::Start, finished: false }
    }

    fn write_hello(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(MAGIC);
        // `local` is ~100 bytes; a block over `u16::MAX` would be a noq bug, and one this
        // side refuses to read anyway.
        let len = u16::try_from(self.local.len()).unwrap_or(u16::MAX);
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&self.local);
    }

    /// Consume a whole `HELLO` from the inbox if one is there.
    fn take_hello(&mut self) -> Result<bool, TransportError> {
        let Some(head) = self.inbox.get(..MAGIC.len()) else {
            return Ok(false);
        };
        if head != MAGIC {
            return Err(violation("not a Slopty handshake"));
        }
        let Some(&[hi, lo]) = self.inbox.get(MAGIC.len()..MAGIC.len().saturating_add(2)) else {
            return Ok(false);
        };
        let len = usize::from(u16::from_be_bytes([hi, lo]));
        if len > MAX_PARAMS {
            return Err(violation("transport parameters too long"));
        }
        let start = MAGIC.len().saturating_add(2);
        let end = start.saturating_add(len);
        let Some(mut block) = self.inbox.get(start..end) else {
            return Ok(false);
        };
        let params = TransportParameters::read(self.side, &mut block).map_err(|e| {
            TransportError::new(
                TransportErrorCode::TRANSPORT_PARAMETER_ERROR,
                format!("bad transport parameters: {e}"),
            )
        })?;
        self.peer = Some(params);
        self.inbox.drain(..end);
        Ok(true)
    }

    /// Consume a `FINISHED` from the inbox if one is there.
    fn take_finished(&mut self) -> Result<(), TransportError> {
        match self.inbox.first() {
            None => Ok(()),
            Some(&FINISHED) => {
                self.finished = true;
                self.inbox.remove(0);
                Ok(())
            }
            Some(_) => Err(violation("expected FINISHED")),
        }
    }
}

fn violation(why: &str) -> TransportError {
    TransportError::new(TransportErrorCode::PROTOCOL_VIOLATION, why.to_owned())
}

impl crypto::Session for Session {
    fn initial_keys(&self, _dst_cid: ConnectionId, _side: Side) -> Keys {
        keys()
    }

    fn handshake_data(&self) -> Option<Box<dyn Any>> {
        let data: Box<dyn Any> = Box::new(());
        self.peer.map(|_| data)
    }

    fn peer_identity(&self) -> Option<Box<dyn Any>> {
        None
    }

    fn early_crypto(&self) -> Option<(Box<dyn HeaderKey>, Box<dyn PacketKey>)> {
        None
    }

    fn early_data_accepted(&self) -> Option<bool> {
        None
    }

    fn is_handshaking(&self) -> bool {
        match self.side {
            // The client has what it needs once its FINISHED is out: noq only calls this after a
            // Handshake packet from the server, which is the server's FINISHED or its ACK.
            Side::Client => !matches!(self.stage, Stage::AwaitFinished | Stage::Done),
            Side::Server => self.stage != Stage::Done,
        }
    }

    fn read_handshake(&mut self, buf: &[u8]) -> Result<bool, TransportError> {
        self.inbox.extend_from_slice(buf);
        if self.inbox.len() > MAX_PARAMS.saturating_add(MAGIC.len()).saturating_add(3) {
            return Err(violation("handshake too long"));
        }
        let mut ready = false;
        if self.peer.is_none() {
            if !self.take_hello()? {
                return Ok(false);
            }
            ready = true;
            if matches!(self.stage, Stage::Start | Stage::AwaitHello) {
                self.stage = Stage::GotHello;
            }
        }
        self.take_finished()?;
        if !self.inbox.is_empty() {
            return Err(violation("trailing handshake bytes"));
        }
        if self.finished && self.stage == Stage::AwaitFinished {
            self.stage = Stage::Done;
        }
        Ok(ready)
    }

    fn transport_parameters(&self) -> Result<Option<TransportParameters>, TransportError> {
        Ok(self.peer)
    }

    fn write_handshake(&mut self, buf: &mut Vec<u8>) -> Option<Keys> {
        match (self.side, self.stage) {
            (Side::Client, Stage::Start) => {
                self.write_hello(buf);
                self.stage = Stage::AwaitHello;
                None
            }
            (Side::Server, Stage::GotHello) => {
                self.write_hello(buf);
                self.stage = Stage::HandshakeKeys;
                Some(keys())
            }
            (Side::Client, Stage::GotHello) => {
                self.stage = Stage::HandshakeKeys;
                Some(keys())
            }
            (_, Stage::HandshakeKeys) => {
                buf.push(FINISHED);
                self.stage = if self.finished { Stage::Done } else { Stage::AwaitFinished };
                Some(keys())
            }
            (_, Stage::Start | Stage::AwaitHello | Stage::AwaitFinished | Stage::Done) => None,
        }
    }

    fn next_1rtt_keys(&mut self) -> Option<KeyPair<Box<dyn PacketKey>>> {
        Some(KeyPair { local: Box::new(Plain), remote: Box::new(Plain) })
    }

    fn is_valid_retry(&self, _orig_dst_cid: ConnectionId, _header: &[u8], payload: &[u8]) -> bool {
        payload
            .len()
            .checked_sub(TAG_LEN)
            .and_then(|at| payload.get(at..))
            .is_some_and(|tag| tag == [0; TAG_LEN])
    }

    fn export_keying_material(
        &self,
        _output: &mut [u8],
        _label: &[u8],
        _context: &[u8],
    ) -> Result<(), ExportKeyingMaterialError> {
        Err(ExportKeyingMaterialError)
    }
}

/// A keyed hash in the shape of an HMAC, for stateless-reset and address-validation tokens.
///
/// Not a MAC an attacker cannot forge: the network is the trust boundary here, as for
/// everything else in this module. The key is the same in every Slopty process so a restarted
/// host answers the old connection's packets with a reset the client recognises, rather than
/// leaving it to time out.
#[derive(Clone, Copy, Debug, Default)]
pub struct Keyed;

/// Fixed key mixed into [`Keyed`].
const KEYED: &[u8] = b"slopty keyed hash";

impl Keyed {
    fn tag(data: &[u8], out: &mut [u8]) {
        for (block, chunk) in (0_u64..).zip(out.chunks_mut(8)) {
            let mut h = std::hash::DefaultHasher::new();
            h.write(KEYED);
            h.write_u64(block);
            h.write(data);
            let bytes = h.finish().to_le_bytes();
            for (o, b) in chunk.iter_mut().zip(bytes) {
                *o = b;
            }
        }
    }
}

impl HmacKey for Keyed {
    fn sign(&self, data: &[u8], signature_out: &mut [u8]) {
        Self::tag(data, signature_out);
    }

    fn signature_len(&self) -> usize {
        TAG_LEN
    }

    fn verify(&self, data: &[u8], signature: &[u8]) -> Result<(), CryptoError> {
        let mut expected = [0; TAG_LEN];
        Self::tag(data, &mut expected);
        if signature == expected { Ok(()) } else { Err(CryptoError) }
    }
}

impl HandshakeTokenKey for Keyed {
    fn seal(&self, token_nonce: u128, data: &mut Vec<u8>) -> Result<(), CryptoError> {
        let mut tag = [0; TAG_LEN];
        let mut input = token_nonce.to_le_bytes().to_vec();
        input.extend_from_slice(data);
        Self::tag(&input, &mut tag);
        data.extend_from_slice(&tag);
        Ok(())
    }

    fn open<'a>(&self, token_nonce: u128, data: &'a mut [u8]) -> Result<&'a [u8], CryptoError> {
        let at = data.len().checked_sub(TAG_LEN).ok_or(CryptoError)?;
        let (plain, tag) = data.split_at(at);
        let mut input = token_nonce.to_le_bytes().to_vec();
        input.extend_from_slice(plain);
        let mut expected = [0; TAG_LEN];
        Self::tag(&input, &mut expected);
        if tag != expected {
            return Err(CryptoError);
        }
        Ok(plain)
    }
}

/// The client side's QUIC config over this provider.
#[must_use]
pub fn client_config() -> noq::ClientConfig {
    let mut config = noq::ClientConfig::new(Arc::new(Client));
    config.version(QUIC_VERSION);
    config
}

/// The server side's QUIC config over this provider, without its transport config.
#[must_use]
pub fn server_config() -> noq::ServerConfig {
    let mut config = noq::ServerConfig::new(Arc::new(Server), Arc::new(Keyed));
    // Address-validation tokens let a returning client skip a check this server never makes.
    config.validation_token.sent(0);
    config
}

/// The endpoint config: Slopty's QUIC version only, and the shared reset key.
#[must_use]
pub fn endpoint_config() -> noq::EndpointConfig {
    let mut config = noq::EndpointConfig::new(Arc::new(Keyed));
    config.supported_versions(vec![QUIC_VERSION]);
    config
}

#[cfg(test)]
mod tests {
    use noq_proto::crypto::{ClientConfig as _, ServerConfig as _};

    use super::*;

    /// The protocol defaults but for a 30 s idle timeout (noq's own constructor is private,
    /// and an all-default block encodes to nothing).
    fn params() -> TransportParameters {
        let block = [0x01, 0x04, 0x80, 0x00, 0x75, 0x30];
        TransportParameters::read(Side::Client, &mut &block[..]).unwrap()
    }

    fn pair() -> (Box<dyn crypto::Session>, Box<dyn crypto::Session>) {
        let client = Client.start_session(QUIC_VERSION, "host", &params()).unwrap();
        let server = Server.start_session(QUIC_VERSION, &params());
        (client, server)
    }

    #[test]
    fn a_handshake_exchanges_parameters_in_one_round_trip() {
        let (mut client, mut server) = pair();
        let mut hello = Vec::new();
        assert!(client.write_handshake(&mut hello).is_none(), "no keys before the server speaks");
        assert!(hello.starts_with(MAGIC));
        assert!(client.is_handshaking() && server.is_handshaking());

        assert!(server.read_handshake(&hello).unwrap(), "the client's parameters are data");
        assert!(server.transport_parameters().unwrap().is_some());
        let mut reply = Vec::new();
        assert!(server.write_handshake(&mut reply).is_some(), "handshake keys");
        let mut fin = Vec::new();
        assert!(server.write_handshake(&mut fin).is_some(), "1-RTT keys");
        assert_eq!(fin, [FINISHED]);
        assert!(server.write_handshake(&mut Vec::new()).is_none());
        assert!(server.is_handshaking(), "the server waits for the client's FINISHED");

        assert!(client.read_handshake(&reply).unwrap());
        assert!(client.read_handshake(&fin).is_ok_and(|ready| !ready));
        let mut out = Vec::new();
        assert!(client.write_handshake(&mut out).is_some(), "handshake keys");
        assert!(out.is_empty());
        assert!(client.write_handshake(&mut out).is_some(), "1-RTT keys");
        assert_eq!(out, [FINISHED]);
        assert!(!client.is_handshaking());
        assert!(client.transport_parameters().unwrap().is_some());

        server.read_handshake(&out).unwrap();
        assert!(!server.is_handshaking());
        assert!(client.next_1rtt_keys().is_some() && server.next_1rtt_keys().is_some());
    }

    #[test]
    fn a_hello_split_across_frames_is_read_whole() {
        let (mut client, mut server) = pair();
        let mut hello = Vec::new();
        client.write_handshake(&mut hello);
        let (a, b) = hello.split_at(5);
        assert!(!server.read_handshake(a).unwrap(), "half a magic is not yet data");
        let (b1, b2) = b.split_at(6);
        assert!(!server.read_handshake(b1).unwrap(), "the length but not the block");
        assert!(server.read_handshake(b2).unwrap());
    }

    #[test]
    fn bad_handshake_bytes_are_an_error_not_a_panic() {
        let tls = [0x16, 0x03, 0x01, 0x02, 0x00, 0x01, 0x00, 0x01, 0xfc, 0x03, 0x03];
        let (_client, mut server) = pair();
        assert!(server.read_handshake(&tls).is_err(), "a TLS ClientHello is not ours");

        let mut bad_params = MAGIC.to_vec();
        bad_params.extend_from_slice(&3_u16.to_be_bytes());
        bad_params.extend_from_slice(&[0x40, 0xff, 0x09]);
        let (_client, mut server) = pair();
        let err = server.read_handshake(&bad_params).unwrap_err();
        assert_eq!(err.code, TransportErrorCode::TRANSPORT_PARAMETER_ERROR);

        let mut long = MAGIC.to_vec();
        long.extend_from_slice(&u16::MAX.to_be_bytes());
        let (_client, mut server) = pair();
        assert!(server.read_handshake(&long).is_err(), "a block past the cap");

        let (mut client, mut server) = pair();
        let mut hello = Vec::new();
        client.write_handshake(&mut hello);
        hello.push(0x00);
        assert!(server.read_handshake(&hello).is_err(), "junk after the hello");

        let (_client, mut server) = pair();
        assert!(server.read_handshake(&[]).is_ok_and(|ready| !ready), "nothing is not an error");
    }

    #[test]
    fn keys_leave_bytes_alone_and_never_run_out() {
        let k = Plain;
        let mut buf = BytesMut::from(&b"payload"[..]);
        PacketKey::decrypt(&k, PathId::ZERO, 7, b"hdr", &mut buf).unwrap();
        assert_eq!(&*buf, b"payload");
        assert_eq!(k.tag_len(), 0);
        assert_eq!(k.sample_size(), 0);
        assert_eq!(k.confidentiality_limit(), u64::MAX);
        assert_eq!(k.integrity_limit(), u64::MAX);
        assert!(Server.initial_keys(1, ConnectionId::new(&[1; 8])).is_err(), "QUIC v1 is refused");
        assert!(Client.start_session(1, "host", &params()).is_err());
    }

    #[test]
    fn a_retry_carries_a_tag_the_client_accepts_and_nothing_else_leaks() {
        let cid = ConnectionId::new(&[3; 8]);
        let tag = Server.retry_tag(QUIC_VERSION, cid, b"packet");
        let (client, _server) = pair();
        let mut payload = b"token".to_vec();
        payload.extend_from_slice(&tag);
        assert!(client.is_valid_retry(cid, b"header", &payload));
        assert!(!client.is_valid_retry(cid, b"header", b"short"));
        payload.push(1);
        assert!(!client.is_valid_retry(cid, b"header", &payload));
        assert!(client.export_keying_material(&mut [0; 32], b"l", b"c").is_err());
    }

    #[test]
    fn keyed_tokens_open_what_they_sealed_and_nothing_altered() {
        let key = Keyed;
        let mut token = b"address + time".to_vec();
        key.seal(42, &mut token).unwrap();
        assert_eq!(key.open(42, &mut token.clone()).unwrap(), b"address + time");
        assert!(key.open(43, &mut token.clone()).is_err(), "another nonce");
        let mut flipped = token.clone();
        flipped[0] ^= 1;
        key.open(42, &mut flipped).unwrap_err();
        assert!(key.open(42, &mut [0; 3]).is_err(), "shorter than a tag");

        let mut sig = [0; TAG_LEN];
        key.sign(b"cid", &mut sig);
        key.verify(b"cid", &sig).unwrap();
        assert!(key.verify(b"cie", &sig).is_err());
    }
}
