//! The client's installation id, as `client.json` in its data dir.
//!
//! Workers are not kept here: the server's directory lists them, and the client caches that
//! (`slopty_client::directory`).

use std::path::Path;

use serde::{Deserialize, Serialize};
use slopty_core::ClientId;

use crate::NetError;

/// File name inside the client's data directory.
pub const FILE_NAME: &str = "client.json";

#[derive(Serialize, Deserialize)]
struct File {
    client_id: ClientId,
}

/// This installation's id from `client.json` inside `data_dir`, made and written there the
/// first time.
///
/// # Errors
///
/// When the file cannot be read, does not parse, or a fresh id cannot be written.
pub fn client_id_in(data_dir: &Path) -> Result<ClientId, NetError> {
    let path = data_dir.join(FILE_NAME);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let file: File = serde_json::from_slice(&bytes).map_err(NetError::Store)?;
            Ok(file.client_id)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let file = File { client_id: ClientId::default() };
            let json = serde_json::to_vec_pretty(&file).map_err(NetError::Store)?;
            let store = |e| NetError::io(path.display(), e);
            std::fs::create_dir_all(data_dir).map_err(store)?;
            slopty_platform::fs::replace(&path, &json).map_err(store)?;
            Ok(file.client_id)
        }
        Err(e) => Err(NetError::io(path.display(), e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_is_made_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("deep");
        let fresh = client_id_in(&data).unwrap();
        assert!(data.join(FILE_NAME).exists(), "a fresh id is written at once");
        assert_eq!(client_id_in(&data).unwrap(), fresh, "the id survives");
    }

    #[test]
    fn a_corrupt_file_is_an_error_not_a_fresh_start() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE_NAME), b"{not json").unwrap();
        client_id_in(dir.path()).unwrap_err();
    }
}
