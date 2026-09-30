//! Which terminal a link speaks from, proven (`docs/decisions/projects.md`, "An agent never has
//! more than the person gave it").
//!
//! A link says what it is and, inside a Slopty terminal, which terminal it runs in
//! (`SLOPTY_SESSION`). That claim alone proves nothing, since any process can set a variable.
//! So when the server starts a task's terminal it gives it a token (`SLOPTY_AGENT_TOKEN`), a
//! keyed hash of the terminal's id under a key only the server holds ([`AgentKey`]). The agent's
//! tools inherit it and show it when they dial, and the server checks it with no state of its
//! own: a restarted server, reading the same key, still knows its agents.

use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use slopty_core::SessionId;

/// The key's file in the server's data directory.
pub const KEY_FILE: &str = "agent.key";

/// What a token hashes besides the terminal's id, so no other use of the key collides with it.
const CONTEXT: &[u8] = b"slopty agent token\0";

/// The server's key for the tokens it gives task terminals.
#[derive(Clone, Copy)]
pub struct AgentKey([u8; blake3::KEY_LEN]);

impl std::fmt::Debug for AgentKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AgentKey(..)")
    }
}

impl AgentKey {
    /// This key.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; blake3::KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// The key kept in `data_dir`, made from the system's random source and kept, readable
    /// by the server's user alone, when there is none.
    ///
    /// # Errors
    /// The file cannot be read or written, or holds something other than a key.
    pub fn load_or_make(data_dir: &Path) -> io::Result<Self> {
        let path = data_dir.join(KEY_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let bytes: [u8; blake3::KEY_LEN] = bytes.try_into().map_err(|held: Vec<u8>| {
                    let (path, held) = (shown(&path), held.len());
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{path} holds {held} bytes, no key"),
                    )
                })?;
                return Ok(Self(bytes));
            }
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            Err(_) => {}
        }
        let mut bytes = [0; blake3::KEY_LEN];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        write_private(&path, &bytes)?;
        Ok(Self(bytes))
    }

    /// The token of the terminal `session`.
    #[must_use]
    pub fn token(&self, session: SessionId) -> String {
        self.hash(session).to_hex().to_string()
    }

    /// Whether `token` is the token of the terminal `session`, compared in constant time.
    #[must_use]
    pub fn vouches(&self, session: SessionId, token: &str) -> bool {
        blake3::Hash::from_hex(token.trim()).is_ok_and(|given| given == self.hash(session))
    }

    fn hash(&self, session: SessionId) -> blake3::Hash {
        let mut hasher = blake3::Hasher::new_keyed(&self.0);
        hasher.update(CONTEXT);
        hasher.update(session.to_string().as_bytes());
        hasher.finalize()
    }
}

fn shown(path: &Path) -> String {
    path.display().to_string()
}

/// Write `bytes` to a new file at `path` that only its owner may read.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial: PathBuf = path.with_extension("key.partial");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&partial)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&partial, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token vouches for its own terminal alone, under its own key alone; the key is made
    /// once, kept private, and read back the same; a file that holds no key is refused.
    #[test]
    fn a_token_vouches_for_its_terminal_under_its_key() {
        let dir = tempfile::tempdir().unwrap();
        let key = AgentKey::load_or_make(dir.path()).unwrap();
        let (mine, other) = (SessionId::new(), SessionId::new());
        let token = key.token(mine);
        assert!(key.vouches(mine, &token));
        assert!(!key.vouches(other, &token), "another terminal's");
        assert!(!key.vouches(mine, "not hex"), "no token");
        assert!(!key.vouches(mine, token.get(..32).unwrap()), "part of one");
        let again = AgentKey::load_or_make(dir.path()).unwrap();
        assert!(again.vouches(mine, &token), "a restarted server knows its agents");
        let elsewhere = AgentKey::load_or_make(&dir.path().join("elsewhere")).unwrap();
        assert!(!elsewhere.vouches(mine, &token), "another server's key");
        let mode = {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::metadata(dir.path().join(KEY_FILE)).unwrap().permissions().mode()
        };
        assert_eq!(mode & 0o777, 0o600);
        std::fs::write(dir.path().join(KEY_FILE), b"short").unwrap();
        assert!(AgentKey::load_or_make(dir.path()).is_err(), "a file that is no key");
    }
}
