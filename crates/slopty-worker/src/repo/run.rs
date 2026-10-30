//! A repository's own archive script, read from the files its other tools keep
//! (`docs/decisions/projects.md`, "The repository's run and archive scripts").
//!
//! The archive script runs in a worktree before it is removed, to stop what its setup or a run
//! left going. It is read from the same files as the setup ([`super::setup::SOURCES`]), from the
//! checkout it would run in: the first file that names one wins, and sources are never merged, as
//! with the setup.

use std::path::Path;

use super::setup::{Found, SOURCES, joined};

/// The archive script the checkout at `tree` keeps: the first of [`SOURCES`] whose archive is
/// not empty.
#[must_use]
pub fn archive(tree: &Path) -> Option<Found> {
    SOURCES.into_iter().find_map(|from| {
        let text = std::fs::read_to_string(tree.join(from)).ok()?;
        let script = read_archive(from, &text)?.trim().to_owned();
        (!script.is_empty()).then_some(Found { from, script })
    })
}

/// The archive script `text`, read as `from`, names.
fn read_archive(from: &str, text: &str) -> Option<String> {
    match from {
        // `[scripts] archive = "…"`.
        ".conductor/settings.toml" => {
            let doc: toml::Table = toml::from_str(text).ok()?;
            Some(doc.get("scripts")?.get("archive")?.as_str()?.to_owned())
        }
        // `{"scripts": {"archive": "…"}}`.
        "conductor.json" => {
            let doc: serde_json::Value = serde_json::from_str(text).ok()?;
            Some(doc.get("scripts")?.get("archive")?.as_str()?.to_owned())
        }
        // `{"teardown": ["…", …]}`.
        ".superset/config.json" => {
            let doc: serde_json::Value = serde_json::from_str(text).ok()?;
            joined(doc.get("teardown")?)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let tree = tempfile::tempdir().expect("temp");
        for (path, text) in files {
            let at = tree.path().join(path);
            std::fs::create_dir_all(at.parent().expect("parent")).expect("mkdir");
            std::fs::write(at, text).expect("write");
        }
        tree
    }

    /// Real files, as their repositories keep them (each fixture names its source): the archive
    /// script each tool would run.
    #[test]
    fn real_files_read_as_their_tools_read_them() {
        let read = |from: &str, text: &str| {
            let tree = tree_with(&[(from, text)]);
            archive(tree.path()).map(|f| f.script)
        };
        // github.com/elie222/inbox-zero, `.conductor/settings.toml`.
        let clean = "node --disable-warning=MODULE_TYPELESS_PACKAGE_JSON --experimental-strip-types scripts/dev-setup.ts clean";
        assert_eq!(
            read(".conductor/settings.toml", include_str!("run_fixtures/inbox-zero.settings.toml"))
                .as_deref(),
            Some(clean)
        );
        // github.com/avo-hq/avo, `conductor.json`.
        assert_eq!(
            read("conductor.json", include_str!("run_fixtures/avo.conductor.json")).as_deref(),
            Some("./conductor-archive")
        );
        // docs.superset.sh/setup-teardown-scripts, its example.
        assert_eq!(
            read(".superset/config.json", include_str!("run_fixtures/superset-docs.config.json"))
                .as_deref(),
            Some("docker-compose down")
        );
    }

    /// Each tool's archive script is read in its own form; the first file that keeps one wins,
    /// and a file that keeps none, or does not parse, passes to the next.
    #[test]
    fn each_tools_archive_script_is_read() {
        let conductor = "[scripts]\nsetup = \"bun install\"\narchive = \"docker compose down\"\n";
        let tree = tree_with(&[(".conductor/settings.toml", conductor)]);
        assert_eq!(
            archive(tree.path()),
            Some(Found {
                from: ".conductor/settings.toml",
                script: "docker compose down".to_owned()
            })
        );

        let superset =
            r#"{"setup":["bun i"],"teardown":["make down","rm -rf tmp"],"run":["./run.sh"]}"#;
        let tree = tree_with(&[
            ("conductor.json", r#"{"scripts":{"setup":"npm ci","archive":"  "}}"#),
            (".superset/config.json", superset),
        ]);
        assert_eq!(
            archive(tree.path()).map(|f| (f.from, f.script)),
            Some((".superset/config.json", "make down && rm -rf tmp".to_owned())),
            "an empty archive passes to the next file"
        );

        let tree = tree_with(&[("conductor.json", "{not json"), ("t3.json", r#"{"scripts":[]}"#)]);
        assert_eq!(archive(tree.path()), None, "a file that does not parse names none");
    }
}
