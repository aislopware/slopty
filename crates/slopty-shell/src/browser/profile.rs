// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Stable device-local browser storage, independent of chats and repositories.
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use slopty_view_proto::WorkspaceScope;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BrowserProfile {
    pub root: PathBuf,
}

impl BrowserProfile {
    /// The browser store of this device's workspace; `None` until the engine has attached and
    /// named its device, so no page writes into a store shared by mistake.
    pub fn for_workspace(
        data_dir: &Path,
        scope: Option<WorkspaceScope>,
        local_device_id: Option<&str>,
    ) -> Option<Self> {
        let WorkspaceScope::Local = scope?;
        Some(Self::new(data_dir, &["local", local_device_id?]))
    }

    fn new(data_dir: &Path, identity: &[&str]) -> Self {
        let mut hash = Sha256::new();
        hash.update(b"slopty-browser-profile-v1");
        for part in identity {
            hash.update((part.len() as u64).to_le_bytes());
            hash.update(part.as_bytes());
        }
        // Resolve aliases so windows using the same installation share a store.
        let data_dir = data_dir.canonicalize().unwrap_or_else(|_| {
            std::path::absolute(data_dir).unwrap_or_else(|_| data_dir.to_path_buf())
        });
        Self {
            root: data_dir.join("browser").join(data_encoding::HEXLOWER.encode(&hash.finalize())),
        }
    }

    #[cfg(any(target_os = "macos", test))]
    pub fn data_store_identifier(&self) -> [u8; 16] {
        // Include the installation directory: fixtures and alternative data roots
        // must never open a real user's WKWebsiteDataStore.
        let mut hash = Sha256::new();
        hash.update(b"slopty-wkwebsite-data-store-v1");
        hash.update(self.root.as_os_str().as_encoded_bytes());
        // The digest's first 16 bytes, as a UUID v8 (an application-defined hash).
        let mut id = [0_u8; 16];
        for (byte, digest) in id.iter_mut().zip(hash.finalize()) {
            *byte = digest;
        }
        let [_, _, _, _, _, _, version, _, variant, ..] = &mut id;
        *version = (*version & 0x0f) | 0x80;
        *variant = (*variant & 0x3f) | 0x80;
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_stable_and_scoped_to_device_and_installation() {
        let root = tempfile::tempdir().unwrap();
        let make = |parts: &[&str]| BrowserProfile::new(root.path(), parts);
        let a = make(&["local", "device-a"]);
        assert_eq!(a, make(&["local", "device-a"]));
        for parts in [["local", "device-b"], ["other", "device-a"]] {
            let b = make(&parts);
            assert_ne!(a.root, b.root);
            assert_ne!(a.data_store_identifier(), b.data_store_identifier());
        }
        let other = tempfile::tempdir().unwrap();
        let b = BrowserProfile::new(other.path(), &["local", "device-a"]);
        assert_ne!(a.data_store_identifier(), b.data_store_identifier());
        assert_ne!(make(&["ab", "c"]), make(&["a", "bc"]));
        assert!(
            make(&["../../outside"])
                .root
                .starts_with(root.path().canonicalize().unwrap().join("browser"))
        );
    }

    #[test]
    fn unresolved_identity_does_not_select_a_shared_persistent_profile() {
        let root = Path::new("/unused");
        assert!(BrowserProfile::for_workspace(root, None, None).is_none());
        assert!(BrowserProfile::for_workspace(root, None, Some("device")).is_none());
        assert!(BrowserProfile::for_workspace(root, Some(WorkspaceScope::Local), None).is_none());
        let local =
            |device| BrowserProfile::for_workspace(root, Some(WorkspaceScope::Local), Some(device));
        assert_eq!(local("one"), local("one"));
        assert_ne!(local("one"), local("two"));
    }
}
