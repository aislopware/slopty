//! Client-side identity and the hosts it has paired with, persisted as JSON with mode 0600.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use iroh::{EndpointAddr, EndpointId, SecretKey};
use serde::{Deserialize, Serialize};
use slopty_core::{ClientId, HostId};

use crate::NetError;
use crate::pairing::{hex, write_private};

/// A host this client is paired with.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct KnownHost {
    /// Host app identity from `HelloAck`.
    pub host: HostId,
    /// Display name from `HelloAck`.
    pub name: String,
    /// Last known address (relay + direct addresses); the id inside is the credential.
    pub addr: EndpointAddr,
    /// Unix seconds.
    pub paired_at: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct IdentityFile {
    secret_key: Option<String>,
    client_id: Option<ClientId>,
    hosts: BTreeMap<String, KnownHost>,
}

/// This installation's transport key, app id, and paired hosts.
#[derive(Debug)]
pub struct Identity {
    path: PathBuf,
    secret: SecretKey,
    client: ClientId,
    hosts: BTreeMap<EndpointId, KnownHost>,
}

impl Identity {
    /// Load, or create fresh.
    pub fn open(path: &Path) -> Result<Self, NetError> {
        let file: IdentityFile = match std::fs::read(path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| NetError::Store(e.to_string()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => IdentityFile::default(),
            Err(e) => return Err(NetError::Store(e.to_string())),
        };
        let secret = match file.secret_key {
            Some(h) => {
                h.parse::<SecretKey>().map_err(|e| NetError::Store(format!("secret key: {e}")))?
            }
            None => SecretKey::generate(),
        };
        let mut hosts = BTreeMap::new();
        for (id, host) in file.hosts {
            let id: EndpointId =
                id.parse().map_err(|e| NetError::Store(format!("endpoint id: {e}")))?;
            hosts.insert(id, host);
        }
        let me = Self {
            path: path.to_path_buf(),
            secret,
            client: file.client_id.unwrap_or_default(),
            hosts,
        };
        me.save()?;
        Ok(me)
    }

    /// Transport key.
    #[must_use]
    pub const fn secret(&self) -> &SecretKey {
        &self.secret
    }

    /// App identity.
    #[must_use]
    pub const fn client(&self) -> ClientId {
        self.client
    }

    /// Paired hosts.
    #[must_use]
    pub fn hosts(&self) -> Vec<(EndpointId, KnownHost)> {
        self.hosts.iter().map(|(k, v)| (*k, v.clone())).collect()
    }

    /// Look up a host by endpoint id, or by a unique prefix of it or of its name.
    #[must_use]
    pub fn find(&self, needle: &str) -> Option<(EndpointId, KnownHost)> {
        let needle = needle.to_lowercase();
        let mut hits = self.hosts.iter().filter(|(id, h)| {
            id.to_string().starts_with(&needle) || h.name.to_lowercase().starts_with(&needle)
        });
        let first = hits.next()?;
        if hits.next().is_some() {
            return None;
        }
        Some((*first.0, first.1.clone()))
    }

    /// Remember a host after a successful pairing (or refresh its address).
    pub fn remember(&mut self, host: KnownHost) -> Result<(), NetError> {
        self.hosts.insert(host.addr.id, host);
        self.save()
    }

    /// Forget a host.
    pub fn forget(&mut self, id: &EndpointId) -> Result<bool, NetError> {
        let removed = self.hosts.remove(id).is_some();
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    fn save(&self) -> Result<(), NetError> {
        let file = IdentityFile {
            secret_key: Some(hex(&self.secret.to_bytes())),
            client_id: Some(self.client),
            hosts: self.hosts.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_persists_and_finds_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.json");
        let host_key = SecretKey::generate();
        let (client, secret) = {
            let mut me = Identity::open(&path).unwrap();
            me.remember(KnownHost {
                host: HostId::new(),
                name: "Mac Studio".to_owned(),
                addr: EndpointAddr::new(host_key.public()),
                paired_at: 1,
            })
            .unwrap();
            (me.client(), me.secret().clone())
        };
        let me = Identity::open(&path).unwrap();
        assert_eq!(me.client(), client);
        assert_eq!(me.secret().to_bytes(), secret.to_bytes());
        assert_eq!(me.find("mac").unwrap().0, host_key.public());
        assert_eq!(me.find(&host_key.public().to_string()[..8]).unwrap().1.name, "Mac Studio");
        assert!(me.find("zzz").is_none());
    }
}
