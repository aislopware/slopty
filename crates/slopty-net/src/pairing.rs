//! Pairing tickets and the host's trust store.
//!
//! A host mints a one-time ticket (its address + a random token); a client presents the token in
//! its first `Hello`; the host then trusts that client's endpoint key until revoked. After that,
//! the QUIC handshake is the authentication.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iroh::{EndpointAddr, EndpointId, SecretKey};
use iroh_tickets::Ticket;
use serde::{Deserialize, Serialize};
use slopty_core::ClientId;

use crate::NetError;

/// How long a minted token stays valid.
pub const TOKEN_TTL: Duration = Duration::from_mins(10);

/// What the host shows as a QR code / string.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PairTicket {
    /// Where the host is.
    pub addr: EndpointAddr,
    /// One-time token.
    pub token: [u8; 32],
}

impl Ticket for PairTicket {
    const KIND: &'static str = "sloptypair";

    fn encode_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).unwrap_or_default()
    }

    fn decode_bytes(bytes: &[u8]) -> Result<Self, iroh_tickets::ParseError> {
        Ok(postcard::from_bytes(bytes)?)
    }
}

impl std::fmt::Display for PairTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.encode_string())
    }
}

impl std::str::FromStr for PairTicket {
    type Err = NetError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::decode_string(s.trim()).map_err(|e| NetError::Ticket(e.to_string()))
    }
}

/// A trusted client.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PairedClient {
    /// The client's stable app identity.
    pub client: ClientId,
    /// Display name from its `Hello`.
    pub name: String,
    /// Unix seconds.
    pub paired_at: u64,
}

/// On-disk shape.
#[derive(Serialize, Deserialize, Default)]
struct StoreFile {
    /// Host secret key, hex.
    secret_key: Option<String>,
    /// Trusted endpoint keys (hex) → client.
    paired: BTreeMap<String, PairedClient>,
}

/// The host's identity and trust list, persisted as JSON with mode 0600.
#[derive(Debug)]
pub struct TrustStore {
    path: PathBuf,
    secret: SecretKey,
    paired: BTreeMap<EndpointId, PairedClient>,
    tokens: Vec<([u8; 32], SystemTime)>,
}

impl TrustStore {
    /// Load, or create with a fresh key.
    pub fn open(path: &Path) -> Result<Self, NetError> {
        let file: StoreFile = match std::fs::read(path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| NetError::Store(e.to_string()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => StoreFile::default(),
            Err(e) => return Err(NetError::Store(e.to_string())),
        };
        let secret = match file.secret_key {
            Some(hex) => {
                hex.parse::<SecretKey>().map_err(|e| NetError::Store(format!("secret key: {e}")))?
            }
            None => SecretKey::generate(),
        };
        let mut paired = BTreeMap::new();
        for (id, client) in file.paired {
            let id: EndpointId =
                id.parse().map_err(|e| NetError::Store(format!("endpoint id: {e}")))?;
            paired.insert(id, client);
        }
        let store = Self { path: path.to_path_buf(), secret, paired, tokens: Vec::new() };
        store.save()?;
        Ok(store)
    }

    /// The host's key.
    #[must_use]
    pub const fn secret(&self) -> &SecretKey {
        &self.secret
    }

    /// Mint a token valid for [`TOKEN_TTL`].
    pub fn mint_token(&mut self) -> [u8; 32] {
        self.expire();
        let token = SecretKey::generate().to_bytes();
        self.tokens.push((token, SystemTime::now()));
        token
    }

    /// Whether `id` may connect without a token.
    #[must_use]
    pub fn is_paired(&self, id: &EndpointId) -> bool {
        self.paired.contains_key(id)
    }

    /// Everyone trusted.
    #[must_use]
    pub fn paired(&self) -> Vec<(EndpointId, PairedClient)> {
        self.paired.iter().map(|(k, v)| (*k, v.clone())).collect()
    }

    /// Redeem a token: trusts `id` and burns the token. False if the token is unknown/expired.
    pub fn redeem(
        &mut self,
        token: &[u8; 32],
        id: EndpointId,
        client: ClientId,
        name: &str,
    ) -> Result<bool, NetError> {
        self.expire();
        let Some(pos) = self.tokens.iter().position(|(t, _)| constant_time_eq(t, token)) else {
            return Ok(false);
        };
        self.tokens.swap_remove(pos);
        self.trust(id, client, name)?;
        Ok(true)
    }

    /// Trust without a token (local CLI, tests).
    pub fn trust(&mut self, id: EndpointId, client: ClientId, name: &str) -> Result<(), NetError> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        self.paired.insert(id, PairedClient { client, name: name.to_owned(), paired_at: now });
        self.save()
    }

