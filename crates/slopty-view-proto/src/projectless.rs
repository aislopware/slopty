// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Working folders of chats without a project.
//!
//! A projectless chat runs in a folder of its own under [`SCRATCH_ROOT`] on its
//! host, kept home-relative because only the host knows its home, or in the
//! host's home itself.

/// Home-relative parent of every projectless chat's folder.
pub const SCRATCH_ROOT: &str = "~/.slopty/scratch";

/// The host's home, a projectless chat's folder when it has none of its own.
fn is_home_cwd(cwd: &str) -> bool {
    matches!(cwd.trim(), "" | "~" | "~/")
}

/// A projectless chat's own folder under [`SCRATCH_ROOT`].
fn is_scratch_cwd(cwd: &str) -> bool {
    cwd.trim()
        .strip_prefix(SCRATCH_ROOT)
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some_and(|name| !name.is_empty() && !name.contains(['/', '\\']))
}

/// Whether a chat with this cwd (and no project) is projectless: no cwd, the
/// host's home, or a scratch folder.
pub fn is_projectless_cwd(cwd: Option<&str>) -> bool {
    cwd.is_none_or(|cwd| is_home_cwd(cwd) || is_scratch_cwd(cwd))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projectless_cwds() {
        for cwd in ["~", "~/", "", "~/.slopty/scratch/2026-10-11-3e4f5a6b"] {
            assert!(is_projectless_cwd(Some(cwd)), "{cwd}");
        }
        assert!(is_projectless_cwd(None));
        for cwd in [
            "/home/w/dev/slopty",
            "~/dev/slopty",
            "~/.slopty/scratch",
            "~/.slopty/scratch/",
            "~/.slopty/scratch/a/b",
            "~/.slopty/worktrees/x",
        ] {
            assert!(!is_projectless_cwd(Some(cwd)), "{cwd}");
        }
    }
}
