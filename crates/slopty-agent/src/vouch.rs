//! Which terminal a program speaks from, proven (`docs/decisions/projects.md`, "An agent never
//! has more than the person gave it").
//!
//! A program says which terminal it runs in (`SLOPTY_SESSION`). That claim alone proves
//! nothing, since any process can set a variable. So the worker gives every terminal it starts
//! a token beside it ([`slopty_proto::ctl::SESSION_TOKEN_ENV`]): a keyed hash of the terminal's
//! id under a key the worker keeps ([`SessionKey`]). The programs in the terminal inherit it and
//! show it: to the worker, which checks it with its key (`slopty hook reports`), and to the
//! server, which the worker gave the key when it registered. Neither keeps a token: a restarted
//! worker or server, holding the same key, still knows every terminal.

use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use slopty_core::SessionId;

/// The key's file in the worker's data directory.
pub const KEY_FILE: &str = "session.key";

/// What a token hashes besides the terminal's id, so no other use of the key collides with it.
const CONTEXT: &[u8] = b"slopty session token\0";

/// A worker's key for the tokens it gives its terminals.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SessionKey([u8; blake3::KEY_LEN]);

impl std::fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionKey(..)")
    }
}

impl SessionKey {
    /// This key.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; blake3::KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// Its bytes, for the server the worker registers with.
    #[must_use]
    pub const fn bytes(&self) -> [u8; blake3::KEY_LEN] {
        self.0
    }

    /// The key kept in `data_dir`, made from the system's random source and kept, readable
    /// by its user alone, when there is none.
    ///
    /// # Errors
    /// The file cannot be read or written, or holds something other than a key.
    pub fn load_or_make(data_dir: &Path) -> io::Result<Self> {
        let path = data_dir.join(KEY_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let bytes: [u8; blake3::KEY_LEN] = bytes.try_into().map_err(|held: Vec<u8>| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{} holds {} bytes, no key", path.display(), held.len()),
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
        let dir = tempfile::tempdir().expect("dir");
        let key = SessionKey::load_or_make(dir.path()).expect("key");
        let (mine, other) = (SessionId::new(), SessionId::new());
        let token = key.token(mine);
        assert!(key.vouches(mine, &token));
        assert!(!key.vouches(other, &token), "another terminal's");
        assert!(!key.vouches(mine, "not hex"), "no token");
        assert!(!key.vouches(mine, token.get(..32).expect("half")), "part of one");
        let again = SessionKey::load_or_make(dir.path()).expect("again");
        assert!(again.vouches(mine, &token), "a restarted worker knows its terminals");
        let elsewhere = SessionKey::load_or_make(&dir.path().join("elsewhere")).expect("other");
        assert!(!elsewhere.vouches(mine, &token), "another worker's key");
        let mode = {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::metadata(dir.path().join(KEY_FILE)).expect("meta").permissions().mode()
        };
        assert_eq!(mode & 0o777, 0o600);
        std::fs::write(dir.path().join(KEY_FILE), b"short").expect("write");
        assert!(SessionKey::load_or_make(dir.path()).is_err(), "a file that is no key");
    }
}