    /// Revoke.
    pub fn revoke(&mut self, id: &EndpointId) -> Result<bool, NetError> {
        let removed = self.paired.remove(id).is_some();
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    fn expire(&mut self) {
        let now = SystemTime::now();
        self.tokens.retain(|(_, at)| now.duration_since(*at).map_or(true, |age| age < TOKEN_TTL));
    }

    fn save(&self) -> Result<(), NetError> {
        let file = StoreFile {
            secret_key: Some(hex(&self.secret.to_bytes())),
            paired: self.paired.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        };
        let json = serde_json::to_vec_pretty(&file).map_err(|e| NetError::Store(e.to_string()))?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| NetError::Store(e.to_string()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        write_private(&tmp, &json)?;
        std::fs::rename(&tmp, &self.path).map_err(|e| NetError::Store(e.to_string()))
    }
}

pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), NetError> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| NetError::Store(e.to_string()))?;
    f.write_all(bytes).map_err(|e| NetError::Store(e.to_string()))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(bytes)
}

fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_round_trips_as_string() {
        let t =
            PairTicket { addr: EndpointAddr::new(SecretKey::generate().public()), token: [7; 32] };
        let s = t.to_string();
        assert!(s.starts_with("sloptypair"));
        assert_eq!(s.parse::<PairTicket>().unwrap(), t);
    }

    #[test]
    fn store_persists_key_and_pairings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let client = SecretKey::generate().public();
        let (key, token) = {
            let mut store = TrustStore::open(&path).unwrap();
            let token = store.mint_token();
            assert!(!store.is_paired(&client));
            assert!(store.redeem(&token, client, ClientId::new(), "phone").unwrap());
            assert!(!store.redeem(&token, client, ClientId::new(), "again").unwrap(), "single use");
            (store.secret().to_bytes(), token)
        };
        let store = TrustStore::open(&path).unwrap();
        assert_eq!(store.secret().to_bytes(), key, "identity survives restart");
        assert!(store.is_paired(&client));
        assert_eq!(store.paired()[0].1.name, "phone");
        let mut store = store;
        assert!(
            !store.redeem(&token, client, ClientId::new(), "x").unwrap(),
            "tokens are not persisted"
        );
        assert!(store.revoke(&client).unwrap());
        assert!(!store.is_paired(&client));
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&path).unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_token_past_its_ttl_is_gone_and_equality_is_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = TrustStore::open(&dir.path().join("trust.json")).unwrap();
        let stale = [9; 32];
        let old = SystemTime::now() - TOKEN_TTL - Duration::from_secs(1);
        store.tokens.push((stale, old));
        let fresh = store.mint_token();
        let client = SecretKey::generate().public();
        assert!(!store.redeem(&stale, client, ClientId::new(), "late").unwrap(), "expired");
        assert!(store.redeem(&fresh, client, ClientId::new(), "in time").unwrap());

        let a = [0; 32];
        let mut one = a;
        one[5] = 1;
        let mut two = a;
        two[0] = 1;
        two[1] = 1;
        assert!(constant_time_eq(&a, &a));
        assert!(!constant_time_eq(&a, &one), "one byte apart");
        assert!(!constant_time_eq(&a, &two), "two bytes apart the same way");
        assert!(!constant_time_eq(&a, &[0xff; 32]));
    }
}
